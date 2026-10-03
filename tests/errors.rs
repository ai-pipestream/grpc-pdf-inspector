// SPDX-License-Identifier: Apache-2.0

//! What the server says when the input is wrong, and why each answer is the
//! code it is.
//!
//! The split the fleet contract asks for, restated as the question each code
//! answers:
//!
//! - `INVALID_ARGUMENT` — "you gave me a broken PDF (or a broken request)."
//!   Fixable by sending a different file; the server would behave the same
//!   tomorrow.
//! - `RESOURCE_EXHAUSTED` — "you gave me more than I am configured to hold."
//!   That includes a document that inflates past the decompression limits,
//!   which is the same complaint about a different measure of size.
//! - `DEADLINE_EXCEEDED` — "this took longer than a call may hold a slot."
//! - `INTERNAL` — a bug here. Nothing in this file should produce one, and a
//!   test that starts to is reporting a real defect.

mod common;

use std::time::Duration;

use grpc_pdf_inspector::proto::v1 as pb;
use tonic::Code;

/// One mebibyte.
const MIB: usize = 1024 * 1024;

/// A server whose streams may inflate to a mebibyte each and whose
/// document reads may inflate to four, so a bomb test costs megabytes
/// rather than the gigabytes the default limits allow.
async fn small_inflation_limits() -> common::Harness {
    common::start_with(grpc_pdf_inspector::Limits {
        max_stream_bytes: MIB as u64,
        max_decompressed_bytes: 4 * MIB as u64,
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn a_stream_that_inflates_past_its_limit_is_resource_exhausted() {
    // Eight mebibytes of content from a few kilobytes of upload.
    let bomb = common::inflating_pdf(1, 8 * MIB);
    assert!(bomb.len() < 64 * 1024, "{} bytes", bomb.len());

    let harness = small_inflation_limits().await;
    for mode in [pb::ProcessMode::DetectOnly, pb::ProcessMode::Full] {
        let status = harness
            .parse(
                &bomb,
                pb::PdfOptions {
                    mode: mode.into(),
                    ..Default::default()
                },
            )
            .await
            .expect_err("the bomb must be refused");
        assert_eq!(
            status.code(),
            Code::ResourceExhausted,
            "{mode:?}: {status:?}"
        );
        assert!(
            status.message().contains("GRPC_PDF_MAX_STREAM_BYTES"),
            "{status:?}"
        );
    }

    // The limit is what refused it: under the default ceiling the same
    // bytes are an ordinary one-line document.
    let events = common::start().await.parse_ok(&bomb).await;
    assert!(
        common::pages(&events)[0]
            .markdown
            .contains("A line of text"),
        "{events:?}"
    );
}

#[tokio::test]
async fn many_streams_under_the_limit_still_spend_the_documents_budget() {
    // Twenty streams of three quarters of a mebibyte: every one fits its
    // own ceiling and together they are fifteen mebibytes.
    let pdf = common::inflating_pdf(20, 3 * MIB / 4);
    let harness = small_inflation_limits().await;
    let status = harness.parse_err(&pdf).await;
    assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
}

#[tokio::test]
async fn a_call_past_its_time_budget_is_deadline_exceeded() {
    let harness = common::start_with(grpc_pdf_inspector::Limits {
        max_parse_time: Duration::from_nanos(1),
        ..Default::default()
    })
    .await;
    let status = harness
        .parse_err(&common::text_pdf(2, 20, "deadline-marker"))
        .await;
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
}

#[tokio::test]
async fn garbage_bytes_are_a_caller_error() {
    let harness = common::start().await;
    let status = harness.parse_err(&common::garbage()).await;
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
}

#[tokio::test]
async fn an_empty_upload_is_a_caller_error() {
    let harness = common::start().await;
    let status = harness.parse_err(&[]).await;
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn a_truncated_document_is_a_caller_error() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "truncation-marker");

    // The cross-reference table lives at the end, so losing the tail is
    // exactly the "upload cut short" case.
    let truncated = &pdf[..pdf.len() / 2];
    let status = harness.parse_err(truncated).await;
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
}

#[tokio::test]
async fn a_pdf_header_on_garbage_is_a_caller_error() {
    let harness = common::start().await;
    // Starts with the magic bytes, then nothing a parser can use.
    let status = harness
        .parse_err(b"%PDF-1.5\nnot a real body".as_slice())
        .await;
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
}

#[tokio::test]
async fn page_zero_is_a_caller_error() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "page-zero-marker");

    let status = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                pages: vec![0, 1],
                ..Default::default()
            },
        )
        .await
        .expect_err("page 0 must be refused");
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
    assert!(status.message().contains("1-indexed"), "{status:?}");
}

#[tokio::test]
async fn an_oversize_upload_is_resource_exhausted() {
    let pdf = common::text_pdf(2, 20, "oversize-marker");
    let harness = common::start_with(grpc_pdf_inspector::Limits {
        max_document_bytes: (pdf.len() / 2) as u64,
        ..Default::default()
    })
    .await;

    let status = harness.parse_err(&pdf).await;
    assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
}

#[tokio::test]
async fn an_oversize_chunk_is_a_caller_error() {
    let pdf = common::text_pdf(2, 20, "chunk-limit-marker");
    let harness = common::start_with(grpc_pdf_inspector::Limits {
        max_chunk_bytes: 1024,
        ..Default::default()
    })
    .await;

    // One frame carrying more than the chunk cap: refused with advice, not
    // with the transport's decoding error.
    let status = harness.parse_err(&pdf).await;
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
    assert!(status.message().contains("frame limit"), "{status:?}");

    // The same upload in small enough frames is fine.
    let events = harness
        .parse_chunked(&pdf, pb::PdfOptions::default(), 512)
        .await
        .expect("small enough chunks must pass");
    assert_eq!(common::shape(&events), ["info", "page", "page", "status"]);
}

#[tokio::test]
async fn the_first_frame_must_carry_options() {
    let harness = common::start().await;
    let mut client = harness.client.clone();
    let frames = vec![pb::ParsePdfRequest {
        frame: Some(pb::parse_pdf_request::Frame::Chunk(common::text_pdf(
            1, 10, "x",
        ))),
    }];

    let status = client
        .parse_pdf(tokio_stream::iter(frames))
        .await
        .expect_err("a chunk before options must be refused");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("options"));
}

#[tokio::test]
async fn options_may_not_be_sent_twice() {
    let harness = common::start().await;
    let mut client = harness.client.clone();
    let options = || pb::ParsePdfRequest {
        frame: Some(pb::parse_pdf_request::Frame::Options(
            pb::PdfOptions::default(),
        )),
    };

    let status = client
        .parse_pdf(tokio_stream::iter(vec![options(), options()]))
        .await
        .expect_err("a second options frame must be refused");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("only be sent once"));
}

/// Malformed inputs that make lopdf panic rather than error must surface as
/// `INTERNAL` via the panic guard — and must not kill the server.
#[tokio::test]
async fn a_hostile_document_can_only_fail_the_call_not_the_server() {
    let harness = common::start().await;

    // A valid header and xref-shaped tail wrapped around nonsense: the sort
    // of bytes fuzzers produce.
    let mut hostile = common::text_pdf(1, 10, "hostile");
    let middle = hostile.len() / 2;
    for (index, byte) in hostile
        .iter_mut()
        .enumerate()
        .take(middle + 64)
        .skip(middle)
    {
        *byte = (index % 251) as u8;
    }

    let result = harness.parse(&hostile, pb::PdfOptions::default()).await;
    if let Err(status) = result {
        assert!(
            matches!(status.code(), Code::InvalidArgument | Code::Internal),
            "a hostile document must be refused cleanly, got {status:?}"
        );
    }

    // Whatever happened, the next ordinary call works.
    let pdf = common::text_pdf(1, 10, "aftermath-marker");
    let events = harness.parse_ok(&pdf).await;
    assert_eq!(common::shape(&events), ["info", "page", "status"]);
}
