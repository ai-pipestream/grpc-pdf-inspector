// SPDX-License-Identifier: Apache-2.0

//! What the page carries because it is a page.
//!
//! Running heads, footers and folio numbers are identified and then
//! deleted, and the deletion was silent: `Document.furniture` was created
//! empty on every response and `CONTENT_LAYER_FURNITURE` went unused, while
//! the lines that belonged there had already gone. This is where they are
//! named.
//!
//! Named on chrome evidence, and on nothing else. Reporting every run the
//! markdown does not contain looks like the same report and is not one: a
//! renderer reads a multi-column page one column at a time and leaves out
//! genuine content, and calling that furniture empties the body of a real
//! paper. `tests/body_reachability.rs` is the test that fails if someone
//! puts the two facts back together.

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
        assert!(page.dropped.is_empty(), "page {}", page.page_no);
    }
}

#[tokio::test]
async fn the_head_and_the_folio_are_named_and_the_body_is_not() {
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
            "the folio is chrome, page {}: {:?}",
            page.page_no,
            page.furniture
        );
        assert!(
            page.furniture.iter().any(|line| line == "A Running Head"),
            "the running head is chrome, page {}: {:?}",
            page.page_no,
            page.furniture
        );
        // The evidence is the repetition and the page edge, not the
        // rendering: the head is in this page's markdown, because the
        // renderer is shown one page at a time and one page repeats
        // nothing, and it is chrome all the same.
        for line in &page.furniture {
            assert!(
                !line.starts_with("Body line"),
                "body prose is never chrome: {line:?}"
            );
        }
        assert!(
            page.dropped.is_empty(),
            "every run of a one-column page reached the markdown: {:?}",
            page.dropped
        );
    }
}

#[tokio::test]
async fn page_chrome_lands_in_the_documents_furniture_layer() {
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
        assert!(
            !base.text.starts_with("Body line"),
            "body prose is never furniture: {:?}",
            base.text
        );
    }

    // The body is the body: the prose is all there, and the chrome the
    // renderer kept in the markdown is not doubled into it.
    let body = document.body.as_ref().expect("a body");
    assert!(!body.children.is_empty());
    let body_texts: Vec<&str> = document
        .texts
        .iter()
        .filter_map(|item| match item.item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref(),
            _ => None,
        })
        .filter(|base| base.content_layer == doc::ContentLayer::Body as i32)
        .map(|base| base.text.as_str())
        .collect();
    assert!(
        body_texts.iter().any(|text| text.contains("Body line 0")),
        "the prose is in the body: {body_texts:?}"
    );
    assert!(
        !body_texts.contains(&"A Running Head"),
        "the head is furniture, so it is not also a body item: {body_texts:?}"
    );
}

/// The head the review fixture prints on every page, ruled underneath.
const REVIEW_HEAD: &str = "Under review as a conference paper";

/// How many rows the review fixture numbers on each page.
const REVIEW_ROWS: u32 = 40;

/// Parse the review fixture, with the chrome report on or off.
async fn review_pages(report_furniture: bool) -> Vec<pb::parse_pdf_response::Event> {
    let harness = common::start().await;
    harness
        .parse(
            &common::review_paper_pdf(11, REVIEW_HEAD),
            pb::PdfOptions {
                report_furniture,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse")
}

#[tokio::test]
async fn chrome_never_reaches_the_markdown() {
    let events = review_pages(true).await;
    let pages = common::pages(&events);
    assert_eq!(pages.len(), 11);

    for page in pages {
        assert!(
            !page.markdown.contains(REVIEW_HEAD),
            "the head is chrome and the rendering never saw it, page {}",
            page.page_no
        );
        assert!(
            !page.markdown.contains("**"),
            "a margin number set in bold prints `**001**` when it fuses into a \
             line; page {} has one: {:?}",
            page.page_no,
            page.markdown.chars().take(120).collect::<String>()
        );
        for row in 0..REVIEW_ROWS {
            let number = format!("{:03}", (page.page_no - 1) * REVIEW_ROWS + row);
            assert!(
                !page.markdown.contains(&number),
                "the margin number {number} is chrome and is not in the body text \
                 of page {}",
                page.page_no
            );
        }
        // Every sentence starts a line or follows a space. A number fused
        // into the line lands immediately before one of them, and this is
        // what that reads like from a consumer's side.
        for (at, _) in page.markdown.match_indices("Body row") {
            let before = page.markdown[..at].chars().next_back();
            assert!(
                before.is_none_or(char::is_whitespace),
                "page {} glued something onto a sentence: {:?}",
                page.page_no,
                page.markdown[at.saturating_sub(12)..at].to_owned()
            );
        }
        assert!(
            page.markdown.contains("Body row 0 of page"),
            "the prose itself survived on page {}",
            page.page_no
        );
    }
}

#[tokio::test]
async fn the_fusion_is_real_and_the_report_is_what_prevents_it() {
    // The same fixture with no chrome verdict asked for: the margin
    // numbers share their rows' baselines, the renderer assembles a line
    // from the runs that share one, and the numbers come back inside the
    // body text. Removing them from the rendering's input is the only
    // thing that separates them, which is why the verdict has to be taken
    // before the page is rendered rather than after.
    let events = review_pages(false).await;
    let page = common::pages(&events)[1];
    assert!(
        page.markdown.contains("**040**"),
        "with no verdict to filter by, the number is in the sentence: {:?}",
        page.markdown.chars().take(120).collect::<String>()
    );
    assert!(page.markdown.contains(REVIEW_HEAD), "and so is the head");
}
