// SPDX-License-Identifier: Apache-2.0

//! Runs the rendering left out, folded back in, driven through the fold by
//! hand.
//!
//! The renderer decides what it drops, and on a fixture built here it drops
//! nothing: every layout this suite can author comes back whole. The path
//! that puts a dropped run back into the body therefore has no end-to-end
//! test that exercises it, and the first document that did exercise it was
//! an eleven-page paper on a live server.
//!
//! So the events are built here instead. `DocumentFold` is public and
//! consumes exactly what the wire carries, so a `page` event carrying
//! `dropped` runs is a fixture like any other, and the invariant it has to
//! keep is the one a body-walking consumer relies on: an item in the body
//! layer hangs under `#/body` and the walk reaches it. Layer and parent are
//! one decision, and this is where saying so is cheap.

mod common;

use grpc_pdf_inspector::document_fold::DocumentFold;
use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// One positioned run of a page, stacked down the page by `index` so the
/// fold can tell the runs apart by their boxes.
fn run(text: &str, index: usize) -> pb::TextSpan {
    pb::TextSpan {
        text: text.to_owned(),
        bbox: Some(pb::Rect {
            x: 72.0,
            y: 700.0 - 20.0 * index as f64,
            width: 400.0,
            height: 12.0,
        }),
        kind: pb::SpanKind::Text.into(),
        ..pb::TextSpan::default()
    }
}

/// The `spans` event a page's runs arrive on, ahead of its markdown.
fn spans(page_no: u32, runs: &[pb::TextSpan]) -> pb::parse_pdf_response::Event {
    pb::parse_pdf_response::Event::Spans(pb::PageSpans {
        page_no,
        spans: runs.to_vec(),
    })
}

/// The `info` event, which names the pages.
fn info(page_count: u32) -> pb::parse_pdf_response::Event {
    pb::parse_pdf_response::Event::Info(pb::PdfInfo {
        pdf_type: pb::PdfType::TextBased as i32,
        confidence: 0.9,
        page_count,
        ..pb::PdfInfo::default()
    })
}

/// Every body-layer item's text, in arena order.
fn body_texts(document: &doc::Document) -> Vec<String> {
    common::placed(document)
        .into_iter()
        .filter(|item| item.layer == doc::ContentLayer::Body as i32)
        .map(|item| item.text)
        .collect()
}

#[tokio::test]
async fn a_dropped_run_is_body_layer_body_parented_and_body_reachable() {
    // The page drew three runs and the renderer emitted two of them, in
    // the order it liked. The third is content.
    let page_runs = [
        run("the lead paragraph of the page", 0),
        run("the run the renderer left out", 1),
        run("the closing paragraph of the page", 2),
    ];

    let mut fold = DocumentFold::new();
    fold.consume(&info(1));
    fold.consume(&spans(1, &page_runs));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 1,
        markdown: "the lead paragraph of the page\n\nthe closing paragraph of the page\n"
            .to_owned(),
        furniture: vec!["A Running Head".to_owned()],
        dropped: vec![page_runs[1].clone()],
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    // Where the page had it: between the two blocks the renderer kept.
    assert_eq!(
        body_texts(&document),
        [
            "the lead paragraph of the page",
            "the run the renderer left out",
            "the closing paragraph of the page",
        ]
    );

    let reinstated = common::placed(&document)
        .into_iter()
        .find(|item| item.text == "the run the renderer left out")
        .expect("the dropped run is an item");
    assert_eq!(
        reinstated.layer,
        doc::ContentLayer::Body as i32,
        "a run the renderer dropped is content"
    );
    assert_eq!(
        reinstated.parent,
        common::BODY,
        "and content hangs off the body, not off the group its report came with"
    );
    assert!(
        common::reachable_from_body(&document).contains(&reinstated.self_ref),
        "a body walk reaches it, which is the whole point of putting it back"
    );

    // The chrome that rode in on the same event is still chrome.
    let head = common::placed(&document)
        .into_iter()
        .find(|item| item.text == "A Running Head")
        .expect("the chrome is an item too");
    assert_eq!(head.layer, doc::ContentLayer::Furniture as i32);
    assert_eq!(head.parent, common::FURNITURE);

    common::assert_layers_and_parents_agree(&document);
}

#[tokio::test]
async fn dropped_runs_keep_their_boxes_and_their_page() {
    let page_runs = [run("kept", 0), run("dropped with a box", 1)];
    let mut fold = DocumentFold::new();
    fold.consume(&info(3));
    fold.consume(&spans(3, &page_runs));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 3,
        markdown: "kept\n".to_owned(),
        dropped: vec![page_runs[1].clone()],
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    let base = match document.texts[1].item.as_ref() {
        Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref().expect("a base"),
        other => panic!("a text item, got {other:?}"),
    };
    assert_eq!(base.text, "dropped with a box");
    assert_eq!(base.prov.len(), 1, "it still names its page");
    assert_eq!(base.prov[0].page_no, 3);
    let bbox = base.prov[0].bbox.as_ref().expect("the box came with it");
    assert!((bbox.b - 680.0).abs() < f64::EPSILON, "{bbox:?}");
    common::assert_layers_and_parents_agree(&document);
}

#[tokio::test]
async fn a_dropped_run_beside_a_list_is_the_bodys_child_not_the_lists() {
    // The renderer emitted a list and left a run out after it. A run it
    // never emitted was in no list of its own, so it is the body's child;
    // the list's items stay the group's.
    let page_runs = [
        run("first bullet", 0),
        run("second bullet", 1),
        run("the run the renderer left out", 2),
    ];
    let mut fold = DocumentFold::new();
    fold.consume(&info(1));
    fold.consume(&spans(1, &page_runs));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 1,
        markdown: "- first bullet\n- second bullet\n".to_owned(),
        dropped: vec![page_runs[2].clone()],
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    assert_eq!(document.groups.len(), 1, "one list, one group");
    let items = common::placed(&document);
    let bullet = items
        .iter()
        .find(|item| item.text == "first bullet")
        .expect("the bullet is an item");
    assert_eq!(bullet.parent, "#/groups/0");
    let reinstated = items
        .iter()
        .find(|item| item.text == "the run the renderer left out")
        .expect("the dropped run is an item");
    assert_eq!(reinstated.parent, common::BODY);

    // Both are reachable: one through the group, one straight off the body.
    let reached = common::reachable_from_body(&document);
    assert!(reached.contains(&bullet.self_ref));
    assert!(reached.contains(&reinstated.self_ref));
    common::assert_layers_and_parents_agree(&document);
}

#[tokio::test]
async fn a_page_that_is_all_dropped_runs_still_has_a_body() {
    // Every run left out and no markdown at all: the flush after the last
    // block is what puts them back, and they are still body.
    let page_runs = [run("first left behind", 0), run("second left behind", 1)];
    let mut fold = DocumentFold::new();
    fold.consume(&info(1));
    fold.consume(&spans(1, &page_runs));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 1,
        markdown: String::new(),
        dropped: page_runs.to_vec(),
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    assert_eq!(
        body_texts(&document),
        ["first left behind", "second left behind"],
        "in the order the page drew them"
    );
    assert_eq!(
        document.body.as_ref().expect("a body").children.len(),
        2,
        "the body lists them, so a walk finds them"
    );
    common::assert_layers_and_parents_agree(&document);
}

#[tokio::test]
async fn a_dropped_run_with_no_runs_to_place_it_among_is_still_body() {
    // No `spans` event arrived, so nothing says where the run stood. It
    // goes to the end of the page, and it is still the body's.
    let mut fold = DocumentFold::new();
    fold.consume(&info(1));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 1,
        markdown: "the rendered block\n".to_owned(),
        furniture: vec!["12".to_owned()],
        dropped: vec![run("unplaceable but real", 4)],
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    assert_eq!(
        body_texts(&document),
        ["the rendered block", "unplaceable but real"]
    );
    common::assert_layers_and_parents_agree(&document);
}

#[tokio::test]
async fn an_underlined_running_head_is_chrome_once_and_not_body_as_well() {
    // The renderer spells a run it considers underlined `<u>text</u>`, and
    // those tags carry letters. Comparing the rendered block to the chrome
    // report on letters alone therefore missed the match, and a head the
    // report had already filed as furniture was folded into the body as
    // well. On a review paper that is every page.
    let mut fold = DocumentFold::new();
    fold.consume(&info(1));
    fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
        page_no: 1,
        markdown: "<u>Under review as a conference paper</u>\n\nthe body of the page\n".to_owned(),
        furniture: vec!["Under review as a conference paper".to_owned()],
        ..pb::PageMarkdown::default()
    }));
    let document = fold.take();

    assert_eq!(
        body_texts(&document),
        ["the body of the page"],
        "the head is chrome, so it is not body as well"
    );
    assert_eq!(document.texts.len(), 2, "one item for the head, not two");
    common::assert_layers_and_parents_agree(&document);
}
