//! Integration tests for the REAPI v2 client against a real gRPC server.
//!
//! The server is in-process and in-memory, but the transport is not: these
//! tests exercise the generated stubs, HTTP/2 framing and protobuf codec, so a
//! mistake in the protocol definitions shows up here rather than against a
//! remote executor. The failure injections the issue requires — a dropped
//! stream, a blob that fails its digest, a batch that comes back short — are
//! each asserted to surface as a typed error the caller can fall back on
//! rather than as a panic or a silently accepted wrong answer.

use std::time::Duration;

use frostbuild_reapi::testing::{Faults, TestServer};
use frostbuild_reapi::{Digest, ReapiClient, ReapiConfig, ReapiError};

fn connect(server: &TestServer) -> ReapiClient {
    ReapiClient::connect(ReapiConfig {
        endpoint: server.endpoint().to_string(),
        instance_name: "test-instance".into(),
        timeout: Duration::from_secs(10),
        auth_header: None,
    })
    .expect("the test server must accept the connection")
}

fn blobs(count: usize) -> Vec<(Digest, Vec<u8>)> {
    (0..count)
        .map(|index| {
            let data = format!("blob-{index}").into_bytes();
            let digest = Digest::sha256(&data);
            (digest, data)
        })
        .collect()
}

#[test]
fn capabilities_are_negotiated_before_any_work() {
    let server = TestServer::start();
    let client = connect(&server);
    let capabilities = client.capabilities();
    assert_eq!(capabilities.digest_function.name(), "SHA256");
    assert!(capabilities.cas_enabled);
    assert!(capabilities.exec_enabled);
    assert_eq!(capabilities.low_api, "2.0.0");
    assert_eq!(capabilities.high_api, "2.2.0");
    assert_eq!(capabilities.max_batch_total_size, 64 * 1024);
}

#[test]
fn cas_round_trips_and_a_downloaded_blob_verifies() {
    let server = TestServer::start();
    let client = connect(&server);
    let blobs = blobs(3);

    let missing = client
        .find_missing_blobs(
            &blobs
                .iter()
                .map(|(digest, _)| digest.clone())
                .collect::<Vec<_>>(),
        )
        .expect("preflight");
    assert_eq!(missing.len(), 3, "an empty CAS is missing every blob");

    client.upload_blobs(&blobs).expect("upload");
    assert_eq!(server.cas_count(), 3);

    let after = client
        .find_missing_blobs(
            &blobs
                .iter()
                .map(|(digest, _)| digest.clone())
                .collect::<Vec<_>>(),
        )
        .expect("preflight");
    assert!(after.is_empty(), "everything just uploaded is present");

    let (digest, data) = &blobs[1];
    assert_eq!(&client.download_blob(digest).expect("download"), data);
}

#[test]
fn find_missing_blobs_reduces_uploads_to_only_what_is_absent() {
    // Criterion: show, with numbers, that a preflight cuts the upload set. Half
    // the candidates are already present; only the other half is uploaded.
    let server = TestServer::start();
    let client = connect(&server);
    let all = blobs(200);
    let (present, absent) = all.split_at(100);
    client.upload_blobs(present).expect("seed half the CAS");

    let candidates: Vec<Digest> = all.iter().map(|(digest, _)| digest.clone()).collect();
    let missing = client.find_missing_blobs(&candidates).expect("preflight");
    assert_eq!(
        missing.len(),
        absent.len(),
        "only the absent half is missing"
    );
    assert_eq!(
        server.batch_update_calls(),
        1,
        "seeding happened in one batch"
    );

    client
        .upload_blobs(
            &absent
                .iter()
                .map(|(digest, data)| (digest.clone(), data.clone()))
                .collect::<Vec<_>>(),
        )
        .expect("upload the missing half");

    let uploads_without_preflight = all.len();
    let uploads_with_preflight = missing.len();
    assert_eq!(uploads_without_preflight, 200);
    assert_eq!(uploads_with_preflight, 100);
    assert_eq!(
        uploads_with_preflight * 2,
        uploads_without_preflight,
        "the preflight halves the upload set here"
    );
    assert!(client
        .find_missing_blobs(&candidates)
        .expect("preflight")
        .is_empty());
}

#[test]
fn a_blob_larger_than_the_batch_limit_uses_bytestream() {
    let server = TestServer::start();
    let client = connect(&server);
    let data = vec![0x5a; 200 * 1024];
    let digest = Digest::sha256(&data);
    assert!(digest.size_bytes > client.capabilities().max_batch_total_size);

    client
        .upload_blobs(&[(digest.clone(), data.clone())])
        .expect("large upload");
    assert_eq!(
        server.bytestream_writes(),
        1,
        "the large blob used ByteStream"
    );

    let downloaded = client.download_blob(&digest).expect("large download");
    assert_eq!(downloaded, data);
}

#[test]
fn the_action_cache_round_trips() {
    let server = TestServer::start();
    let client = connect(&server);
    let action_digest = Digest::sha256(b"action-1");
    assert!(client
        .get_action_result(&action_digest)
        .expect("a miss is not an error")
        .is_none());

    let result = frostbuild_reapi::proto::build::bazel::remote::execution::v2::ActionResult {
        exit_code: 0,
        ..Default::default()
    };
    client
        .update_action_result(&action_digest, result)
        .expect("update");
    let stored = client
        .get_action_result(&action_digest)
        .expect("get")
        .expect("the entry is present");
    assert_eq!(stored.exit_code, 0);
}

#[test]
fn execute_returns_the_servers_result() {
    let server = TestServer::start();
    let client = connect(&server);
    let action_digest = Digest::sha256(b"action-execute");
    client
        .update_action_result(
            &action_digest,
            frostbuild_reapi::proto::build::bazel::remote::execution::v2::ActionResult {
                exit_code: 0,
                stdout_raw: b"hello".to_vec(),
                ..Default::default()
            },
        )
        .expect("seed the action cache");
    let response = client
        .execute(&action_digest, true)
        .expect("execute must complete");
    let result = response
        .result
        .expect("the response carries an ActionResult");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout_raw, b"hello");
}

#[test]
fn a_blob_that_fails_its_digest_is_refused() {
    let server = TestServer::start_with(Faults {
        corrupt_read: true,
        ..Faults::default()
    });
    let client = connect(&server);
    let (digest, data) = blobs(1).pop().unwrap();
    server.put(&digest, &data);
    match client.download_blob(&digest) {
        Err(ReapiError::DigestMismatch { .. }) => {}
        other => panic!("a corrupt blob must be a DigestMismatch, got {other:?}"),
    }
}

#[test]
fn a_short_batch_response_is_a_partial_response() {
    let server = TestServer::start_with(Faults {
        partial_read: true,
        ..Faults::default()
    });
    let client = connect(&server);
    let (digest, data) = blobs(1).pop().unwrap();
    server.put(&digest, &data);
    match client.download_blob(&digest) {
        Err(ReapiError::PartialResponse { .. }) => {}
        other => panic!("a short batch must be a PartialResponse, got {other:?}"),
    }
}

#[test]
fn a_refused_upload_is_reported_per_blob() {
    let server = TestServer::start_with(Faults {
        reject_write: true,
        ..Faults::default()
    });
    let client = connect(&server);
    let blobs = blobs(2);
    match client.upload_blobs(&blobs) {
        Err(ReapiError::BlobRejected { .. }) => {}
        other => panic!("a refused upload must be a BlobRejected, got {other:?}"),
    }
}

#[test]
fn a_stream_that_ends_unfinished_is_a_transport_failure() {
    let server = TestServer::start_with(Faults {
        disconnect_execute: true,
        ..Faults::default()
    });
    let client = connect(&server);
    let action_digest = Digest::sha256(b"action-disconnected");
    match client.execute(&action_digest, true) {
        Err(ReapiError::Transport(_)) => {}
        other => panic!("a dropped Execute stream must be a Transport error, got {other:?}"),
    }
}

#[test]
fn only_the_two_grpc_schemes_are_accepted() {
    assert!(ReapiClient::connect(ReapiConfig::new("https://cache.example")).is_err());
    assert!(ReapiClient::connect(ReapiConfig::new("unix:///run/cache.sock")).is_err());
    assert!(ReapiClient::connect(ReapiConfig::new("cache.example:50051")).is_err());
}

#[test]
fn an_authorization_header_is_sent_when_configured() {
    let server = TestServer::start_with(Faults {
        require_auth: Some("Bearer token".into()),
        ..Faults::default()
    });
    assert!(
        ReapiClient::connect(ReapiConfig::new(server.endpoint())).is_err(),
        "a server that requires auth must refuse a client without it"
    );
    let client = ReapiClient::connect(ReapiConfig {
        endpoint: server.endpoint().to_string(),
        instance_name: String::new(),
        timeout: Duration::from_secs(10),
        auth_header: Some("Bearer token".into()),
    })
    .expect("the header is sent and accepted");
    assert!(client.capabilities().cas_enabled);
}

#[test]
fn a_trace_key_maps_to_one_stable_digest() {
    let first = frostbuild_reapi::trace_key_digest("a".repeat(64).as_str());
    let second = frostbuild_reapi::trace_key_digest("a".repeat(64).as_str());
    let other = frostbuild_reapi::trace_key_digest("b".repeat(64).as_str());
    assert_eq!(first, second, "the same key must address the same entry");
    assert_ne!(first, other, "different keys must not collide");
    assert_eq!(first.hash.len(), 64, "it is a SHA-256");
}
