// SPDX-License-Identifier: Apache-2.0

//! When a call is let in, and what it may hold while it waits.
//!
//! A call used to buffer its whole upload before it asked for a parse slot,
//! so every call queued behind the slots held up to the upload cap in
//! memory, and a burst of large uploads held that many times the cap. A
//! call now takes its slot first and only then reads its upload, so the
//! slots bound the buffered uploads as well as the parses, and a call that
//! waits holds nothing the transport's flow control did not already allow.

mod common;

use std::time::{Duration, Instant};

use grpc_pdf_inspector::proto::v1 as pb;
use grpc_pdf_inspector::{Limits, PdfGrpc};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;

/// Open a call that sends its options and the first half of `pdf`, and then
/// keeps the upload open without sending anything more. Returns the sender
/// that holds it open and the task waiting on the call's response.
fn open_unfinished_upload(
    harness: &common::Harness,
    pdf: &[u8],
) -> (
    mpsc::Sender<pb::ParsePdfRequest>,
    tokio::task::JoinHandle<Result<(), tonic::Status>>,
) {
    let (frames, outbound) = mpsc::channel(4);
    let mut client = harness.client.clone();
    let first_half = pdf[..pdf.len() / 2].to_vec();
    let sender = frames.clone();
    let call = tokio::spawn(async move {
        sender
            .send(pb::ParsePdfRequest {
                frame: Some(pb::parse_pdf_request::Frame::Options(
                    pb::PdfOptions::default(),
                )),
            })
            .await
            .expect("send options");
        sender
            .send(pb::ParsePdfRequest {
                frame: Some(pb::parse_pdf_request::Frame::Chunk(first_half)),
            })
            .await
            .expect("send a chunk");
        drop(sender);
        let mut stream = client
            .parse_pdf(ReceiverStream::new(outbound))
            .await?
            .into_inner();
        while stream.message().await?.is_some() {}
        Ok(())
    });
    (frames, call)
}

/// Open a call that sends its options and then `frame` once every `every`,
/// `frames` times, before closing its upload. Returns the call's outcome and
/// how long it took.
async fn trickle(
    harness: &common::Harness,
    frame: impl Fn(usize) -> pb::ParsePdfRequest + Send + 'static,
    every: Duration,
    frames: usize,
) -> (Result<(), tonic::Status>, Duration) {
    let (sender, outbound) = mpsc::channel(4);
    tokio::spawn(async move {
        let options = pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Options(
                pb::PdfOptions::default(),
            )),
        };
        if sender.send(options).await.is_err() {
            return;
        }
        for n in 0..frames {
            tokio::time::sleep(every).await;
            // The server ending the call drops the stream; stop with it.
            if sender.send(frame(n)).await.is_err() {
                return;
            }
        }
    });
    let started = Instant::now();
    let mut client = harness.client.clone();
    let outcome = async {
        let mut stream = client
            .parse_pdf(ReceiverStream::new(outbound))
            .await?
            .into_inner();
        while stream.message().await?.is_some() {}
        Ok(())
    }
    .await;
    (outcome, started.elapsed())
}

/// A DETECT_ONLY call goes through, so the slot the test's call held is
/// free again.
async fn assert_the_slot_is_free(harness: &common::Harness) {
    let pdf = common::text_pdf(2, 20, "slot-marker");
    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                ..Default::default()
            },
        )
        .await
        .expect("the next call is admitted");
    assert_eq!(common::info(&events).page_count, 2);
}

#[tokio::test]
async fn a_call_waiting_for_a_slot_has_not_buffered_its_upload() {
    let harness = common::start_with(Limits {
        max_concurrent_parses: 1,
        ..Limits::default()
    })
    .await;
    let pdf = common::text_pdf(4, 60, "admission-marker");

    // The first call takes the only slot and is still uploading.
    let (hold_open, first) = open_unfinished_upload(&harness, &pdf);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The second call is complete on the client side and has to wait.
    let second = {
        let harness_client = harness.client.clone();
        let pdf = pdf.clone();
        tokio::spawn(async move {
            let harness = common::Harness {
                client: harness_client,
                metrics: grpc_pdf_inspector::Metrics::new(),
            };
            harness.parse(&pdf, pb::PdfOptions::default()).await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !second.is_finished(),
        "the second call was parsed while the only slot was taken"
    );
    assert_eq!(
        harness.metrics.snapshot().bytes_uploaded,
        0,
        "neither upload was buffered: the first is still arriving and the second is waiting \
         for a slot"
    );

    // The first call finishes its upload (half of a document, which is a
    // caller error) and gives the slot back; the second goes through.
    drop(hold_open);
    let status = first
        .await
        .expect("the first call's task")
        .expect_err("half a document is refused");
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
    let events = second
        .await
        .expect("the second call's task")
        .expect("the second call is parsed once the slot is free");
    assert_eq!(common::pages(&events).len(), 4);
}

#[tokio::test]
async fn an_upload_that_stalls_ends_at_the_calls_time_budget() {
    let harness = common::start_with(Limits {
        max_concurrent_parses: 1,
        max_parse_time: Duration::from_millis(300),
        ..Limits::default()
    })
    .await;
    let pdf = common::text_pdf(2, 20, "stall-marker");

    let started = Instant::now();
    let (hold_open, call) = open_unfinished_upload(&harness, &pdf);
    let status = call
        .await
        .expect("the call's task")
        .expect_err("an upload that never finishes cannot hold the slot forever");
    drop(hold_open);
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the stalled call held its slot for {:?}",
        started.elapsed()
    );

    // And the slot is free again.
    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                ..Default::default()
            },
        )
        .await
        .expect("the next call is admitted");
    assert_eq!(common::info(&events).page_count, 2);
}

#[tokio::test]
async fn an_upload_that_trickles_ends_at_its_own_budget() {
    // The parse budget is the default five minutes; only the upload's own
    // budget can end this call before the client finishes trickling.
    let harness = common::start_with(Limits {
        max_concurrent_parses: 1,
        max_upload_time: Duration::from_secs(1),
        ..Limits::default()
    })
    .await;

    // A byte every 100 ms is never a stall, and the client would go on for
    // five seconds and then close an upload that is not a PDF.
    let (outcome, elapsed) = trickle(
        &harness,
        |_| pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Chunk(b"%".to_vec())),
        },
        Duration::from_millis(100),
        50,
    )
    .await;
    let status = outcome.expect_err("a trickled upload cannot hold the slot past its budget");
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert!(
        status.message().contains("GRPC_PDF_MAX_UPLOAD_SECONDS"),
        "{status:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "the trickled upload held its slot for {elapsed:?}"
    );
    assert_the_slot_is_free(&harness).await;
}

#[tokio::test]
async fn frames_without_bytes_do_not_keep_an_upload_alive() {
    let harness = common::start_with_service(|metrics| {
        PdfGrpc::with_metrics(
            Limits {
                max_concurrent_parses: 1,
                ..Limits::default()
            },
            metrics,
        )
        .with_upload_stall(Duration::from_millis(300))
    })
    .await;

    // A frame every 50 ms, each either an empty chunk or no frame at all,
    // for three seconds: never quiet for the stall window, and never a byte.
    let (outcome, elapsed) = trickle(
        &harness,
        |n| pb::ParsePdfRequest {
            frame: (n % 2 == 0).then(|| pb::parse_pdf_request::Frame::Chunk(Vec::new())),
        },
        Duration::from_millis(50),
        60,
    )
    .await;
    let status = outcome.expect_err("frames without bytes are not progress");
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert!(status.message().contains("no upload bytes"), "{status:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "the byteless upload held its slot for {elapsed:?}"
    );
    assert_the_slot_is_free(&harness).await;
}
