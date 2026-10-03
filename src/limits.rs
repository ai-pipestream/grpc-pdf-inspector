// SPDX-License-Identifier: Apache-2.0

//! The caps that make it safe to point this server at a file someone else
//! made.
//!
//! Unlike the per-call options of some fleet siblings, these ceilings are
//! process-wide only: [`Limits`] is what the process was started with, from
//! defaults or the environment, and it is final. A caller narrows nothing,
//! because every one of them protects the process rather than the call.

use std::time::Duration;

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
/// whole machine. Calls past the bound wait, and a call takes its slot
/// before its upload is read, so the waiting ones hold no upload either.
pub const DEFAULT_MAX_CONCURRENT_PARSES: usize = 8;

/// Default ceiling on how far any one stream of a document may decompress:
/// 256 MiB.
///
/// A Flate stream inflates about a thousand to one, so a few megabytes of
/// upload can name tens of gigabytes. Real content, font and CMap streams
/// stay far below this; a stream past it fails the call rather than the
/// process.
pub const DEFAULT_MAX_STREAM_BYTES: u64 = 256 * MIB;

/// Default ceiling on how much one read of a document may decompress in
/// all: 4 GiB.
///
/// The per-stream cap bounds memory; this bounds the work a document made
/// of many streams just under that cap can demand. A FULL call reads the
/// document more than once and each read has its own budget.
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: u64 = 4096 * MIB;

/// Default wall-clock budget for one call, from the moment it is admitted
/// to a parse slot until its trailer: five minutes, which is also the
/// longest gRParse waits for this collector.
pub const DEFAULT_MAX_PARSE_SECONDS: u64 = 300;

/// Ceilings the process enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest upload accepted.
    pub max_document_bytes: u64,
    /// Largest single inbound `chunk` frame.
    pub max_chunk_bytes: u64,
    /// Largest number of calls that may parse at once.
    pub max_concurrent_parses: usize,
    /// Largest size any one stream of a document may decompress to.
    pub max_stream_bytes: u64,
    /// Largest total one read of a document may decompress to.
    pub max_decompressed_bytes: u64,
    /// Longest a call may hold its parse slot, upload included.
    pub max_parse_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
            max_concurrent_parses: DEFAULT_MAX_CONCURRENT_PARSES,
            max_stream_bytes: DEFAULT_MAX_STREAM_BYTES,
            max_decompressed_bytes: DEFAULT_MAX_DECOMPRESSED_BYTES,
            max_parse_time: Duration::from_secs(DEFAULT_MAX_PARSE_SECONDS),
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
            max_stream_bytes: env_u64("GRPC_PDF_MAX_STREAM_BYTES", DEFAULT_MAX_STREAM_BYTES),
            max_decompressed_bytes: env_u64(
                "GRPC_PDF_MAX_DECOMPRESSED_BYTES",
                DEFAULT_MAX_DECOMPRESSED_BYTES,
            ),
            max_parse_time: Duration::from_secs(env_u64(
                "GRPC_PDF_MAX_PARSE_SECONDS",
                DEFAULT_MAX_PARSE_SECONDS,
            )),
        }
    }

    /// The decompression bounds one call parses under, as the parser's
    /// guard takes them. The deadline and the cancellation flag are the
    /// call's own and are added by the caller.
    #[must_use]
    pub fn parse_guard(self) -> pdf_inspector::ParseGuard {
        pdf_inspector::ParseGuard::new(
            usize::try_from(self.max_stream_bytes).unwrap_or(usize::MAX),
            self.max_decompressed_bytes,
        )
    }

    /// Render these limits for `GetServiceInfo`.
    #[must_use]
    pub fn to_proto(self) -> pb::ServerLimits {
        pb::ServerLimits {
            max_document_bytes: self.max_document_bytes,
            max_chunk_bytes: self.max_chunk_bytes,
            max_concurrent_parses: u32::try_from(self.max_concurrent_parses).unwrap_or(u32::MAX),
            max_stream_bytes: self.max_stream_bytes,
            max_decompressed_bytes: self.max_decompressed_bytes,
            max_parse_seconds: self.max_parse_time.as_secs(),
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
        assert_eq!(limits.max_stream_bytes, 256 * MIB);
        assert_eq!(limits.max_decompressed_bytes, 4096 * MIB);
        assert_eq!(limits.max_parse_time, Duration::from_secs(300));
    }
}
