// SPDX-License-Identifier: Apache-2.0

//! A scanned verdict is a verdict on every page, so every page is read.
//!
//! Detection samples eight pages of a long document. A text or mixed
//! verdict was always checked page by page afterwards, but a scanned one
//! was not: it listed every page for OCR and turned extraction off, so a
//! born-digital page the sample missed lost its exact text layer, and the
//! wire did not say the verdict came from a sample.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

/// Twenty pages, scanned except page 4. The sample reads 1, 3, 5, 7, 9,
/// 11, 13 and 20, all scans, so page 4 is outside it.
fn scan_with_one_born_digital_page() -> Vec<u8> {
    let scanned: Vec<u32> = (1..=20).filter(|&page| page != 4).collect();
    common::mixed_text_and_scan_pdf(20, &scanned)
}

#[tokio::test]
async fn a_born_digital_page_the_sample_missed_keeps_its_text() {
    let harness = common::start().await;
    let events = harness.parse_ok(&scan_with_one_born_digital_page()).await;

    let info = common::info(&events);
    assert_eq!(
        info.pdf_type(),
        pb::PdfType::Mixed,
        "a scan with a page of text is mixed: {info:?}"
    );
    assert!(
        !info.pages_needing_ocr.contains(&4),
        "the born-digital page has a text layer to read: {info:?}"
    );
    assert_eq!(info.pages_needing_ocr.len(), 19, "{info:?}");

    let pages = common::pages(&events);
    let four = pages
        .iter()
        .find(|page| page.page_no == 4)
        .expect("page 4 is streamed");
    assert!(
        four.markdown.contains("Born digital page 4"),
        "page 4's own text comes back: {four:?}"
    );
    assert!(!four.needs_ocr, "{four:?}");
}

#[tokio::test]
async fn info_says_how_many_pages_the_verdict_read() {
    let harness = common::start().await;

    // A pure scan: the sample found no text, so every page was read before
    // the scanned verdict was given.
    let events = harness
        .parse_ok(&common::mixed_text_and_scan_pdf(
            20,
            &(1..=20).collect::<Vec<_>>(),
        ))
        .await;
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::Scanned, "{info:?}");
    assert_eq!(info.pages_sampled, 20, "{info:?}");
    assert_eq!(info.pages_with_text, 0, "{info:?}");
    assert_eq!(info.pages_needing_ocr.len(), 20, "{info:?}");

    // A text document: the eight-page sample decided, and says so.
    let events = harness.parse_ok(&common::text_pdf(20, 40, "Sampled")).await;
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased, "{info:?}");
    assert_eq!(info.pages_sampled, 8, "{info:?}");
    assert_eq!(info.pages_with_text, 8, "{info:?}");
    assert_eq!(info.page_count, 20);
}
