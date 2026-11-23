// SPDX-License-Identifier: Apache-2.0

//! The whole-document pipeline's empty answers, and why they are empty.
//!
//! ANALYZE, and FULL with a password, run the library's whole-document
//! pipeline. It returns no markdown when extraction failed on a mixed
//! document, when a mixed document's text is garbage (and it becomes
//! "scanned"), when a text document's text is garbage, and when every page
//! uses glyph-id fonts. The service used to send no page, report the
//! markdown as "delivered as a single page event", leave out the pages
//! the pipeline wanted OCR for, and end OK as if the document were empty.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

/// The status warnings with the given code.
fn warnings_with(status: &pb::ParseStatus, code: pb::ParseWarningCode) -> Vec<&pb::ParseWarning> {
    status
        .warnings
        .iter()
        .filter(|warning| warning.code == code as i32)
        .collect()
}

#[tokio::test]
async fn a_password_parse_that_withholds_garbage_says_why_and_which_pages_need_ocr() {
    let events = common::start()
        .await
        .parse(
            &common::soup_only_pdf(),
            pb::PdfOptions {
                // The file is not encrypted; any password takes the
                // whole-document path.
                password: "secret".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect("the parse succeeds");

    assert!(
        common::pages(&events).is_empty(),
        "the control: no text came back"
    );
    let status = common::status(&events);
    assert_eq!(status.pages_extracted, 0);

    assert_eq!(
        status.text_withheld,
        pb::TextWithheld::GarbageText as i32,
        "the trailer says why the document came back empty: {status:?}"
    );
    assert_eq!(
        warnings_with(status, pb::ParseWarningCode::TextWithheld).len(),
        1,
        "{:?}",
        status.warnings
    );
    assert_eq!(
        status.pages_needing_ocr,
        [1],
        "the pipeline flagged every page for OCR, and the trailer says so"
    );

    let fallback = warnings_with(status, pb::ParseWarningCode::PasswordFallback);
    assert_eq!(fallback.len(), 1);
    assert!(
        !fallback[0]
            .message
            .contains("delivered as a single page event"),
        "nothing was delivered: {:?}",
        fallback[0].message
    );
}

#[tokio::test]
async fn a_password_parse_of_ordinary_text_withholds_nothing() {
    let events = common::start()
        .await
        .parse(
            &common::text_pdf(2, 60, "PLAINMARK"),
            pb::PdfOptions {
                password: "secret".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect("the parse succeeds");

    assert_eq!(common::pages(&events).len(), 1);
    let status = common::status(&events);
    assert_eq!(status.text_withheld, pb::TextWithheld::Unspecified as i32);
    assert!(warnings_with(status, pb::ParseWarningCode::TextWithheld).is_empty());
    assert!(status.pages_needing_ocr.is_empty(), "{status:?}");
}

#[tokio::test]
async fn a_password_parse_names_pages_flagged_for_ocr_without_a_reason_code() {
    // A sparse text document: one short word on its page. The pipeline
    // keeps the text but flags every page for OCR without a reason code,
    // so before the trailer carried `pages_needing_ocr` nothing said so.
    let events = common::start()
        .await
        .parse(
            &common::framed_pdf("BT /F1 12 Tf 72 700 Td (Hello) Tj ET", None, None),
            pb::PdfOptions {
                password: "secret".to_owned(),
                ..Default::default()
            },
        )
        .await
        .expect("the parse succeeds");
    assert_eq!(common::pages(&events).len(), 1, "the text itself is kept");
    let status = common::status(&events);
    assert_eq!(status.text_withheld, pb::TextWithheld::Unspecified as i32);
    assert!(
        status.extraction_ocr_reasons.is_empty(),
        "the control: the sparse verdict carries no reason code: {status:?}"
    );
    assert_eq!(status.pages_needing_ocr, [1], "{status:?}");
}
