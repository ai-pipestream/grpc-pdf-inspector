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

#[tokio::test]
async fn a_landscape_page_drawn_turned_is_measured_as_it_is_shown() {
    // Distiller's landscape output: a portrait sheet with /Rotate 90 and
    // every line drawn turned a quarter. The runs used to come back with
    // negative y, below the bottom of a page still measured as portrait.
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::landscape_pdf(),
            pb::PdfOptions {
                emit_spans: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let page = common::pages(&events)[0];
    assert!(
        page.markdown.starts_with("The analytical engine"),
        "the reading order is unchanged: {:?}",
        page.markdown
    );

    let runs = &common::spans(&events)[0].spans;
    assert!(!runs.is_empty());
    for run in runs {
        let bbox = run.bbox.as_ref().expect("a box");
        assert!(
            bbox.x >= 0.0
                && bbox.x + bbox.width <= 792.5
                && bbox.y >= 0.0
                && bbox.y + bbox.height <= 612.5,
            "{:?} sits on the 792 x 612 page as shown: {bbox:?}",
            run.text
        );
    }
    let first = runs
        .iter()
        .find(|run| run.text.starts_with("The analytical"))
        .and_then(|run| run.bbox.as_ref())
        .expect("the first line has a box");
    assert!(
        (first.x - 54.0).abs() < 1.0,
        "54 points from the left: {first:?}"
    );
    assert!(
        (first.y - (612.0 - 60.0)).abs() < 2.0,
        "its baseline 60 points from the top: {first:?}"
    );

    // The form the page invokes at user space (100, 200) rides the spans
    // after the runs. It used to be turned twice, once by the library and
    // once more on the way out, and landed at x = -150, off the page.
    let form = runs
        .iter()
        .find(|run| run.kind == pb::SpanKind::Form as i32)
        .and_then(|run| run.bbox.as_ref())
        .expect("the form placement has a box");
    assert!(
        (form.x - 200.0).abs() < 0.5
            && (form.y - 462.0).abs() < 0.5
            && (form.width - 30.0).abs() < 0.5
            && (form.height - 50.0).abs() < 0.5,
        "the form sits 200 points from the left, 30 wide and 50 tall, bottom edge at 462: {form:?}"
    );

    let document = common::documents(&events)[0];
    let size = document.pages[&1]
        .size
        .as_ref()
        .expect("the page is measured");
    assert_eq!(
        (size.width, size.height),
        (792.0, 612.0),
        "the page as shown"
    );
}

/// Parse `pdf` with runs and a Document, returning the page's runs and the
/// Document.
async fn framed(pdf: &[u8]) -> (Vec<pb::TextSpan>, doc::Document) {
    let harness = common::start().await;
    let events = harness
        .parse(
            pdf,
            pb::PdfOptions {
                emit_spans: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");
    let runs = common::spans(&events)
        .first()
        .map(|page| page.spans.clone())
        .unwrap_or_default();
    (runs, common::documents(&events)[0].clone())
}

fn run_box<'a>(runs: &'a [pb::TextSpan], text: &str) -> &'a pb::Rect {
    runs.iter()
        .find(|run| run.text.trim() == text)
        .and_then(|run| run.bbox.as_ref())
        .unwrap_or_else(|| {
            panic!(
                "no run {text:?} among {:?}",
                runs.iter().map(|run| &run.text).collect::<Vec<_>>()
            )
        })
}

fn page_size(document: &doc::Document) -> (f64, f64) {
    let size = document.pages[&1].size.as_ref().expect("a measured page");
    (size.width, size.height)
}

/// Every box of every placed item, as (page, l, t, r, b).
fn placed_boxes(document: &doc::Document) -> Vec<(i32, f64, f64, f64, f64)> {
    let mut boxes = Vec::new();
    let mut take = |prov: &[doc::ProvenanceItem]| {
        for item in prov {
            if let Some(bbox) = &item.bbox {
                boxes.push((item.page_no, bbox.l, bbox.t, bbox.r, bbox.b));
            }
        }
    };
    for item in &document.texts {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Text(text) => {
                take(&text.base.as_ref().expect("a base").prov)
            }
            doc::base_text_item::Item::SectionHeader(h) => {
                take(&h.base.as_ref().expect("a base").prov)
            }
            doc::base_text_item::Item::ListItem(l) => take(&l.base.as_ref().expect("a base").prov),
            _ => {}
        }
    }
    for picture in &document.pictures {
        take(&picture.prov);
    }
    for table in &document.tables {
        take(&table.prov);
    }
    boxes
}

fn assert_inside(document: &doc::Document) {
    let (width, height) = page_size(document);
    for (page, l, t, r, b) in placed_boxes(document) {
        assert!(
            l >= -0.01 && r <= width + 0.01 && b >= -0.01 && t <= height + 0.01,
            "p{page} [{l:.1},{t:.1},{r:.1},{b:.1}] is outside the {width} x {height} page"
        );
    }
}

#[tokio::test]
async fn a_crop_box_is_the_page_and_moves_every_box_by_its_corner() {
    // The sheet is letter, the visible page is `[36 0 432 396]`: a run at
    // user space x = 100 is 64 points from the left edge a reader sees.
    let (runs, document) = framed(&common::framed_pdf(
        "BT /F1 12 Tf 1 0 0 1 100 300 Tm (cropped sheet) Tj ET",
        Some([36, 0, 432, 396]),
        None,
    ))
    .await;
    assert_eq!(page_size(&document), (396.0, 396.0));
    let bbox = run_box(&runs, "cropped sheet");
    assert!(
        (bbox.x - 64.0).abs() < 0.01 && (bbox.y - 300.0).abs() < 0.01,
        "{bbox:?}"
    );
    assert_inside(&document);
}

#[tokio::test]
async fn a_page_turned_anticlockwise_is_measured_as_it_is_shown() {
    // /Rotate 270 with the text drawn reading down the sheet, as such a
    // page is written: shown, it is 792 x 612, the line starts
    // 792 - 762 = 30 points from the left and its baseline is 500 points
    // up. The library reads such a page in a frame of its own, which is a
    // half turn away from the shown page.
    let (runs, document) = framed(&common::framed_pdf(
        "BT /F1 1 Tf 0 -11 11 0 500 762 Tm (The analytical engine weaves) Tj ET\n\
         BT /F1 1 Tf 0 -11 11 0 480 762 Tm (algebraic patterns just as) Tj ET\n\
         BT /F1 1 Tf 0 -11 11 0 460 762 Tm (the loom weaves flowers) Tj ET",
        None,
        Some(270),
    ))
    .await;
    assert_eq!(page_size(&document), (792.0, 612.0));
    let bbox = run_box(&runs, "The analytical engine weaves");
    assert!((bbox.x - 30.0).abs() < 0.01, "{bbox:?}");
    assert!((bbox.y - 500.0).abs() < 0.01, "{bbox:?}");
    assert!((bbox.height - 11.0).abs() < 0.01, "{bbox:?}");
    assert!(
        bbox.width > 100.0 && bbox.x + bbox.width < 792.0,
        "{bbox:?}"
    );
    assert_inside(&document);
}

#[tokio::test]
async fn glyphs_turned_on_an_upright_page_are_boxed_where_they_stand() {
    // A word set vertically up the right margin of an otherwise upright
    // page, one glyph per show operator, 28 points tall: each glyph covers
    // 28 points of x to the left of its origin and its advance of y above
    // it. The old box was zero wide and 28 tall, which put the last ones
    // above the page.
    let mut content =
        String::from("BT /F1 12 Tf 1 0 0 1 72 700 Tm (An upright paragraph on the page) Tj ET\n");
    for (index, letter) in ["A", "U", "T", "O"].iter().enumerate() {
        let y = 700 + index as i32 * 20;
        content.push_str(&format!(
            "BT /F1 28 Tf 0 1 -1 0 580 {y} Tm ({letter}) Tj ET\n"
        ));
    }
    let (runs, document) = framed(&common::framed_pdf(&content, None, None)).await;
    // The library joins the four glyphs into one word; the word's box is
    // the hull around them: 28 points wide, from the first origin up to
    // the top of the last glyph.
    let bbox = run_box(&runs, "AUTO");
    assert!(
        (bbox.x - 552.0).abs() < 0.01 && (bbox.width - 28.0).abs() < 0.01,
        "{bbox:?}"
    );
    assert!((bbox.y - 700.0).abs() < 0.01, "{bbox:?}");
    assert!(
        bbox.height > 70.0 && bbox.y + bbox.height <= 792.0,
        "{bbox:?}"
    );
    assert_inside(&document);
}

#[tokio::test]
async fn text_drawn_off_the_page_is_not_on_it() {
    // A hidden tag below the sheet and a heading off its left edge are
    // seen by nobody and are neither rendered nor folded; a line across
    // the right edge is seen in part, and its box stops at the edge.
    let (runs, document) = framed(&common::framed_pdf(
        "BT /F1 6 Tf 1 0 0 1 218 -14 Tm (<UN>) Tj ET\n\
         BT /F1 29 Tf 1 0 0 1 -506 747 Tm (Introduction and Methodology) Tj ET\n\
         BT /F1 12 Tf 1 0 0 1 72 700 Tm (A paragraph that is on the page) Tj ET\n\
         BT /F1 12 Tf 1 0 0 1 560 500 Tm (straddles the edge) Tj ET",
        None,
        None,
    ))
    .await;
    let texts: Vec<&str> = runs.iter().map(|run| run.text.trim()).collect();
    assert!(
        !texts.contains(&"<UN>") && !texts.contains(&"Introduction and Methodology"),
        "{texts:?}"
    );
    let folded: Vec<String> = common::placed(&document)
        .into_iter()
        .map(|item| item.text)
        .collect();
    assert!(
        !folded
            .iter()
            .any(|text| text.contains("<UN>") || text.contains("Introduction")),
        "{folded:?}"
    );
    let bbox = run_box(&runs, "straddles the edge");
    assert!(
        (bbox.x - 560.0).abs() < 0.01 && (bbox.x + bbox.width - 612.0).abs() < 0.01,
        "{bbox:?}"
    );
    assert_inside(&document);
}

#[tokio::test]
async fn an_upside_down_page_is_mirrored_both_ways_end_to_end() {
    // /Rotate 180: a run at user space (100, 700), 12 points tall, is
    // shown with its right edge 612 - 100 from the left and its baseline
    // 792 - 712 = 80 up; the page keeps its portrait size.
    let (runs, document) = framed(&common::framed_pdf(
        "BT /F1 12 Tf 1 0 0 1 100 700 Tm (upside down) Tj ET",
        None,
        Some(180),
    ))
    .await;
    assert_eq!(page_size(&document), (612.0, 792.0));
    let bbox = run_box(&runs, "upside down");
    assert!((bbox.x + bbox.width - 512.0).abs() < 0.01, "{bbox:?}");
    assert!(
        (bbox.y - 80.0).abs() < 0.01 && (bbox.height - 12.0).abs() < 0.01,
        "{bbox:?}"
    );
    assert_inside(&document);
}

#[tokio::test]
async fn a_rotation_written_as_a_real_turns_the_page_all_the_same() {
    // `/Rotate 90.0`: the metadata reader and the extraction walk must
    // agree, or the fold would say 612 x 792 over boxes placed on a
    // 792 x 612 page.
    let (runs, document) = framed(&common::framed_pdf_with(common::FramedPage {
        content: "BT /F1 1 Tf 0 11 -11 0 60 54 Tm (The analytical engine) Tj ET".to_owned(),
        rotate: Some(lopdf::Object::Real(90.0)),
        ..common::FramedPage::default()
    }))
    .await;
    assert_eq!(page_size(&document), (792.0, 612.0));
    let quality = document.pages[&1]
        .quality
        .as_ref()
        .expect("the turned page's quality");
    assert_eq!(quality.rotation_degrees, Some(90.0));
    let bbox = run_box(&runs, "The analytical engine");
    assert!(
        (bbox.x - 54.0).abs() < 0.01 && (bbox.y - (612.0 - 60.0)).abs() < 0.01,
        "{bbox:?}"
    );
    assert_inside(&document);

    // A real off the multiple of 90 rounds the same way in both readers:
    // 269.5 is a quarter turn anticlockwise for the fold and the walk.
    let (_, document) = framed(&common::framed_pdf_with(common::FramedPage {
        content: "BT /F1 1 Tf 0 -11 11 0 500 762 Tm (reading down the sheet) Tj ET".to_owned(),
        rotate: Some(lopdf::Object::Real(269.5)),
        ..common::FramedPage::default()
    }))
    .await;
    assert_eq!(page_size(&document), (792.0, 612.0));
    assert_eq!(
        document.pages[&1]
            .quality
            .as_ref()
            .and_then(|quality| quality.rotation_degrees),
        Some(270.0)
    );
    assert_inside(&document);
}

#[tokio::test]
async fn a_media_box_with_a_negative_origin_moves_every_box_by_its_corner() {
    // MediaBox [-100 -100 512 692], no crop box: the page is letter-sized
    // and a run at the user-space origin is 100 points in from both edges.
    let (runs, document) = framed(&common::framed_pdf_with(common::FramedPage {
        content: "BT /F1 12 Tf 1 0 0 1 0 0 Tm (at the origin) Tj ET".to_owned(),
        media_box: [-100, -100, 512, 692],
        ..common::FramedPage::default()
    }))
    .await;
    assert_eq!(page_size(&document), (612.0, 792.0));
    let bbox = run_box(&runs, "at the origin");
    assert!(
        (bbox.x - 100.0).abs() < 0.01 && (bbox.y - 100.0).abs() < 0.01,
        "{bbox:?}"
    );
    assert_inside(&document);
}
