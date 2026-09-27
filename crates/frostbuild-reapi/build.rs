//! Generate the REAPI v2 gRPC client and server from the vendored protos.
//!
//! The protocol definitions under `proto/` are the checked-in REAPI v2 subset
//! the adapter speaks. `google/protobuf/*` is vendored alongside them and the
//! `PROTOC` used is `protoc-bin-vendored`, so no host protoc install is assumed
//! and the generated wire types are reproducible from the repository alone.
//!
//! The Google API HTTP-transcoding annotations are stripped from
//! `remote_execution.proto` when it is vendored: they describe a REST surface
//! frost does not call, and keeping them would pull a large `google/api`
//! descriptor tree into the build for no wire difference. Field numbers and
//! service or method names are untouched.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", protoc);
        }
    }
    let protos = [
        "proto/build/bazel/remote/execution/v2/remote_execution.proto",
        "proto/google/bytestream/bytestream.proto",
    ];
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &["proto"])?;
    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }
    println!("cargo:rerun-if-changed=proto/google/rpc/status.proto");
    println!("cargo:rerun-if-changed=proto/google/longrunning/operations.proto");
    println!("cargo:rerun-if-changed=proto/build/bazel/semver/semver.proto");
    Ok(())
}
