//! External integration against a running REAPI server.
//!
//! These tests are skipped unless `FROST_REAPI_ENDPOINT` names one, so the
//! ordinary `cargo test` stays hermetic. The `reapi` GitHub workflow starts
//! BuildGrid 0.8.4 (the certified server) and runs this file against it, which
//! is what proves the wire types interoperate with an implementation other than
//! the in-process test server.

use std::time::Duration;

use frostbuild_reapi::proto::build::bazel::remote::execution::v2 as repb;
use frostbuild_reapi::{Digest, ReapiClient, ReapiConfig};

fn connect() -> Option<ReapiClient> {
    let endpoint = std::env::var("FROST_REAPI_ENDPOINT").ok()?;
    if endpoint.is_empty() {
        return None;
    }
    Some(
        ReapiClient::connect(ReapiConfig {
            endpoint,
            instance_name: std::env::var("FROST_REAPI_INSTANCE").unwrap_or_default(),
            timeout: Duration::from_secs(30),
            auth_header: std::env::var("FROST_REAPI_AUTH").ok(),
        })
        .expect("the external REAPI server must accept the connection"),
    )
}

#[test]
fn cas_and_action_cache_round_trip_against_a_real_server() {
    let Some(client) = connect() else {
        eprintln!("skipping: set FROST_REAPI_ENDPOINT to a running REAPI server");
        return;
    };
    assert_eq!(
        client.capabilities().digest_function.name(),
        "SHA256",
        "the certified server offers SHA-256"
    );

    let data = format!(
        "frost-reapi-integration-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )
    .into_bytes();
    let digest = Digest::sha256(&data);

    let missing = client
        .find_missing_blobs(std::slice::from_ref(&digest))
        .expect("preflight");
    assert!(
        missing.contains(&digest),
        "a fresh blob is missing before it is uploaded"
    );

    client
        .upload_blobs(&[(digest.clone(), data.clone())])
        .expect("upload");
    let missing = client
        .find_missing_blobs(std::slice::from_ref(&digest))
        .expect("preflight");
    assert!(!missing.contains(&digest), "the uploaded blob is present");
    assert_eq!(client.download_blob(&digest).expect("download"), data);

    let action = frostbuild_reapi::trace_key_digest("frost-reapi-integration");
    client
        .update_action_result(
            &action,
            repb::ActionResult {
                exit_code: 7,
                ..Default::default()
            },
        )
        .expect("update the action cache");
    let stored = client
        .get_action_result(&action)
        .expect("get the action cache entry")
        .expect("the entry is present");
    assert_eq!(stored.exit_code, 7);
}
