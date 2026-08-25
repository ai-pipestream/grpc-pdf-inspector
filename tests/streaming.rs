// SPDX-License-Identifier: Apache-2.0

//! The tests that fail if this service is ever turned back into a batch API.
//!
//! Everything else in the suite would still pass if someone rewrote the driver
//! to extract the whole document, collect the events in a `Vec`, and send them
//! at the end: the same events would arrive in the same order. That rewrite is
//! the single most likely way to lose the property this service exists for —
//! `info` is the cheap routing answer, and it is only cheap if it goes out
//! before extraction runs.
//!
//! What can tell the difference is *when* `info` arrives relative to the
//! extraction work. Wall-clock thresholds cannot say that portably — a release
//! build extracts an order of magnitude faster than a debug one — so each
//! test calibrates against itself: it times a bare extraction of the same
//! bytes first (E), then asserts `info` arrives in a small fraction of E. A
//! live stream delivers `info` after detection alone (~E/10); a batch cannot
//! deliver it before paying the whole E.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use grpc_pdf_inspector::metrics::Metrics;
use grpc_pdf_inspector::parse::{self, Sink};
use grpc_pdf_inspector::proto::v1 as pb;
use tokio::sync::mpsc;

/// Pages in the fixtures below.
const PAGES: u32 = 400;

/// Body words per page.
///
/// Sized so extraction takes ~130ms even in a release build (and ~1.4s in a
/// debug one): long enough that scheduling jitter cannot cross the
/// calibration margins, short enough to keep the suite quick.
const WORDS: usize = 400;

/// Time a bare full-document extraction of `pdf`, the quantity a batched
/// implementation would have to finish before it could send `info`.
fn extraction_cost(pdf: &[u8]) -> Duration {
    let started = Instant::now();
    let extracted = pdf_inspector::extract_pages_markdown_mem(pdf, None).expect("extract");
    assert_eq!(extracted.pages.len(), PAGES as usize);
    started.elapsed()
}

/// `info` must reach the reader before extraction has run.
///
/// The load-bearing test of this repository. Driven in-process against a
/// one-slot channel so there is no transport buffer to blur anything: the
/// parser can be at most one event ahead of the reader.
#[tokio::test]
async fn info_arrives_before_extraction_runs() {
    let pdf = common::text_pdf(PAGES, WORDS, "liveness-marker");
    let cost = extraction_cost(&pdf);

    let metrics = Metrics::new();
    let (tx, mut rx) = mpsc::channel(1);

    let counters = Arc::clone(&metrics);
    let driver = pdf.clone();
    let parser = tokio::task::spawn_blocking(move || {
        let sink = Sink::new(tx, Duration::from_secs(10));
        parse::run(&driver, &pb::PdfOptions::default(), &counters, &sink)
    });

    let opened = Instant::now();
    let first = rx
        .recv()
        .await
        .expect("a stream always opens")
        .expect("no error");
    let info_at = opened.elapsed();
    let Some(pb::parse_pdf_response::Event::Info(info)) = first.event else {
        panic!("the first event must be `info`");
    };
    assert_eq!(info.page_count, PAGES);
    assert_eq!(info.pdf_type, pb::PdfType::TextBased as i32);

    // The whole assertion. `info` took the detection path (a tenth of E);
    // a batch would have taken detection plus all of E before sending it.
    assert!(
        info_at < cost / 2,
        "`info` arrived after {info_at:?} when a bare extraction of the same bytes takes \
         {cost:?}; extraction is supposed to happen after `info`, not before it"
    );

    // No page can have been emitted yet: with one slot the parser had to
    // wait for this read before sending anything past `info`.
    assert_eq!(
        metrics.snapshot().pages_emitted,
        0,
        "a page was emitted before `info` was even read"
    );

    // Backpressure: while the reader does nothing, the parser must not run
    // ahead into a buffer of its own. It can fill the one freed slot with
    // page 1 and no more.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let idle = metrics.snapshot();
    assert!(
        idle.pages_emitted <= 1,
        "the parser kept going while nobody was reading, reaching page {}; the outbound \
         channel is supposed to be the only buffer",
        idle.pages_emitted
    );

    // Drain, and check the shape while we are here.
    let mut page_events = 0;
    let mut saw_trailer = false;
    while let Some(event) = rx.recv().await {
        match event.expect("no error").event.expect("an event") {
            pb::parse_pdf_response::Event::Page(_) => page_events += 1,
            pb::parse_pdf_response::Event::Status(_) => saw_trailer = true,
            pb::parse_pdf_response::Event::Info(_) => panic!("a second `info`"),
            pb::parse_pdf_response::Event::Document(_) => {
                panic!("a `document` without `emit_document`")
            }
            pb::parse_pdf_response::Event::Spans(_) => {
                panic!("a `spans` event without `emit_spans`")
            }
        }
    }
    assert!(matches!(
        parser.await.expect("the parser thread"),
        parse::Outcome::Complete
    ));
    assert_eq!(page_events, PAGES as usize);
    assert!(saw_trailer, "the stream must end with `status`");
}

/// The same question end to end, over a socket.
///
/// Extraction is monolithic inside the library, so once it returns the page
/// events all leave at once — the per-page lead time the EPUB sibling
/// asserts is not the observable here. What distinguishes a live stream from
/// a batch on the socket is still the arrival time of `info`.
#[tokio::test]
async fn info_reaches_the_client_while_extraction_is_still_running() {
    let harness = common::start().await;
    let pdf = common::text_pdf(PAGES, WORDS, "socket-liveness-marker");
    let cost = extraction_cost(&pdf);

    let mut client = harness.client.clone();
    let frames = vec![
        pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Options(
                pb::PdfOptions::default(),
            )),
        },
        pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Chunk(pdf)),
        },
    ];

    let opened = Instant::now();
    let mut stream = client
        .parse_pdf(tokio_stream::iter(frames))
        .await
        .expect("open the call")
        .into_inner();

    let first = stream.message().await.expect("no error").expect("an event");
    let info_at = opened.elapsed();
    assert!(matches!(
        first.event,
        Some(pb::parse_pdf_response::Event::Info(_))
    ));
    assert!(
        info_at < cost / 2,
        "`info` crossed the socket after {info_at:?} when a bare extraction takes {cost:?}; \
         that is what a batched implementation looks like"
    );

    // Drain and confirm the trailer really is last.
    let mut seen = 0;
    let mut trailer = None;
    while let Some(event) = stream.message().await.expect("no error") {
        match event.event.expect("every response carries an event") {
            pb::parse_pdf_response::Event::Page(_) => {
                assert!(trailer.is_none(), "a page arrived after `status`");
                seen += 1;
            }
            pb::parse_pdf_response::Event::Status(status) => trailer = Some(status),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(seen, PAGES);
    assert_eq!(trailer.expect("a trailer").pages_extracted, PAGES);
}

/// A client that hangs up mid-stream must not wedge the server.
///
/// The drop lands while extraction is still running (the fixture is sized to
/// keep that true even in a release build), so the parser must notice the
/// closed channel on its first page send and stop — not finish the document
/// for nobody.
#[tokio::test]
async fn dropping_the_stream_early_frees_the_parser() {
    let harness = common::start().await;
    let pdf = common::text_pdf(PAGES, WORDS, "drop-marker");

    let mut client = harness.client.clone();
    let frames = vec![
        pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Options(
                pb::PdfOptions::default(),
            )),
        },
        pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Chunk(pdf)),
        },
    ];
    let mut stream = client
        .parse_pdf(tokio_stream::iter(frames))
        .await
        .expect("open the call")
        .into_inner();

    let _info = stream.message().await.expect("no error").expect("an event");
    drop(stream);

    // The parser notices a closed channel on its next send and stops. If it
    // did not, this counter would keep climbing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stopped = harness.metrics.snapshot();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        harness.metrics.snapshot().pages_emitted,
        stopped.pages_emitted,
        "the parser kept working for a client that had gone away"
    );
    assert!(
        stopped.pages_emitted < u64::from(PAGES),
        "the parser finished the whole document for a client that had gone away"
    );
}
