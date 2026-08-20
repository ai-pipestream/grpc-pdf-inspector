// SPDX-License-Identifier: Apache-2.0

//! The caps that make it safe to point this server at a file someone else
//! made.
//!
//! Unlike the per-call options of some fleet siblings, these ceilings are
//! process-wide only: a PDF call has nothing worth letting a caller narrow
//! (no archive to inflate, no entry count), so [`Limits`] is what the
//! process was started with, from defaults or the environment, and it is
//! final.

use crate::proto::v1 as pb;

/// One mebibyte, the unit the defaults are expressed in.
pub const MIB: u64 = 1024 * 1024;

/// Default ceiling on the upload: 128 MiB.
///
/// Sized for real documents, not for archives: a 128 MiB PDF is a scanned
/// book of a thousand pages, and anything past it is almost always a file
/// that should be split before it is parsed.
pub const DEFAULT_MAX_DOCUMENT_BYTES: u64 = 128 * MIB;

/// Default ceiling on a single inbound `chunk` frame.
///
/// Not a document limit: an upload is any number of chunks. This bounds the
/// transient per-call buffer and, with it, how much one hostile length prefix
/// can make the transport allocate.
pub const DEFAULT_MAX_CHUNK_BYTES: u64 = 16 * MIB;

/// Default ceiling on concurrent parsing calls.
///
/// The bound exists to cap CPU and heap, not to shed load: each in-flight
/// call can hold its upload plus the extracted text, and extraction
/// parallelizes across cores with rayon, so eight at once is already the
/// whole machine. Calls past the bound wait.
pub const DEFAULT_MAX_CONCURRENT_PARSES: usize = 8;

/// Ceilings the process enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest upload accepted.
    pub max_document_bytes: u64,
    /// Largest single inbound `chunk` frame.
    pub max_chunk_bytes: u64,
    /// Largest number of calls that may parse at once.
    pub max_concurrent_parses: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
            max_concurrent_parses: DEFAULT_MAX_CONCURRENT_PARSES,
        }
    }
}

/// Read a `u64` environment variable, falling back to `default`.
///
/// A value that does not parse is ignored rather than fatal, and a zero is
/// treated as "not set": no limit here has a meaningful zero, and silently
/// running with an unbounded cap because of a typo is the failure mode worth
/// designing out.
fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(0) | None => default,
        Some(value) => value,
    }
}

impl Limits {
    /// Build the process limits from `GRPC_PDF_*` environment variables,
    /// falling back to the defaults above.
    ///
    /// See the README for the full list.
    #[must_use]
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            max_document_bytes: env_u64("GRPC_PDF_MAX_BYTES", DEFAULT_MAX_DOCUMENT_BYTES),
            max_chunk_bytes: env_u64("GRPC_PDF_MAX_CHUNK_BYTES", DEFAULT_MAX_CHUNK_BYTES),
            max_concurrent_parses: usize::try_from(env_u64(
                "GRPC_PDF_MAX_CONCURRENT_PARSES",
                defaults.max_concurrent_parses as u64,
            ))
            .unwrap_or(defaults.max_concurrent_parses),
        }
    }

    /// Render these limits for `GetServiceInfo`.
    #[must_use]
    pub fn to_proto(self) -> pb::ServerLimits {
        pb::ServerLimits {
            max_document_bytes: self.max_document_bytes,
            max_chunk_bytes: self.max_chunk_bytes,
            max_concurrent_parses: u32::try_from(self.max_concurrent_parses).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let limits = Limits::default();
        assert_eq!(limits.max_document_bytes, 128 * MIB);
        assert_eq!(limits.max_chunk_bytes, 16 * MIB);
        assert_eq!(limits.max_concurrent_parses, 8);
    }
}
