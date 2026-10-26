// SPDX-License-Identifier: Apache-2.0

//! Characters the page carries and draws nothing for.
//!
//! A zero-width space marks where a word may break without showing a space.
//! The extraction used to strip it, and when the producer had kerned a gap
//! around each mark the layout then saw a word gap there, so "approved"
//! written with seven marks came back as "a p p r o v e d" with no trace of
//! the marks in the markdown or in any Document item. The other PDF text
//! extractors keep them, and so does this one now.

mod common;

use common::ZeroWidthLayout;
use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// "approved" with a U+200B between each pair of letters.
const MARKED: &str = "a\u{200B}p\u{200B}p\u{200B}r\u{200B}o\u{200B}v\u{200B}e\u{200B}d";

fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
    match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref().expect("a base"),
        other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
    }
}

async fn parse(layout: ZeroWidthLayout) -> Vec<pb::parse_pdf_response::Event> {
    common::start()
        .await
        .parse(
            &common::zero_width_pdf(layout),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture parses")
}

fn assert_marks_survive(events: &[pb::parse_pdf_response::Event], layout: ZeroWidthLayout) {
    let markdown = &common::pages(events)[0].markdown;
    assert!(
        markdown.contains(MARKED),
        "{layout:?}: the markdown keeps every mark and adds no space: {markdown:?}"
    );
    assert!(
        markdown.contains("Plain approved line."),
        "{layout:?}: the unmarked line is untouched: {markdown:?}"
    );

    let document = common::documents(events)[0];
    let item = document
        .texts
        .iter()
        .map(base_of)
        .find(|base| base.orig.contains("contract"))
        .unwrap_or_else(|| panic!("{layout:?}: an item holds the first line"));
    assert!(
        item.orig.contains(MARKED),
        "{layout:?}: orig is the page's own characters: {:?}",
        item.orig
    );
    assert!(
        item.text.contains(MARKED),
        "{layout:?}: text applies no policy that drops them: {:?}",
        item.text
    );
}

#[tokio::test]
async fn zero_width_spaces_written_in_one_run_survive_extraction() {
    let events = parse(ZeroWidthLayout::Packed).await;
    assert_marks_survive(&events, ZeroWidthLayout::Packed);
}

#[tokio::test]
async fn a_kerned_gap_beside_a_zero_width_space_is_not_a_word_space() {
    let events = parse(ZeroWidthLayout::Kerned).await;
    assert_marks_survive(&events, ZeroWidthLayout::Kerned);
}
