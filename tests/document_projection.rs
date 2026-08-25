// SPDX-License-Identifier: Apache-2.0

//! The `emit_document` contract: one `document` event, after the last
//! `page`, before `status`, carrying the fold of exactly this stream.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;
use grpc_pdf_inspector::{COLLECTOR, PARSER, VERSION};

/// The base of any text item the fold makes.
fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
    match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref().expect("a base"),
        other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
    }
}

#[tokio::test]
async fn emit_document_adds_one_document_between_pages_and_status() {
    let harness = common::start().await;
    let pdf = common::text_pdf(3, 40, "document-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    // Exactly once, after the pages, before the trailer.
    assert_eq!(
        common::shape(&events),
        ["info", "page", "page", "page", "document", "status"]
    );

    let documents = common::documents(&events);
    assert_eq!(
        documents.len(),
        1,
        "the document event arrives exactly once"
    );
    let document = documents[0];

    // One PageItem per page, measured from the file's own page boxes.
    assert_eq!(document.pages.len(), 3, "one PageItem per page");
    for page_no in 1..=3 {
        let item = document
            .pages
            .get(&page_no)
            .unwrap_or_else(|| panic!("page {page_no} is named"));
        assert_eq!(item.page_no, page_no);
        let size = item.size.as_ref().expect("the page's visible box");
        assert!((size.width - 612.0).abs() < f64::EPSILON, "{size:?}");
        assert!((size.height - 792.0).abs() < f64::EPSILON, "{size:?}");
        assert_eq!(item.unit.as_deref(), Some("pt"));
    }

    // The pages' markdown became text items, and every item carries the
    // collector convention with the detection confidence from `info`.
    assert!(!document.texts.is_empty(), "the markdown became items");
    let confidence = f64::from(common::info(&events).confidence);
    let mut carries_marker = false;
    for item in &document.texts {
        let base = base_of(item);
        assert_eq!(base.source.len(), 1, "one source per item");
        let Some(doc::source_type::Source::Collector(source)) = base.source[0].source.as_ref()
        else {
            panic!("the source is a collector");
        };
        assert_eq!(source.collector, COLLECTOR);
        assert_eq!(source.model.as_deref(), Some(PARSER));
        assert_eq!(source.version.as_deref(), Some(VERSION));
        assert_eq!(source.confidence, Some(confidence));
        carries_marker |= base.text.contains("document-marker");
    }
    assert!(carries_marker, "the page text survived the fold");
}

#[tokio::test]
async fn without_emit_document_no_document_event_is_sent() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "no-document-marker");

    let events = harness.parse_ok(&pdf).await;
    assert_eq!(common::shape(&events), ["info", "page", "page", "status"]);
    assert!(common::documents(&events).is_empty());
}

#[tokio::test]
async fn emit_document_in_detect_only_names_pages_without_text() {
    let harness = common::start().await;
    let pdf = common::text_pdf(4, 40, "detect-document-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("detect-only parse");

    assert_eq!(common::shape(&events), ["info", "document", "status"]);
    let documents = common::documents(&events);
    assert_eq!(
        documents[0].pages.len(),
        4,
        "detection still names the pages"
    );
    assert!(
        documents[0].texts.is_empty(),
        "detect-only extracts nothing, so the fold has nothing to structure"
    );
}
