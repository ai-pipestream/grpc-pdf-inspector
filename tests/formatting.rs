// SPDX-License-Identifier: Apache-2.0

//! Emphasis as a property of characters, not of paragraphs.
//!
//! Bold, italic, underline and strikeout are detected per run and rendered
//! into `**`, `*`, `<u>` and `<s>`; the fold kept those markers as literal
//! characters inside a paragraph's text, so a partially bold paragraph was
//! unrecoverable and `Formatting` was permanently empty. The runs are on
//! this wire now, so the spans can say which characters they cover.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The base of any plain text item.
fn text_bases(document: &doc::Document) -> Vec<&doc::TextItemBase> {
    document
        .texts
        .iter()
        .filter_map(|item| match item.item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref(),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_runs_report_the_face_they_are_set_in() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::styled_pdf(),
            pb::PdfOptions {
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let runs = &common::spans(&events)[0].spans;
    let bold: Vec<(&str, bool)> = runs
        .iter()
        .map(|run| (run.font_family.as_str(), run.bold))
        .collect();
    assert_eq!(
        bold,
        [
            ("Helvetica", false),
            ("Helvetica-Bold", true),
            ("Helvetica", false),
        ]
    );
}

#[tokio::test]
async fn a_partially_bold_paragraph_says_which_characters_are_bold() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::styled_pdf(),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    let base = text_bases(document)
        .into_iter()
        .find(|base| base.text.contains("bold face"))
        .expect("the three lines joined into one paragraph");

    assert_eq!(base.spans.len(), 1, "one bold run: {:?}", base.spans);
    let span = &base.spans[0];

    let formatting = span.formatting.as_ref().expect("a run says how it is set");
    assert!(formatting.bold);
    assert!(!formatting.italic);
    assert!(!formatting.underline);
    assert!(!formatting.strikethrough);
    assert_eq!(span.font_family.as_deref(), Some("Helvetica-Bold"));
    assert_eq!(span.font_size_pt, Some(11.0));
    assert!(span.hyperlink.is_none(), "nothing here is a link");

    // The range covers the emphasised phrase and only it.
    let range = span.range.as_ref().expect("a run names its characters");
    let characters: Vec<char> = base.text.chars().collect();
    let start = usize::try_from(range.start).expect("a non-negative start");
    let end = usize::try_from(range.end).expect("a non-negative end");
    let covered: String = characters[start..end].iter().collect();
    assert!(
        covered.contains("a phrase set in a bold face"),
        "the run covers the emphasis: {covered:?}"
    );
    assert!(
        !covered.contains("Ordinary prose leading"),
        "and not the prose around it: {covered:?}"
    );
    assert!(
        !covered.contains("after it again"),
        "on either side: {covered:?}"
    );
}

#[tokio::test]
async fn plain_prose_carries_no_spans_at_all() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(2, 30, "plain-marker"),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    let document = common::documents(&events)[0];
    for base in text_bases(document) {
        assert!(
            base.spans.is_empty(),
            "no span means the item's own default: {:?}",
            base.text
        );
    }
}
