// SPDX-License-Identifier: Apache-2.0

//! Which pages a call gets, when it names them.
//!
//! A repeated page used to come back twice, the second time as an empty
//! page event that was folded into the Document a second time. A page past
//! the end was dropped without a word, and a selection of nothing but such
//! pages succeeded with no pages at all, which a caller routing on the
//! result reads as an empty document. And the only way to ask for a span
//! was to spell out every page of it.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;
use tonic::Code;

/// The page numbers of the `page` events, in order.
fn page_numbers(events: &[pb::parse_pdf_response::Event]) -> Vec<u32> {
    common::pages(events)
        .iter()
        .map(|page| page.page_no)
        .collect()
}

/// The warning codes on the trailer.
fn warning_codes(events: &[pb::parse_pdf_response::Event]) -> Vec<pb::ParseWarningCode> {
    common::status(events)
        .warnings
        .iter()
        .map(|warning| warning.code())
        .collect()
}

#[tokio::test]
async fn a_page_listed_twice_is_extracted_once() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(3, 20, "twice-marker"),
            pb::PdfOptions {
                pages: vec![2, 2, 1, 2],
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert_eq!(
        page_numbers(&events),
        [2, 1],
        "first-listed order, once each"
    );
    assert_eq!(common::status(&events).pages_extracted, 2);
    assert!(
        warning_codes(&events).is_empty(),
        "a repeat is not a mistake"
    );

    let document = common::documents(&events)[0];
    let page_two = common::placed(document)
        .into_iter()
        .filter(|item| item.text.contains("twice-marker page 2"))
        .count();
    assert_eq!(page_two, 1, "page 2 is folded into the Document once");
}

#[tokio::test]
async fn listed_pages_past_the_end_are_left_out_with_a_warning() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(2, 20, "past-the-end-marker"),
            pb::PdfOptions {
                pages: vec![1, 99, 100],
                ..Default::default()
            },
        )
        .await
        .expect("the pages that exist are extracted");

    assert_eq!(page_numbers(&events), [1]);
    assert_eq!(
        warning_codes(&events),
        [pb::ParseWarningCode::PagesOutOfRange]
    );
    let warning = &common::status(&events).warnings[0];
    assert!(warning.message.contains("page 99"), "{warning:?}");
}

#[tokio::test]
async fn a_selection_with_no_page_in_the_document_is_a_caller_error() {
    let harness = common::start().await;
    let pdf = common::text_pdf(2, 20, "nothing-selected-marker");
    for (mode, options) in [
        (
            "a list",
            pb::PdfOptions {
                pages: vec![7, 9],
                ..Default::default()
            },
        ),
        (
            "a list, detection only",
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                pages: vec![7],
                ..Default::default()
            },
        ),
        (
            "a span",
            pb::PdfOptions {
                first_page: Some(5),
                ..Default::default()
            },
        ),
    ] {
        let status = harness
            .parse(&pdf, options)
            .await
            .expect_err("an empty success would read as an empty document");
        assert_eq!(status.code(), Code::InvalidArgument, "{mode}: {status:?}");
        assert!(status.message().contains("2 page"), "{mode}: {status:?}");
    }
}

#[tokio::test]
async fn a_span_selects_its_pages_and_is_clamped_to_the_document() {
    let harness = common::start().await;
    let pdf = common::text_pdf(5, 20, "span-marker");

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                first_page: Some(4),
                ..Default::default()
            },
        )
        .await
        .expect("from page 4 to the end");
    assert_eq!(page_numbers(&events), [4, 5]);

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                first_page: Some(2),
                last_page: Some(u32::MAX),
                ..Default::default()
            },
        )
        .await
        .expect("a span past the end is clamped to it");
    assert_eq!(page_numbers(&events), [2, 3, 4, 5]);
    assert!(
        warning_codes(&events).is_empty(),
        "clamping is what a span means"
    );

    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                last_page: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("up to page 2");
    assert_eq!(page_numbers(&events), [1, 2]);
}

#[tokio::test]
async fn an_ambiguous_or_backwards_selection_is_a_caller_error() {
    let harness = common::start().await;
    let pdf = common::text_pdf(5, 20, "ambiguous-marker");
    for (what, options) in [
        (
            "a list and a span",
            pb::PdfOptions {
                pages: vec![1],
                first_page: Some(1),
                ..Default::default()
            },
        ),
        (
            "a backwards span",
            pb::PdfOptions {
                first_page: Some(3),
                last_page: Some(2),
                ..Default::default()
            },
        ),
        (
            "a span from page 0",
            pb::PdfOptions {
                first_page: Some(0),
                ..Default::default()
            },
        ),
    ] {
        let status = harness
            .parse(&pdf, options)
            .await
            .expect_err("refused before the document is read");
        assert_eq!(status.code(), Code::InvalidArgument, "{what}: {status:?}");
    }
}

#[tokio::test]
async fn the_trailer_answers_only_for_the_selected_pages() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::mixed_text_and_scan_pdf(20, &[16, 17]),
            pb::PdfOptions {
                pages: vec![1, 16],
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert_eq!(
        common::info(&events).pages_needing_ocr,
        [16, 17],
        "`info` is about the whole document"
    );
    let flagged: Vec<u32> = common::status(&events)
        .extraction_ocr_reasons
        .iter()
        .map(|reasons| reasons.page)
        .collect();
    assert_eq!(flagged, [16], "the trailer is about the pages read");
}
