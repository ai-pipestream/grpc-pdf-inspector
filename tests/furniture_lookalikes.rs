// SPDX-License-Identifier: Apache-2.0

//! Body text that looks like the page's chrome is still body text.
//!
//! The fold used to drop any block of the rendering whose letters matched
//! a furniture line. The chrome runs are taken out of the page before it is
//! rendered, so a block can only match one by being a different run with
//! the same letters: a section heading spelled like the running head, or,
//! for a furniture line with no letters at all, every other block with
//! none. Those blocks never reached `#/body`, and nothing said so.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// Fold `pdf` and return the document.
async fn fold(pdf: &[u8]) -> doc::Document {
    let events = common::start()
        .await
        .parse(
            pdf,
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");
    common::documents(&events)
        .first()
        .copied()
        .cloned()
        .expect("a document event")
}

/// The texts of the items in a layer.
fn texts_in(document: &doc::Document, layer: doc::ContentLayer) -> Vec<String> {
    common::placed(document)
        .into_iter()
        .filter(|item| item.layer == layer as i32)
        .map(|item| item.text)
        .collect()
}

#[tokio::test]
async fn a_heading_spelled_like_the_running_head_stays_in_the_body() {
    let document = fold(&common::furniture_pdf_with(
        3,
        "Results",
        Some((2, "Results")),
    ))
    .await;

    let furniture = texts_in(&document, doc::ContentLayer::Furniture);
    assert!(
        furniture.iter().filter(|text| *text == "Results").count() >= 3,
        "the control: the running head is chrome on every page: {furniture:?}"
    );
    let body = texts_in(&document, doc::ContentLayer::Body);
    assert!(
        body.iter().any(|text| text == "Results"),
        "page 2's own heading was dropped from the body: {body:?}"
    );
    let reachable = common::reachable_from_body(&document);
    assert!(
        common::placed(&document)
            .iter()
            .any(|item| item.text == "Results"
                && item.layer == doc::ContentLayer::Body as i32
                && reachable.contains(&item.self_ref)),
        "and it hangs under #/body"
    );
}

#[tokio::test]
async fn a_letterless_furniture_line_does_not_take_letterless_body_lines_with_it() {
    let document = fold(&common::furniture_pdf_with(
        3,
        "* * * * *",
        Some((2, "= = = = =")),
    ))
    .await;

    let furniture = texts_in(&document, doc::ContentLayer::Furniture);
    assert!(
        !furniture.is_empty(),
        "the control: the rule at the top of every page is chrome: {furniture:?}"
    );
    let body = texts_in(&document, doc::ContentLayer::Body);
    assert!(
        body.iter().any(|text| text.contains("= = =")),
        "page 2's own line has no letters, like the chrome, and was dropped: {body:?}"
    );
}
