// SPDX-License-Identifier: Apache-2.0

//! Runs drawn wholly outside the visible page box are counted.
//!
//! The walk drops a run or image no reader sees: the facing page of an
//! imposed spread, a producer's tag below the sheet, the text a wrong or
//! deliberately small crop box hides. It used to do so with a debug log
//! and nothing else, so a caller could not tell that the page carried
//! more text than came back.

mod common;

/// One letter page cropped to its lower half, with `on` lines of prose
/// inside the crop box and `off` lines above it.
fn cropped_pdf(on: usize, off: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf\n");
    for line in 0..on {
        let y = 340 - 18 * line as i64;
        content.push_str(&format!(
            "1 0 0 1 72 {y} Tm (Visible line {line} of the cropped page) Tj\n"
        ));
    }
    for line in 0..off {
        let y = 740 - 18 * line as i64;
        content.push_str(&format!(
            "1 0 0 1 72 {y} Tm (Hidden line {line} above the crop box) Tj\n"
        ));
    }
    content.push_str("ET");
    common::framed_pdf_with(common::FramedPage {
        content,
        crop_box: Some([0, 0, 612, 396]),
        ..Default::default()
    })
}

#[tokio::test]
async fn runs_outside_the_crop_box_are_counted_on_the_page_and_the_trailer() {
    let harness = common::start().await;
    let events = harness.parse_ok(&cropped_pdf(6, 4)).await;

    let pages = common::pages(&events);
    let page = pages.first().expect("one page");
    assert!(
        page.markdown.contains("Visible line 0"),
        "the visible text comes back: {page:?}"
    );
    assert!(
        !page.markdown.contains("Hidden line"),
        "no reader sees the cropped-off text, so it stays out of the markdown: {page:?}"
    );
    assert_eq!(page.runs_off_page, 4, "and is counted: {page:?}");

    let status = common::status(&events);
    assert_eq!(status.runs_off_page, 4, "{status:?}");
}

#[tokio::test]
async fn a_page_with_nothing_off_its_box_counts_nothing() {
    let harness = common::start().await;
    let events = harness.parse_ok(&cropped_pdf(6, 0)).await;

    let pages = common::pages(&events);
    assert_eq!(pages[0].runs_off_page, 0, "{:?}", pages[0]);
    assert_eq!(common::status(&events).runs_off_page, 0);
}

#[tokio::test]
async fn a_paragraph_of_off_box_text_is_counted_whichever_clip_takes_it() {
    // Ten or more wordy runs off the box are clipped by the walk's first
    // pass, which takes the neighbouring page of a spread; fewer are left
    // to the per-run hull check. Both are counted.
    let harness = common::start().await;
    let events = harness.parse_ok(&cropped_pdf(6, 12)).await;

    let pages = common::pages(&events);
    assert!(!pages[0].markdown.contains("Hidden line"), "{:?}", pages[0]);
    assert_eq!(pages[0].runs_off_page, 12, "{:?}", pages[0]);
    assert_eq!(common::status(&events).runs_off_page, 12);
}
