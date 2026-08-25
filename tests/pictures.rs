// SPDX-License-Identifier: Apache-2.0

//! Images as placed items, and links that cover no words.
//!
//! The content-stream walker emits a run for every image XObject with the
//! box the transformation matrix put it at. The markdown renderer discards
//! those runs by default, so `Document.pictures` was structurally always
//! empty and `DOC_ITEM_LABEL_PICTURE` never appeared. A link annotation
//! over a figure had nowhere to go at all: its rectangle covers no text, so
//! no inline span can carry it. It leaves the document as the picture's
//! `hyperlink` and jumps back into it as the picture's `target`.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

const FIGURE_LINK: &str = "https://example.invalid/figure";

#[tokio::test]
async fn an_image_reaches_the_wire_as_a_placed_run() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::illustrated_pdf(FIGURE_LINK),
            pb::PdfOptions {
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let image = common::spans(&events)[0]
        .spans
        .iter()
        .find(|span| span.kind == pb::SpanKind::Image as i32)
        .expect("the page drew an image");
    let bbox = image.bbox.as_ref().expect("with a box");
    assert!((bbox.x - 72.0).abs() < f64::EPSILON, "{bbox:?}");
    assert!((bbox.width - 120.0).abs() < f64::EPSILON, "{bbox:?}");
    assert!((bbox.height - 80.0).abs() < f64::EPSILON, "{bbox:?}");
}

#[tokio::test]
async fn the_fold_places_the_picture_and_hangs_the_region_link_on_it() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::illustrated_pdf(FIGURE_LINK),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    assert_eq!(document.pictures.len(), 1, "one image, one picture");

    let picture = &document.pictures[0];
    assert_eq!(picture.self_ref, "#/pictures/0");
    assert_eq!(picture.label, doc::DocItemLabel::Picture as i32);
    assert_eq!(picture.content_layer, doc::ContentLayer::Body as i32);
    assert!(
        picture.image.is_none(),
        "the bytes were never decoded, so nothing is claimed about them"
    );

    let prov = &picture.prov[0];
    assert_eq!(prov.page_no, 1);
    let bbox = prov.bbox.as_ref().expect("a placed picture has a place");
    assert!((bbox.l - 72.0).abs() < f64::EPSILON, "{bbox:?}");
    assert!((bbox.b - 120.0).abs() < f64::EPSILON, "{bbox:?}");
    assert!((bbox.t - 200.0).abs() < f64::EPSILON, "{bbox:?}");

    assert_eq!(
        picture.hyperlink.as_deref(),
        Some(FIGURE_LINK),
        "a link over a figure has nowhere else to go"
    );
    assert!(picture.target.is_none(), "this one leads out, not in");

    // The picture is a child of whatever it sits under, like any item.
    let parent = &picture.parent.as_ref().expect("a parent").r#ref;
    let body = document.body.as_ref().expect("a body");
    assert_eq!(parent, "#/body");
    assert!(
        body.children
            .iter()
            .any(|child| child.r#ref == picture.self_ref),
        "and its parent lists it"
    );

    // The link over the image did not also become an inline span on the
    // prose: the annotation covers no words.
    for item in &document.texts {
        if let Some(doc::base_text_item::Item::Text(text)) = item.item.as_ref() {
            let base = text.base.as_ref().expect("a base");
            assert!(base.spans.is_empty(), "{:?}", base.text);
            assert!(base.hyperlink.is_none(), "{:?}", base.text);
        }
    }
}

#[tokio::test]
async fn a_figure_that_jumps_into_the_document_carries_a_target() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::illustrated_pdf_linking_inward(),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    let picture = &document.pictures[0];
    assert!(
        picture.hyperlink.is_none(),
        "a jump inside the document is not a URL"
    );
    assert_eq!(
        picture.target.as_ref().expect("a target").r#ref,
        "#/pages/2",
        "the figure points at the page item the destination lands on"
    );
    assert!(
        document.pages.contains_key(&2),
        "and that item is one this fragment emitted"
    );
}

#[tokio::test]
async fn a_document_that_draws_no_images_has_no_pictures() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(2, 20, "no-figure-marker"),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");
    assert!(common::documents(&events)[0].pictures.is_empty());
}
