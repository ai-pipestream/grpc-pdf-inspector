// SPDX-License-Identifier: Apache-2.0

//! A page the walk stopped reading partway is not a blank page.
//!
//! The extraction walk has limits (a million operators per stream, ten
//! thousand form invocations and a million form operators per page, forms
//! five deep) and meets faults (a form that is not a stream, a content
//! stream lopdf stops parsing at a stray token). Each used to end the walk
//! of the page, or of the form, with no word to the caller: the page went
//! out as an ordinary OK `page` event with what had been read, or with
//! nothing, and `needs_ocr` false. The only trace was a `log::warn!` to a
//! logger this binary never installs. Every case below was reproduced on
//! `d8891a1` before the fix: the label was missing and the page was not
//! flagged.

mod common;

use common::Overrun;
use grpc_pdf_inspector::proto::v1 as pb;

const ALL: [(Overrun, pb::WalkTruncation); 6] = [
    (Overrun::PageOperators, pb::WalkTruncation::OperatorCap),
    (
        Overrun::PageStopsParsing,
        pb::WalkTruncation::UnparsedContent,
    ),
    (Overrun::FormInvocations, pb::WalkTruncation::FormBudget),
    (Overrun::FormDepth, pb::WalkTruncation::FormDepth),
    (
        Overrun::FormStopsParsing,
        pb::WalkTruncation::UnparsedContent,
    ),
    (
        Overrun::FormNotAStream,
        pb::WalkTruncation::XobjectUnreadable,
    ),
];

async fn full(harness: &common::Harness, pdf: &[u8]) -> Vec<pb::parse_pdf_response::Event> {
    harness
        .parse(
            pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::Full.into(),
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse")
}

#[tokio::test]
async fn a_page_the_walk_stopped_reading_needs_ocr_and_says_why() {
    let harness = common::start().await;
    for (overrun, why) in ALL {
        let events = full(&harness, &common::truncated_walk_pdf(overrun, "Atlas")).await;
        let pages = common::pages(&events);
        assert_eq!(pages.len(), 2, "{overrun:?}");

        let stopped = pages[0];
        // The label is past the stop: this is the loss being reported.
        assert!(!stopped.markdown.contains("Atlas label"), "{overrun:?}");
        assert!(stopped.needs_ocr, "{overrun:?}");
        assert_eq!(
            stopped.ocr_reason(),
            pb::OcrReason::ContentTruncated,
            "{overrun:?}"
        );
        assert_eq!(
            stopped.truncations().collect::<Vec<_>>(),
            [why],
            "{overrun:?}"
        );

        let whole = pages[1];
        assert!(!whole.needs_ocr, "{overrun:?}");
        assert!(whole.truncations.is_empty(), "{overrun:?}");

        let status = common::status(&events);
        let warnings: Vec<_> = status
            .warnings
            .iter()
            .filter(|warning| warning.code() == pb::ParseWarningCode::ContentTruncated)
            .collect();
        assert_eq!(warnings.len(), 1, "{overrun:?}: {status:?}");
        assert!(
            warnings[0].message.contains("page(s) 1 ("),
            "{overrun:?}: {}",
            warnings[0].message
        );
        assert!(
            status
                .extraction_ocr_reasons
                .iter()
                .any(|page| page.page == 1
                    && page
                        .reasons()
                        .any(|reason| reason == pb::OcrReason::ContentTruncated)),
            "{overrun:?}: {status:?}"
        );
    }
}

#[tokio::test]
async fn what_was_read_before_the_stop_is_kept() {
    let harness = common::start().await;
    for (overrun, _) in ALL {
        let events = full(&harness, &common::truncated_walk_pdf(overrun, "Atlas")).await;
        let markdown = &common::pages(&events)[0].markdown;
        if overrun == Overrun::PageOperators {
            // A stream past the operator cap is not walked at all: the
            // page's own text is gone with the rest, which is why the page
            // must not pass for blank.
            assert!(markdown.is_empty(), "{markdown}");
        } else {
            assert!(
                markdown.contains("Atlas body text on page 1"),
                "{overrun:?}: {markdown}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_read_to_the_end_carries_no_truncation() {
    let events = full(&common::start().await, &common::text_pdf(3, 40, "Plain")).await;
    for page in common::pages(&events) {
        assert!(page.truncations.is_empty());
        assert!(!page.needs_ocr);
    }
    assert!(
        common::status(&events)
            .warnings
            .iter()
            .all(|warning| warning.code() != pb::ParseWarningCode::ContentTruncated)
    );
}
