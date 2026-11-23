// SPDX-License-Identifier: Apache-2.0

//! An object stream past the inflation limit fails the call.
//!
//! The extractor's PDF library steps over an object stream that would
//! inflate past `GRPC_PDF_MAX_STREAM_BYTES`, and every object packed in it
//! is gone: a page's font, its resources, the page itself. The parse used
//! to end OK with no sign of the loss, while a content stream over the same
//! limit fails the call with RESOURCE_EXHAUSTED.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;
use tonic::Code;

const MIB: usize = 1024 * 1024;

/// A server whose streams may inflate to one mebibyte.
async fn small_stream_limit() -> common::Harness {
    common::start_with(grpc_pdf_inspector::Limits {
        max_stream_bytes: MIB as u64,
        max_decompressed_bytes: 4 * MIB as u64,
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn the_fixture_reads_whole_under_a_limit_it_fits() {
    let harness = common::start().await;
    let events = harness
        .parse_ok(&common::packed_font_pdf("PACKEDMARK", 2 * MIB))
        .await;
    let pages = common::pages(&events);
    assert!(
        pages[0].markdown.contains("PACKEDMARK"),
        "an object stream within the limit is read: {:?}",
        pages[0]
    );
}

#[tokio::test]
async fn an_object_stream_past_the_limit_fails_the_call() {
    let status = small_stream_limit()
        .await
        .parse(
            &common::packed_font_pdf("PACKEDMARK", 2 * MIB),
            pb::PdfOptions::default(),
        )
        .await
        .expect_err("the page's font was in a stream nobody read");
    assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
    assert!(
        status.message().contains("decompression limit"),
        "{status:?}"
    );
}

#[tokio::test]
async fn an_oversized_object_stream_nothing_refers_to_is_not_a_loss() {
    // `oversized_object_stream_pdf` packs an object no cross-reference
    // entry names, so stepping over it loses nothing the document uses.
    let events = small_stream_limit()
        .await
        .parse(
            &common::oversized_object_stream_pdf("Kept Title", "KEPTMARK", 2 * MIB),
            pb::PdfOptions::default(),
        )
        .await
        .expect("nothing the document uses was in the stream");
    assert!(common::pages(&events)[0].markdown.contains("KEPTMARK"));
}
