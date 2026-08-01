//! `frost lsp`: the workspace frost already has, spoken as Language Server
//! Protocol.
//!
//! `frost.toml` is edited as plain TOML today. An editor knows nothing about
//! labels or kinds, so `deps = ["//core:core"]` with a typo in it is correct
//! TOML and stays silent until a build says otherwise — and the build says it
//! correctly, in a terminal, which is not where the cursor is. There is also
//! no way to jump to where a target is declared, and no way to ask what
//! depends on it.
//!
//! Frost already has all three answers: the manifest loader produces the
//! errors, the graph knows the declarations, and `query rdeps` knows the
//! dependents. So this is a projection of those onto the protocol, not a
//! second analysis — `workspace.rs` says what that costs to keep true. The VS
//! Code extension in `tools/vscode/` is the first client; anything speaking
//! LSP gets the same features from the same server.
//!
//! Everything here is one thread and one document at a time. A manifest
//! workspace is a few dozen small files, the answers are already computed by
//! the graph store, and a request that took long enough to need cancelling
//! would be a bug rather than a scheduling problem.

pub mod locate;
pub mod protocol;
pub mod workspace;

use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use workspace::{Diagnostic, Location, Span, Workspace};

/// Serve the protocol on stdin/stdout until the client disconnects.
///
/// stdout is the wire from the first byte, so nothing else may print to it.
/// Anything to say about the server itself goes to stderr, where an editor
/// collects it into its own log.
pub fn serve(root: &Path) -> Result<i32> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut server = Server::new(root.to_path_buf());

    while let Some(message) = protocol::read_message(&mut reader)? {
        for reply in server.handle(&message) {
            protocol::write_message(&mut writer, &reply)?;
        }
        if server.exited {
            break;
        }
    }
    writer.flush()?;
    // `exit` without a preceding `shutdown` is the client saying it gave up,
    // and the protocol asks a server to report that in its exit code.
    Ok(if server.shutdown_requested || !server.exited {
        0
    } else {
        1
    })
}

struct Server {
    workspace: Workspace,
    /// Open documents by URI, holding what the editor has rather than what is
    /// on disk. They differ for as long as an edit is unsaved, and the buffer
    /// is what the diagnostics are about.
    documents: BTreeMap<String, String>,
    shutdown_requested: bool,
    exited: bool,
}

impl Server {
    fn new(root: PathBuf) -> Self {
        Self {
            workspace: Workspace::new(root),
            documents: BTreeMap::new(),
            shutdown_requested: false,
            exited: false,
        }
    }

    fn handle(&mut self, message: &Value) -> Vec<Value> {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let id = message.get("id").cloned();

        match (method, id) {
            ("initialize", Some(id)) => vec![protocol::response(id, capabilities())],
            ("shutdown", Some(id)) => {
                self.shutdown_requested = true;
                vec![protocol::response(id, Value::Null)]
            }
            ("initialized", None) => Vec::new(),
            ("exit", _) => {
                self.exited = true;
                Vec::new()
            }
            ("textDocument/didOpen", None) => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let text = params["textDocument"]["text"].as_str().unwrap_or_default();
                self.documents.insert(uri.to_string(), text.to_string());
                self.publish(uri)
            }
            ("textDocument/didChange", None) => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                // Full sync: the capability asks for whole documents, so the
                // last change carries the whole text.
                if let Some(text) = params["contentChanges"]
                    .as_array()
                    .and_then(|changes| changes.last())
                    .and_then(|change| change["text"].as_str())
                {
                    self.documents.insert(uri.to_string(), text.to_string());
                }
                self.publish(uri)
            }
            ("textDocument/didSave", None) => {
                // Saving is the moment the workspace on disk changed, and the
                // only moment worth re-reading it. The graph store's sources
                // stamp decides whether that costs a parse.
                self.workspace.reload();
                self.republish()
            }
            ("textDocument/didClose", None) => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                self.documents.remove(uri);
                // Clearing is required: a diagnostic on a closed document
                // otherwise stays in the editor's list with nothing to open.
                vec![protocol::notification(
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "diagnostics": [] }),
                )]
            }
            ("textDocument/completion", Some(id)) => {
                let items = self
                    .at(params)
                    .map(|(path, cursor)| {
                        self.workspace
                            .completions(&path, &cursor)
                            .into_iter()
                            .map(|item| {
                                json!({
                                    "label": item.label,
                                    "kind": item.kind,
                                    "detail": item.detail,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                vec![protocol::response(id, json!(items))]
            }
            ("textDocument/definition", Some(id)) => {
                let location = self
                    .at(params)
                    .and_then(|(path, cursor)| self.workspace.definition(&path, &cursor));
                vec![protocol::response(
                    id,
                    location.map_or(Value::Null, |location| location_json(&location)),
                )]
            }
            ("textDocument/references", Some(id)) => {
                let include = params["context"]["includeDeclaration"]
                    .as_bool()
                    .unwrap_or(false);
                let locations = self
                    .at(params)
                    .map(|(path, cursor)| self.workspace.references(&path, &cursor, include))
                    .unwrap_or_default();
                vec![protocol::response(
                    id,
                    json!(locations.iter().map(location_json).collect::<Vec<_>>()),
                )]
            }
            ("textDocument/hover", Some(id)) => {
                let hover = self
                    .at(params)
                    .and_then(|(path, cursor)| self.workspace.hover(&path, &cursor));
                vec![protocol::response(
                    id,
                    match hover {
                        Some(markdown) => json!({
                            "contents": { "kind": "markdown", "value": markdown },
                        }),
                        None => Value::Null,
                    },
                )]
            }
            // A notification frost has nothing to do about is dropped; a
            // request is always answered, because a client waits for one.
            (method, Some(id)) => vec![protocol::method_not_found(id, method)],
            _ => Vec::new(),
        }
    }

    /// The document and cursor a positional request names.
    fn at(&self, params: &Value) -> Option<(PathBuf, locate::Cursor)> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let path = protocol::uri_to_path(uri)?;
        let text = self.text_of(uri, &path)?;
        let line = params["position"]["line"].as_u64()? as usize;
        let character = params["position"]["character"].as_u64()? as usize;
        Some((path, locate::locate(&text, line, character)))
    }

    /// The editor's copy of a document, or the file when it is not open.
    ///
    /// A client may ask about a document it never opened — a definition
    /// request landing in another package's manifest, most often — and
    /// answering from disk is better than not answering.
    fn text_of(&self, uri: &str, path: &Path) -> Option<String> {
        self.documents
            .get(uri)
            .cloned()
            .or_else(|| std::fs::read_to_string(path).ok())
    }

    fn publish(&self, uri: &str) -> Vec<Value> {
        let Some(path) = protocol::uri_to_path(uri) else {
            return Vec::new();
        };
        // Only manifests. Source files belong to their own language's server,
        // and frost has nothing to say about them that a build would not.
        if path.file_name() != Some(frostbuild_core::manifest::MANIFEST_FILE.as_ref()) {
            return Vec::new();
        }
        let Some(text) = self.documents.get(uri) else {
            return Vec::new();
        };
        let diagnostics: Vec<Value> = self
            .workspace
            .diagnostics(&path, text)
            .iter()
            .map(diagnostic_json)
            .collect();
        vec![protocol::notification(
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        )]
    }

    /// Re-publish every open document.
    ///
    /// One save can fix or break a diagnostic in a different package — that is
    /// the whole reason a workspace-wide loader is behind this — so a stale
    /// squiggle in a file nobody touched is the failure mode to avoid.
    fn republish(&self) -> Vec<Value> {
        self.documents
            .keys()
            .flat_map(|uri| self.publish(uri))
            .collect()
    }
}

fn capabilities() -> Value {
    json!({
        "capabilities": {
            // Whole documents: a manifest is small, and applying incremental
            // edits correctly is a second copy of a text buffer to get wrong.
            "textDocumentSync": {
                "openClose": true,
                "change": 1,
                "save": { "includeText": false },
            },
            "completionProvider": { "triggerCharacters": ["\"", ":", "/"] },
            "definitionProvider": true,
            "referencesProvider": true,
            "hoverProvider": true,
        },
        "serverInfo": { "name": "frost", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn diagnostic_json(diagnostic: &Diagnostic) -> Value {
    json!({
        "range": range_json(&diagnostic.span),
        "severity": 1,
        "source": "frost",
        "message": diagnostic.message,
    })
}

fn location_json(location: &Location) -> Value {
    json!({
        "uri": protocol::path_to_uri(&location.path),
        "range": range_json(&location.span),
    })
}

fn range_json(span: &Span) -> Value {
    json!({
        "start": { "line": span.line, "character": span.start },
        "end": { "line": span.line, "character": span.end },
    })
}
