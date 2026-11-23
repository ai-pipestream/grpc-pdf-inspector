// SPDX-License-Identifier: Apache-2.0

//! Where the vendored parser's own diagnostics go.
//!
//! The parser reports some things only through the `log` facade: a page
//! whose content stream is past the operator cap, a structure tree cut off
//! at its node budget, a CMap whose `usecmap` base could not be loaded.
//! With no logger installed those calls are no-ops, and the operator never
//! sees them. This is the smallest backend that fixes that: one line per
//! record on standard error, at `warn` and above unless `GRPC_PDF_LOG`
//! says otherwise.

use std::io::Write;

use crate::limits::InvalidLimit;

/// The level records are kept at when `GRPC_PDF_LOG` is not set.
pub const DEFAULT_LEVEL: log::LevelFilter = log::LevelFilter::Warn;

/// Read `GRPC_PDF_LOG` through `var`: `off`, `error`, `warn`, `info`,
/// `debug` or `trace`, in any case.
///
/// # Errors
///
/// [`InvalidLimit`] for any other value, so a typo stops the server rather
/// than quietly logging at a level nobody chose.
pub fn level_from_vars(
    var: impl Fn(&str) -> Option<String>,
) -> Result<log::LevelFilter, InvalidLimit> {
    let Some(value) = var("GRPC_PDF_LOG") else {
        return Ok(DEFAULT_LEVEL);
    };
    value.trim().parse().map_err(|_| InvalidLimit {
        name: "GRPC_PDF_LOG",
        value,
        reason: "is not one of off, error, warn, info, debug or trace",
    })
}

/// One record, as the line it is written as.
#[must_use]
pub fn line(record: &log::Record<'_>) -> String {
    format!(
        "grpc-pdf-inspector: {} {}: {}",
        record.level(),
        record.target(),
        record.args()
    )
}

/// Writes every record at or above its level to standard error.
struct StderrLogger {
    /// The least severe level written.
    level: log::LevelFilter,
}

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            // A write to standard error that fails has nowhere else to go.
            let _ = writeln!(std::io::stderr().lock(), "{}", line(record));
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().lock().flush();
    }
}

/// Install the standard-error logger at `level`, process-wide.
///
/// # Errors
///
/// When a logger is already installed; the process keeps that one.
pub fn install(level: log::LevelFilter) -> Result<(), log::SetLoggerError> {
    log::set_boxed_logger(Box::new(StderrLogger { level }))?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(value: Option<&str>) -> Result<log::LevelFilter, InvalidLimit> {
        level_from_vars(|name| {
            assert_eq!(name, "GRPC_PDF_LOG");
            value.map(ToOwned::to_owned)
        })
    }

    #[test]
    fn the_level_defaults_to_warn_and_reads_every_name() {
        assert_eq!(level(None), Ok(log::LevelFilter::Warn));
        assert_eq!(level(Some("DEBUG")), Ok(log::LevelFilter::Debug));
        assert_eq!(level(Some(" off ")), Ok(log::LevelFilter::Off));
        let error = level(Some("loud")).expect_err("not a level");
        assert_eq!(error.name, "GRPC_PDF_LOG");
    }

    #[test]
    fn a_record_is_one_line_naming_its_level_and_where_it_came_from() {
        let record = log::Record::builder()
            .level(log::Level::Warn)
            .target("pdf_inspector::structure_tree")
            .args(format_args!("structure tree parsing was truncated"))
            .build();
        assert_eq!(
            line(&record),
            "grpc-pdf-inspector: WARN pdf_inspector::structure_tree: \
             structure tree parsing was truncated"
        );
    }

    #[test]
    fn the_logger_keeps_its_level_and_drops_what_is_below_it() {
        use log::Log as _;
        let logger = StderrLogger {
            level: log::LevelFilter::Warn,
        };
        let at = |level| log::Metadata::builder().level(level).build();
        assert!(logger.enabled(&at(log::Level::Error)));
        assert!(logger.enabled(&at(log::Level::Warn)));
        assert!(!logger.enabled(&at(log::Level::Info)));
    }
}
