// SPDX-License-Identifier: Apache-2.0

//! What the markdown leaves out is counted on every call.
//!
//! A plain FULL call (no `report_furniture`, no Document) renders each page
//! with the renderer's own header, footer and page-number strippers on, and
//! the renderer does not always emit every run it read. Nothing on the wire
//! said so: `furniture` and `dropped` are filled only when the report is
//! asked for, and the trailer had no count.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

/// On a two-column page the renderer's own header stripper takes the
/// running title out, and a plain call is told nothing about it.
#[tokio::test]
async fn a_plain_call_counts_the_runs_its_markdown_does_not_carry() {
    const HEAD: &str = "Proceedings Running Title";
    let events = common::start()
        .await
        .parse(&common::two_column_pdf(2, HEAD), pb::PdfOptions::default())
        .await
        .expect("the fixture should parse");

    let pages = common::pages(&events);
    assert_eq!(pages.len(), 2);
    for page in &pages {
        assert!(
            !page.markdown.contains(HEAD),
            "the control: the renderer stripped the head on page {}",
            page.page_no
        );
        assert!(
            page.furniture.is_empty() && page.dropped.is_empty(),
            "the control: no report was asked for"
        );
        assert!(
            page.runs_not_in_markdown >= 1,
            "page {} drew a head its markdown does not carry, and nothing said so",
            page.page_no
        );
    }
    let total: u32 = pages.iter().map(|page| page.runs_not_in_markdown).sum();
    assert_eq!(common::status(&events).runs_not_in_markdown, total);
}

#[tokio::test]
async fn the_count_agrees_with_the_report_when_the_report_is_asked_for() {
    let events = common::start()
        .await
        .parse(
            &common::furniture_pdf(3, "A Running Head"),
            pb::PdfOptions {
                report_furniture: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");
    for page in common::pages(&events) {
        assert_eq!(
            page.runs_not_in_markdown as usize,
            page.furniture.len() + page.dropped.len(),
            "page {}",
            page.page_no
        );
        assert!(page.runs_not_in_markdown >= 2, "the head and the folio");
    }
}

#[tokio::test]
async fn a_page_whose_every_run_reached_the_markdown_counts_none() {
    let events = common::start()
        .await
        .parse(
            &common::text_pdf(1, 40, "WHOLEMARK"),
            pb::PdfOptions::default(),
        )
        .await
        .expect("the fixture should parse");
    assert_eq!(common::pages(&events)[0].runs_not_in_markdown, 0);
    assert_eq!(common::status(&events).runs_not_in_markdown, 0);
}
