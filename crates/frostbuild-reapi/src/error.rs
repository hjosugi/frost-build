//! Every way a REAPI exchange can end without an answer.
//!
//! The adapter's contract is that none of these is fatal: each one names what
//! happened so the caller can count it and fall back to local execution. They
//! are classified rather than collapsed into one string because the failure
//! injections the integration tests use — a dropped stream, a blob that fails
//! its digest, a batch that comes back short — have to be told apart from an
//! ordinary cache miss.

/// A remote exchange that did not produce a usable answer.
#[derive(Debug, thiserror::Error)]
pub enum ReapiError {
    /// The channel could not be established, a stream ended early, or a request
    /// timed out. Costs time and nothing else.
    #[error("transport failure: {0}")]
    Transport(String),

    /// The server answered with a non-OK gRPC status.
    #[error("gRPC status {code}: {message}")]
    Rpc { code: i32, message: String },

    /// A blob the server returned does not hash to the digest it was asked for.
    /// The bytes are discarded, never staged.
    #[error("digest mismatch for {digest}")]
    DigestMismatch { digest: String },

    /// The server described an object that the response is missing, or a batch
    /// response did not answer every digest in order.
    #[error("the response for {digest} was missing or out of order")]
    PartialResponse { digest: String },

    /// A blob was refused or unreadable on the server side, with its status.
    #[error("the server rejected {digest}: {message}")]
    BlobRejected { digest: String, message: String },

    /// A digest or resource name frost would not send.
    #[error("invalid digest: {0}")]
    InvalidDigest(String),

    /// The server does not offer the digest function or capability the caller
    /// needs. Negotiation is a hard stop for remote use, not a fallback.
    #[error("the server does not support {0}")]
    Unsupported(String),

    #[error("{0}")]
    Other(String),
}

impl ReapiError {
    /// Whether the failure is a protocol or configuration problem that should
    /// stop remote use entirely rather than be retried as a miss.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }

    pub(crate) fn from_status(status: &tonic::Status) -> Self {
        Self::Rpc {
            code: status.code() as i32,
            message: status.message().to_string(),
        }
    }
}
