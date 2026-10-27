// SPDX-License-Identifier: Apache-2.0

//! Small documents built to make a reader pay before it has read anything.
//!
//! Every fixture here is a few hundred bytes. What each one attacks is a
//! number the file states about itself that the server used to act on
//! before checking it: a page count the tree does not hold, a page
//! numbering that starts near the top of its range. The assertions bound
//! time and output, because the failure mode is a process that is gone or
//! a reply nobody can hold, not a wrong answer.

mod common;

use std::time::{Duration, Instant};

use grpc_pdf_inspector::proto::v1 as pb;

/// Longer than any of these documents takes to read, and shorter than
/// what a stalled reader would cost.
const BUDGET: Duration = Duration::from_secs(20);

#[tokio::test]
async fn a_page_tree_node_lying_about_its_count_costs_only_the_pages_it_holds() {
    let harness = common::start().await;
    // A count the allocator refuses, and one the arithmetic refuses.
    for count in [1i64 << 40, 1i64 << 60] {
        let pdf = common::count_lie_pdf(count);
        assert!(pdf.len() < 1024, "{} bytes", pdf.len());

        let started = Instant::now();
        let events = harness
            .parse(&pdf, pb::PdfOptions::default())
            .await
            .unwrap_or_else(|status| panic!("count {count}: {status:?}"));
        assert!(
            started.elapsed() < BUDGET,
            "count {count}: {:?}",
            started.elapsed()
        );

        assert_eq!(common::info(&events).page_count, 1, "count {count}");
        assert_eq!(common::shape(&events), ["info", "page", "status"]);
        assert!(
            common::pages(&events)[0]
                .markdown
                .contains("count-lie marker"),
            "{events:?}"
        );
    }

    // The server is still there for the next caller.
    let events = harness
        .parse_ok(&common::text_pdf(2, 20, "aftermath"))
        .await;
    assert_eq!(common::pages(&events).len(), 2);
}

#[tokio::test]
async fn a_page_label_range_starting_near_the_top_stays_a_label() {
    let harness = common::start().await;
    for style in ["a", "A", "r", "R"] {
        let pdf = common::labelled_pdf(2, style, u32::MAX - 1);
        assert!(pdf.len() < 2048, "{} bytes", pdf.len());

        let started = Instant::now();
        let events = harness
            .parse(
                &pdf,
                pb::PdfOptions {
                    mode: pb::ProcessMode::DetectOnly.into(),
                    emit_metadata: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|status| panic!("style {style}: {status:?}"));
        assert!(
            started.elapsed() < BUDGET,
            "style {style}: {:?}",
            started.elapsed()
        );

        let pages = &common::metadata(&events).pages;
        assert_eq!(pages.len(), 2, "style {style}");
        for page in pages {
            assert!(
                !page.label.is_empty() && page.label.len() <= 64,
                "style {style}: page {} has a label of {} bytes",
                page.page_no,
                page.label.len()
            );
        }
    }

    // A label that fits its style is still rendered in it.
    let pdf = common::labelled_pdf(1, "a", 27);
    let events = harness
        .parse(
            &pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                emit_metadata: true,
                ..Default::default()
            },
        )
        .await
        .expect("a sane label range parses");
    assert_eq!(common::metadata(&events).pages[0].label, "aa");
}
