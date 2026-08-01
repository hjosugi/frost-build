//! The Language Server Protocol's transport: `Content-Length` framing over a
//! pipe, and `file:` URIs.
//!
//! Written out rather than taken from a crate. The subset a `frost.toml`
//! server needs is one header, one length and a JSON body, and the dependency
//! that would replace it brings an async runtime and a generated model of the
//! whole protocol along with it. What is here is the part that is used.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Read one message, or `None` at end of input.
///
/// End of input is how an editor that exited without `shutdown` says so, and
/// it is a normal way for a session to end rather than a failure.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).context("reading a header")? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        // Header names are case-insensitive, and `Content-Type` is the other
        // one clients send. Anything else is ignored rather than rejected.
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = Some(
                    value
                        .trim()
                        .parse()
                        .with_context(|| format!("Content-Length {value:?} is not a length"))?,
                );
            }
        }
    }
    let Some(length) = length else {
        bail!("a message arrived with no Content-Length header");
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).context("reading a body")?;
    Ok(Some(
        serde_json::from_slice(&body).context("parsing a message body")?,
    ))
}

pub fn write_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub fn response(id: Value, result: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A method this server does not implement. Returning an error rather than
/// silently answering null lets a client stop asking.
pub fn method_not_found(id: Value, method: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": format!("frost lsp does not implement {method}") },
    })
}

pub fn notification(method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// `file:///path/to/frost.toml` to a path.
///
/// Only the `file` scheme: this server has nothing to say about a document it
/// cannot read from disk to compare against the workspace.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let decoded = percent_decode(uri.strip_prefix("file://")?);
    // `file:///C:/x` is the correct spelling of a Windows path and
    // `file://C:/x` is the one some clients emit; both name the same local
    // file. Anything else sitting in the authority position is a UNC share,
    // which this server does not serve — and answering `None` there is what
    // keeps it from being mistaken for a relative path.
    match decoded.strip_prefix('/') {
        Some(rest) if is_drive_path(rest) => Some(PathBuf::from(rest)),
        Some(_) => Some(PathBuf::from(decoded)),
        None if is_drive_path(&decoded) => Some(PathBuf::from(decoded)),
        None => None,
    }
}

pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // Windows canonicalization yields a `\\?\` verbatim prefix, and `\\?\UNC\`
    // for a share. An editor cannot match a URI carrying one against the
    // document it opened, so it comes off here the same way frost strips it
    // before handing a path to a child process.
    let text = match (text.strip_prefix("//?/UNC/"), text.strip_prefix("//?/")) {
        (Some(share), _) => format!("//{share}"),
        (None, Some(rest)) => rest.to_string(),
        (None, None) => text,
    };
    let absolute = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
    format!("file://{}", percent_encode(&absolute))
}

/// Whether a path begins with a Windows drive letter.
fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_framed_message_survives_a_round_trip() {
        let message = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let mut wire = Vec::new();
        write_message(&mut wire, &message).unwrap();

        let text = String::from_utf8(wire.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "), "{text}");
        assert!(text.contains("\r\n\r\n"), "{text}");

        let mut reader = std::io::BufReader::new(wire.as_slice());
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
        assert_eq!(
            read_message(&mut reader).unwrap(),
            None,
            "end of input ends the session rather than failing it"
        );
    }

    #[test]
    fn headers_are_read_the_way_clients_write_them() {
        // Two messages back to back, a Content-Type header in between, and a
        // header name in the other case: all of these appear in the wild.
        let wire = concat!(
            "Content-Length: 13\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{\"id\": 1}\r\n\r\n",
            "content-length: 8\r\n\r\n{\"id\":2}",
        );
        let mut reader = std::io::BufReader::new(wire.as_bytes());
        assert_eq!(read_message(&mut reader).unwrap().unwrap()["id"], 1);
        assert_eq!(read_message(&mut reader).unwrap().unwrap()["id"], 2);
    }

    #[test]
    fn a_body_is_read_by_length_not_by_looking_for_a_delimiter() {
        // A frost.toml diagnostic can contain anything a manifest does,
        // including braces and newlines, so the length is the only framing.
        let body = serde_json::json!({ "message": "unknown target {\"a\"}\nsecond line" });
        let mut wire = Vec::new();
        write_message(&mut wire, &body).unwrap();
        let mut reader = std::io::BufReader::new(wire.as_slice());
        assert_eq!(read_message(&mut reader).unwrap(), Some(body));
    }

    #[test]
    fn uris_and_paths_convert_both_ways() {
        let path = Path::new("/tmp/frost ws/core/frost.toml");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///tmp/frost%20ws/core/frost.toml");
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));

        assert_eq!(
            uri_to_path("file:///tmp/a/frost.toml"),
            Some(PathBuf::from("/tmp/a/frost.toml"))
        );
        assert_eq!(
            uri_to_path("untitled:Untitled-1"),
            None,
            "a document with no file has nothing to compare against the workspace"
        );
    }

    #[test]
    fn a_windows_path_survives_both_spellings_clients_use() {
        // `file://C:/x` treats the drive as an authority and is what broke
        // every request on Windows: the path came back as nothing, so no
        // document mapped and the server answered null to everything.
        for uri in [
            "file:///C:/Users/dev/repo/frost.toml",
            "file://C:/Users/dev/repo/frost.toml",
        ] {
            assert_eq!(
                uri_to_path(uri),
                Some(PathBuf::from("C:/Users/dev/repo/frost.toml")),
                "{uri}"
            );
        }
        // A real authority is a UNC share, which is not a local file.
        assert_eq!(uri_to_path("file://server/share/frost.toml"), None);
    }

    #[test]
    fn a_canonicalized_windows_path_loses_its_verbatim_prefix() {
        // `Path::canonicalize` returns `\\?\C:\…` on Windows, and the workspace
        // root is canonicalized. A URI carrying that prefix matches nothing an
        // editor opened.
        assert_eq!(
            path_to_uri(Path::new(r"\\?\C:\repo\core\frost.toml")),
            "file:///C:/repo/core/frost.toml"
        );
        assert_eq!(
            path_to_uri(Path::new(r"\\?\UNC\server\share\frost.toml")),
            "file:////server/share/frost.toml"
        );
    }
}
