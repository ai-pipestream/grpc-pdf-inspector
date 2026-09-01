// SPDX-License-Identifier: Apache-2.0

//! The body invariant, on the layout that broke it.
//!
//! A two-column paper with a running head and a line number beside every
//! row is where the furniture report went wrong: the report compared each
//! page's runs against its markdown and called everything missing chrome,
//! the renderer reads columns one at a time so nothing matched in order,
//! and the body ended up with a handful of items while the furniture layer
//! held the paper. A consumer that walks `#/body`, which is the document
//! viewer and every chunker, saw almost nothing.
//!
//! Two invariants hold this shut, and both are asserted here over the same
//! fixture:
//!
//! - Only chrome is furniture. The head and the margin numbers are; the
//!   columns are not, whatever the renderer did with them.
//! - Every body-layer item is reachable from `#/body`, walking groups. A
//!   text item is never another text item's parent, so there is no branch
//!   of the fragment a body walk cannot get to.
//! - An item's layer and the group it hangs under say the same thing. An
//!   item that declares the body layer and hangs off `#/furniture` is body
//!   content no body walk can reach, and it is exactly what a fold that
//!   decides the layer in one place and the parent in another produces.
//!   `common::assert_layers_and_parents_agree` states it once and both
//!   fixtures here are held to it, as is every hand-built fragment in
//!   `tests/dropped_runs.rs`.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The running head the fixture prints on every page.
const HEAD: &str = "Journal of Synthetic Studies";

/// How many pages the fixture has. Three or more, so cross-page repetition
/// is evidence at all.
const PAGES: u32 = 4;

/// The base of any text item that has one.
fn base_of(item: &doc::BaseTextItem) -> Option<&doc::TextItemBase> {
    match item.item.as_ref()? {
        doc::base_text_item::Item::Text(text) => text.base.as_ref(),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref(),
        doc::base_text_item::Item::ListItem(list_item) => list_item.base.as_ref(),
        _ => None,
    }
}

/// Parse the fixture with the furniture report and the Document on.
async fn parse() -> Vec<pb::parse_pdf_response::Event> {
    let harness = common::start().await;
    harness
        .parse(
            &common::two_column_pdf(PAGES, HEAD),
            pb::PdfOptions {
                report_furniture: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse")
}

/// Whether a run of the fixture is one of its column notes: the prose that
/// must be body, and body-reachable, on every page.
fn is_column_note(text: &str) -> bool {
    text.starts_with("Method note") || text.starts_with("Result note")
}

#[tokio::test]
async fn only_the_head_the_folio_and_the_margin_numbers_are_furniture() {
    let events = parse().await;
    let pages = common::pages(&events);
    assert_eq!(pages.len(), PAGES as usize);

    for page in pages {
        let mut named: Vec<&str> = page.furniture.iter().map(String::as_str).collect();
        named.sort_unstable();
        let mut expected: Vec<String> = (1..=18).map(|row| row.to_string()).collect();
        expected.push(HEAD.to_owned());
        expected.push(page.page_no.to_string());
        let mut expected: Vec<&str> = expected.iter().map(String::as_str).collect();
        expected.sort_unstable();
        assert_eq!(
            named, expected,
            "page {} names its chrome and nothing else",
            page.page_no
        );
        assert!(
            !page.furniture.iter().any(|line| is_column_note(line)),
            "a column of prose is not chrome, page {}",
            page.page_no
        );
    }
}

#[tokio::test]
async fn every_body_item_is_reachable_from_the_body() {
    let events = parse().await;
    let document = common::documents(&events)[0];
    let reached = common::reachable_from_body(document);

    for (index, item) in document.texts.iter().enumerate() {
        let Some(base) = base_of(item) else { continue };
        if base.content_layer != doc::ContentLayer::Body as i32 {
            continue;
        }
        assert!(
            reached.contains(&format!("#/texts/{index}")),
            "body item #/texts/{index} is unreachable from #/body: {:?}",
            base.text
        );
    }
    for (index, table) in document.tables.iter().enumerate() {
        if table.content_layer == doc::ContentLayer::Body as i32 {
            assert!(reached.contains(&format!("#/tables/{index}")));
        }
    }
    for (index, picture) in document.pictures.iter().enumerate() {
        if picture.content_layer == doc::ContentLayer::Body as i32 {
            assert!(reached.contains(&format!("#/pictures/{index}")));
        }
    }
    assert!(
        !reached.is_empty(),
        "a fixture with four pages of prose has a body"
    );
}

#[tokio::test]
async fn no_text_item_is_another_text_items_parent() {
    let events = parse().await;
    let document = common::documents(&events)[0];
    for item in &document.texts {
        let Some(base) = base_of(item) else { continue };
        let parent = &base.parent.as_ref().expect("a parent").r#ref;
        assert!(
            !parent.starts_with("#/texts/"),
            "{:?} hangs off the text item {parent}",
            base.text
        );
        assert!(
            base.children.is_empty(),
            "{:?} owns children a body walk would have to know about",
            base.text
        );
    }
}

#[tokio::test]
async fn the_columns_are_body_and_the_chrome_is_not_in_them() {
    let events = parse().await;
    let document = common::documents(&events)[0];
    let reached = common::reachable_from_body(document);

    // Every reachable body item's text, joined: the columns are rendered
    // as one block per column, so a run of the page is in the body when the
    // body's text contains it.
    let mut body = String::new();
    let mut furniture = Vec::new();
    for (index, item) in document.texts.iter().enumerate() {
        let Some(base) = base_of(item) else { continue };
        if base.content_layer == doc::ContentLayer::Body as i32
            && reached.contains(&format!("#/texts/{index}"))
        {
            body.push_str(&base.text);
            body.push('\n');
        }
        if base.content_layer == doc::ContentLayer::Furniture as i32 {
            furniture.push(base.text.as_str());
        }
    }

    for page in 1..=PAGES {
        for row in 1..=18 {
            for note in [
                format!("Method note {row} of page {page} on the setup."),
                format!("Result note {row} of page {page} on the yield."),
            ] {
                assert!(
                    body.contains(&note),
                    "the renderer's ordering is not a verdict about {note:?}"
                );
                assert!(
                    !furniture.iter().any(|line| line.contains(&note)),
                    "nothing is both body and furniture: {note:?}"
                );
            }
        }
    }

    // The head is chrome on every page and is not a body item on any of
    // them, even though the renderer kept it in the markdown.
    assert_eq!(
        furniture.iter().filter(|line| **line == HEAD).count(),
        PAGES as usize
    );
    assert!(
        !document.texts.iter().filter_map(base_of).any(|base| {
            base.content_layer == doc::ContentLayer::Body as i32 && base.text == HEAD
        }),
        "the running head is furniture, so it is not also body"
    );
}

#[tokio::test]
async fn every_items_layer_and_parent_say_the_same_thing() {
    let events = parse().await;
    common::assert_layers_and_parents_agree(common::documents(&events)[0]);
}

/// The review format, which is where the remaining faults were found: an
/// underlined running head one ordinary line above the body, and a line
/// number in the margin of every row.
async fn review_paper() -> Vec<pb::parse_pdf_response::Event> {
    let harness = common::start().await;
    harness
        .parse(
            &common::review_paper_pdf(11, "Under review as a conference paper"),
            pb::PdfOptions {
                report_furniture: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse")
}

#[tokio::test]
async fn a_review_papers_head_and_line_numbers_are_chrome_and_its_prose_is_not() {
    let events = review_paper().await;
    for page in common::pages(&events) {
        assert!(
            page.furniture
                .iter()
                .any(|line| line == "Under review as a conference paper"),
            "the head is chrome on page {}, rule under it or not: {:?}",
            page.page_no,
            page.furniture
        );
        assert_eq!(
            page.furniture
                .iter()
                .filter(|line| line.chars().all(|c| c.is_ascii_digit()))
                .count(),
            41,
            "forty margin numbers and a folio, page {}",
            page.page_no
        );
        assert!(
            !page.furniture.iter().any(|line| line.contains("Body row")),
            "prose is never chrome, page {}",
            page.page_no
        );
    }

    let document = common::documents(&events)[0];
    let body: Vec<String> = common::placed(document)
        .into_iter()
        .filter(|item| item.layer == doc::ContentLayer::Body as i32)
        .map(|item| item.text)
        .collect();
    assert!(
        !body.iter().any(|text| text.contains("Under review")),
        "the head is chrome, so it is not also body: {body:?}"
    );
    for page in 1..=11 {
        assert!(
            body.iter()
                .any(|text| text.contains(&format!("Body row 0 of page {page}"))),
            "page {page}'s prose is in the body"
        );
    }
    common::assert_layers_and_parents_agree(document);
}
