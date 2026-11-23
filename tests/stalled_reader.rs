// SPDX-License-Identifier: Apache-2.0

//! A reader that stops taking events is told its document is incomplete.
//!
//! The parser cannot wait for ever on a reader that takes nothing: it would
//! pin a blocking thread and a parse slot. What it must not do is give up in
//! silence. A stream that simply ends carries gRPC status OK, and a caller
//! that was only slow (back-pressured by its own consumer, paused) would read
//! the events that were buffered and then a clean end, with pages missing
//! and nothing saying so. These tests hold the two cases apart: a reader that
//! stalled gets `ABORTED`; a reader that hung up gets nothing, because there
//! is nobody to tell.

mod common;

use std::sync::Arc;
use std::time::Duration;

use grpc_pdf_inspector::limits::Limits;
use grpc_pdf_inspector::metrics::Metrics;
use grpc_pdf_inspector::parse::{self, Outcome, Sink};
use grpc_pdf_inspector::proto::v1 as pb;
use grpc_pdf_inspector::service::PdfGrpc;
use tokio::sync::mpsc;
use tonic::Code;

/// Pages in the document the stalled readers are sent.
///
/// With [`WORDS`] it renders to about two megabytes of markdown. The client
/// below takes [`WINDOW`] bytes ahead of its reader and the server's HTTP/2
/// send buffer a few hundred kilobytes more, so the parser is certain to end
/// up waiting on a reader that is not reading.
const PAGES: u32 = 600;

/// Body words per page, few enough that every line stays on the page.
const WORDS: usize = 600;

/// The client's HTTP/2 flow-control window, per stream and per connection.
///
/// The transport's default lets megabytes arrive ahead of the reader, which
/// would swallow the whole document and leave nothing to stall on.
const WINDOW: u32 = 65_535;

/// Drive one parse in-process against `tx`, giving up on the reader after
/// `stall`.
async fn drive(
    tx: mpsc::Sender<Result<pb::ParsePdfResponse, tonic::Status>>,
    stall: Duration,
) -> Outcome {
    let pdf = common::text_pdf(8, 40, "stall-marker");
    tokio::task::spawn_blocking(move || {
        let sink = Sink::new(tx, stall);
        parse::run(&pdf, &pb::PdfOptions::default(), &Metrics::new(), &sink)
    })
    .await
    .expect("the parser does not panic")
}

#[tokio::test]
async fn a_reader_that_stalls_fails_the_parse() {
    // One slot, and nobody reads it: `info` fills the channel and the first
    // page waits out the stall window.
    let (tx, mut rx) = mpsc::channel(1);
    let outcome = drive(tx, Duration::from_millis(200)).await;

    let Outcome::Failed(status) = outcome else {
        panic!(
            "a reader that is still connected and took nothing ended the parse as \
             {outcome:?}; the stream would close with gRPC OK and pages missing"
        );
    };
    assert_eq!(status.code(), Code::Aborted, "{status:?}");
    assert!(status.message().contains("reader"), "{status:?}");

    // What was sent before the stall is still there to be read.
    let first = rx.recv().await.expect("the buffered event").expect("ok");
    assert!(matches!(
        first.event,
        Some(pb::parse_pdf_response::Event::Info(_))
    ));
}

#[tokio::test]
async fn a_reader_that_hung_up_is_not_sent_a_status() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    let outcome = drive(tx, Duration::from_millis(200)).await;
    assert!(
        matches!(outcome, Outcome::Abandoned),
        "a reader that is gone has nobody to report to, got {outcome:?}"
    );
}

/// A one-slot server that gives a stalled reader 300 ms.
async fn start() -> common::Harness {
    common::start_with_service_and_endpoint(
        |metrics| {
            PdfGrpc::with_metrics(
                Limits {
                    max_concurrent_parses: 1,
                    ..Limits::default()
                },
                metrics,
            )
            .with_consumer_stall(Duration::from_millis(300))
        },
        |endpoint| {
            endpoint
                .initial_stream_window_size(WINDOW)
                .initial_connection_window_size(WINDOW)
        },
    )
    .await
}

/// Open a FULL parse of the large document and read its `info`.
async fn open(harness: &common::Harness) -> tonic::Streaming<pb::ParsePdfResponse> {
    let pdf = common::text_pdf(PAGES, WORDS, "stall-marker");
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
    let mut stream = harness
        .client
        .clone()
        .max_decoding_message_size(usize::MAX)
        .parse_pdf(tokio_stream::iter(frames))
        .await
        .expect("the call opens")
        .into_inner();
    let first = stream
        .message()
        .await
        .expect("no error")
        .expect("a stream opens with an event");
    assert!(matches!(
        first.event,
        Some(pb::parse_pdf_response::Event::Info(_))
    ));
    stream
}

/// Wait until the server has given the stalled call up.
async fn wait_for_the_parse_to_end(metrics: &Arc<Metrics>) {
    for _ in 0..600 {
        let counters = metrics.snapshot();
        assert_eq!(
            counters.parses_succeeded, 0,
            "the whole document was buffered ahead of the reader, so nothing stalled; the \
             fixture is too small for the transport's buffers"
        );
        if counters.parses_failed > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the server never gave up on a reader that took nothing");
}

#[tokio::test]
async fn a_slow_reader_finds_aborted_at_the_end_of_its_stream() {
    let harness = start().await;
    let mut stream = open(&harness).await;

    // The reader pauses. Nothing is wrong with its connection.
    wait_for_the_parse_to_end(&harness.metrics).await;

    // It reads again: the events that were on their way arrive, and then the
    // stream says it was cut short instead of ending as a complete one would.
    let mut pages = 0u32;
    let mut saw_trailer = false;
    let status = loop {
        match stream.message().await {
            Ok(Some(response)) => match response.event {
                Some(pb::parse_pdf_response::Event::Page(_)) => pages += 1,
                Some(pb::parse_pdf_response::Event::Status(_)) => saw_trailer = true,
                _ => {}
            },
            Ok(None) => panic!(
                "the stream ended with gRPC OK after {pages} of {PAGES} pages; a reader that \
                 checks only the status would take that for the whole document"
            ),
            Err(status) => break status,
        }
    };
    assert_eq!(status.code(), Code::Aborted, "{status:?}");
    assert!(!saw_trailer, "a cut-short stream carries no `status` event");
    assert!(pages < PAGES, "the stall happened mid-document");
}

#[tokio::test]
async fn a_reader_that_never_reads_again_does_not_keep_its_parse_slot() {
    let harness = start().await;
    // Kept open and never read again.
    let _stalled = open(&harness).await;
    wait_for_the_parse_to_end(&harness.metrics).await;

    // The only slot is free for the next caller, although the status meant
    // for the stalled reader is still waiting on the channel it is not
    // draining.
    let pdf = common::text_pdf(2, 20, "slot-marker");
    let next = tokio::time::timeout(
        Duration::from_secs(20),
        harness.parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("the stalled reader's call still holds the only parse slot")
    .expect("the next call is admitted");
    assert_eq!(common::info(&next).page_count, 2);
}
