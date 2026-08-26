// SPDX-License-Identifier: Apache-2.0

//! The garble score: how far a page's letters sit from where a language
//! puts them.
//!
//! A broken ToUnicode CMap maps every character through a constant offset,
//! so the extracted text is printable ASCII with word-like token lengths
//! and no replacement character anywhere. Counting replacement runs says
//! nothing about it. What gives it away is the letter histogram, which is a
//! permutation of a natural one: the right shape, the wrong positions.
//!
//! That correlation used to be computed inside the parser crate and thrown
//! away, and `PageQuality.garble_score` had no source. It has one now.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

#[tokio::test]
async fn the_score_separates_ciphered_text_from_the_same_prose_uncyphered() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::garbled_pdf()).await;

    let pages = common::pages(&events);
    assert_eq!(pages.len(), 2);
    let clean = pages[0].garble_score.expect("both pages carry letters");
    let garbled = pages[1].garble_score.expect("both pages carry letters");

    assert!(
        clean < 0.10,
        "ordinary prose sits close to where a language puts its letters: {clean}"
    );
    assert!(
        garbled > 0.25,
        "the same prose, every letter substituted, does not: {garbled}"
    );
    assert!(
        garbled > clean * 4.0,
        "and the two are not close: clean {clean}, garbled {garbled}"
    );

    assert!(
        !pages[1].markdown.is_empty(),
        "the text is still delivered; the score is a warning, not a filter"
    );
    assert_eq!(
        pages[1].replacement_runs, 0,
        "and nothing failed to decode, which is why a count could not catch this"
    );
}

#[tokio::test]
async fn the_ciphered_page_is_the_one_routed_to_ocr() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::garbled_pdf()).await;

    let pages = common::pages(&events);
    assert!(!pages[0].needs_ocr, "clean prose is usable");
    assert!(pages[1].needs_ocr, "ciphered text is not");
    assert_eq!(pages[1].ocr_reason, pb::OcrReason::SuspectedGarbled as i32);

    let status = common::status(&events);
    assert!(status.has_encoding_issues);
    let reasons: Vec<u32> = status
        .extraction_ocr_reasons
        .iter()
        .map(|page| page.page)
        .collect();
    assert_eq!(reasons, [2], "and the trailer names the page, not the file");
}

#[tokio::test]
async fn a_page_with_too_few_letters_to_measure_reports_no_score() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::text_pdf(1, 5, "short")).await;

    let page = common::pages(&events)[0];
    assert!(
        !page.markdown.is_empty(),
        "the page has text, just not enough of it: {:?}",
        page.markdown
    );
    assert_eq!(
        page.garble_score, None,
        "a statistic over forty letters is noise, and saying 0.0 would be a claim"
    );
}

#[tokio::test]
async fn the_document_carries_the_score_on_the_pages_quality() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::garbled_pdf(),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    let quality = |page_no: i32| {
        document.pages[&page_no]
            .quality
            .as_ref()
            .unwrap_or_else(|| panic!("page {page_no} was measured"))
    };

    let clean = quality(1).garble_score.expect("measured");
    let garbled = quality(2).garble_score.expect("measured");
    assert!(garbled > clean, "clean {clean}, garbled {garbled}");
    assert_eq!(quality(2).ocr_recommended, Some(true));
    assert_eq!(
        quality(1).ocr_recommended,
        None,
        "a measurement is not a verdict"
    );
}
