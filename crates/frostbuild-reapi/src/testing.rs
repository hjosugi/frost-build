//! An in-process REAPI server for tests.
//!
//! It implements the CAS, Action Cache, Capabilities and Execution services in
//! memory, plus the ByteStream the CAS delegates to for large blobs. It is a
//! deliberately small server: its purpose is to let the client be tested
//! against a real gRPC stack and to make the three failure modes the adapter
//! must survive injectable, not to emulate BuildGrid.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use prost::Message;
use tonic::{Request, Response, Status};

use crate::proto::build::bazel::remote::execution::v2 as repb;
use crate::proto::google::longrunning::operation::Result as OperationResult;
use crate::proto::google::longrunning::Operation;

type ResponseStream<T> =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<T, Status>> + Send + 'static>>;

/// Server behaviours the client must turn into a local fallback.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// `BatchReadBlobs` returns bytes that do not hash to the requested digest.
    pub corrupt_read: bool,
    /// `BatchReadBlobs` answers with no responses at all.
    pub partial_read: bool,
    /// `BatchUpdateBlobs` reports a per-blob failure status.
    pub reject_write: bool,
    /// `Execute` ends the stream without a completed operation.
    pub disconnect_execute: bool,
    /// `FindMissingBlobs` claims every blob is missing, even present ones.
    pub always_missing: bool,
    /// When set, every call must carry this exact `authorization` header.
    pub require_auth: Option<String>,
}

#[derive(Default)]
struct Counters {
    find_missing_calls: AtomicU64,
    batch_read_calls: AtomicU64,
    batch_update_calls: AtomicU64,
    bytestream_reads: AtomicU64,
    bytestream_writes: AtomicU64,
    execute_calls: AtomicU64,
}

struct Inner {
    cas: Mutex<HashMap<String, Vec<u8>>>,
    action_cache: Mutex<HashMap<String, repb::ActionResult>>,
    faults: Faults,
    counters: Counters,
    max_batch_total_size: i64,
}

/// A running in-process server.
pub struct TestServer {
    endpoint: String,
    inner: Arc<Inner>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn start() -> Self {
        Self::start_with(Faults::default())
    }

    pub fn start_with(faults: Faults) -> Self {
        let inner = Arc::new(Inner {
            cas: Mutex::new(HashMap::new()),
            action_cache: Mutex::new(HashMap::new()),
            faults,
            counters: Counters::default(),
            max_batch_total_size: 64 * 1024,
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("server address");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let (shutdown, receiver) = tokio::sync::oneshot::channel();
        let service = State {
            inner: inner.clone(),
        };
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("test server runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
                let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
                let services = (
                    repb::capabilities_server::CapabilitiesServer::new(service.clone()),
                    // BuildGrid's storage compresses its CAS responses; the test
                    // server does too, so the client's gzip path is exercised.
                    repb::content_addressable_storage_server::ContentAddressableStorageServer::new(
                        service.clone(),
                    )
                    .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
                    .send_compressed(tonic::codec::CompressionEncoding::Gzip),
                    repb::action_cache_server::ActionCacheServer::new(service.clone()),
                    repb::execution_server::ExecutionServer::new(service.clone()),
                    crate::proto::google::bytestream::byte_stream_server::ByteStreamServer::new(
                        service,
                    ),
                );
                tonic::transport::Server::builder()
                    .add_service(services.0)
                    .add_service(services.1)
                    .add_service(services.2)
                    .add_service(services.3)
                    .add_service(services.4)
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = receiver.await;
                    })
                    .await
                    .expect("test server");
            });
        });
        Self {
            endpoint: format!("grpc://{address}"),
            inner,
            shutdown: Some(shutdown),
            handle: Some(handle),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn cas_count(&self) -> usize {
        self.inner.cas.lock().unwrap().len()
    }

    /// Insert a blob directly, bypassing the client, so a read fault can be
    /// injected against a well-formed object.
    pub fn put(&self, digest: &crate::digest::Digest, data: &[u8]) {
        self.inner
            .cas
            .lock()
            .unwrap()
            .insert(digest.key(), data.to_vec());
    }

    pub fn find_missing_calls(&self) -> u64 {
        self.inner
            .counters
            .find_missing_calls
            .load(Ordering::Relaxed)
    }

    pub fn batch_read_calls(&self) -> u64 {
        self.inner.counters.batch_read_calls.load(Ordering::Relaxed)
    }

    pub fn batch_update_calls(&self) -> u64 {
        self.inner
            .counters
            .batch_update_calls
            .load(Ordering::Relaxed)
    }

    pub fn bytestream_writes(&self) -> u64 {
        self.inner
            .counters
            .bytestream_writes
            .load(Ordering::Relaxed)
    }

    pub fn address(&self) -> SocketAddr {
        let rest = self.endpoint.trim_start_matches("grpc://");
        rest.parse().expect("test server address")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
struct State {
    inner: Arc<Inner>,
}

fn status(code: i32, message: impl Into<String>) -> crate::proto::google::rpc::Status {
    crate::proto::google::rpc::Status {
        code,
        message: message.into(),
        details: Vec::new(),
    }
}

fn digest_key(digest: &repb::Digest) -> String {
    format!("{}/{}", digest.hash, digest.size_bytes)
}

#[tonic::async_trait]
impl repb::capabilities_server::Capabilities for State {
    async fn get_capabilities(
        &self,
        request: Request<repb::GetCapabilitiesRequest>,
    ) -> Result<Response<repb::ServerCapabilities>, Status> {
        if let Some(expected) = &self.inner.faults.require_auth {
            let got = request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok());
            if got != Some(expected.as_str()) {
                return Err(Status::unauthenticated("authorization header required"));
            }
        }
        Ok(Response::new(repb::ServerCapabilities {
            cache_capabilities: Some(repb::CacheCapabilities {
                digest_functions: vec![1],
                max_batch_total_size_bytes: self.inner.max_batch_total_size,
                ..Default::default()
            }),
            execution_capabilities: Some(repb::ExecutionCapabilities {
                digest_function: 1,
                exec_enabled: true,
                ..Default::default()
            }),
            deprecated_api_version: None,
            low_api_version: Some(crate::proto::build::bazel::semver::SemVer {
                major: 2,
                minor: 0,
                patch: 0,
                prerelease: String::new(),
            }),
            high_api_version: Some(crate::proto::build::bazel::semver::SemVer {
                major: 2,
                minor: 2,
                patch: 0,
                prerelease: String::new(),
            }),
        }))
    }
}

#[tonic::async_trait]
impl repb::content_addressable_storage_server::ContentAddressableStorage for State {
    type GetTreeStream = ResponseStream<repb::GetTreeResponse>;
    type GetChunkMappingStream = ResponseStream<repb::GetChunkMappingResponse>;

    async fn find_missing_blobs(
        &self,
        request: Request<repb::FindMissingBlobsRequest>,
    ) -> Result<Response<repb::FindMissingBlobsResponse>, Status> {
        self.inner
            .counters
            .find_missing_calls
            .fetch_add(1, Ordering::Relaxed);
        let cas = self.inner.cas.lock().unwrap();
        let missing = if self.inner.faults.always_missing {
            request.into_inner().blob_digests
        } else {
            request
                .into_inner()
                .blob_digests
                .into_iter()
                .filter(|digest| !cas.contains_key(&digest_key(digest)))
                .collect()
        };
        Ok(Response::new(repb::FindMissingBlobsResponse {
            missing_blob_digests: missing,
        }))
    }

    async fn batch_update_blobs(
        &self,
        request: Request<repb::BatchUpdateBlobsRequest>,
    ) -> Result<Response<repb::BatchUpdateBlobsResponse>, Status> {
        self.inner
            .counters
            .batch_update_calls
            .fetch_add(1, Ordering::Relaxed);
        let request = request.into_inner();
        let mut cas = self.inner.cas.lock().unwrap();
        let mut responses = Vec::new();
        for item in request.requests {
            let digest = item.digest.clone().unwrap_or_default();
            let key = digest_key(&digest);
            let ok = !self.inner.faults.reject_write
                && digest.hash == crate::digest::Digest::sha256(&item.data).hash;
            if ok {
                cas.insert(key, item.data);
            }
            responses.push(repb::batch_update_blobs_response::Response {
                digest: Some(digest),
                status: Some(if ok {
                    status(0, String::new())
                } else {
                    status(3, "the upload was rejected")
                }),
            });
        }
        Ok(Response::new(repb::BatchUpdateBlobsResponse { responses }))
    }

    async fn batch_read_blobs(
        &self,
        request: Request<repb::BatchReadBlobsRequest>,
    ) -> Result<Response<repb::BatchReadBlobsResponse>, Status> {
        self.inner
            .counters
            .batch_read_calls
            .fetch_add(1, Ordering::Relaxed);
        let request = request.into_inner();
        if self.inner.faults.partial_read {
            return Ok(Response::new(repb::BatchReadBlobsResponse {
                responses: Vec::new(),
            }));
        }
        let cas = self.inner.cas.lock().unwrap();
        let mut responses = Vec::new();
        for digest in request.digests {
            let key = digest_key(&digest);
            let mut data = cas.get(&key).cloned().unwrap_or_default();
            if self.inner.faults.corrupt_read && !data.is_empty() {
                data = b"tampered".to_vec();
            }
            responses.push(repb::batch_read_blobs_response::Response {
                digest: Some(digest),
                data,
                compressor: 0,
                status: Some(status(0, String::new())),
            });
        }
        Ok(Response::new(repb::BatchReadBlobsResponse { responses }))
    }

    async fn get_tree(
        &self,
        _request: Request<repb::GetTreeRequest>,
    ) -> Result<Response<Self::GetTreeStream>, Status> {
        Err(Status::unimplemented(
            "GetTree is not exercised by these tests",
        ))
    }

    async fn split_blob(
        &self,
        _request: Request<repb::SplitBlobRequest>,
    ) -> Result<Response<repb::SplitBlobResponse>, Status> {
        Err(Status::unimplemented(
            "SplitBlob is not exercised by these tests",
        ))
    }

    async fn get_chunk_mapping(
        &self,
        _request: Request<repb::GetChunkMappingRequest>,
    ) -> Result<Response<Self::GetChunkMappingStream>, Status> {
        Err(Status::unimplemented(
            "GetChunkMapping is not exercised by these tests",
        ))
    }

    async fn splice_blob(
        &self,
        _request: Request<repb::SpliceBlobRequest>,
    ) -> Result<Response<repb::SpliceBlobResponse>, Status> {
        Err(Status::unimplemented(
            "SpliceBlob is not exercised by these tests",
        ))
    }

    async fn register_chunk_mapping(
        &self,
        _request: Request<tonic::Streaming<repb::RegisterChunkMappingRequest>>,
    ) -> Result<Response<repb::RegisterChunkMappingResponse>, Status> {
        Err(Status::unimplemented(
            "RegisterChunkMapping is not exercised by these tests",
        ))
    }
}

#[tonic::async_trait]
impl repb::action_cache_server::ActionCache for State {
    async fn get_action_result(
        &self,
        request: Request<repb::GetActionResultRequest>,
    ) -> Result<Response<repb::ActionResult>, Status> {
        let digest = request
            .into_inner()
            .action_digest
            .ok_or_else(|| Status::invalid_argument("action digest is required"))?;
        match self
            .inner
            .action_cache
            .lock()
            .unwrap()
            .get(&digest_key(&digest))
        {
            Some(result) => Ok(Response::new(result.clone())),
            None => Err(Status::not_found("no cached action result")),
        }
    }

    async fn update_action_result(
        &self,
        request: Request<repb::UpdateActionResultRequest>,
    ) -> Result<Response<repb::ActionResult>, Status> {
        let request = request.into_inner();
        let digest = request
            .action_digest
            .ok_or_else(|| Status::invalid_argument("action digest is required"))?;
        let result = request
            .action_result
            .ok_or_else(|| Status::invalid_argument("action result is required"))?;
        self.inner
            .action_cache
            .lock()
            .unwrap()
            .insert(digest_key(&digest), result.clone());
        Ok(Response::new(result))
    }
}

#[tonic::async_trait]
impl repb::execution_server::Execution for State {
    type ExecuteStream = ResponseStream<Operation>;
    type WaitExecutionStream = ResponseStream<Operation>;

    async fn execute(
        &self,
        request: Request<repb::ExecuteRequest>,
    ) -> Result<Response<Self::ExecuteStream>, Status> {
        self.inner
            .counters
            .execute_calls
            .fetch_add(1, Ordering::Relaxed);
        let action_digest = request
            .into_inner()
            .action_digest
            .ok_or_else(|| Status::invalid_argument("action digest is required"))?;
        if self.inner.faults.disconnect_execute {
            let unfinished = Operation {
                name: "operations/unfinished".into(),
                done: false,
                ..Default::default()
            };
            return Ok(Response::new(Box::pin(tokio_stream::iter(vec![Ok(
                unfinished,
            )]))));
        }
        let result = self
            .inner
            .action_cache
            .lock()
            .unwrap()
            .get(&digest_key(&action_digest))
            .cloned()
            .unwrap_or_default();
        let response = repb::ExecuteResponse {
            result: Some(result),
            cached_result: false,
            status: Some(status(0, String::new())),
            ..Default::default()
        };
        let operation = Operation {
            name: "operations/1".into(),
            done: true,
            result: Some(OperationResult::Response(prost_types::Any {
                type_url: "type.googleapis.com/build.bazel.remote.execution.v2.ExecuteResponse"
                    .into(),
                value: response.encode_to_vec(),
            })),
            ..Default::default()
        };
        Ok(Response::new(Box::pin(tokio_stream::iter(vec![Ok(
            operation,
        )]))))
    }

    async fn wait_execution(
        &self,
        _request: Request<repb::WaitExecutionRequest>,
    ) -> Result<Response<Self::WaitExecutionStream>, Status> {
        Err(Status::unimplemented(
            "WaitExecution is not exercised by these tests",
        ))
    }
}

/// Store a blob under the `…/blobs/{hash}/{size}` tail of a resource name.
fn parse_resource(resource: &str) -> Option<String> {
    let mut parts = resource.rsplit('/');
    let size = parts.next()?;
    let hash = parts.next()?;
    if hash.is_empty() || size.is_empty() {
        return None;
    }
    Some(format!("{hash}/{size}"))
}

#[tonic::async_trait]
impl crate::proto::google::bytestream::byte_stream_server::ByteStream for State {
    type ReadStream = ResponseStream<crate::proto::google::bytestream::ReadResponse>;

    async fn read(
        &self,
        request: Request<crate::proto::google::bytestream::ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        self.inner
            .counters
            .bytestream_reads
            .fetch_add(1, Ordering::Relaxed);
        let resource = request.into_inner().resource_name;
        let key =
            parse_resource(&resource).ok_or_else(|| Status::invalid_argument("bad resource"))?;
        let cas = self.inner.cas.lock().unwrap();
        let data = cas.get(&key).cloned().unwrap_or_default();
        let response = crate::proto::google::bytestream::ReadResponse { data };
        Ok(Response::new(Box::pin(tokio_stream::iter(vec![Ok(
            response,
        )]))))
    }

    async fn write(
        &self,
        request: Request<tonic::Streaming<crate::proto::google::bytestream::WriteRequest>>,
    ) -> Result<Response<crate::proto::google::bytestream::WriteResponse>, Status> {
        self.inner
            .counters
            .bytestream_writes
            .fetch_add(1, Ordering::Relaxed);
        let mut stream = request.into_inner();
        let mut key = None;
        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await? {
            if key.is_none() {
                key = parse_resource(&chunk.resource_name);
            }
            data.extend_from_slice(&chunk.data);
        }
        let key = key.ok_or_else(|| Status::invalid_argument("no resource name"))?;
        self.inner.cas.lock().unwrap().insert(key, data.clone());
        Ok(Response::new(
            crate::proto::google::bytestream::WriteResponse {
                committed_size: data.len() as i64,
            },
        ))
    }

    async fn query_write_status(
        &self,
        _request: Request<crate::proto::google::bytestream::QueryWriteStatusRequest>,
    ) -> Result<Response<crate::proto::google::bytestream::QueryWriteStatusResponse>, Status> {
        Err(Status::unimplemented(
            "QueryWriteStatus is not exercised by these tests",
        ))
    }
}
