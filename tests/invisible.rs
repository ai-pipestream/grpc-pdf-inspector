// SPDX-License-Identifier: Apache-2.0

//! Text the page drew and no reader saw.
//!
//! Text rendering mode 3 paints no glyphs. The show operator runs, the text
//! matrix advances, and nothing appears: it is how a scan's OCR layer hides
//! behind its raster and how a watermark rides along without printing. It
//! is not content, so it does not belong in the markdown, and every
//! extraction this service did left it out. What it did not do was say so,
//! and a document that carries text nothing downstream ever hears about is
//! the thing this file is about.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The watermark every fixture here hides.
const WATERMARK: &str = "CONFIDENTIAL DRAFT";

#[tokio::test]
async fn a_hidden_watermark_stays_out_of_the_markdown_and_is_reported_anyway() {
    let harness = common::start().await;
    let events = harness
        .parse_ok(&common::invisible_text_pdf(WATERMARK))
        .await;

    let page = common::pages(&events)[0];
    assert!(
        !page.markdown.contains(WATERMARK),
        "invisible text is not content a reader saw: {:?}",
        page.markdown
    );
    assert!(
        page.markdown.contains("Visible line 0"),
        "the article itself is untouched: {:?}",
        page.markdown
    );
    assert!(
        common::status(&events).has_invisible_text,
        "the caller is told the layer exists without having to ask for it"
    );
    assert!(
        page.invisible.is_empty(),
        "the runs themselves stay off the wire until they are asked for"
    );
}

#[tokio::test]
async fn the_hidden_runs_arrive_with_their_boxes_when_they_are_asked_for() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::invisible_text_pdf(WATERMARK),
            pb::PdfOptions {
                report_invisible: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let page = common::pages(&events)[0];
    assert_eq!(
        page.invisible.len(),
        1,
        "one hidden run, and only the hidden one: {:?}",
        page.invisible
    );
    let run = &page.invisible[0];
    assert_eq!(run.text, WATERMARK);
    assert_eq!(run.kind, pb::SpanKind::Text as i32);
    let bbox = run.bbox.as_ref().expect("a placed run has a box");
    assert!((bbox.x - 140.0).abs() < 1.0, "{bbox:?}");
    assert!((bbox.y - 300.0).abs() < 1.0, "{bbox:?}");
    assert!(bbox.width > 0.0 && bbox.height > 0.0, "{bbox:?}");
    assert!(
        !page.markdown.contains(WATERMARK),
        "reporting it is not putting it back into the body"
    );
}

#[tokio::test]
async fn a_document_that_hides_nothing_says_so_and_pays_nothing() {
    let harness = common::start().await;
    let before = harness.metrics.snapshot().parser_passes;
    let events = harness
        .parse(
            &common::text_pdf(1, 40, "plain"),
            pb::PdfOptions {
                report_invisible: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    assert!(!common::status(&events).has_invisible_text);
    assert!(common::pages(&events)[0].invisible.is_empty());
    assert_eq!(
        harness.metrics.snapshot().parser_passes - before,
        2,
        "asking for a layer the first walk said is not there costs nothing"
    );
}

#[tokio::test]
async fn the_fold_puts_the_hidden_run_in_the_invisible_layer() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::invisible_text_pdf(WATERMARK),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    let hidden = document
        .texts
        .iter()
        .filter_map(|item| match item.item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref(),
            _ => None,
        })
        .find(|base| base.text == WATERMARK)
        .expect("the hidden run is an item, not an absence");

    assert_eq!(
        hidden.content_layer,
        doc::ContentLayer::Invisible as i32,
        "which layer it is in is the whole of what makes it honest"
    );
    assert_eq!(hidden.prov[0].page_no, 1);
    assert!(
        hidden.prov[0].bbox.is_some(),
        "a coordinator can say where on the page the hidden text sits"
    );
    assert!(
        document
            .body
            .as_ref()
            .expect("a body")
            .children
            .iter()
            .all(|child| child.r#ref != hidden.self_ref),
        "and it is not in the body"
    );
}
