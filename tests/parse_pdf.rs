// SPDX-License-Identifier: Apache-2.0

//! What a successful stream looks like for each kind of document and mode.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

#[tokio::test]
async fn a_text_document_classifies_and_streams_markdown() {
    let harness = common::start().await;
    let pdf = common::text_pdf(3, 40, "stream-test-marker");

    let events = harness.parse_ok(&pdf).await;
    assert_eq!(
        common::shape(&events),
        ["info", "page", "page", "page", "status"]
    );

    let info = common::info(&events);
    assert_eq!(info.pdf_type, pb::PdfType::TextBased as i32);
    assert_eq!(info.page_count, 3);
    assert!(info.pages_needing_ocr.is_empty(), "{info:?}");
    assert!(info.confidence > 0.0);

    let pages = common::pages(&events);
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(
            page.page_no,
            index as u32 + 1,
            "pages are 1-indexed in order"
        );
        assert!(
            page.markdown.contains("stream-test-marker"),
            "page {} markdown should carry the fixture text: {:?}",
            page.page_no,
            page.markdown
        );
    }

    let status = common::status(&events);
    assert_eq!(status.pages_extracted, 3);
    assert!(status.warnings.is_empty());
    assert!(!status.has_encoding_issues);
}

#[tokio::test]
async fn an_image_only_document_reports_pages_needing_ocr_and_extracts_nothing() {
    let harness = common::start().await;
    let pdf = common::image_pdf(2);

    let events = harness.parse_ok(&pdf).await;
    assert_eq!(common::shape(&events), ["info", "status"]);

    let info = common::info(&events);
    assert!(
        matches!(
            pb::PdfType::try_from(info.pdf_type).expect("a known type"),
            pb::PdfType::Scanned | pb::PdfType::ImageBased
        ),
        "an image-only document must classify as scanned or image-based: {info:?}"
    );
    assert_eq!(info.page_count, 2);
    assert_eq!(
        info.pages_needing_ocr.len(),
        2,
        "every page of a scan needs OCR: {info:?}"
    );
    assert!(!info.ocr_reasons.is_empty(), "{info:?}");
}

#[tokio::test]
async fn detect_only_skips_extraction() {
    let harness = common::start().await;
    let pdf = common::text_pdf(4, 40, "detect-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                ..Default::default()
            },
        )
        .await
        .expect("detect-only parse");

    assert_eq!(common::shape(&events), ["info", "status"]);
    assert_eq!(common::info(&events).page_count, 4);
    assert_eq!(common::status(&events).pages_extracted, 0);
}

#[tokio::test]
async fn analyze_reports_layout_without_streaming_pages() {
    let harness = common::start().await;
    let pdf = common::text_pdf(3, 40, "analyze-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::Analyze.into(),
                ..Default::default()
            },
        )
        .await
        .expect("analyze parse");

    assert_eq!(common::shape(&events), ["info", "status"]);
    let status = common::status(&events);
    assert_eq!(status.pages_extracted, 0);
    assert!(status.layout.is_some(), "analyze must carry the layout");
}

#[tokio::test]
async fn a_page_selection_streams_only_those_pages() {
    let harness = common::start().await;
    let pdf = common::text_pdf(5, 20, "selection-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                pages: vec![2, 4],
                ..Default::default()
            },
        )
        .await
        .expect("selected parse");

    assert_eq!(common::shape(&events), ["info", "page", "page", "status"]);
    let selected: Vec<u32> = common::pages(&events)
        .iter()
        .map(|page| page.page_no)
        .collect();
    assert_eq!(selected, [2, 4], "only the asked-for pages, in order");
    assert_eq!(common::status(&events).pages_extracted, 2);
}

#[tokio::test]
async fn an_upload_in_small_chunks_parses_the_same() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "chunked-marker");

    let events = harness
        .parse_chunked(&pdf, pb::PdfOptions::default(), 997)
        .await
        .expect("chunked parse");

    assert_eq!(common::shape(&events), ["info", "page", "page", "status"]);
    assert!(
        common::pages(&events)[0]
            .markdown
            .contains("chunked-marker")
    );
}
