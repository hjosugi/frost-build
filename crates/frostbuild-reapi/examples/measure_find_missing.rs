//! Measure the upload reduction a `FindMissingBlobs` preflight buys.
//!
//! Run it to reproduce the checked-in figure:
//!
//! ```text
//! cargo run -p frostbuild-reapi --example measure_find_missing
//! ```
//!
//! The number depends only on how many candidates are already present, so the
//! shape is deterministic; the in-process server keeps the measurement free of
//! network variance.

use std::time::Duration;

use frostbuild_reapi::testing::TestServer;
use frostbuild_reapi::{Digest, ReapiClient, ReapiConfig};

fn main() {
    let total = 1000usize;
    let present = total / 2;
    let server = TestServer::start();
    let client = ReapiClient::connect(ReapiConfig {
        endpoint: server.endpoint().to_string(),
        instance_name: "measure".into(),
        timeout: Duration::from_secs(30),
        auth_header: None,
    })
    .expect("connect to the in-process server");

    let blobs: Vec<(Digest, Vec<u8>)> = (0..total)
        .map(|index| {
            let data = format!("measure-{index}").into_bytes();
            (Digest::sha256(&data), data)
        })
        .collect();
    client
        .upload_blobs(&blobs[..present])
        .expect("seed the CAS");

    let candidates: Vec<Digest> = blobs.iter().map(|(digest, _)| digest.clone()).collect();
    let missing = client
        .find_missing_blobs(&candidates)
        .expect("preflight the whole candidate set");

    let without_preflight = total;
    let with_preflight = missing.len();
    let reduction_percent =
        100.0 * (without_preflight - with_preflight) as f64 / without_preflight as f64;
    println!(
        "{{\n  \"schema\": \"frost-reapi-find-missing-v1\",\n  \
         \"command\": \"cargo run -p frostbuild-reapi --example measure_find_missing\",\n  \
         \"server\": \"in-process tonic\",\n  \
         \"candidates\": {total},\n  \
         \"already_present\": {present},\n  \
         \"uploads_without_preflight\": {without_preflight},\n  \
         \"uploads_with_preflight\": {with_preflight},\n  \
         \"reduction_percent\": {reduction_percent:.1}\n\
         }}"
    );
}
