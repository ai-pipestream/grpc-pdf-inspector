// SPDX-License-Identifier: Apache-2.0

//! The positioned runs, and the provenance they put on a folded Document.
//!
//! Geometry is the thing this wire had none of: the parser measures every
//! run it extracts and the whole measurement used to be thrown away when
//! the markdown was rendered. These tests are the ones that fail if it
//! starts being thrown away again.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The base of any text item the fold makes.
fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
    match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref().expect("a base"),
        other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
    }
}

#[tokio::test]
async fn runs_stay_off_the_wire_unless_they_are_asked_for() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "quiet-marker");

    let events = harness.parse_ok(&pdf).await;
    assert_eq!(common::shape(&events), ["info", "page", "page", "status"]);
    assert!(common::spans(&events).is_empty());
}

#[tokio::test]
async fn emit_spans_puts_each_pages_runs_before_its_markdown() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "spans-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert_eq!(
        common::shape(&events),
        ["info", "spans", "page", "spans", "page", "status"],
        "the runs arrive before the rendering they produced"
    );

    let spans = common::spans(&events);
    let page_numbers: Vec<u32> = spans.iter().map(|page| page.page_no).collect();
    assert_eq!(page_numbers, [1, 2], "runs are attributed to their page");

    let first = spans[0];
    assert!(!first.spans.is_empty(), "a text page has runs");
    let marker = first
        .spans
        .iter()
        .find(|span| span.text.contains("spans-marker"))
        .expect("the fixture's marker is one of the runs");

    let bbox = marker.bbox.as_ref().expect("every run is measured");
    assert!(bbox.width > 0.0, "a drawn run has width: {bbox:?}");
    assert!(bbox.height > 0.0, "a drawn run has height: {bbox:?}");
    assert!(
        bbox.x >= 0.0 && bbox.y >= 0.0,
        "the fixture draws inside its page box: {bbox:?}"
    );
    assert!(
        marker.font_family.contains("Helvetica"),
        "the run names the face it was drawn with: {:?}",
        marker.font_family
    );
    assert!(marker.font_size > 0.0);
    assert_eq!(marker.kind, pb::SpanKind::Text as i32);
}

#[tokio::test]
async fn a_folded_document_carries_page_and_box_provenance() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "prov-marker");

    // Deliberately without `emit_spans`: the fold sees the runs whether or
    // not the caller asked for them on the wire.
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
    assert!(
        common::spans(&events).is_empty(),
        "the fold's input is not the caller's bandwidth"
    );

    let document = common::documents(&events)[0];
    assert!(!document.texts.is_empty());

    let mut boxed = 0;
    for item in &document.texts {
        let base = base_of(item);
        assert_eq!(base.prov.len(), 1, "every item names where it came from");
        let prov = &base.prov[0];
        assert!(
            (1..=2).contains(&prov.page_no),
            "the page is a page of this document: {prov:?}"
        );
        assert!(
            base.meta
                .as_ref()
                .is_none_or(|meta| meta.custom_fields.is_empty()),
            "the page is typed provenance, not a custom field"
        );
        if let Some(bbox) = prov.bbox.as_ref() {
            assert!(bbox.r > bbox.l, "a box has positive width: {bbox:?}");
            assert!(bbox.t > bbox.b, "a box has positive height: {bbox:?}");
            assert_eq!(
                bbox.coord_origin,
                Some(doc::CoordOrigin::Bottomleft as i32),
                "boxes are in the space the file is written in"
            );
            boxed += 1;
        }
    }
    assert!(
        boxed > 0,
        "the runs located at least one item on the page it came from"
    );

    for (page_no, page) in &document.pages {
        assert_eq!(page.page_no, *page_no);
        assert_eq!(
            page.unit.as_deref(),
            Some("pt"),
            "a page declares the unit its items' boxes are measured in"
        );
    }
}

#[tokio::test]
async fn a_page_selection_only_measures_the_pages_it_asked_for() {
    let harness = common::start().await;
    let pdf = common::text_pdf(4, 20, "selected-spans-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                pages: vec![3],
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert_eq!(common::shape(&events), ["info", "spans", "page", "status"]);
    let spans = common::spans(&events);
    assert_eq!(spans[0].page_no, 3);
    assert!(
        spans[0]
            .spans
            .iter()
            .any(|span| span.text.contains("page 3")),
        "the runs are page 3's, not another page's"
    );
}

#[tokio::test]
async fn a_page_past_the_end_of_the_document_is_not_invented() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "short-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                pages: vec![1, 99],
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert_eq!(common::shape(&events), ["info", "page", "status"]);
    assert_eq!(common::pages(&events)[0].page_no, 1);
    assert_eq!(common::status(&events).pages_extracted, 1);
}

#[tokio::test]
async fn the_trailer_carries_the_extraction_passs_own_page_verdicts() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "verdict-marker");

    let events = harness.parse_ok(&pdf).await;
    let status = common::status(&events);
    // A clean fixture has nothing to report, and reports it as an empty
    // list rather than as a missing field.
    assert!(
        status.extraction_ocr_reasons.is_empty(),
        "a readable document needs no OCR: {:?}",
        status.extraction_ocr_reasons
    );
    for page in common::pages(&events) {
        assert!(!page.needs_ocr, "page {} is readable", page.page_no);
        assert_eq!(page.ocr_reason, pb::OcrReason::Unspecified as i32);
    }
}
