// SPDX-License-Identifier: Apache-2.0

//! Process counters and the interval line they are printed on.
//!
//! Deliberately small: a handful of monotonic counters and one line of text
//! every N seconds on stdout, which is what the fleet siblings do and what a
//! container log aggregator can already read. There is no Prometheus endpoint
//! in v1; adding one later means reading the same [`Snapshot`].
//!
//! Counters are `Relaxed`. They are read for a log line and for tests, never
//! to order anything, so paying for stronger ordering on a per-page counter
//! would be paying for nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Monotonic counters for the lifetime of the process.
#[derive(Debug, Default)]
pub struct Metrics {
    /// `ParsePdf` calls that got as far as a complete upload.
    parses_started: AtomicU64,
    /// `ParsePdf` calls that ended with a `status` event.
    parses_succeeded: AtomicU64,
    /// `ParsePdf` calls that ended with a gRPC error status.
    parses_failed: AtomicU64,
    /// `page` events emitted.
    pages_emitted: AtomicU64,
    /// Bytes received on request streams.
    bytes_uploaded: AtomicU64,
    /// Markdown bytes emitted across all `page` events.
    markdown_bytes_emitted: AtomicU64,
}

/// A consistent-enough read of every counter, for logging and for tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// See [`Metrics::parses_started`].
    pub parses_started: u64,
    /// See [`Metrics::parses_succeeded`].
    pub parses_succeeded: u64,
    /// See [`Metrics::parses_failed`].
    pub parses_failed: u64,
    /// See [`Metrics::pages_emitted`].
    pub pages_emitted: u64,
    /// See [`Metrics::bytes_uploaded`].
    pub bytes_uploaded: u64,
    /// See [`Metrics::markdown_bytes_emitted`].
    pub markdown_bytes_emitted: u64,
}

impl Metrics {
    /// A fresh set of counters.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Count a call whose upload completed and whose parse is starting.
    pub fn parse_started(&self) {
        self.parses_started.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a call that ended with a `status` event.
    pub fn parse_succeeded(&self) {
        self.parses_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a call that ended with a gRPC error status.
    pub fn parse_failed(&self) {
        self.parses_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one emitted `page` event and its markdown bytes.
    pub fn page_emitted(&self, markdown_bytes: u64) {
        self.pages_emitted.fetch_add(1, Ordering::Relaxed);
        self.markdown_bytes_emitted
            .fetch_add(markdown_bytes, Ordering::Relaxed);
    }

    /// Count uploaded bytes.
    pub fn uploaded(&self, bytes: u64) {
        self.bytes_uploaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Read every counter.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            parses_started: self.parses_started.load(Ordering::Relaxed),
            parses_succeeded: self.parses_succeeded.load(Ordering::Relaxed),
            parses_failed: self.parses_failed.load(Ordering::Relaxed),
            pages_emitted: self.pages_emitted.load(Ordering::Relaxed),
            bytes_uploaded: self.bytes_uploaded.load(Ordering::Relaxed),
            markdown_bytes_emitted: self.markdown_bytes_emitted.load(Ordering::Relaxed),
        }
    }
}

impl std::fmt::Display for Snapshot {
    /// One line, key=value, ordered so a `grep` over a day of logs lines up.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "grpc-pdf-inspector metrics parses_started={} parses_succeeded={} \
             parses_failed={} pages_emitted={} bytes_uploaded={} markdown_bytes_emitted={}",
            self.parses_started,
            self.parses_succeeded,
            self.parses_failed,
            self.pages_emitted,
            self.bytes_uploaded,
            self.markdown_bytes_emitted,
        )
    }
}

/// Print the counters on `interval` until the process ends.
///
/// A zero interval disables reporting entirely, which is the right default for
/// a test or a one-shot container. The task is detached: it holds a `Weak`
/// nothing else waits on and prints nothing after the last counter is dropped.
pub fn spawn_reporter(metrics: &Arc<Metrics>, interval: Duration) {
    if interval.is_zero() {
        return;
    }
    let metrics = Arc::downgrade(metrics);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The first tick fires immediately; skip it so startup logs are not
        // padded with a line of zeroes.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(metrics) = metrics.upgrade() else {
                return;
            };
            println!("{}", metrics.snapshot());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_render() {
        let metrics = Metrics::new();
        metrics.parse_started();
        metrics.page_emitted(512);
        metrics.page_emitted(256);
        metrics.parse_succeeded();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.parses_started, 1);
        assert_eq!(snapshot.pages_emitted, 2);
        assert_eq!(snapshot.markdown_bytes_emitted, 768);
        assert_eq!(snapshot.parses_failed, 0);

        let line = snapshot.to_string();
        assert!(line.contains("pages_emitted=2"), "{line}");
        assert!(line.contains("parses_failed=0"), "{line}");
    }
}
