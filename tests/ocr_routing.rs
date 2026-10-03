// SPDX-License-Identifier: Apache-2.0

//! Routing to OCR: the pages whose words a reader sees only as pixels.
//!
//! A caller routes on this stream with no second opinion. gRParse takes a
//! text-based document with no OCR pages as finished and never runs its own
//! models over it, so a scanned page this server does not name is a page
//! that silently disappears from the output.
//!
//! Two shapes used to disappear. A scan made searchable (OCRmyPDF, ABBYY,
//! Acrobat) draws its OCR layer with text rendering mode 3, and detection
//! counted those invisible show operators as a text layer: the document
//! classified text-based at full confidence with no OCR pages, and the
//! extraction, which leaves the invisible layer out, returned empty pages.
//! And a scanned page inside a mostly born-digital document was only ever
//! named when detection's eight-page sample happened to land on it and the
//! sample then called the whole document mixed.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

/// Classify only.
fn detect_only() -> pb::PdfOptions {
    pb::PdfOptions {
        mode: pb::ProcessMode::DetectOnly.into(),
        ..Default::default()
    }
}

/// The reasons `info` gives for one page.
fn info_reasons(info: &pb::PdfInfo, page: u32) -> Vec<pb::OcrReason> {
    info.ocr_reasons
        .iter()
        .find(|reasons| reasons.page == page)
        .map(|reasons| reasons.reasons().collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_searchable_scan_is_not_text_based_and_every_page_needs_ocr() {
    let harness = common::start().await;
    let events = harness
        .parse(&common::searchable_scan_pdf(3), detect_only())
        .await
        .expect("the fixture should parse");

    let info = common::info(&events);
    assert_ne!(
        info.pdf_type(),
        pb::PdfType::TextBased,
        "no glyph on any page is visible: {info:?}"
    );
    assert_eq!(
        info.pdf_type(),
        pb::PdfType::Mixed,
        "an OCR layer is still a text layer extraction can recover"
    );
    assert_eq!(info.pages_needing_ocr, [1, 2, 3]);
    for page in 1..=3 {
        assert_eq!(
            info_reasons(info, page),
            [pb::OcrReason::Scanned],
            "page {page}"
        );
    }
}

#[tokio::test]
async fn a_scanned_page_the_sample_never_reached_is_still_named() {
    // Twenty pages sample as 1, 3, 5, 7, 9, 11, 13 and 20, so the scans on
    // 16 and 17 are outside the sample and the document reads as text.
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::mixed_text_and_scan_pdf(20, &[16, 17]),
            detect_only(),
        )
        .await
        .expect("the fixture should parse");

    let info = common::info(&events);
    assert_eq!(
        info.pdf_type(),
        pb::PdfType::TextBased,
        "eighteen pages of prose are a text document"
    );
    assert_eq!(
        info.pages_needing_ocr,
        [16, 17],
        "and its two scans are named all the same: {info:?}"
    );
    assert_eq!(info_reasons(info, 16), [pb::OcrReason::Scanned]);
    assert_eq!(info_reasons(info, 17), [pb::OcrReason::Scanned]);
}

#[tokio::test]
async fn hidden_text_on_a_page_of_prose_does_not_make_it_a_scan() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::invisible_text_pdf("CONFIDENTIAL DRAFT"),
            detect_only(),
        )
        .await
        .expect("the fixture should parse");

    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased);
    assert!(info.pages_needing_ocr.is_empty(), "{info:?}");
}

#[tokio::test]
async fn a_scan_with_no_text_layer_at_all_is_still_scanned() {
    let harness = common::start().await;
    let events = harness
        .parse(&common::image_pdf(2), detect_only())
        .await
        .expect("the fixture should parse");
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::Scanned);
    assert_eq!(info.pages_needing_ocr, [1, 2]);
}
