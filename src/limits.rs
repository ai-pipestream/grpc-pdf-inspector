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

/// Default wall-clock budget for one call's upload, from the moment it is
/// admitted to a parse slot until its last frame: one minute.
///
/// The slot is taken before the upload is read, so the upload has a budget
/// of its own, well inside the call's: without it, a client trickling a byte
/// at a time could keep a slot for the whole parse budget without ever
/// handing over a document. A minute carries the default upload cap at
/// about 18 Mbit/s; a deployment fed over slower links raises it.
pub const DEFAULT_MAX_UPLOAD_SECONDS: u64 = 60;

/// The longest either time budget may be set to: a day.
///
/// A deadline is an [`std::time::Instant`], which cannot reach arbitrarily
/// far into the future, so a budget past this is refused at startup rather
/// than overflowing on the first call. No parse is worth a slot for longer.
pub const MAX_TIME_BUDGET_SECONDS: u64 = 24 * 60 * 60;

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
    /// Longest a call's upload may take once it holds its slot. The upload
    /// also spends `max_parse_time`, so the shorter of the two ends it.
    pub max_upload_time: Duration,
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
            max_upload_time: Duration::from_secs(DEFAULT_MAX_UPLOAD_SECONDS),
        }
    }
}

/// A limit the environment set to a value the server will not run with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidLimit {
    /// The environment variable.
    pub name: &'static str,
    /// What it was set to.
    pub value: String,
    /// Why that cannot be used.
    pub reason: &'static str,
}

impl std::fmt::Display for InvalidLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={:?} {}", self.name, self.value, self.reason)
    }
}

impl std::error::Error for InvalidLimit {}

/// Read a whole number that must be positive if it is set, falling back to
/// `default` only when the variable is not set at all.
///
/// A value that does not parse, or a zero, stops the server. No limit here
/// has a meaningful zero, and `512M` or `10m` quietly meaning the default
/// left an operator believing a limit was in force that was not.
///
/// # Errors
///
/// [`InvalidLimit`] naming the variable and what it was set to.
pub fn env_positive(
    var: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: u64,
) -> Result<u64, InvalidLimit> {
    let Some(value) = var(name) else {
        return Ok(default);
    };
    match value.trim().parse::<u64>() {
        Ok(0) => Err(InvalidLimit {
            name,
            value,
            reason: "must be at least 1",
        }),
        Ok(number) => Ok(number),
        Err(_) => Err(InvalidLimit {
            name,
            value,
            reason: "is not a whole number (write sizes in bytes and times in seconds)",
        }),
    }
}

/// [`env_positive`], for a setting where zero means "off".
///
/// # Errors
///
/// [`InvalidLimit`] when the value does not parse.
pub fn env_whole(
    var: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: u64,
) -> Result<u64, InvalidLimit> {
    match var(name) {
        Some(value) if value.trim() == "0" => Ok(0),
        _ => env_positive(var, name, default),
    }
}

/// Read a whole number of seconds that must be positive if it is set, and
/// no longer than a day.
fn env_seconds(
    var: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: u64,
) -> Result<u64, InvalidLimit> {
    let Some(value) = var(name) else {
        return Ok(default);
    };
    match value.trim().parse::<u64>() {
        Ok(0) => Err(InvalidLimit {
            name,
            value,
            reason: "must be at least one second",
        }),
        Ok(seconds) => at_most_a_day(name, seconds),
        Err(_) => Err(InvalidLimit {
            name,
            value,
            reason: "is not a whole number of seconds",
        }),
    }
}

/// Refuse a time budget past [`MAX_TIME_BUDGET_SECONDS`].
fn at_most_a_day(name: &'static str, seconds: u64) -> Result<u64, InvalidLimit> {
    if seconds > MAX_TIME_BUDGET_SECONDS {
        return Err(InvalidLimit {
            name,
            value: seconds.to_string(),
            reason: "is longer than the 86400 second (one day) ceiling",
        });
    }
    Ok(seconds)
}

impl Limits {
    /// Build the process limits from `GRPC_PDF_*` environment variables,
    /// falling back to the defaults above.
    ///
    /// See the README for the full list.
    ///
    /// # Errors
    ///
    /// [`InvalidLimit`] when a variable is set to a value the server must
    /// not start with, so a misconfigured process fails at startup rather
    /// than running with a limit nobody asked for.
    pub fn from_env() -> Result<Self, InvalidLimit> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// [`Self::from_env`], reading each variable through `var` rather than
    /// from the process environment.
    ///
    /// # Errors
    ///
    /// As [`Self::from_env`].
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, InvalidLimit> {
        let defaults = Self::default();
        Ok(Self {
            max_document_bytes: env_positive(
                &var,
                "GRPC_PDF_MAX_BYTES",
                DEFAULT_MAX_DOCUMENT_BYTES,
            )?,
            max_chunk_bytes: env_positive(
                &var,
                "GRPC_PDF_MAX_CHUNK_BYTES",
                DEFAULT_MAX_CHUNK_BYTES,
            )?,
            max_concurrent_parses: usize::try_from(env_positive(
                &var,
                "GRPC_PDF_MAX_CONCURRENT_PARSES",
                defaults.max_concurrent_parses as u64,
            )?)
            .unwrap_or(usize::MAX),
            max_stream_bytes: env_positive(
                &var,
                "GRPC_PDF_MAX_STREAM_BYTES",
                DEFAULT_MAX_STREAM_BYTES,
            )?,
            max_decompressed_bytes: env_positive(
                &var,
                "GRPC_PDF_MAX_DECOMPRESSED_BYTES",
                DEFAULT_MAX_DECOMPRESSED_BYTES,
            )?,
            max_parse_time: Duration::from_secs(env_seconds(
                &var,
                "GRPC_PDF_MAX_PARSE_SECONDS",
                DEFAULT_MAX_PARSE_SECONDS,
            )?),
            max_upload_time: Duration::from_secs(env_seconds(
                &var,
                "GRPC_PDF_MAX_UPLOAD_SECONDS",
                DEFAULT_MAX_UPLOAD_SECONDS,
            )?),
        })
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

/// The server's sizing knobs: not safety limits, but read and refused the
/// same way, so a typo in one does not quietly mean its default either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerSettings {
    /// Tokio worker threads (`GRPC_PDF_WORKERS`). At least one.
    pub workers: usize,
    /// HTTP/2 initial window for the stream and the connection
    /// (`GRPC_PDF_WINDOW_BYTES`). At least one byte and at most 2^31 - 1,
    /// the largest window HTTP/2 allows.
    pub window_bytes: u32,
    /// Seconds between metrics lines (`GRPC_PDF_METRICS_INTERVAL_SECS`);
    /// zero turns them off.
    pub metrics_interval: Duration,
}

/// The largest HTTP/2 flow-control window, 2^31 - 1 bytes.
const MAX_WINDOW_BYTES: u64 = (1 << 31) - 1;

impl ServerSettings {
    /// Read the settings through `var`, with `workers` as the default
    /// worker count and the given defaults for the rest.
    ///
    /// # Errors
    ///
    /// [`InvalidLimit`] when a variable is set to a value the server must
    /// not start with.
    pub fn from_vars(
        var: impl Fn(&str) -> Option<String>,
        workers: usize,
        window_bytes: u32,
        metrics_interval_secs: u64,
    ) -> Result<Self, InvalidLimit> {
        let window = env_positive(&var, "GRPC_PDF_WINDOW_BYTES", u64::from(window_bytes))?;
        if window > MAX_WINDOW_BYTES {
            return Err(InvalidLimit {
                name: "GRPC_PDF_WINDOW_BYTES",
                value: window.to_string(),
                reason: "is larger than the 2147483647 byte HTTP/2 window ceiling",
            });
        }
        Ok(Self {
            workers: usize::try_from(env_positive(&var, "GRPC_PDF_WORKERS", workers as u64)?)
                .unwrap_or(usize::MAX),
            window_bytes: u32::try_from(window).unwrap_or(u32::MAX),
            metrics_interval: Duration::from_secs(env_whole(
                &var,
                "GRPC_PDF_METRICS_INTERVAL_SECS",
                metrics_interval_secs,
            )?),
        })
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
        assert_eq!(limits.max_upload_time, Duration::from_secs(60));
    }

    /// Limits read from `vars` alone, as if they were the whole environment.
    fn read(vars: &[(&str, &str)]) -> Result<Limits, InvalidLimit> {
        Limits::from_vars(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn an_empty_environment_is_the_defaults() {
        assert_eq!(read(&[]), Ok(Limits::default()));
    }

    #[test]
    fn the_upload_budget_is_read_from_the_environment() {
        let limits = read(&[("GRPC_PDF_MAX_UPLOAD_SECONDS", "15")]).expect("a valid budget");
        assert_eq!(limits.max_upload_time, Duration::from_secs(15));
    }

    #[test]
    fn a_zero_or_unreadable_upload_budget_stops_startup() {
        for value in ["0", "", "sixty", "-5", "1.5"] {
            let error = read(&[("GRPC_PDF_MAX_UPLOAD_SECONDS", value)])
                .expect_err("the server must not start with this");
            assert_eq!(error.name, "GRPC_PDF_MAX_UPLOAD_SECONDS", "{value:?}");
            assert_eq!(error.value, value);
        }
    }

    /// Every limit read as a byte size, a count or seconds.
    const NUMERIC_LIMITS: [&str; 6] = [
        "GRPC_PDF_MAX_BYTES",
        "GRPC_PDF_MAX_CHUNK_BYTES",
        "GRPC_PDF_MAX_CONCURRENT_PARSES",
        "GRPC_PDF_MAX_STREAM_BYTES",
        "GRPC_PDF_MAX_DECOMPRESSED_BYTES",
        "GRPC_PDF_MAX_PARSE_SECONDS",
    ];

    #[test]
    fn a_limit_that_does_not_parse_or_is_zero_stops_startup() {
        // "512M" and "10m" are how operators write sizes and durations
        // elsewhere; here they used to mean the default, silently.
        for name in NUMERIC_LIMITS {
            for value in ["512M", "10m", "0", "", "-1", "1.5", "eight"] {
                let error = read(&[(name, value)])
                    .expect_err("the server must not start with a limit nobody chose");
                assert_eq!(error.name, name, "{value:?}");
                assert_eq!(error.value, value, "{name}");
            }
        }
    }

    #[test]
    fn every_limit_is_read_from_the_environment() {
        for name in NUMERIC_LIMITS {
            let limits = read(&[(name, " 42 ")]).expect("a valid limit");
            let read_back = match name {
                "GRPC_PDF_MAX_BYTES" => limits.max_document_bytes,
                "GRPC_PDF_MAX_CHUNK_BYTES" => limits.max_chunk_bytes,
                "GRPC_PDF_MAX_CONCURRENT_PARSES" => limits.max_concurrent_parses as u64,
                "GRPC_PDF_MAX_STREAM_BYTES" => limits.max_stream_bytes,
                "GRPC_PDF_MAX_DECOMPRESSED_BYTES" => limits.max_decompressed_bytes,
                _ => limits.max_parse_time.as_secs(),
            };
            assert_eq!(read_back, 42, "{name}");
        }
    }

    /// Server settings read from `vars` alone.
    fn settings(vars: &[(&str, &str)]) -> Result<ServerSettings, InvalidLimit> {
        ServerSettings::from_vars(
            |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            },
            4,
            4 * 1024 * 1024,
            60,
        )
    }

    #[test]
    fn the_server_settings_refuse_what_they_cannot_use() {
        assert_eq!(
            settings(&[]),
            Ok(ServerSettings {
                workers: 4,
                window_bytes: 4 * 1024 * 1024,
                metrics_interval: Duration::from_secs(60),
            })
        );
        for (name, value) in [
            ("GRPC_PDF_WORKERS", "0"),
            ("GRPC_PDF_WORKERS", "all"),
            ("GRPC_PDF_WINDOW_BYTES", "4M"),
            ("GRPC_PDF_WINDOW_BYTES", "0"),
            ("GRPC_PDF_WINDOW_BYTES", "2147483648"),
            ("GRPC_PDF_METRICS_INTERVAL_SECS", "1m"),
            ("GRPC_PDF_METRICS_INTERVAL_SECS", "-1"),
        ] {
            let error = settings(&[(name, value)]).expect_err("refused");
            assert_eq!(error.name, name, "{value:?}");
        }
        let off = settings(&[("GRPC_PDF_METRICS_INTERVAL_SECS", "0")]).expect("zero is off");
        assert_eq!(off.metrics_interval, Duration::ZERO);
        let set = settings(&[
            ("GRPC_PDF_WORKERS", "16"),
            ("GRPC_PDF_WINDOW_BYTES", "65535"),
        ])
        .expect("valid");
        assert_eq!((set.workers, set.window_bytes), (16, 65_535));
    }

    #[test]
    fn a_time_budget_past_a_day_stops_startup() {
        for name in ["GRPC_PDF_MAX_PARSE_SECONDS", "GRPC_PDF_MAX_UPLOAD_SECONDS"] {
            let a_day = MAX_TIME_BUDGET_SECONDS.to_string();
            assert!(read(&[(name, &a_day)]).is_ok(), "{name} of a day");
            for value in ["86401", "18446744073709551615"] {
                let error = read(&[(name, value)]).expect_err("past the ceiling");
                assert_eq!(error.name, name);
                assert_eq!(error.value, value);
            }
        }
    }
}
