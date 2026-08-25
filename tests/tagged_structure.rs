// SPDX-License-Identifier: Apache-2.0

//! The roles a tagged document gives its own content.
//!
//! The markdown pipeline decides how many `#` characters to print by
//! comparing type sizes, and a tagged document does not have to be guessed
//! at: it says which run is a third-level heading. The fixture here draws
//! its heading at body size precisely so that the guess cannot reach that
//! depth and only the tagging can.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// The base of any text item the fold makes.
fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
    match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
        doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref().expect("a base"),
        other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
    }
}

#[tokio::test]
async fn structure_stays_off_the_wire_unless_it_is_asked_for() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::tagged_pdf()).await;
    assert_eq!(common::shape(&events), ["info", "page", "status"]);
}

#[tokio::test]
async fn the_authored_roles_reach_the_wire_with_their_join_key() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::tagged_pdf(),
            pb::PdfOptions {
                emit_structure: true,
                emit_spans: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    assert_eq!(
        common::shape(&events),
        ["info", "structure", "spans", "page", "status"],
        "the roles arrive before the runs they describe"
    );

    let structure = common::structure(&events);
    assert_eq!(structure[0].page_no, 1);
    let roles: Vec<(i64, &str)> = structure[0]
        .elements
        .iter()
        .map(|element| (element.mcid, element.role_raw.as_str()))
        .collect();
    assert_eq!(roles, [(0, "H3"), (1, "P")]);
    assert_eq!(
        structure[0].elements[0].role,
        pb::StructureRole::H3 as i32,
        "a standard role is typed as well as named"
    );

    // The join key is on both sides of the join: every run names the
    // marked-content region it was drawn inside, and every region the
    // structure tree named is one of them.
    let spans = common::spans(&events);
    let mut tagged: Vec<i64> = spans[0].spans.iter().filter_map(|span| span.mcid).collect();
    tagged.dedup();
    assert_eq!(tagged, [0, 1], "in the order the page draws them");
}

#[tokio::test]
async fn an_authored_heading_beats_the_markdown_guess() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::tagged_pdf(),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    // Whatever the markdown pipeline made of the page, it cannot have made
    // a third-level heading: it infers depth by comparing type sizes and
    // every line here is the same size.
    let markdown = &common::pages(&events)[0].markdown;
    assert!(
        !markdown.contains("### "),
        "the guess could not reach level 3: {markdown:?}"
    );

    let document = common::documents(&events)[0];
    let heading = document
        .texts
        .iter()
        .map(base_of)
        .find(|base| base.text.contains("An Authored Heading"))
        .expect("the heading line became an item");
    assert_eq!(
        heading.label,
        doc::DocItemLabel::SectionHeader as i32,
        "the document said it was a heading"
    );
    assert_eq!(
        heading.style_name.as_deref(),
        Some("H3"),
        "and the source's own word for it is kept"
    );

    match document.texts[0].item.as_ref() {
        Some(doc::base_text_item::Item::SectionHeader(header)) => {
            assert_eq!(header.level, 3, "at the depth the document declared");
        }
        other => panic!("the first item is a section header: {other:?}"),
    }

    let prose = document
        .texts
        .iter()
        .map(base_of)
        .find(|base| base.text.contains("Ordinary body prose"))
        .expect("the prose line became an item");
    assert_eq!(prose.label, doc::DocItemLabel::Paragraph as i32);
    assert_eq!(prose.style_name.as_deref(), Some("P"));
    assert_eq!(
        prose.parent.as_ref().expect("a parent").r#ref,
        "#/texts/0",
        "the prose hangs off the heading the tagging found"
    );
}

#[tokio::test]
async fn an_untagged_document_claims_no_roles() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(1, 20, "untagged-marker"),
            pb::PdfOptions {
                emit_structure: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    assert!(
        common::structure(&events).is_empty(),
        "an untagged document has no roles, and says so by having none"
    );
    let document = common::documents(&events)[0];
    for base in document.texts.iter().map(base_of) {
        assert!(base.style_name.is_none(), "{:?}", base.text);
    }
}
