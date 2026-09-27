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
//!
//! A third, `grpc://` / `grpcs://`, speaks REAPI v2 directly through
//! `frostbuild-reapi`. REAPI addresses blobs by SHA-256, while the local CAS is
//! BLAKE3-keyed, so a publication records each output's SHA-256 alongside its
//! frost digest and a consumer asks for the SHA-256 name; both digests are then
//! checked before anything is staged. REAPI has no "store this JSON under this
//! key" call, so a frost trace entry travels as the `stdout_raw` of an
//! `ActionResult` under a synthetic, deterministic `Action` digest derived from
//! the trace key. That keeps the Action Cache's key/value contract and its
//! `Get/UpdateActionResult` wire calls, without pretending the synthetic action
//! is the build action (translating that is the executor's job, not a cache's).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[cfg(feature = "reapi")]
use frostbuild_reapi::{Digest as ReapiDigest, ReapiClient, ReapiConfig};

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
    /// For a REAPI backend, the SHA-256 name of each output blob keyed by its
    /// frost (BLAKE3) digest. A directory or HTTP backend leaves it empty, and
    /// a reader from an older writer sees no entries rather than a bad one.
    #[serde(default)]
    pub remote: BTreeMap<String, String>,
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

enum Backend {
    /// A shared directory. `frost-cache/{ac,cas}/…`.
    Directory(PathBuf),
    Http {
        authority: String,
        /// Path prefix, without a trailing slash.
        prefix: String,
    },
    #[cfg(feature = "reapi")]
    Reapi(ReapiBackend),
}

/// A lazily connected REAPI backend.
///
/// `--remote-cache` is parsed before the build starts and must not connect
/// then: an endpoint that is merely unreachable has to cost speed per request
/// and nothing else, exactly as it does for the HTTP backend. The first use
/// connects, and every later use reuses the channel; a failed connection is
/// retried on the next request rather than cached as permanent.
#[cfg(feature = "reapi")]
struct ReapiBackend {
    config: ReapiConfig,
    client: std::sync::Mutex<Option<std::sync::Arc<ReapiClient>>>,
}

#[cfg(feature = "reapi")]
impl ReapiBackend {
    fn client(&self) -> Option<std::sync::Arc<ReapiClient>> {
        let mut guard = self.client.lock().unwrap();
        if let Some(client) = guard.as_ref() {
            return Some(client.clone());
        }
        match ReapiClient::connect(self.config.clone()) {
            Ok(client) => {
                let client = std::sync::Arc::new(client);
                *guard = Some(client.clone());
                Some(client)
            }
            Err(_) => None,
        }
    }

    fn display(&self) -> String {
        self.config.endpoint.clone()
    }
}

pub struct RemoteCache {
    backend: Backend,
    timeout: Duration,
    upload: bool,
    counters: RemoteCounters,
}

impl std::fmt::Debug for RemoteCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backend = match &self.backend {
            Backend::Directory(path) => format!("directory:{}", path.display()),
            Backend::Http { authority, prefix } => format!("http://{authority}{prefix}"),
            #[cfg(feature = "reapi")]
            Backend::Reapi(backend) => backend.display(),
        };
        formatter
            .debug_struct("RemoteCache")
            .field("backend", &backend)
            .field("timeout", &self.timeout)
            .field("upload", &self.upload)
            .finish()
    }
}

impl RemoteCache {
    /// Parse `--remote-cache`: `file:///path`, a bare path, `http://host/prefix`,
    /// or, with the `reapi` feature, `grpc://host/[instance][?authorization=…]`.
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
        } else if spec.starts_with("grpc://") || spec.starts_with("grpcs://") {
            #[cfg(feature = "reapi")]
            {
                Backend::Reapi(reapi_backend(spec, timeout)?)
            }
            #[cfg(not(feature = "reapi"))]
            {
                bail!(
                    "this frost was built without the reapi feature, so {spec:?} \
                     cannot be used; rebuild with default features"
                );
            }
        } else if spec.starts_with("https://") {
            // Silently downgrading to plaintext, or pretending to verify a
            // certificate frost cannot check, are both worse than saying so.
            // `grpcs://` is the TLS path for the REAPI backend.
            bail!("remote cache does not support https yet; use grpcs:// for REAPI or terminate TLS locally and use http://");
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
        #[cfg(feature = "reapi")]
        if let Backend::Reapi(backend) = &self.backend {
            let Some(client) = backend.client() else {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            return self.reapi_action(&client, key);
        }
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
        #[cfg(feature = "reapi")]
        if let Backend::Reapi(backend) = &self.backend {
            let Some(client) = backend.client() else {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let (digest, blobs) = frostbuild_reapi::trace_key_blobs(key);
            // Put the synthetic Action (and its Command) in the CAS first, so a
            // server that validates an ActionResult against its action accepts
            // the entry.
            if client.upload_blobs(&blobs).is_err() {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let result =
                frostbuild_reapi::proto::build::bazel::remote::execution::v2::ActionResult {
                    stdout_raw: bytes,
                    ..Default::default()
                };
            if client.update_action_result(&digest, result).is_err() {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
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
    ///
    /// `remote` is the backend-native name of the blob — for REAPI, the
    /// `hash/size` of its SHA-256 — and is ignored by the directory and HTTP
    /// backends, which address the blob by `digest` itself.
    pub fn stage_blob(&self, digest: &str, remote: Option<&str>, destination: &Path) -> bool {
        let bytes = match &self.backend {
            #[cfg(feature = "reapi")]
            Backend::Reapi(backend) => {
                let Some(client) = backend.client() else {
                    self.counters.errors.fetch_add(1, Ordering::Relaxed);
                    return false;
                };
                let Some(remote) = remote.and_then(ReapiDigest::parse) else {
                    // A REAPI entry that does not name the SHA-256 of every
                    // output cannot be fetched, so it is a miss, not a guess.
                    self.counters.errors.fetch_add(1, Ordering::Relaxed);
                    return false;
                };
                match client.download_blob(&remote) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        self.counters.errors.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            }
            _ => match self.get("cas", digest) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return false,
                Err(_) => {
                    self.counters.errors.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            },
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

    /// Publish a blob. Returns the backend-native name it was stored under,
    /// which a REAPI publication records so a consumer can fetch it by SHA-256;
    /// the directory and HTTP backends return `None`.
    pub fn put_blob(&self, digest: &str, source: &Path) -> Option<String> {
        if !self.upload {
            return None;
        }
        let Ok(bytes) = std::fs::read(source) else {
            return None;
        };
        #[cfg(feature = "reapi")]
        if let Backend::Reapi(backend) = &self.backend {
            let Some(client) = backend.client() else {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let remote = ReapiDigest::sha256(&bytes);
            return match client.upload_blobs(&[(remote.clone(), bytes)]) {
                Ok(()) => {
                    self.counters.blobs_uploaded.fetch_add(1, Ordering::Relaxed);
                    self.counters
                        .bytes_uploaded
                        .fetch_add(remote.size_bytes.max(0) as u64, Ordering::Relaxed);
                    Some(remote.key())
                }
                Err(_) => {
                    self.counters.errors.fetch_add(1, Ordering::Relaxed);
                    None
                }
            };
        }
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
        None
    }

    /// Read a trace entry through the REAPI Action Cache.
    ///
    /// A `NotFound` is a miss, unreadable JSON is a rejected entry, and any
    /// transport error is an error — the same three outcomes the file and HTTP
    /// backends distinguish, so the counters mean the same thing everywhere.
    #[cfg(feature = "reapi")]
    fn reapi_action(&self, client: &ReapiClient, key: &str) -> Option<RemoteAction> {
        match client.get_action_result(&frostbuild_reapi::trace_key_digest(key)) {
            Ok(Some(result)) => match serde_json::from_slice::<RemoteAction>(&result.stdout_raw) {
                Ok(action) => {
                    self.counters.action_hits.fetch_add(1, Ordering::Relaxed);
                    Some(action)
                }
                Err(_) => {
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
            // The REAPI backend answers `action` and `stage_blob` before this
            // point; nothing else routes a REAPI address through here.
            #[cfg(feature = "reapi")]
            Backend::Reapi(_) => bail!("the reapi backend does not use the generic store"),
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
            #[cfg(feature = "reapi")]
            Backend::Reapi(_) => bail!("the reapi backend does not use the generic store"),
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

/// Build a REAPI backend from `grpc://host[:port][/instance][?authorization=…]`.
///
/// The instance name is taken from the path so a multi-instance server needs no
/// separate flag. `?authorization=` carries an auth header value verbatim; a
/// deployment that needs a bearer token writes `?authorization=Bearer%20…`, and
/// the value is percent-decoded so the space survives a shell. Nothing connects
/// here; that is deferred to the first request.
#[cfg(feature = "reapi")]
fn reapi_backend(spec: &str, timeout: Duration) -> Result<ReapiBackend> {
    let (base, query) = match spec.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (spec, None),
    };
    let (scheme, rest) = if let Some(rest) = base.strip_prefix("grpcs://") {
        ("grpcs", rest)
    } else if let Some(rest) = base.strip_prefix("grpc://") {
        ("grpc", rest)
    } else {
        bail!("not a REAPI endpoint: {spec:?}");
    };
    let (authority, instance) = match rest.split_once('/') {
        Some((authority, instance)) => (authority, instance.trim_matches('/').to_string()),
        None => (rest, String::new()),
    };
    if authority.is_empty() {
        bail!("remote cache URL has no host: {spec:?}");
    }
    let mut auth_header = None;
    if let Some(query) = query {
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                if key == "authorization" {
                    auth_header = Some(percent_decode(value));
                }
            }
        }
    }
    Ok(ReapiBackend {
        config: ReapiConfig {
            endpoint: format!("{scheme}://{authority}"),
            instance_name: instance,
            timeout,
            auth_header,
        },
        client: std::sync::Mutex::new(None),
    })
}

/// Decode the `%XX` escapes a URL uses, leaving anything else untouched. Only
/// the auth-header value is decoded, so a malformed escape is not an error.
#[cfg(feature = "reapi")]
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(feature = "reapi")]
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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
        // A REAPI endpoint parses without connecting: an unreachable server is a
        // per-request fallback, not a configuration error, so parse succeeds.
        assert!(RemoteCache::parse("grpc://cache.example:50051", timeout, false).is_ok());
        assert!(RemoteCache::parse("grpcs://cache.example/inst", timeout, false).is_ok());
        assert!(RemoteCache::parse("grpc://", timeout, false).is_err());
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
            ..Default::default()
        };
        cache.put_action(&key, &recorded);
        let read = cache.action(&key).expect("stored entry is found");
        assert_eq!(read.outputs, recorded.outputs);
        assert_eq!(read.discovered, recorded.discovered);

        let source = root.join("payload");
        std::fs::write(&source, b"artifact bytes").unwrap();
        let digest = crate::hashcache::hash_file(&source).unwrap();
        let _ = cache.put_blob(&digest, &source);
        let destination = root.join("restored");
        assert!(cache.stage_blob(&digest, None, &destination));
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
        let _ = cache.put_blob(&digest, &source);

        // Someone else's cache, a truncated upload, a damaged volume: the
        // remote no longer holds what this digest names.
        std::fs::write(root.join("cas").join(&digest), b"tampered bytes").unwrap();
        let destination = root.join("restored");
        assert!(
            !cache.stage_blob(&digest, None, &destination),
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
        let _ = cache.put_blob(&digest, &source);

        let destination = root.join("restored-tool");
        assert!(cache.stage_blob(&digest, None, &destination));
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
            ..Default::default()
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
    #[cfg(feature = "reapi")]
    fn a_reapi_backend_round_trips_actions_and_blobs() {
        let server = frostbuild_reapi::testing::TestServer::start();
        let cache = RemoteCache::parse(server.endpoint(), Duration::from_secs(5), true).unwrap();
        let key = "c".repeat(64);

        assert!(cache.action(&key).is_none(), "an empty server misses");
        let recorded = RemoteAction {
            discovered: BTreeMap::new(),
            outputs: BTreeMap::from([("out/app".into(), "placeholder".into())]),
            duration_ms: 12,
            ..Default::default()
        };
        cache.put_action(&key, &recorded);
        let read = cache.action(&key).expect("stored entry is found");
        assert_eq!(read.outputs, recorded.outputs);

        // A REAPI blob is addressed remotely by SHA-256 but verified locally by
        // the frost digest, so a wrong remote answer is still refused.
        let source = std::env::temp_dir().join(format!("frost-reapi-{}", std::process::id()));
        std::fs::write(&source, b"artifact bytes").unwrap();
        let digest = crate::hashcache::hash_file(&source).unwrap();
        let remote = cache
            .put_blob(&digest, &source)
            .expect("reapi names the stored blob");
        assert!(remote.contains('/'), "the name is a hash/size pair");
        let destination = source.with_extension("restored");
        assert!(cache.stage_blob(&digest, Some(&remote), &destination));
        assert_eq!(std::fs::read(&destination).unwrap(), b"artifact bytes");
        assert!(
            !cache.stage_blob(&digest, None, &destination),
            "without a remote name the REAPI backend cannot fetch it"
        );
        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&destination);
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
