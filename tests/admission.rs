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

use grpc_pdf_inspector::Limits;
use grpc_pdf_inspector::proto::v1 as pb;
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
