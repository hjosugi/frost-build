//! The workspace, as the language server sees it.
//!
//! Every answer here is the build's answer. Diagnostics are the loader's own
//! errors, formatted the way the CLI formats them. References are
//! `graph.rdeps_closure`, the function `frost query rdeps` calls. Hover reads
//! the configured graph. Nothing re-derives what a label means or whether a
//! manifest is valid, because a second implementation of that is a second set
//! of answers, and the editor's would be the one nobody tested.
//!
//! What is here that is not in the build: where each answer goes. A label is
//! resolved to the manifest that declares it and the line that declares it,
//! by looking for the declaration — a lookup in a file, not a parse.

use std::path::{Path, PathBuf};

use frostbuild_core::graph::BuildGraph;
use frostbuild_core::graph_store::GraphStore;
use frostbuild_core::manifest::{Manifest, TargetKind, MANIFEST_FILE};

use super::locate::{byte_to_utf16, Cursor};

/// A range in a document, in LSP coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

impl Span {
    fn whole_line(line: usize, text: &str) -> Self {
        Self {
            line,
            start: 0,
            end: text.chars().map(char::len_utf16).sum(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub span: Span,
    /// Byte for byte what `frost` prints for the same mistake. An editor that
    /// shows a different sentence than the build is a second source of truth.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: Option<String>,
    /// LSP `CompletionItemKind`.
    pub kind: u32,
}

const KIND_VALUE: u32 = 12;
const KIND_FIELD: u32 = 5;
const KIND_MODULE: u32 = 9;

/// Keys a target of each kind accepts.
///
/// A table rather than a reflection of the parser, which does not expose one.
/// `every_offered_key_is_accepted_by_the_parser` keeps it from offering a key
/// that does not exist; a key added to the manifest and not added here is a
/// missing completion, which is the harmless direction.
fn keys_for(kind: TargetKind) -> &'static [&'static str] {
    match kind {
        TargetKind::CcLibrary => &["kind", "srcs", "deps", "includes", "cflags", "timeout"],
        TargetKind::CcBinary => &[
            "kind", "srcs", "deps", "includes", "cflags", "ldflags", "timeout",
        ],
        TargetKind::CcTest => &[
            "kind",
            "srcs",
            "deps",
            "includes",
            "cflags",
            "ldflags",
            "timeout",
            "shard_count",
        ],
        TargetKind::Genrule => &["kind", "cmd", "inputs", "outputs", "deps", "includes"],
        TargetKind::Test => &[
            "kind",
            "cmd",
            "tool",
            "args",
            "inputs",
            "deps",
            "env",
            "pass_env",
            "sandbox",
            "timeout",
            "shard_count",
        ],
        TargetKind::KofunBinary => &["kind", "srcs", "deps", "timeout"],
        TargetKind::Command => &[
            "kind",
            "tool",
            "args",
            "steps",
            "inputs",
            "outputs",
            "output_dirs",
            "clean_dirs",
            "deps",
            "env",
            "pass_env",
            "sandbox",
            "timeout",
            "depfile",
            "depfile_format",
            "preserve_outputs",
        ],
    }
}

/// Keys offered before `kind` says which target this is.
const UNKNOWN_KIND_KEYS: &[&str] = &["kind", "deps"];

pub struct Workspace {
    root: PathBuf,
    /// The configured graph. Present whenever the workspace configures, and
    /// the only thing that can answer about outputs or dependents.
    graph: Option<BuildGraph>,
    /// The merged manifests, kept even when a cross-file check rejected them.
    ///
    /// This is what keeps navigation working while one label is wrong: the
    /// declarations are all still there and still correct, and going quiet
    /// about them is not a service to somebody trying to fix the label.
    manifest: Option<Manifest>,
    /// The whole-workspace load failure, formatted as the CLI formats it.
    error: Option<String>,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Self {
        let mut workspace = Self {
            root,
            graph: None,
            manifest: None,
            error: None,
        };
        workspace.reload();
        workspace
    }

    /// Re-read the workspace, taking the warm path when the sources stamp says
    /// nothing changed.
    ///
    /// The warm path is the point: an editor saves constantly, and re-parsing
    /// every manifest in the workspace on each save is what makes a language
    /// server feel worse than the build it describes. A cached graph is proof
    /// that the manifests loaded, so there is nothing to report either.
    pub fn reload(&mut self) {
        let profile = "debug";
        let platform = frostbuild_core::manifest::HOST_PLATFORM;
        // A cached graph is proof that every manifest loaded and configured,
        // so it is both the answer and the absence of anything to report.
        if let Some(graph) = GraphStore::load_cached(&self.root, profile, platform) {
            self.graph = Some(graph);
            self.error = None;
            return;
        }
        let load = Manifest::load_reporting(&self.root);
        self.error = load.error.map(|error| format!("{error:#}"));
        let Some(manifest) = load.manifest else {
            // Nothing was assembled — an unreadable or unparseable root. The
            // previous state is the best remaining answer, because a manifest
            // spends most of the time it is being edited in this condition.
            return;
        };
        // Configuring assumes a validated workspace — it orders a graph whose
        // edges are known to resolve — so a manifest that failed validation is
        // never handed to it. The manifest below still knows every
        // declaration, which is what navigation runs on meanwhile.
        self.graph = if self.error.is_some() {
            None
        } else {
            match GraphStore::load_or_compile_configured(&self.root, &manifest, profile, platform) {
                Ok(graph) => Some(graph),
                Err(error) => {
                    self.error = Some(format!("{error:#}"));
                    None
                }
            }
        };
        self.manifest = Some(manifest);
    }

    /// Every target label the workspace declares, with its kind.
    ///
    /// From the graph when there is one, and from the manifests when a
    /// cross-file check rejected the workspace. Both are the loader's own
    /// view; neither is a rederivation of it.
    fn labels(&self) -> Vec<(&str, &'static str)> {
        if let Some(graph) = &self.graph {
            return graph
                .targets
                .values()
                .map(|target| (target.name.as_str(), target.kind.as_str()))
                .collect();
        }
        match &self.manifest {
            Some(manifest) => manifest
                .targets
                .iter()
                .map(|(name, target)| (name.as_str(), target.kind.as_str()))
                .collect(),
            None => Vec::new(),
        }
    }

    fn kind_of(&self, label: &str) -> Option<TargetKind> {
        if let Some(graph) = &self.graph {
            return graph.targets.get(label).map(|target| target.kind);
        }
        self.manifest
            .as_ref()?
            .targets
            .get(label)
            .map(|target| target.kind)
    }

    fn direct_deps(&self, label: &str) -> Option<&[String]> {
        if let Some(graph) = &self.graph {
            return graph
                .targets
                .get(label)
                .map(|target| target.deps.as_slice());
        }
        self.manifest
            .as_ref()?
            .targets
            .get(label)
            .map(|target| target.deps.as_slice())
    }

    /// What is wrong with `document`, as the build would say it.
    pub fn diagnostics(&self, path: &Path, text: &str) -> Vec<Diagnostic> {
        // The buffer first: it is what is on screen, it may not be saved, and
        // its own syntax and per-target errors need nothing else to be found.
        if let Err(error) = Manifest::parse_document(path, text) {
            let message = format!("{error:#}");
            return vec![Diagnostic {
                span: locate_error(text, &error, &message),
                message,
            }];
        }
        // Then the workspace, which is where a label that names nothing shows
        // up. Reported against this document only when the token it names is
        // written here.
        let Some(error) = &self.error else {
            return Vec::new();
        };
        match locate_message(text, error) {
            Some(span) => vec![Diagnostic {
                span,
                message: error.clone(),
            }],
            None => Vec::new(),
        }
    }

    pub fn completions(&self, path: &Path, cursor: &Cursor) -> Vec<Completion> {
        // A key position and a value position want different things, and the
        // difference is whether the cursor is inside a string.
        if cursor.at_key {
            return self
                .keys_in_scope(path, cursor)
                .iter()
                .map(|key| Completion {
                    label: (*key).to_string(),
                    detail: None,
                    kind: KIND_FIELD,
                })
                .collect();
        }
        if cursor.literal.is_none() {
            return Vec::new();
        }
        match cursor.key.as_deref() {
            Some("kind") => TargetKind::ALL
                .iter()
                .map(|kind| Completion {
                    label: kind.as_str().to_string(),
                    detail: None,
                    kind: KIND_VALUE,
                })
                .collect(),
            Some("tool") => self
                .tools()
                .map(|(name, program)| Completion {
                    label: name.clone(),
                    detail: Some(program.clone()),
                    kind: KIND_VALUE,
                })
                .collect(),
            // Labels are offered in full even from inside a package, because
            // the absolute spelling is the one that keeps meaning the same
            // target when the block it is written in moves.
            Some("deps") | Some("default_targets") => self
                .labels()
                .into_iter()
                .map(|(name, kind)| Completion {
                    label: self.absolute_label(name),
                    detail: Some(kind.to_string()),
                    kind: KIND_MODULE,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Named `[toolchain.tools]` entries, from whichever view is loaded.
    fn tools(&self) -> Box<dyn Iterator<Item = (&String, &String)> + '_> {
        if let Some(graph) = &self.graph {
            return Box::new(graph.toolchain.tools.iter());
        }
        match &self.manifest {
            Some(manifest) => Box::new(manifest.toolchain.tools.iter()),
            None => Box::new(std::iter::empty()),
        }
    }

    /// Where the label under the cursor is declared.
    pub fn definition(&self, path: &Path, cursor: &Cursor) -> Option<Location> {
        let label = self.label_at(path, cursor)?;
        self.declaration(&label)
    }

    /// Every target that depends on the one under the cursor.
    ///
    /// This is `graph.rdeps_closure`, which is what `frost query rdeps` calls,
    /// so the two cannot disagree. Each is reported at its own declaration.
    pub fn references(&self, path: &Path, cursor: &Cursor, include_self: bool) -> Vec<Location> {
        let Some(graph) = &self.graph else {
            return Vec::new();
        };
        let Some(label) = self.label_at(path, cursor) else {
            return Vec::new();
        };
        let Ok(rdeps) = graph.rdeps_closure(&label) else {
            return Vec::new();
        };
        rdeps
            .into_iter()
            .filter(|name| include_self || *name != label)
            .filter_map(|name| self.declaration(&name))
            .collect()
    }

    /// A summary of the target under the cursor, as Markdown.
    pub fn hover(&self, path: &Path, cursor: &Cursor) -> Option<String> {
        let label = self.label_at(path, cursor)?;
        let kind = self.kind_of(&label)?;
        let mut text = format!("**{label}** — `{}`\n", kind.as_str());

        // Outputs are a property of the configured graph, not of the text: a
        // workspace that does not configure has none to report yet, and
        // guessing them from the kind would be inventing an answer.
        if let Some(graph) = &self.graph {
            let outputs: Vec<&str> = graph
                .targets
                .get(&label)
                .into_iter()
                .flat_map(|target| target.outputs.iter())
                .map(|&file| graph.files[file].path.as_str())
                .collect();
            if outputs.is_empty() {
                text.push_str("\nDeclares no file output.\n");
            } else {
                text.push_str("\nOutputs\n");
                for output in outputs {
                    text.push_str(&format!("- `{output}`\n"));
                }
            }
        }

        match self.direct_deps(&label) {
            Some([]) | None => text.push_str("\nDepends on nothing.\n"),
            Some(deps) => {
                text.push_str(&format!("\nDirect dependencies ({})\n", deps.len()));
                for dep in deps {
                    text.push_str(&format!("- `{}`\n", self.absolute_label(dep)));
                }
            }
        }
        // The closure size comes from the same call `frost query deps` makes,
        // so the number in the hover is the number the query prints.
        if let Some(closure) = self
            .graph
            .as_ref()
            .and_then(|graph| graph.deps_closure(&label).ok())
        {
            text.push_str(&format!(
                "\n{} targets in `frost query deps {label}`.\n",
                closure.len()
            ));
        }
        Some(text)
    }

    /// Keys valid where the cursor is: the ones this target's `kind` accepts,
    /// or the two that decide it while it is still unstated.
    fn keys_in_scope(&self, path: &Path, cursor: &Cursor) -> &'static [&'static str] {
        let Some(target) = &cursor.target else {
            return &[];
        };
        let label = self.resolve(target, &self.package_of(path));
        match self.kind_of(&label) {
            Some(kind) => keys_for(kind),
            None => UNKNOWN_KIND_KEYS,
        }
    }

    /// The target label the cursor is on: a dependency it names, or the target
    /// whose `[target.<name>]` block it is in.
    fn label_at(&self, path: &Path, cursor: &Cursor) -> Option<String> {
        let package = self.package_of(path);
        if let Some(literal) = &cursor.literal {
            if matches!(cursor.key.as_deref(), Some("deps" | "default_targets")) {
                let label = self.resolve(&literal.text, &package);
                if self.knows(&label) {
                    return Some(label);
                }
            }
        }
        let target = cursor.target.as_ref()?;
        let label = self.resolve(target, &package);
        self.knows(&label).then_some(label)
    }

    fn knows(&self, label: &str) -> bool {
        self.kind_of(label).is_some()
    }

    /// A dependency as written, resolved against the package that wrote it.
    ///
    /// The root package is the one case core's rule does not cover: its
    /// targets keep bare names, so `//:app` and `app` both mean `app`.
    fn resolve(&self, raw: &str, package: &str) -> String {
        if package.is_empty() {
            return raw.strip_prefix("//:").unwrap_or(raw).to_string();
        }
        frostbuild_core::manifest::resolve_label(raw, package)
    }

    /// How a label is written when it is written from somewhere else.
    fn absolute_label(&self, label: &str) -> String {
        if label.starts_with("//") {
            label.to_string()
        } else {
            format!("//:{label}")
        }
    }

    /// The package a manifest belongs to: its directory relative to the root,
    /// with `/` separators, empty for the root manifest.
    ///
    /// The path comes from an editor's URI, so it is whatever the user opened,
    /// while the root has been resolved. On macOS that alone is enough to make
    /// them disagree — `/var` is a symlink to `/private/var` — and any
    /// symlinked checkout does the same on every host. Getting this wrong is
    /// not subtle: every file looks like it is in the root package, so every
    /// local label resolves to a target that does not exist and the server
    /// answers nothing anywhere.
    fn package_of(&self, path: &Path) -> String {
        let Some(dir) = path.parent() else {
            return String::new();
        };
        let relative = match dir.strip_prefix(&self.root) {
            Ok(relative) => relative.to_path_buf(),
            // Resolving is the fallback rather than the rule: it touches the
            // filesystem, and a document being edited may not be on it yet.
            Err(_) => match dir.canonicalize().ok().and_then(|resolved| {
                resolved
                    .strip_prefix(&self.root)
                    .map(Path::to_path_buf)
                    .ok()
            }) {
                Some(relative) => relative,
                None => return String::new(),
            },
        };
        relative.to_string_lossy().replace('\\', "/")
    }

    /// The manifest and line where a label is declared.
    fn declaration(&self, label: &str) -> Option<Location> {
        let (package, name) = match label.strip_prefix("//") {
            Some(rest) => rest.split_once(':')?,
            None => ("", label),
        };
        let path = if package.is_empty() {
            self.root.join(MANIFEST_FILE)
        } else {
            self.root.join(package).join(MANIFEST_FILE)
        };
        let text = std::fs::read_to_string(&path).ok()?;
        let span = find_target_header(&text, name)?;
        Some(Location { path, span })
    }
}

/// The line declaring `[target.<name>]`, allowing the quoted spellings TOML
/// permits for a key.
fn find_target_header(text: &str, name: &str) -> Option<Span> {
    let wanted = [
        format!("[target.{name}]"),
        format!("[target.\"{name}\"]"),
        format!("[target.'{name}']"),
    ];
    text.lines().enumerate().find_map(|(index, line)| {
        let code = line.split('#').next().unwrap_or_default().trim();
        wanted.iter().any(|form| code == form).then(|| {
            let start = line.find("[target.").unwrap_or(0);
            Span {
                line: index,
                start: byte_to_utf16(line, start),
                end: byte_to_utf16(line, line.trim_end().len()),
            }
        })
    })
}

/// Place a parse failure using the span the TOML parser recorded.
///
/// A syntax error knows exactly where it is, so use that. Everything else
/// falls back to naming a token from the message.
fn locate_error(text: &str, error: &anyhow::Error, message: &str) -> Span {
    if let Some(span) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<toml::de::Error>())
        .and_then(|toml| toml.span())
    {
        if let Some(located) = span_of_byte(text, span.start, span.end) {
            return located;
        }
    }
    locate_message(text, message)
        .unwrap_or_else(|| Span::whole_line(0, text.lines().next().unwrap_or_default()))
}

/// Find where a diagnostic belongs by looking for a token it names.
///
/// Frost's errors quote the thing they are about — `unknown dep "//a:b"` — so
/// the token is already exact and this is a search for it, not a guess at what
/// the error meant. A message whose tokens are not written in this document is
/// about a different file, and gets no diagnostic here.
fn locate_message(text: &str, message: &str) -> Option<Span> {
    let tokens = quoted_tokens(message);
    // Every token is tried exactly before any of them is approximated. A
    // message naming both a target and its bad dependency would otherwise
    // land on the target's header — which is written here — rather than on
    // the dependency, which is the thing that is wrong.
    for token in &tokens {
        let needle = format!("\"{token}\"");
        for (index, line) in text.lines().enumerate() {
            if let Some(at) = line.find(&needle) {
                return Some(Span {
                    line: index,
                    start: byte_to_utf16(line, at),
                    end: byte_to_utf16(line, at + needle.len()),
                });
            }
        }
    }
    // A label may be written in a short form the message resolved away, so
    // the block declaring it is the closest place still worth pointing at.
    tokens
        .iter()
        .filter_map(|token| find_target_header(text, token.rsplit(':').next()?))
        .next()
}

fn quoted_tokens(message: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = message;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        tokens.push(&after[..close]);
        rest = &after[close + 1..];
    }
    tokens
}

fn span_of_byte(text: &str, start: usize, end: usize) -> Option<Span> {
    let mut offset = 0usize;
    for (index, line) in text.lines().enumerate() {
        // +1 for the newline the iterator removed. A file with CRLF endings
        // shifts by one more per line, which moves the column and not the
        // line; the line is what an editor puts the squiggle on.
        let next = offset + line.len() + 1;
        if start < next {
            let in_line = start - offset;
            return Some(Span {
                line: index,
                start: byte_to_utf16(line, in_line),
                end: byte_to_utf16(line, (end - offset).min(line.len())),
            });
        }
        offset = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_key_is_accepted_by_the_parser() {
        // The table is written by hand because the parser exposes no list of
        // its own fields. Offering a key that does not exist would be worse
        // than offering none, so each one is put through the parser.
        for kind in TargetKind::ALL {
            for key in keys_for(kind) {
                if *key == "kind" {
                    continue;
                }
                let manifest = format!(
                    "[target.probe]\nkind = \"{}\"\n{key} = {}\n",
                    kind.as_str(),
                    placeholder(key)
                );
                let error = Manifest::parse_document(Path::new("frost.toml"), &manifest)
                    .err()
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_default();
                assert!(
                    !error.contains("unknown field"),
                    "{} does not accept {key}: {error}",
                    kind.as_str()
                );
            }
        }
    }

    /// A value of the right shape for a key. Its content does not matter —
    /// only that a wrong *type* is not mistaken for a rejected key.
    fn placeholder(key: &str) -> &'static str {
        match key {
            "timeout" | "shard_count" => "1",
            "sandbox" | "preserve_outputs" => "false",
            "cmd" | "tool" | "depfile" | "depfile_format" => "\"x\"",
            "env" => "{ A = \"b\" }",
            "steps" => "[{ tool = \"x\", args = [] }]",
            _ => "[]",
        }
    }

    #[test]
    fn a_diagnostic_lands_on_the_token_the_message_names() {
        let text = "\
[target.app]
kind = \"cc_binary\"
srcs = [\"src/main.c\"]
deps = [\"//core:missing\"]
";
        let span = locate_message(text, "target \"app\" has unknown dep \"//core:missing\"")
            .expect("the token is written in this document");
        assert_eq!(span.line, 3);
        assert_eq!(
            &text.lines().nth(3).unwrap()[span.start..span.end],
            "\"//core:missing\""
        );
    }

    #[test]
    fn a_message_about_another_file_is_not_reported_against_this_one() {
        let text = "[target.core]\nkind = \"cc_library\"\nsrcs = [\"src/core.c\"]\n";
        assert!(
            locate_message(
                text,
                "target \"//apps/cli:cli\" has unknown dep \"//nope:nope\""
            )
            .is_none(),
            "neither token is written here, so neither is this document's problem"
        );
    }

    #[test]
    fn a_short_dependency_spelling_falls_back_to_the_block_that_holds_it() {
        // The loader reports resolved labels, and a package may have written
        // the local form. The declaration is still the right place to point.
        let text = "[target.app]\nkind = \"cc_binary\"\nsrcs = [\"a.c\"]\ndeps = [\":gone\"]\n";
        let span = locate_message(text, "target \"//p:app\" has unknown dep \"//p:gone\"")
            .expect("falls back to the target block");
        assert_eq!(span.line, 0);
    }

    #[test]
    fn a_syntax_error_is_placed_where_the_parser_saw_it() {
        let text = "[target.app]\nkind = \"cc_binary\"\nsrcs = [\n";
        let error = Manifest::parse_document(Path::new("frost.toml"), text).unwrap_err();
        let span = locate_error(text, &error, &format!("{error:#}"));
        assert!(
            span.line >= 2,
            "the unterminated array is on line 2: {span:?}"
        );
    }

    /// macOS reaches every temp directory through `/var`, whose real path is
    /// `/private/var`, and a symlinked checkout does the same anywhere. The
    /// root has been resolved and the editor's URI has not, so a server that
    /// compared them literally would place every file in the root package and
    /// then answer nothing about any of them.
    #[cfg(unix)]
    #[test]
    fn a_document_reached_through_a_symlink_is_still_in_its_package() {
        let base = std::env::temp_dir().join(format!("frost-lsp-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(real.join("core/src")).unwrap();
        std::fs::write(
            real.join("core/src/core.c"),
            "int core(void) { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            real.join("frost.toml"),
            "[workspace]\ndefault_targets = [\"//core:core\"]\n",
        )
        .unwrap();
        std::fs::write(
            real.join("core/frost.toml"),
            "[target.core]\nkind = \"cc_library\"\nsrcs = [\"src/core.c\"]\n",
        )
        .unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // The root as `frost -C` resolves it; the document as an editor sends it.
        let workspace = Workspace::new(real.canonicalize().unwrap());
        assert_eq!(
            workspace.package_of(&link.join("core/frost.toml")),
            "core",
            "a symlinked path is the same package as the path it resolves to"
        );
        assert_eq!(workspace.package_of(&real.join("core/frost.toml")), "core");
        assert_eq!(workspace.package_of(&real.join("frost.toml")), "");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_target_header_is_found_in_every_spelling_toml_allows() {
        for text in [
            "[target.app]\n",
            "  [target.app]  \n",
            "[target.\"app\"]\n",
            "[target.'app']\n",
            "[target.app] # the entry point\n",
        ] {
            assert!(
                find_target_header(text, "app").is_some(),
                "not found in {text:?}"
            );
        }
        assert!(find_target_header("[target.application]\n", "app").is_none());
        assert!(
            find_target_header("# [target.app]\n", "app").is_none(),
            "a commented-out block declares nothing"
        );
    }
}
