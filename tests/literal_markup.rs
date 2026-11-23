// SPDX-License-Identifier: Apache-2.0

//! Characters the page drew are text, whatever markdown would make of them.
//!
//! The renderer copies page text into its markdown unescaped and writes its
//! own markup around it, and the Document is folded out of that markdown.
//! A reader that cannot tell the two apart takes drawn characters out of
//! the text: `x*y and z*w` lost both asterisks and gained an italic run
//! over `y and z`, a heading ending in `C#` lost its hash, a paragraph
//! opening with `# of units` became a section header, and a paragraph
//! opening with three backticks swallowed the rest of the page into one
//! code block. All of it arrived in an ordinary OK Document.
//!
//! The renderer says which characters it copied
//! (`PageMarkdown.literal_offsets`), and the fold reads the markdown on
//! that word. These tests hold both halves on a real parse.

mod common;

use grpc_pdf_inspector::document_fold::DocumentFold;
use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The heading of the page below.
const HEADING: &str = "Programming in C#";

/// The paragraphs of the page below, each a list of lines. Three lines
/// each, so that the page's ordinary leading outnumbers the gaps between
/// its paragraphs and the renderer can tell the two apart.
const PARAGRAPHS: [&[&str]; 5] = [
    &[
        "The area of the plate is x*y and the volume of the tank is z*w",
        "in every case that was measured for the report of this year,",
        "and the two figures are given in the units the plant works in.",
    ],
    &[
        "# of units sold was five in total for the whole of the year,",
        "which is fewer than were sold in the year that came before it,",
        "and the count is the one the warehouse keeps for its own books.",
    ],
    &[
        "``` opens a fenced block in that notation and the same mark",
        "closes it again, so a page that prints one prints it as text,",
        "and the words around it are prose like any other on the page.",
    ],
    &[
        "Press <u>enter</u> to continue, as the manual of the terminal",
        "says on the page that describes what its keyboard can send,",
        "where the tag is printed for the reader to type as it stands.",
    ],
    &[
        "The last paragraph of the page is ordinary prose that draws",
        "none of those characters and must still arrive as a paragraph",
        "of its own after everything that was set above it on the page.",
    ],
];

/// Parse the page and return its events.
async fn parse() -> Vec<pb::parse_pdf_response::Event> {
    let harness = common::start().await;
    harness
        .parse(
            &common::literal_markup_pdf(HEADING, &PARAGRAPHS),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse")
}

/// Every text item of a Document as (label, text, spans).
fn texts(document: &doc::Document) -> Vec<(&'static str, &str, &[doc::InlineSpan])> {
    document
        .texts
        .iter()
        .map(|item| match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::SectionHeader(header) => {
                let base = header.base.as_ref().expect("a base");
                ("heading", base.text.as_str(), base.spans.as_slice())
            }
            doc::base_text_item::Item::Text(text) => {
                let base = text.base.as_ref().expect("a base");
                ("text", base.text.as_str(), base.spans.as_slice())
            }
            doc::base_text_item::Item::Code(code) => ("code", code.text.as_str(), &[][..]),
            doc::base_text_item::Item::ListItem(item) => {
                let base = item.base.as_ref().expect("a base");
                ("list item", base.text.as_str(), base.spans.as_slice())
            }
            _ => ("other", "", &[][..]),
        })
        .collect()
}

/// Whether any span says its characters are emphasised.
fn emphasised(spans: &[doc::InlineSpan]) -> bool {
    spans.iter().any(|span| {
        span.formatting.as_ref().is_some_and(|formatting| {
            formatting.bold || formatting.italic || formatting.underline || formatting.strikethrough
        })
    })
}

#[tokio::test]
async fn the_document_keeps_every_character_the_page_drew() {
    let events = parse().await;
    let document = common::documents(&events)[0];
    let texts = texts(document);

    // One heading and five paragraphs, in the order the page has them, and
    // nothing retyped: no second heading, no code block, no list.
    let shape: Vec<&str> = texts.iter().map(|(label, _, _)| *label).collect();
    assert_eq!(
        shape,
        ["heading", "text", "text", "text", "text", "text"],
        "{texts:#?}"
    );

    assert_eq!(texts[0].1, HEADING, "the heading keeps its hash");
    for ((_, text, spans), lines) in texts[1..].iter().zip(PARAGRAPHS) {
        assert_eq!(*text, lines.join(" "), "a paragraph is the page's own text");
        assert!(
            !emphasised(spans),
            "nothing on the page is emphasised, but {text:?} carries {spans:?}"
        );
    }
}

#[tokio::test]
async fn the_page_event_names_the_drawn_characters_and_leaves_its_markdown_alone() {
    let events = parse().await;
    let page = common::pages(&events)[0];

    // The markdown is what it always was: page text copied as it is.
    assert!(page.markdown.starts_with("# Programming in C#\n"));
    assert!(
        page.markdown
            .contains("x*y and the volume of the tank is z*w")
    );
    assert!(page.markdown.contains("\n# of units sold"));
    assert!(page.markdown.contains("\n``` opens a fenced block"));
    assert!(page.markdown.contains("Press <u>enter</u> to continue"));

    // And beside it, which characters are the page's: the hash of `C#`,
    // two asterisks, the leading hash, three backticks, and the `<` of
    // each tag. The heading's own marker at offset 0 is not among them.
    let drawn: String = page
        .literal_offsets
        .iter()
        .map(|offset| char::from(page.markdown.as_bytes()[*offset as usize]))
        .collect();
    assert_eq!(drawn, "#**#```<<", "{:?}", page.literal_offsets);
    assert!(!page.literal_offsets.contains(&0));
    assert!(
        page.literal_offsets.is_sorted(),
        "{:?}",
        page.literal_offsets
    );
}

#[tokio::test]
async fn a_reader_without_the_offsets_shows_what_they_are_for() {
    // The same page event with the renderer's account taken off it reads
    // the way every page did before the field existed. This is the loss
    // the offsets prevent, kept here so that the test above cannot pass
    // by the fold ignoring markup altogether.
    let events = parse().await;
    let mut fold = DocumentFold::new();
    for event in &events {
        match event {
            pb::parse_pdf_response::Event::Page(page) => {
                fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    literal_offsets: Vec::new(),
                    ..page.clone()
                }));
            }
            pb::parse_pdf_response::Event::Document(_) => {}
            other => fold.consume(other),
        }
    }
    let document = fold.take();
    let texts = texts(&document);
    let shape: Vec<&str> = texts.iter().map(|(label, _, _)| *label).collect();
    // The heading lost its hash, the first paragraph its asterisks, the
    // second became a heading, and the fence took the two paragraphs
    // under it into one code block.
    assert_eq!(shape, ["heading", "text", "heading", "code"], "{texts:#?}");
    assert_eq!(texts[0].1, "Programming in C");
    assert!(texts[1].1.contains("is xy and"), "{:?}", texts[1].1);
    assert!(emphasised(texts[1].2), "{:?}", texts[1].2);
    assert!(texts[2].1.starts_with("of units sold"), "{:?}", texts[2].1);
    assert!(
        texts[3].1.contains("The last paragraph"),
        "{:?}",
        texts[3].1
    );
}

#[tokio::test]
async fn emphasis_the_renderer_wrote_is_still_emphasis() {
    // The styled fixture has a bold run and draws none of the four
    // characters, so its page names no offsets and its bold still lands
    // on a span over the phrase.
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
    let page = common::pages(&events)[0];
    assert!(page.markdown.contains("**a phrase set in a bold face,**"));
    assert!(
        page.literal_offsets.is_empty(),
        "{:?}",
        page.literal_offsets
    );

    let document = common::documents(&events)[0];
    let texts = texts(document);
    let (_, text, spans) = texts
        .iter()
        .find(|(_, text, _)| text.contains("bold face"))
        .expect("the paragraph");
    assert!(!text.contains('*'), "the markers came off: {text:?}");
    assert!(emphasised(spans), "and became a span: {spans:?}");
}
