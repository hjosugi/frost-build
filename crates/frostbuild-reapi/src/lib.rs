//! REAPI v2 client.
//!
//! This crate speaks the Remote Execution API over gRPC so frost can use a
//! REAPI-compatible CAS / Action Cache and, at minimum, an `Execute` path. It is
//! deliberately isolated from the rest of the workspace: the protocol stack
//! (gRPC over HTTP/2, TLS, protobuf) is a large dependency surface, and nothing
//! here may weaken the v1 invariants. A remote answer is only ever used after
//! the caller verifies it against the digest it asked for; every failure this
//! crate reports means "fall back to local", never "fail the build".

pub mod client;
pub mod digest;
pub mod error;

#[doc(hidden)]
pub mod testing;

/// The generated REAPI v2 messages, services and clients.
///
/// prost copies every protocol comment into a doc comment, and the upstream
/// protos are full of unindented list continuations. The generated code is
/// vendored, not authored here, so the formatting lints are silenced at the
/// module boundary rather than by rewriting the protos.
#[allow(clippy::doc_lazy_continuation)]
pub mod proto {
    /// `build.bazel.semver.SemanticVersion`, used by `GetCapabilities`.
    pub mod build {
        pub mod bazel {
            pub mod semver {
                include!(concat!(env!("OUT_DIR"), "/build.bazel.semver.rs"));
            }
            pub mod remote {
                pub mod execution {
                    pub mod v2 {
                        include!(concat!(env!("OUT_DIR"), "/build.bazel.remote.execution.v2.rs"));
                    }
                }
            }
        }
    }
    pub mod google {
        pub mod bytestream {
            include!(concat!(env!("OUT_DIR"), "/google.bytestream.rs"));
        }
        pub mod longrunning {
            include!(concat!(env!("OUT_DIR"), "/google.longrunning.rs"));
        }
        pub mod rpc {
            include!(concat!(env!("OUT_DIR"), "/google.rpc.rs"));
        }
    }
}

pub use client::{Capabilities, ReapiClient, ReapiConfig};
pub use digest::{Digest, DigestFunction};
pub use error::ReapiError;
