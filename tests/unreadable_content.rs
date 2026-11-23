// SPDX-License-Identifier: Apache-2.0

//! A page nobody could read is not a page without text.
//!
//! Detection counts text operators, images and paths in each page's
//! content, and used to count a page whose content was missing or would
//! not decode the same as a page with nothing drawn on it. Zero text
//! operators then fell through to "scanned": a born-digital document with
//! damaged streams went out as `PDF_TYPE_SCANNED` at confidence 0.9 with
//! every page sent to OCR, and a document with no pages at all got the same
//! answer about pages that do not exist. Every case below was reproduced
//! on `b3b87d5` before the fix:
//!
//! | input | answer before |
//! |---|---|
//! | no pages | `info{SCANNED, 0.9, page_count 0}`, OK |
//! | every page's content damaged | `info{SCANNED, 0.9}`, all pages `NO_TEXT`, OK, nothing extracted |
//! | one page's content damaged | `TEXT_BASED`, the damaged page not named at all, an empty page event |
//! | every page decoding only partway | `TEXT_BASED 1.0`, no page named, a third of each page missing |

mod common;

use common::Contents;
use grpc_pdf_inspector::proto::v1 as pb;
use tonic::Code;

const MODES: [pb::ProcessMode; 2] = [pb::ProcessMode::DetectOnly, pb::ProcessMode::Full];

fn options(mode: pb::ProcessMode) -> pb::PdfOptions {
    pb::PdfOptions {
        mode: mode.into(),
        ..Default::default()
    }
}

/// Every page that `info` names, with its reasons.
fn named(info: &pb::PdfInfo) -> Vec<(u32, Vec<pb::OcrReason>)> {
    info.ocr_reasons
        .iter()
        .map(|page| (page.page, page.reasons().collect()))
        .collect()
}

#[tokio::test]
async fn a_document_with_no_pages_is_refused() {
    let harness = common::start().await;
    let pdf = common::pageless_pdf();
    for mode in MODES {
        let status = harness
            .parse(&pdf, options(mode))
            .await
            .expect_err("there is nothing on it to classify");
        assert_eq!(status.code(), Code::InvalidArgument, "{mode:?}: {status:?}");
        assert!(status.message().contains("no pages"), "{status:?}");
    }
}

#[tokio::test]
async fn a_document_none_of_whose_pages_can_be_read_is_refused() {
    let harness = common::start().await;
    for damage in [
        Contents::BrokenFlate,
        Contents::MissingObject,
        Contents::NotAStream,
    ] {
        let pdf = common::damaged_content_pdf(3, &[1, 2, 3], damage, "Ledger");
        for mode in MODES {
            let status = harness
                .parse(&pdf, options(mode))
                .await
                .expect_err("no page of it says what the document is");
            assert_eq!(
                status.code(),
                Code::InvalidArgument,
                "{damage:?} {mode:?}: {status:?}"
            );
            assert!(
                status.message().contains("missing or damaged"),
                "{status:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_damaged_page_is_named_unreadable_and_not_scanned() {
    let harness = common::start().await;
    for damage in [
        Contents::BrokenFlate,
        Contents::MissingObject,
        Contents::NotAStream,
        Contents::CorruptedMidway,
        Contents::SecondStreamBroken,
    ] {
        let pdf = common::damaged_content_pdf(3, &[2], damage, "Ledger");

        let events = harness
            .parse(&pdf, options(pb::ProcessMode::DetectOnly))
            .await
            .expect("the document should parse");
        let info = common::info(&events);
        assert_eq!(info.pdf_type(), pb::PdfType::TextBased, "{damage:?}");
        assert_eq!(info.pages_needing_ocr, [2], "{damage:?}");
        assert_eq!(
            named(info),
            [(2, vec![pb::OcrReason::UnreadableContent])],
            "{damage:?}"
        );

        let events = harness
            .parse(&pdf, options(pb::ProcessMode::Full))
            .await
            .expect("the document should parse");
        let pages = common::pages(&events);
        assert_eq!(pages.len(), 3, "{damage:?}");
        for page in &pages {
            let damaged = page.page_no == 2;
            assert_eq!(page.needs_ocr, damaged, "{damage:?} page {}", page.page_no);
            if damaged {
                assert_eq!(page.ocr_reason(), pb::OcrReason::UnreadableContent);
            } else {
                assert!(page.markdown.contains("line 39 of page"), "{damage:?}");
            }
        }
        let status = common::status(&events);
        assert!(
            status
                .extraction_ocr_reasons
                .iter()
                .any(|page| page.page == 2
                    && page.reasons().eq([pb::OcrReason::UnreadableContent])),
            "{damage:?}: {status:?}"
        );
    }
}

#[tokio::test]
async fn pages_that_decode_partway_are_unreadable_and_keep_what_was_read() {
    let harness = common::start().await;
    for damage in [Contents::CorruptedMidway, Contents::SecondStreamBroken] {
        let pdf = common::damaged_content_pdf(3, &[1, 2, 3], damage, "Ledger");
        let events = harness
            .parse(&pdf, options(pb::ProcessMode::Full))
            .await
            .expect("the document should parse");
        let info = common::info(&events);
        // The text that survived is real text, so the document is still
        // what it was written as; every page is named for what it lost.
        assert_eq!(info.pdf_type(), pb::PdfType::TextBased, "{damage:?}");
        assert_eq!(info.pages_needing_ocr, [1, 2, 3], "{damage:?}");
        for (_, reasons) in named(info) {
            assert_eq!(reasons, [pb::OcrReason::UnreadableContent], "{damage:?}");
        }
        for page in common::pages(&events) {
            assert!(page.needs_ocr, "{damage:?} page {}", page.page_no);
            assert_eq!(page.ocr_reason(), pb::OcrReason::UnreadableContent);
            assert!(page.markdown.contains("Ledger page"), "{damage:?}");
        }
    }
    // Partway really is partway: the first lines are there and the last
    // are not.
    let pdf = common::damaged_content_pdf(1, &[1], Contents::CorruptedMidway, "Ledger");
    let events = harness
        .parse(&pdf, options(pb::ProcessMode::Full))
        .await
        .expect("the document should parse");
    let markdown = &common::pages(&events)[0].markdown;
    assert!(markdown.contains("line 0 of page 1"), "{markdown}");
    assert!(!markdown.contains("line 39 of page 1"), "{markdown}");
}

#[tokio::test]
async fn a_sample_of_damaged_pages_does_not_decide_the_document() {
    // Detection samples eight pages of a long document: the first, the
    // last and six spread between. Here those are exactly the damaged
    // ones, and the twenty-two pages it did not sample are whole text.
    let sampled = [1, 5, 9, 13, 17, 21, 25, 30];
    let pdf = common::damaged_content_pdf(30, &sampled, Contents::BrokenFlate, "Ledger");
    let events = common::start()
        .await
        .parse(&pdf, options(pb::ProcessMode::DetectOnly))
        .await
        .expect("the document should parse");
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased);
    assert_eq!(info.pages_needing_ocr, sampled);
    for (_, reasons) in named(info) {
        assert_eq!(reasons, [pb::OcrReason::UnreadableContent]);
    }
}

#[tokio::test]
async fn what_looks_damaged_and_is_not_is_left_alone() {
    let harness = common::start().await;
    // A Flate stream without its checksum decodes whole in every reader.
    let pdf = common::damaged_content_pdf(3, &[1, 2, 3], Contents::NoChecksum, "Ledger");
    let events = harness
        .parse(&pdf, options(pb::ProcessMode::Full))
        .await
        .expect("the document should parse");
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased);
    assert!(info.pages_needing_ocr.is_empty(), "{info:?}");
    for page in common::pages(&events) {
        assert!(!page.needs_ocr);
        assert!(page.markdown.contains("line 39 of page"));
    }

    // A page with no `/Contents` is how a blank page is written.
    let pdf = common::damaged_content_pdf(3, &[2], Contents::Absent, "Ledger");
    let events = harness
        .parse(&pdf, options(pb::ProcessMode::DetectOnly))
        .await
        .expect("the document should parse");
    let info = common::info(&events);
    assert_eq!(info.pdf_type(), pb::PdfType::TextBased);
    assert!(
        named(info)
            .iter()
            .all(|(_, reasons)| !reasons.contains(&pb::OcrReason::UnreadableContent)),
        "{info:?}"
    );
}
