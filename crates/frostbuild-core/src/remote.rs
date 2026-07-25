//! Remote action cache and CAS.
//!
//! The remote cache only ever makes a build faster. Every response is verified
//! against the digest that was asked for, an entry that cannot be verified is
//! treated as absent, and any transport failure falls back to local execution
//! rather than failing the build. Nothing here can change what a build produces
//! — only whether it had to run.
//!
//! Two backends exist because they answer different deployments with the same
//! semantics: a shared directory (NFS/SMB, or a bind-mounted CI volume) and
//! plain HTTP. Both address blobs by the digest the local CAS already uses, so
//! the layout translates to REAPI's `ContentAddressableStorage` and
//! `ActionCache` without changing what is stored.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// What one action produced, stored under a key over its *declared* inputs.
///
/// frost discovers some inputs only by running the action (a compiler's header
/// list), so a cold workspace cannot compute the full input set in advance. The
/// entry therefore records the inputs the producing run discovered together
/// with their digests: a consumer accepts it only when every one of those paths
/// currently has the recorded digest, which makes the entry a constructive
/// trace rather than a guess.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteAction {
    pub discovered: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
    pub duration_ms: u64,
}

#[derive(Debug, Default)]
pub struct RemoteCounters {
    pub action_hits: AtomicU64,
    pub action_misses: AtomicU64,
    pub blobs_downloaded: AtomicU64,
    pub bytes_downloaded: AtomicU64,
    pub blobs_uploaded: AtomicU64,
    pub bytes_uploaded: AtomicU64,
    /// Responses that failed verification, which are discarded rather than used.
    pub rejected: AtomicU64,
    /// Transport failures. Each one costs speed and nothing else.
    pub errors: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSummary {
    pub action_hits: u64,
    pub action_misses: u64,
    pub blobs_downloaded: u64,
    pub bytes_downloaded: u64,
    pub blobs_uploaded: u64,
    pub bytes_uploaded: u64,
    pub rejected: u64,
    pub errors: u64,
}

#[derive(Debug)]
enum Backend {
    /// A shared directory. `frost-cache/{ac,cas}/…`.
    Directory(PathBuf),
    Http {
        authority: String,
        /// Path prefix, without a trailing slash.
        prefix: String,
    },
}

#[derive(Debug)]
pub struct RemoteCache {
    backend: Backend,
    timeout: Duration,
    upload: bool,
    counters: RemoteCounters,
}

impl RemoteCache {
    /// Parse `--remote-cache`: `file:///path`, a bare path, or `http://host/prefix`.
    pub fn parse(spec: &str, timeout: Duration, upload: bool) -> Result<Self> {
        let backend = if let Some(rest) = spec.strip_prefix("http://") {
            let (authority, prefix) = match rest.split_once('/') {
                Some((authority, prefix)) => {
                    (authority, format!("/{}", prefix.trim_end_matches('/')))
                }
                None => (rest, String::new()),
            };
            if authority.is_empty() {
                bail!("remote cache URL has no host: {spec:?}");
            }
            Backend::Http {
                authority: if authority.contains(':') {
                    authority.to_string()
                } else {
                    format!("{authority}:80")
                },
                prefix,
            }
        } else if let Some(path) = spec.strip_prefix("file://") {
            Backend::Directory(PathBuf::from(path))
        } else if spec.starts_with("https://") {
            // Silently downgrading to plaintext, or pretending to verify a
            // certificate frost cannot check, are both worse than saying so.
            bail!("remote cache does not support https yet; terminate TLS locally and use http://");
        } else if spec.contains("://") {
            bail!("unsupported remote cache scheme: {spec:?}");
        } else {
            Backend::Directory(PathBuf::from(spec))
        };
        Ok(Self {
            backend,
            timeout,
            upload,
            counters: RemoteCounters::default(),
        })
    }

    pub fn uploads(&self) -> bool {
        self.upload
    }

    pub fn summary(&self) -> RemoteSummary {
        let counters = &self.counters;
        RemoteSummary {
            action_hits: counters.action_hits.load(Ordering::Relaxed),
            action_misses: counters.action_misses.load(Ordering::Relaxed),
            blobs_downloaded: counters.blobs_downloaded.load(Ordering::Relaxed),
            bytes_downloaded: counters.bytes_downloaded.load(Ordering::Relaxed),
            blobs_uploaded: counters.blobs_uploaded.load(Ordering::Relaxed),
            bytes_uploaded: counters.bytes_uploaded.load(Ordering::Relaxed),
            rejected: counters.rejected.load(Ordering::Relaxed),
            errors: counters.errors.load(Ordering::Relaxed),
        }
    }

    /// The recorded result for a trace key, or `None` for a miss, an
    /// unreadable entry or a transport failure.
    pub fn action(&self, key: &str) -> Option<RemoteAction> {
        match self.get("ac", key) {
            Ok(Some(bytes)) => match serde_json::from_slice::<RemoteAction>(&bytes) {
                Ok(action) => {
                    self.counters.action_hits.fetch_add(1, Ordering::Relaxed);
                    Some(action)
                }
                Err(_) => {
                    // An entry frost cannot read is not a reason to fail; it is
                    // a reason to build, and to say that it happened.
                    self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                    None
                }
            },
            Ok(None) => {
                self.counters.action_misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(_) => {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn put_action(&self, key: &str, action: &RemoteAction) {
        if !self.upload {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(action) else {
            return;
        };
        if self.put("ac", key, &bytes).is_err() {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Fetch a blob and stage it at `destination` with the mode its digest
    /// implies. Returns false for a miss, a transport failure, or bytes that do
    /// not hash to `digest` — every one of which means "build it instead".
    ///
    /// The mode is recovered rather than transported: a frost blob digest covers
    /// the executable bit alongside the content, so the digest that matches
    /// identifies the mode, and a blob whose neither mode matches is corrupt.
    pub fn stage_blob(&self, digest: &str, destination: &Path) -> bool {
        let bytes = match self.get("cas", digest) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return false,
            Err(_) => {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        };
        if std::fs::write(destination, &bytes).is_err() {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        for executable in [false, true] {
            if set_executable(destination, executable).is_err() {
                break;
            }
            if crate::hashcache::hash_file(destination).is_ok_and(|actual| actual == digest) {
                self.counters
                    .blobs_downloaded
                    .fetch_add(1, Ordering::Relaxed);
                self.counters
                    .bytes_downloaded
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                return true;
            }
        }
        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::remove_file(destination);
        false
    }

    pub fn put_blob(&self, digest: &str, source: &Path) {
        if !self.upload {
            return;
        }
        let Ok(bytes) = std::fs::read(source) else {
            return;
        };
        match self.put("cas", digest, &bytes) {
            Ok(()) => {
                self.counters.blobs_uploaded.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .bytes_uploaded
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            }
            Err(_) => {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn get(&self, kind: &str, name: &str) -> Result<Option<Vec<u8>>> {
        if !safe_name(name) {
            bail!("refusing to address remote entry {name:?}");
        }
        match &self.backend {
            Backend::Directory(root) => {
                let path = root.join(kind).join(name);
                match std::fs::read(&path) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
                }
            }
            Backend::Http { authority, prefix } => {
                self.http(authority, "GET", &format!("{prefix}/{kind}/{name}"), None)
            }
        }
    }

    fn put(&self, kind: &str, name: &str, bytes: &[u8]) -> Result<()> {
        if !safe_name(name) {
            bail!("refusing to address remote entry {name:?}");
        }
        match &self.backend {
            Backend::Directory(root) => {
                let directory = root.join(kind);
                std::fs::create_dir_all(&directory)?;
                let path = directory.join(name);
                if path.exists() {
                    return Ok(());
                }
                // A reader must never observe half of an entry, and two
                // writers of the same digest must not collide.
                let temp = directory.join(format!(
                    ".{name}.{}.{}",
                    std::process::id(),
                    NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::write(&temp, bytes)?;
                if std::fs::rename(&temp, &path).is_err() {
                    let _ = std::fs::remove_file(&temp);
                }
                Ok(())
            }
            Backend::Http { authority, prefix } => self
                .http(
                    authority,
                    "PUT",
                    &format!("{prefix}/{kind}/{name}"),
                    Some(bytes),
                )
                .map(|_| ()),
        }
    }

    /// One HTTP/1.1 request per call, connection closed afterwards.
    ///
    /// Written directly on `TcpStream` rather than pulled in as a dependency:
    /// the surface used here is a request line, `Content-Length` and a status
    /// code, and a cache client is exactly the place where a smaller
    /// dependency footprint is worth more than convenience.
    fn http(
        &self,
        authority: &str,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<Option<Vec<u8>>> {
        let address = authority
            .to_socket_addrs()
            .with_context(|| format!("resolving {authority}"))?
            .next()
            .with_context(|| format!("no address for {authority}"))?;
        let mut stream = TcpStream::connect_timeout(&address, self.timeout)
            .with_context(|| format!("connecting to {authority}"))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\
             User-Agent: frost\r\nContent-Length: {}\r\n\r\n",
            body.map_or(0, <[u8]>::len)
        )
        .into_bytes();
        if let Some(body) = body {
            request.extend_from_slice(body);
        }
        stream.write_all(&request)?;
        stream.flush()?;
        // Half-close after the complete request. A server that reads to end of
        // stream would otherwise wait for bytes that are never coming, and the
        // exchange would only end when this side's read timeout fired.
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        parse_http_response(&response)
    }
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Split a response into status and body without buffering a second copy.
fn parse_http_response(response: &[u8]) -> Result<Option<Vec<u8>>> {
    let head_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("remote cache response has no header terminator")?;
    let head = std::str::from_utf8(&response[..head_end])
        .context("remote cache response header is not UTF-8")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("remote cache response has no status code")?;
    let body = &response[head_end + 4..];
    match status {
        200..=299 => Ok(Some(body.to_vec())),
        404 | 410 => Ok(None),
        other => bail!("remote cache returned HTTP {other}"),
    }
}

/// Keys and digests are hex or base32-like tokens. Anything else could address
/// a path outside the cache, so it is refused rather than sanitized.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, executable: bool) -> std::io::Result<()> {
    // Nothing carries the bit on this host, and `hash_file` reports every file
    // as non-executable there, so only the first attempt can match.
    if executable {
        Err(std::io::Error::other("no executable bit on this host"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("frost-remote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_every_accepted_spec_and_refuses_the_rest() {
        let timeout = Duration::from_secs(1);
        assert!(RemoteCache::parse("/mnt/shared/cache", timeout, false).is_ok());
        assert!(RemoteCache::parse("file:///mnt/shared/cache", timeout, false).is_ok());
        assert!(RemoteCache::parse("http://cache.example:8080/frost", timeout, false).is_ok());
        assert!(RemoteCache::parse("http://cache.example", timeout, false).is_ok());
        // Pretending to have TLS would be worse than not having it.
        assert!(RemoteCache::parse("https://cache.example", timeout, false).is_err());
        assert!(RemoteCache::parse("grpc://cache.example", timeout, false).is_err());
        assert!(RemoteCache::parse("http:///frost", timeout, false).is_err());
    }

    #[test]
    fn a_directory_backend_round_trips_actions_and_blobs() {
        let root = temp_dir("directory");
        let cache =
            RemoteCache::parse(root.to_str().unwrap(), Duration::from_secs(1), true).unwrap();
        let key = "a".repeat(64);

        assert!(cache.action(&key).is_none(), "empty cache misses");
        let recorded = RemoteAction {
            discovered: BTreeMap::from([("include/util.h".into(), "digest".into())]),
            outputs: BTreeMap::from([("out/app".into(), "digest".into())]),
            duration_ms: 12,
        };
        cache.put_action(&key, &recorded);
        let read = cache.action(&key).expect("stored entry is found");
        assert_eq!(read.outputs, recorded.outputs);
        assert_eq!(read.discovered, recorded.discovered);

        let source = root.join("payload");
        std::fs::write(&source, b"artifact bytes").unwrap();
        let digest = crate::hashcache::hash_file(&source).unwrap();
        cache.put_blob(&digest, &source);
        let destination = root.join("restored");
        assert!(cache.stage_blob(&digest, &destination));
        assert_eq!(std::fs::read(&destination).unwrap(), b"artifact bytes");

        let summary = cache.summary();
        assert_eq!(summary.blobs_uploaded, 1);
        assert_eq!(summary.blobs_downloaded, 1);
        assert_eq!(summary.rejected, 0);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_blob_that_does_not_hash_to_its_digest_is_refused() {
        let root = temp_dir("corrupt");
        let cache =
            RemoteCache::parse(root.to_str().unwrap(), Duration::from_secs(1), true).unwrap();
        let source = root.join("payload");
        std::fs::write(&source, b"honest bytes").unwrap();
        let digest = crate::hashcache::hash_file(&source).unwrap();
        cache.put_blob(&digest, &source);

        // Someone else's cache, a truncated upload, a damaged volume: the
        // remote no longer holds what this digest names.
        std::fs::write(root.join("cas").join(&digest), b"tampered bytes").unwrap();
        let destination = root.join("restored");
        assert!(
            !cache.stage_blob(&digest, &destination),
            "a blob that does not hash to its digest must not be staged"
        );
        assert!(!destination.exists(), "and must not be left behind");
        assert_eq!(cache.summary().rejected, 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    // Only a host that has the bit can carry it. `hash_file` reports every file
    // as non-executable elsewhere, so there is no mode to recover there.
    #[cfg(unix)]
    fn executable_mode_is_recovered_from_the_digest() {
        let root = temp_dir("mode");
        let cache =
            RemoteCache::parse(root.to_str().unwrap(), Duration::from_secs(1), true).unwrap();
        let source = root.join("tool");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        set_executable(&source, true).unwrap();
        let digest = crate::hashcache::hash_file(&source).unwrap();
        cache.put_blob(&digest, &source);

        let destination = root.join("restored-tool");
        assert!(cache.stage_blob(&digest, &destination));
        // The digest covers the mode, so a staged blob that verifies has it.
        assert_eq!(
            crate::hashcache::hash_file(&destination).unwrap(),
            digest,
            "a restored executable must carry its mode"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_name_that_could_escape_the_cache_is_refused() {
        let root = temp_dir("escape");
        let cache =
            RemoteCache::parse(root.to_str().unwrap(), Duration::from_secs(1), true).unwrap();
        assert!(cache.get("cas", "../../etc/passwd").is_err());
        assert!(cache.get("cas", "").is_err());
        assert!(cache.put("ac", "a/b", b"x").is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// A minimal store-and-serve HTTP endpoint, so the request frost actually
    /// writes on a socket is exercised rather than only its response parser.
    fn serve_http_once(root: PathBuf) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            for connection in listener.incoming().take(3) {
                let mut stream = connection.unwrap();
                let mut request = Vec::new();
                // The client always closes its side, so reading to the end
                // yields exactly one complete request.
                stream.read_to_end(&mut request).unwrap();
                let head_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap();
                let head = String::from_utf8_lossy(&request[..head_end]).to_string();
                let mut words = head.split_whitespace();
                let method = words.next().unwrap_or_default().to_string();
                let path = words.next().unwrap_or_default().to_string();
                let name = path.rsplit('/').next().unwrap_or_default().to_string();
                let file = root.join(name);
                let response = match method.as_str() {
                    "PUT" => {
                        std::fs::write(&file, &request[head_end + 4..]).unwrap();
                        b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".to_vec()
                    }
                    _ => match std::fs::read(&file) {
                        Ok(bytes) => {
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                                bytes.len()
                            )
                            .into_bytes();
                            response.extend_from_slice(&bytes);
                            response
                        }
                        Err(_) => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    },
                };
                stream.write_all(&response).unwrap();
            }
        });
        (address, handle)
    }

    #[test]
    fn the_http_backend_round_trips_over_a_real_socket() {
        let root = temp_dir("http");
        let (address, server) = serve_http_once(root.clone());
        let cache = RemoteCache::parse(
            &format!("http://{address}/frost"),
            Duration::from_secs(5),
            true,
        )
        .unwrap();

        let key = "b".repeat(64);
        assert!(cache.action(&key).is_none(), "empty endpoint misses");
        let recorded = RemoteAction {
            discovered: BTreeMap::new(),
            outputs: BTreeMap::from([("out/app".into(), "digest".into())]),
            duration_ms: 7,
        };
        cache.put_action(&key, &recorded);
        assert_eq!(
            cache.action(&key).expect("stored entry is found").outputs,
            recorded.outputs
        );
        server.join().unwrap();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn http_responses_are_classified_by_status() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                .unwrap()
                .unwrap(),
            b"hi"
        );
        assert!(parse_http_response(b"HTTP/1.1 404 Not Found\r\n\r\n")
            .unwrap()
            .is_none());
        assert!(parse_http_response(b"HTTP/1.1 500 Boom\r\n\r\n").is_err());
        assert!(parse_http_response(b"not http at all").is_err());
    }
}
