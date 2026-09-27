//! Digests and the functions that produce them.
//!
//! REAPI addresses every blob and message by `(hash, size)`. Frost's local CAS
//! is BLAKE3-keyed, so the adapter computes SHA-256 separately rather than
//! reinterpreting a local digest; the two never get conflated. A digest this
//! module prints is always lowercase hex, matching what a REAPI server expects.

use sha2::{Digest as _, Sha256};

/// A content digest as REAPI names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest {
    pub hash: String,
    pub size_bytes: i64,
}

impl Digest {
    /// SHA-256 of `bytes`. Negative sizes cannot occur here, but the field is
    /// signed because the protocol declares it `int64`.
    pub fn sha256(bytes: &[u8]) -> Self {
        let sum = Sha256::digest(bytes);
        Self {
            hash: hex(&sum),
            size_bytes: bytes.len() as i64,
        }
    }

    /// Parse the `hash/size` form used in REAPI resource names.
    pub fn parse(value: &str) -> Option<Self> {
        let (hash, size) = value.split_once('/')?;
        if hash.is_empty() || size.is_empty() {
            return None;
        }
        let size_bytes = size.parse::<i64>().ok()?;
        if size_bytes < 0 {
            return None;
        }
        Some(Self {
            hash: hash.to_string(),
            size_bytes,
        })
    }

    /// The `hash/size` form, for resource names and for the counters.
    pub fn key(&self) -> String {
        format!("{}/{}", self.hash, self.size_bytes)
    }

    /// Recompute the digest of `bytes` and require it to equal this one.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        *self == Self::sha256(bytes)
    }
}

/// The digest function the adapter negotiates. SHA-256 is the only one every
/// REAPI v2 server must offer; the type exists so a capability mismatch is
/// named rather than assumed away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestFunction {
    Sha256,
}

impl DigestFunction {
    pub const fn reapi_value(self) -> i32 {
        match self {
            Self::Sha256 => 1,
        }
    }

    pub fn from_reapi(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Sha256),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA256",
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_hashes_to_the_known_vector() {
        assert_eq!(
            Digest::sha256(b"").hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(Digest::sha256(b"abc").hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn digest_round_trips_through_its_resource_name() {
        let digest = Digest::sha256(b"payload");
        assert_eq!(Digest::parse(&digest.key()), Some(digest.clone()));
        assert!(digest.verify(b"payload"));
        assert!(!digest.verify(b"other"));
        assert_eq!(Digest::parse("nonsense"), None);
        assert_eq!(Digest::parse("hash/-1"), None);
    }
}
