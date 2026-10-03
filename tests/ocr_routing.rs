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

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
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

/// Every `PageOcrReasons` page on the trailer, in order.
fn trailer_pages(status: &pb::ParseStatus) -> Vec<u32> {
    status
        .extraction_ocr_reasons
        .iter()
        .map(|reasons| reasons.page)
        .collect()
}

#[tokio::test]
async fn a_searchable_scan_comes_back_flagged_and_carrying_its_ocr_layer() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::searchable_scan_pdf(3),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    // Every page needs OCR, and says so on its own event: a caller that
    // reads the pages, not only `info`, routes it.
    let pages = common::pages(&events);
    assert_eq!(pages.len(), 3);
    for page in &pages {
        assert!(page.needs_ocr, "page {} is a scan", page.page_no);
        assert_eq!(page.ocr_reason(), pb::OcrReason::Scanned);
        // The library's own OCR-layer fallback: the page's text is its
        // layer, not nothing, so a caller without OCR still has the words.
        assert!(
            page.markdown.contains("analytical engine weaves"),
            "page {} carries its OCR layer: {:?}",
            page.page_no,
            page.markdown
        );
    }

    let status = common::status(&events);
    assert!(status.has_invisible_text, "the layer was invisible");
    assert_eq!(trailer_pages(status), [1, 2, 3]);
    for reasons in &status.extraction_ocr_reasons {
        assert!(
            reasons
                .reasons()
                .any(|reason| reason == pb::OcrReason::Scanned),
            "{reasons:?}"
        );
    }

    // The Document says the same thing per page, and its body is the text
    // rather than empty.
    let document = common::documents(&events)[0];
    for page_no in 1..=3 {
        let quality = document.pages[&page_no]
            .quality
            .as_ref()
            .expect("a page needing OCR has a quality record");
        assert_eq!(quality.ocr_recommended, Some(true), "page {page_no}");
    }
    let body: Vec<String> = common::placed(document)
        .into_iter()
        .filter(|item| item.layer == doc::ContentLayer::Body as i32)
        .map(|item| item.text)
        .collect();
    assert!(
        body.iter()
            .any(|text| text.contains("analytical engine weaves")),
        "the body carries the OCR layer: {body:?}"
    );
}

#[tokio::test]
async fn an_adopted_ocr_layer_is_not_reported_as_hidden_text_as_well() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::searchable_scan_pdf(2),
            pb::PdfOptions {
                emit_document: true,
                report_invisible: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    for page in common::pages(&events) {
        assert!(
            page.invisible.is_empty(),
            "the layer is the page's markdown, so it is not also hidden runs: {:?}",
            page.invisible
        );
    }
    let document = common::documents(&events)[0];
    let hidden = common::placed(document)
        .into_iter()
        .filter(|item| item.layer == doc::ContentLayer::Invisible as i32)
        .count();
    assert_eq!(
        hidden, 0,
        "nothing is in both the body and the hidden layer"
    );
}

#[tokio::test]
async fn scanned_pages_the_sample_missed_are_flagged_on_their_pages_and_the_trailer() {
    let harness = common::start().await;
    let events = harness
        .parse_ok(&common::mixed_text_and_scan_pdf(20, &[16, 17]))
        .await;

    let flagged: Vec<u32> = common::pages(&events)
        .into_iter()
        .filter(|page| page.needs_ocr)
        .map(|page| page.page_no)
        .collect();
    assert_eq!(flagged, [16, 17]);
    assert_eq!(trailer_pages(common::status(&events)), [16, 17]);
}

#[tokio::test]
async fn a_page_with_a_picture_and_no_text_is_flagged_where_detection_missed_it() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::photo_with_empty_text_pdf()).await;

    // The empty show operators keep the page off detection's lists, which
    // is what makes this the extraction's own verdict.
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased);
    assert!(info.pages_needing_ocr.is_empty(), "{info:?}");

    let pages = common::pages(&events);
    assert!(!pages[0].needs_ocr && !pages[2].needs_ocr);
    assert!(
        pages[1].needs_ocr,
        "the photo page has no word that is not pixels"
    );
    assert_eq!(pages[1].ocr_reason(), pb::OcrReason::Scanned);
    assert_eq!(trailer_pages(common::status(&events)), [2]);
}

#[tokio::test]
async fn info_says_when_detection_recommends_ocr() {
    let harness = common::start().await;
    for (pdf, recommended) in [
        (common::searchable_scan_pdf(2), true),
        (common::image_pdf(2), true),
        (common::text_pdf(3, 60, "prose"), false),
    ] {
        let events = harness
            .parse(&pdf, detect_only())
            .await
            .expect("the fixture should parse");
        let info = common::info(&events);
        assert_eq!(info.ocr_recommended, recommended, "{info:?}");
    }
}

#[tokio::test]
async fn a_misread_ocr_layer_flags_its_page_and_not_the_whole_document() {
    // A document-wide encoding verdict sends every page to recognition,
    // so one scanned page's misread OCR layer must not raise it: the
    // born-digital pages around it have a perfectly good text layer.
    let harness = common::start().await;
    let events = harness
        .parse_ok(&common::text_with_misread_scan_pdf())
        .await;

    let pages = common::pages(&events);
    assert!(!pages[0].needs_ocr && !pages[2].needs_ocr);
    assert!(pages[1].needs_ocr);
    assert_eq!(pages[1].ocr_reason(), pb::OcrReason::Scanned);

    let status = common::status(&events);
    let reasons: Vec<pb::OcrReason> = status
        .extraction_ocr_reasons
        .iter()
        .find(|reasons| reasons.page == 2)
        .map(|reasons| reasons.reasons().collect())
        .unwrap_or_default();
    assert!(
        reasons.contains(&pb::OcrReason::SuspectedGarbled),
        "the misreading is still reported for its page: {reasons:?}"
    );
    assert!(
        !status.has_encoding_issues,
        "an OCR layer's misreadings are not a broken font encoding"
    );
}
