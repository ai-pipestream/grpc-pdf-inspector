// SPDX-License-Identifier: Apache-2.0

//! The link layer: the annotations the file actually carries, rather than
//! URLs spotted in the visible text.
//!
//! A `/Link` annotation is a rectangle and a target. The words under that
//! rectangle are the anchor, and they are frequently not a URL — "click
//! here", a figure number, a person's name. Reading the annotation layer is
//! the only way to see those, and the only way to avoid inventing a link
//! for a bare URL that nobody linked.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

const TARGET: &str = "https://example.invalid/target";

/// The base of any text item the fold makes.
fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
    match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref().expect("a base"),
        other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
    }
}

#[tokio::test]
async fn a_link_annotation_reaches_the_wire_as_a_run_with_its_target() {
    let harness = common::start().await;
    let pdf = common::link_pdf("click here", TARGET);

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

    let spans = common::spans(&events);
    let link = spans[0]
        .spans
        .iter()
        .find(|span| span.kind == pb::SpanKind::Link as i32)
        .expect("the annotation is one of the page's runs");
    assert_eq!(link.link_uri, TARGET);

    let rect = link.bbox.as_ref().expect("an annotation has a rectangle");
    assert!(rect.width > 0.0 && rect.height > 0.0, "{rect:?}");
}

#[tokio::test]
async fn the_anchored_text_carries_the_link_as_an_inline_span() {
    let harness = common::start().await;
    let pdf = common::link_pdf("click here", TARGET);

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

    let document = common::documents(&events)[0];
    let anchored = document
        .texts
        .iter()
        .map(base_of)
        .find(|base| !base.spans.is_empty())
        .expect("some item carries a link run");

    assert_eq!(anchored.spans.len(), 1, "one annotation, one run");
    let span = &anchored.spans[0];
    assert_eq!(span.hyperlink.as_deref(), Some(TARGET));

    let range = span.range.as_ref().expect("a run names its characters");
    let text: Vec<char> = anchored.text.chars().collect();
    let start = usize::try_from(range.start).expect("a non-negative start");
    let end = usize::try_from(range.end).expect("a non-negative end");
    assert!(end <= text.len(), "the range is inside the text: {range:?}");
    let linked: String = text[start..end].iter().collect();
    assert!(
        linked.contains("click here"),
        "the run covers the anchor text, not the whole page: {linked:?}"
    );
}

#[tokio::test]
async fn an_item_that_is_entirely_one_link_says_so_at_item_level_too() {
    let harness = common::start().await;
    let pdf = common::link_pdf("click here", TARGET);

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

    let document = common::documents(&events)[0];
    let anchored = document
        .texts
        .iter()
        .map(base_of)
        .find(|base| base.text.trim() == "click here")
        .expect("the anchor line is its own block");
    assert_eq!(
        anchored.hyperlink.as_deref(),
        Some(TARGET),
        "a block that is entirely a link is a link in the upstream dialect too"
    );
}

#[tokio::test]
async fn unlinked_prose_gets_no_link() {
    let harness = common::start().await;
    let pdf = common::link_pdf("click here", TARGET);

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

    let document = common::documents(&events)[0];
    for base in document.texts.iter().map(base_of) {
        if base.text.contains("click here") {
            continue;
        }
        assert!(
            base.spans.is_empty(),
            "prose outside the annotation is not linked: {:?}",
            base.text
        );
        assert!(base.hyperlink.is_none(), "{:?}", base.text);
    }
}

#[tokio::test]
async fn a_document_with_no_annotations_carries_no_link_runs() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "unlinked-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                emit_document: true,
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    for page in common::spans(&events) {
        assert!(
            page.spans
                .iter()
                .all(|span| span.kind != pb::SpanKind::Link as i32),
            "no annotations, no link runs"
        );
    }
    let document = common::documents(&events)[0];
    for base in document.texts.iter().map(base_of) {
        assert!(base.spans.is_empty());
        assert!(base.hyperlink.is_none());
    }
}
