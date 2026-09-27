//! A synchronous REAPI v2 client.
//!
//! Every method blocks the calling thread on a shared multi-thread Tokio
//! runtime. The rest of the build is synchronous and stays that way: an async
//! colour would spread through the executor for the sake of a cache, which is
//! the wrong trade. One runtime per client keeps connection state pooled.
//!
//! The client never trusts a response. `download_blob` re-hashes what it got
//! and refuses a mismatch; `find_missing_blobs` is a preflight, not an
//! authority on what exists; and a short batch response is `PartialResponse`
//! rather than silently zipped.

use std::time::Duration;

use tonic::codec::CompressionEncoding;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};

use crate::digest::{Digest, DigestFunction};
use crate::error::ReapiError;
use crate::proto::build::bazel::remote::execution::v2 as repb;

/// How to reach one REAPI server.
#[derive(Clone, Debug)]
pub struct ReapiConfig {
    /// `grpc://host:port` or `grpcs://host:port`, optionally with a path.
    pub endpoint: String,
    /// REAPI instance name; empty for a single-instance server.
    pub instance_name: String,
    /// Per-request deadline.
    pub timeout: Duration,
    /// Value of the `authorization` metadata header, if the server wants one.
    pub auth_header: Option<String>,
}

impl ReapiConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            instance_name: String::new(),
            timeout: Duration::from_secs(30),
            auth_header: None,
        }
    }
}

/// What the server said it can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub digest_function: DigestFunction,
    pub cas_enabled: bool,
    pub exec_enabled: bool,
    pub max_batch_total_size: i64,
    pub low_api: String,
    pub high_api: String,
}

/// A connected REAPI v2 client.
pub struct ReapiClient {
    runtime: tokio::runtime::Runtime,
    channel: Channel,
    config: ReapiConfig,
    capabilities: Capabilities,
}

// BuildGrid's storage service is configured with `grpc-compression: Gzip`, so
// its CAS responses arrive gzip-encoded. Every client accepts gzip, so a server
// that compresses is usable without the caller opting in.

type CasClient = repb::content_addressable_storage_client::ContentAddressableStorageClient<Channel>;
type CapabilitiesClient = repb::capabilities_client::CapabilitiesClient<Channel>;
type ActionCacheClient = repb::action_cache_client::ActionCacheClient<Channel>;
type ExecutionClient = repb::execution_client::ExecutionClient<Channel>;
type ByteStreamClient =
    crate::proto::google::bytestream::byte_stream_client::ByteStreamClient<Channel>;

fn cas_client(channel: Channel) -> CasClient {
    CasClient::new(channel).accept_compressed(CompressionEncoding::Gzip)
}

fn capabilities_client(channel: Channel) -> CapabilitiesClient {
    CapabilitiesClient::new(channel).accept_compressed(CompressionEncoding::Gzip)
}

fn action_cache_client(channel: Channel) -> ActionCacheClient {
    ActionCacheClient::new(channel).accept_compressed(CompressionEncoding::Gzip)
}

fn execution_client(channel: Channel) -> ExecutionClient {
    ExecutionClient::new(channel).accept_compressed(CompressionEncoding::Gzip)
}

fn byte_stream_client(channel: Channel) -> ByteStreamClient {
    ByteStreamClient::new(channel).accept_compressed(CompressionEncoding::Gzip)
}

/// Request header `authorization` is carried per call rather than through a
/// generic interceptor, so the client types stay concrete and the auth is
/// visible at each call site.
fn request<T>(
    auth: Option<&MetadataValue<tonic::metadata::Ascii>>,
    payload: T,
) -> tonic::Request<T> {
    let mut request = tonic::Request::new(payload);
    if let Some(value) = auth {
        request
            .metadata_mut()
            .insert("authorization", value.clone());
    }
    request
}

impl ReapiClient {
    /// Connect and negotiate capabilities. A server that does not offer
    /// SHA-256 is refused here, before any work is attempted.
    pub fn connect(config: ReapiConfig) -> Result<Self, ReapiError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("frost-reapi")
            .build()
            .map_err(|error| ReapiError::Transport(error.to_string()))?;
        let uri = normalize_endpoint(&config.endpoint)?;
        let endpoint = Endpoint::from_shared(uri)
            .map_err(|error| ReapiError::Transport(error.to_string()))?
            .connect_timeout(config.timeout)
            .timeout(config.timeout);
        // `Endpoint::connect` needs the runtime; `runtime.block_on` drives it.
        let channel = runtime
            .block_on(endpoint.connect())
            .map_err(|error| ReapiError::Transport(error.to_string()))?;
        let mut client = Self {
            runtime,
            channel,
            capabilities: Capabilities {
                digest_function: DigestFunction::Sha256,
                cas_enabled: false,
                exec_enabled: false,
                max_batch_total_size: 4 * 1024 * 1024,
                low_api: String::new(),
                high_api: String::new(),
            },
            config,
        };
        client.capabilities = client.negotiate()?;
        Ok(client)
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn auth(&self) -> Option<MetadataValue<tonic::metadata::Ascii>> {
        self.config
            .auth_header
            .as_ref()
            .and_then(|value| value.parse().ok())
    }

    fn negotiate(&mut self) -> Result<Capabilities, ReapiError> {
        let mut client = capabilities_client(self.channel.clone());
        let auth = self.auth();
        let response = self
            .runtime
            .block_on(async {
                let request = request(
                    auth.as_ref(),
                    repb::GetCapabilitiesRequest {
                        instance_name: self.config.instance_name.clone(),
                    },
                );
                client.get_capabilities(request).await
            })
            .map_err(|status| ReapiError::from_status(&status))?
            .into_inner();
        let cache = response.cache_capabilities.unwrap_or_default();
        let execution = response.execution_capabilities.unwrap_or_default();
        let digest_function = cache
            .digest_functions
            .iter()
            .find_map(|&value| DigestFunction::from_reapi(value))
            .ok_or_else(|| {
                ReapiError::Unsupported(format!(
                    "any digest function this adapter implements (server offered {:?})",
                    cache.digest_functions
                ))
            })?;
        Ok(Capabilities {
            digest_function,
            cas_enabled: true,
            exec_enabled: execution.exec_enabled,
            max_batch_total_size: if cache.max_batch_total_size_bytes > 0 {
                cache.max_batch_total_size_bytes
            } else {
                4 * 1024 * 1024
            },
            low_api: version(&response.low_api_version),
            high_api: version(&response.high_api_version),
        })
    }

    /// Ask which of `digests` the server is missing. Batching respects the
    /// negotiated size limit so a large preflight does not become one request
    /// the server rejects.
    pub fn find_missing_blobs(&self, digests: &[Digest]) -> Result<Vec<Digest>, ReapiError> {
        let mut missing = Vec::new();
        for chunk in batch(digests, self.capabilities.max_batch_total_size) {
            let mut client = cas_client(self.channel.clone());
            let auth = self.auth();
            let payload = repb::FindMissingBlobsRequest {
                instance_name: self.config.instance_name.clone(),
                blob_digests: chunk.iter().map(to_proto_digest).collect(),
                digest_function: self.capabilities.digest_function.reapi_value(),
            };
            let response = self
                .runtime
                .block_on(async {
                    client
                        .find_missing_blobs(request(auth.as_ref(), payload))
                        .await
                })
                .map_err(|status| ReapiError::from_status(&status))?
                .into_inner();
            for found in response.missing_blob_digests {
                missing.push(from_proto_digest(&found));
            }
        }
        Ok(missing)
    }

    /// Upload blobs. Small ones go through `BatchUpdateBlobs` in
    /// size-bounded batches; a blob larger than the batch limit uses
    /// ByteStream, which is the only path that can carry it.
    pub fn upload_blobs(&self, blobs: &[(Digest, Vec<u8>)]) -> Result<(), ReapiError> {
        let mut small = Vec::new();
        for (digest, data) in blobs {
            if digest.size_bytes > self.capabilities.max_batch_total_size {
                self.write_bytestream(digest, data)?;
            } else {
                small.push((digest.clone(), data.as_slice()));
            }
        }
        for chunk in batch_pairs(&small, self.capabilities.max_batch_total_size) {
            let mut client = cas_client(self.channel.clone());
            let auth = self.auth();
            let payload = repb::BatchUpdateBlobsRequest {
                instance_name: self.config.instance_name.clone(),
                requests: chunk
                    .iter()
                    .map(|(digest, data)| repb::batch_update_blobs_request::Request {
                        digest: Some(to_proto_digest(digest)),
                        data: data.to_vec(),
                        compressor: 0,
                    })
                    .collect(),
                digest_function: self.capabilities.digest_function.reapi_value(),
            };
            let expected: Vec<String> = chunk.iter().map(|(digest, _)| digest.key()).collect();
            let response = self
                .runtime
                .block_on(async {
                    client
                        .batch_update_blobs(request(auth.as_ref(), payload))
                        .await
                })
                .map_err(|status| ReapiError::from_status(&status))?
                .into_inner();
            // A batch response answers its requests in order; a short one is a
            // partial answer, and every unreported blob is an upload failure.
            if response.responses.len() != expected.len() {
                return Err(ReapiError::PartialResponse {
                    digest: expected
                        .get(response.responses.len())
                        .cloned()
                        .unwrap_or_default(),
                });
            }
            for (answer, digest) in response.responses.iter().zip(&expected) {
                if let Some(status) = &answer.status {
                    if status.code != 0 {
                        return Err(ReapiError::BlobRejected {
                            digest: digest.clone(),
                            message: status.message.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Fetch one blob and verify it against `digest`. Small blobs use
    /// `BatchReadBlobs`; the rest use ByteStream.
    pub fn download_blob(&self, digest: &Digest) -> Result<Vec<u8>, ReapiError> {
        let data = if digest.size_bytes > self.capabilities.max_batch_total_size {
            self.read_bytestream(digest)?
        } else {
            let mut client = cas_client(self.channel.clone());
            let auth = self.auth();
            let payload = repb::BatchReadBlobsRequest {
                instance_name: self.config.instance_name.clone(),
                digests: vec![to_proto_digest(digest)],
                acceptable_compressors: vec![0],
                digest_function: self.capabilities.digest_function.reapi_value(),
            };
            let response = self
                .runtime
                .block_on(async {
                    client
                        .batch_read_blobs(request(auth.as_ref(), payload))
                        .await
                })
                .map_err(|status| ReapiError::from_status(&status))?
                .into_inner();
            let answer = response.responses.into_iter().next().ok_or_else(|| {
                ReapiError::PartialResponse {
                    digest: digest.key(),
                }
            })?;
            if let Some(status) = &answer.status {
                if status.code != 0 {
                    return Err(ReapiError::BlobRejected {
                        digest: digest.key(),
                        message: status.message.clone(),
                    });
                }
            }
            answer.data
        };
        if !digest.verify(&data) {
            return Err(ReapiError::DigestMismatch {
                digest: digest.key(),
            });
        }
        Ok(data)
    }

    pub fn get_action_result(
        &self,
        action_digest: &Digest,
    ) -> Result<Option<repb::ActionResult>, ReapiError> {
        let mut client = action_cache_client(self.channel.clone());
        let auth = self.auth();
        let payload = repb::GetActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(to_proto_digest(action_digest)),
            inline_stdout: false,
            inline_stderr: false,
            inline_output_files: Vec::new(),
            digest_function: self.capabilities.digest_function.reapi_value(),
        };
        let result = self.runtime.block_on(async {
            client
                .get_action_result(request(auth.as_ref(), payload))
                .await
        });
        match result {
            Ok(response) => Ok(Some(response.into_inner())),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(status) => Err(ReapiError::from_status(&status)),
        }
    }

    pub fn update_action_result(
        &self,
        action_digest: &Digest,
        result: repb::ActionResult,
    ) -> Result<(), ReapiError> {
        let mut client = action_cache_client(self.channel.clone());
        let auth = self.auth();
        let payload = repb::UpdateActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(to_proto_digest(action_digest)),
            action_result: Some(result),
            results_cache_policy: None,
            digest_function: self.capabilities.digest_function.reapi_value(),
        };
        self.runtime
            .block_on(async {
                client
                    .update_action_result(request(auth.as_ref(), payload))
                    .await
            })
            .map_err(|status| ReapiError::from_status(&status))?;
        Ok(())
    }

    /// Run an action remotely. Returns the first completed operation's response;
    /// a stream that ends unfinished is a transport failure, and any error the
    /// server reports is returned so the caller can fall back to local.
    pub fn execute(
        &self,
        action_digest: &Digest,
        skip_cache_lookup: bool,
    ) -> Result<repb::ExecuteResponse, ReapiError> {
        let mut client = execution_client(self.channel.clone());
        let auth = self.auth();
        let payload = repb::ExecuteRequest {
            instance_name: self.config.instance_name.clone(),
            skip_cache_lookup,
            action_digest: Some(to_proto_digest(action_digest)),
            execution_policy: None,
            digest_function: self.capabilities.digest_function.reapi_value(),
            results_cache_policy: None,
            inline_stdout: false,
            inline_stderr: false,
            inline_output_files: Vec::new(),
        };
        let timeout = self.config.timeout;
        let stream = self
            .runtime
            .block_on(async {
                let response = client
                    .execute(request(auth.as_ref(), payload))
                    .await?
                    .into_inner();
                collect_operations(response, timeout).await
            })
            .map_err(|status| ReapiError::from_status(&status))?;
        finish_execution(stream)
    }

    fn resource_name(&self, digest: &Digest, upload: bool) -> String {
        let instance = self.config.instance_name.trim_matches('/');
        let kind = if upload {
            format!("uploads/{}/blobs", uuid())
        } else {
            "blobs".to_string()
        };
        if instance.is_empty() {
            format!("{kind}/{}/{}", digest.hash, digest.size_bytes)
        } else {
            format!("{instance}/{kind}/{}/{}", digest.hash, digest.size_bytes)
        }
    }

    fn read_bytestream(&self, digest: &Digest) -> Result<Vec<u8>, ReapiError> {
        let mut client = byte_stream_client(self.channel.clone());
        let auth = self.auth();
        let payload = crate::proto::google::bytestream::ReadRequest {
            resource_name: self.resource_name(digest, false),
            read_offset: 0,
            read_limit: 0,
        };
        let timeout = self.config.timeout;
        self.runtime
            .block_on(async {
                let response = client
                    .read(request(auth.as_ref(), payload))
                    .await?
                    .into_inner();
                let mut data = Vec::with_capacity(digest.size_bytes.max(0) as usize);
                let mut stream = response;
                while let Some(chunk) = tokio::time::timeout(timeout, stream.message())
                    .await
                    .map_err(|_| tonic::Status::deadline_exceeded("ByteStream read timed out"))??
                {
                    data.extend_from_slice(&chunk.data);
                }
                Ok::<_, tonic::Status>(data)
            })
            .map_err(|status| ReapiError::from_status(&status))
    }

    fn write_bytestream(&self, digest: &Digest, data: &[u8]) -> Result<(), ReapiError> {
        let mut client = byte_stream_client(self.channel.clone());
        let auth = self.auth();
        let resource = self.resource_name(digest, true);
        let chunk_size = 1024 * 1024;
        // An empty blob still needs one framed message with `finish_write`.
        let chunks: Vec<Vec<u8>> = if data.is_empty() {
            vec![Vec::new()]
        } else {
            data.chunks(chunk_size).map(<[u8]>::to_vec).collect()
        };
        let total = chunks.len();
        let timeout = self.config.timeout;
        self.runtime
            .block_on(async {
                let (sender, receiver) =
                    tokio::sync::mpsc::channel::<crate::proto::google::bytestream::WriteRequest>(1);
                let resource = resource.clone();
                let producer = tokio::spawn(async move {
                    let mut offset = 0i64;
                    for (index, chunk) in chunks.into_iter().enumerate() {
                        let size = chunk.len();
                        let message = crate::proto::google::bytestream::WriteRequest {
                            resource_name: resource.clone(),
                            write_offset: offset,
                            finish_write: index + 1 == total,
                            data: chunk,
                        };
                        if sender.send(message).await.is_err() {
                            break;
                        }
                        offset += size as i64;
                    }
                });
                let stream = tokio_stream::wrappers::ReceiverStream::new(receiver);
                let mut message = tonic::Request::new(stream);
                if let Some(value) = auth.as_ref() {
                    message
                        .metadata_mut()
                        .insert("authorization", value.clone());
                }
                let response = tokio::time::timeout(timeout, client.write(message)).await;
                let _ = producer.await;
                match response {
                    Ok(Ok(response)) => Ok::<_, tonic::Status>(response.into_inner()),
                    Ok(Err(status)) => Err(status),
                    Err(_) => Err(tonic::Status::deadline_exceeded(
                        "ByteStream write timed out",
                    )),
                }
            })
            .map_err(|status| ReapiError::from_status(&status))?;
        Ok(())
    }
}

async fn collect_operations(
    mut stream: tonic::Streaming<crate::proto::google::longrunning::Operation>,
    timeout: Duration,
) -> Result<Vec<crate::proto::google::longrunning::Operation>, tonic::Status> {
    let mut operations = Vec::new();
    while let Some(operation) = tokio::time::timeout(timeout, stream.message())
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("Execute stream timed out"))??
    {
        let done = operation.done;
        operations.push(operation);
        if done {
            break;
        }
    }
    Ok(operations)
}

fn finish_execution(
    operations: Vec<crate::proto::google::longrunning::Operation>,
) -> Result<repb::ExecuteResponse, ReapiError> {
    use crate::proto::google::longrunning::operation::Result as OperationResult;
    let last = operations.last().ok_or_else(|| {
        ReapiError::Transport("the Execute stream ended before an operation".into())
    })?;
    if !last.done {
        return Err(ReapiError::Transport(
            "the Execute stream ended before the operation completed".into(),
        ));
    }
    match &last.result {
        Some(OperationResult::Error(status)) if status.code != 0 => Err(ReapiError::Rpc {
            code: status.code,
            message: status.message.clone(),
        }),
        Some(OperationResult::Response(any)) => prost::Message::decode(any.value.as_slice())
            .map_err(|error| ReapiError::Other(error.to_string())),
        _ => Err(ReapiError::PartialResponse {
            digest: last.name.clone(),
        }),
    }
}

/// A deterministic REAPI `Action` digest for a frost trace key, with the
/// `Command` and `Action` blobs it references.
///
/// A REAPI Action Cache is addressed by an `Action` digest, but a frost trace
/// entry is keyed by a key over the action's declared inputs, not by a
/// serialized build action. This builds the smallest valid, deterministic
/// `Action` whose identity is the key, so an entry can be read and written
/// through the real `Get/UpdateActionResult` calls. The blobs are returned so a
/// publisher can put them in the CAS first, which a server that validates an
/// `ActionResult` against its action requires. The action is never executed.
pub fn trace_key_blobs(key: &str) -> (Digest, Vec<(Digest, Vec<u8>)>) {
    use prost::Message;
    let command_bytes = repb::Command::default().encode_to_vec();
    let command_digest = Digest::sha256(&command_bytes);
    let action = repb::Action {
        command_digest: Some(to_proto_digest(&command_digest)),
        input_root_digest: Some(to_proto_digest(&Digest::sha256(key.as_bytes()))),
        ..Default::default()
    };
    let action_bytes = action.encode_to_vec();
    let action_digest = Digest::sha256(&action_bytes);
    let blobs = vec![
        (command_digest, command_bytes),
        (action_digest.clone(), action_bytes),
    ];
    (action_digest, blobs)
}

/// The `Action` digest `trace_key_blobs` names, for a lookup.
pub fn trace_key_digest(key: &str) -> Digest {
    trace_key_blobs(key).0
}

/// Translate `grpc://` / `grpcs://` into the `http`/`https` scheme tonic needs.
fn normalize_endpoint(endpoint: &str) -> Result<String, ReapiError> {
    if let Some(rest) = endpoint.strip_prefix("grpc://") {
        Ok(format!("http://{rest}"))
    } else if let Some(rest) = endpoint.strip_prefix("grpcs://") {
        Ok(format!("https://{rest}"))
    } else if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        Ok(endpoint.to_string())
    } else {
        Err(ReapiError::InvalidDigest(format!(
            "unsupported REAPI endpoint scheme: {endpoint:?}"
        )))
    }
}

fn version(value: &Option<crate::proto::build::bazel::semver::SemVer>) -> String {
    value
        .as_ref()
        .map(|value| format!("{}.{}.{}", value.major, value.minor, value.patch))
        .unwrap_or_default()
}

fn to_proto_digest(digest: &Digest) -> repb::Digest {
    repb::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}

fn from_proto_digest(digest: &repb::Digest) -> Digest {
    Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}

fn batch(digests: &[Digest], max_bytes: i64) -> Vec<&[Digest]> {
    // Digest wire size: 2-byte tag for the hash length plus hash plus the size
    // varint, conservatively budgeted. The goal is to stay under the server's
    // total-request limit, not to pack it perfectly.
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0i64;
    for (index, digest) in digests.iter().enumerate() {
        let cost = digest.hash.len() as i64 + 24;
        if index > start && bytes + cost > max_bytes {
            chunks.push(&digests[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += cost;
    }
    if start < digests.len() {
        chunks.push(&digests[start..]);
    }
    chunks
}

fn batch_pairs<'a, 'b>(
    blobs: &'a [(Digest, &'b [u8])],
    max_bytes: i64,
) -> Vec<&'a [(Digest, &'b [u8])]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0i64;
    for (index, (digest, data)) in blobs.iter().enumerate() {
        let cost = data.len() as i64 + digest.hash.len() as i64 + 32;
        if index > start && bytes + cost > max_bytes {
            chunks.push(&blobs[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += cost;
    }
    if start < blobs.len() {
        chunks.push(&blobs[start..]);
    }
    chunks
}

/// A v4-shaped identifier for an upload resource name. The server only
/// requires uniqueness per upload, not cryptographic strength.
fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}-{:08x}", nanos, count as u32)
}
