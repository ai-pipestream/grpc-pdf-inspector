// SPDX-License-Identifier: Apache-2.0

//! How many times one call reads the document.
//!
//! FULL used to read it three times: classification, then a whole analysis
//! pass for the layout verdict and the per-page text-quality verdicts, then
//! the extraction pass whose runs the markdown is rendered from. The
//! analysis pass existed only because the column detector and the
//! text-quality module were private to the parser crate, so neither answer
//! could be computed from runs a caller already held. Both are public in
//! the vendored crate now, the answers are computed from the runs, and the
//! pass is gone.
//!
//! `Metrics::parser_pass` counts one walk of the buffer by one reader,
//! which turns that from a claim in a comment into a number.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

/// Passes a single call cost, measured across it.
async fn passes(options: pb::PdfOptions) -> u64 {
    let harness = common::start().await;
    let before = harness.metrics.snapshot().parser_passes;
    harness
        .parse(&common::text_pdf(3, 60, "counted"), options)
        .await
        .expect("the fixture should parse");
    harness.metrics.snapshot().parser_passes - before
}

#[tokio::test]
async fn classification_alone_reads_the_document_once() {
    assert_eq!(
        passes(pb::PdfOptions {
            mode: pb::ProcessMode::DetectOnly as i32,
            ..Default::default()
        })
        .await,
        1,
        "the routing answer is one read and always was"
    );
}

#[tokio::test]
async fn full_reads_the_document_twice_and_not_three_times() {
    assert_eq!(
        passes(pb::PdfOptions::default()).await,
        2,
        "classification, then extraction. The analysis pass between them is gone"
    );
}

#[tokio::test]
async fn every_further_read_is_one_a_caller_asked_for() {
    // Each of these reads something the extraction pass does not produce:
    // the document's own dictionaries, and its tagged structure tree.
    assert_eq!(
        passes(pb::PdfOptions {
            emit_metadata: true,
            ..Default::default()
        })
        .await,
        3
    );
    assert_eq!(
        passes(pb::PdfOptions {
            emit_metadata: true,
            emit_structure: true,
            ..Default::default()
        })
        .await,
        4
    );
    // Everything computed from the runs is free: tables, spans, the layout
    // verdict, the furniture report.
    assert_eq!(
        passes(pb::PdfOptions {
            emit_tables: true,
            emit_spans: true,
            report_furniture: true,
            ..Default::default()
        })
        .await,
        2,
        "the runs answer all of these, and they are already in hand"
    );
    // A Document is the exception, and it is one on purpose: the fold
    // consumes every event this parse can produce, including the two the
    // caller did not name, so it pays for both reads.
    assert_eq!(
        passes(pb::PdfOptions {
            emit_document: true,
            ..Default::default()
        })
        .await,
        4
    );
}

#[tokio::test]
async fn the_invisible_layer_is_the_one_extra_read_a_document_can_force() {
    let harness = common::start().await;
    let before = harness.metrics.snapshot().parser_passes;
    harness
        .parse(
            &common::invisible_text_pdf("CONFIDENTIAL DRAFT"),
            pb::PdfOptions {
                report_invisible: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");
    assert_eq!(
        harness.metrics.snapshot().parser_passes - before,
        3,
        "the parser has one switch for the invisible layer and it governs the \
         whole walk, so separating hidden runs from visible ones takes both walks"
    );
}
