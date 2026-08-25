// SPDX-License-Identifier: Apache-2.0

//! What the page had and the markdown does not.
//!
//! Repeated headers, footers and folio numbers are identified and then
//! deleted, and the deletion was silent: `Document.furniture` was created
//! empty on every response and `CONTENT_LAYER_FURNITURE` went unused, while
//! the lines that belonged there had already gone. Other runs are dropped
//! by the layout pass for reasons no option reaches. Both are the same
//! thing from a consumer's side — text that was on the page and is not in
//! the output — and both are reported here.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

#[tokio::test]
async fn nothing_is_reported_unless_the_report_is_asked_for() {
    let harness = common::start().await;
    let events = harness
        .parse_ok(&common::furniture_pdf(3, "A Running Head"))
        .await;
    for page in common::pages(&events) {
        assert!(page.furniture.is_empty(), "page {}", page.page_no);
    }
}

#[tokio::test]
async fn a_folio_that_never_reached_the_markdown_is_named() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::furniture_pdf(3, "A Running Head"),
            pb::PdfOptions {
                report_furniture: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    for page in common::pages(&events) {
        assert!(
            page.markdown.contains("Body line 0"),
            "the body survived on page {}",
            page.page_no
        );
        assert!(
            page.furniture
                .iter()
                .any(|line| line.trim() == page.page_no.to_string()),
            "the folio was dropped and is now named, page {}: {:?}",
            page.page_no,
            page.furniture
        );
        for line in &page.furniture {
            assert!(
                !page
                    .markdown
                    .lines()
                    .any(|rendered| rendered.trim() == line.trim()),
                "a run cannot be both dropped and rendered: {line:?}"
            );
        }
    }
}

#[tokio::test]
async fn dropped_runs_land_in_the_documents_furniture_layer() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::furniture_pdf(2, "A Running Head"),
            pb::PdfOptions {
                report_furniture: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    let furniture = document.furniture.as_ref().expect("a furniture group");
    assert!(
        !furniture.children.is_empty(),
        "the group that was always empty is not empty any more"
    );

    for child in &furniture.children {
        let index: usize = child
            .r#ref
            .strip_prefix("#/texts/")
            .and_then(|rest| rest.parse().ok())
            .expect("a furniture child is a text item");
        let base = match document.texts[index].item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref().expect("a base"),
            other => panic!("furniture is plain text, got {other:?}"),
        };
        assert_eq!(base.content_layer, doc::ContentLayer::Furniture as i32);
        assert_eq!(base.prov.len(), 1, "furniture still names its page");
    }

    // The body is unaffected: nothing was moved out of it, only reported.
    let body = document.body.as_ref().expect("a body");
    assert!(!body.children.is_empty());
}
