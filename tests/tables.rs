// SPDX-License-Identifier: Apache-2.0

//! Tables as grids.
//!
//! The detector measures column and row boundaries in page points and knows
//! whether it found data or a table of contents. Markdown can hold none of
//! that: it flattens a grid into pipe characters, and the fold used to keep
//! the pipe characters as the text of a paragraph. A consumer wanting a
//! table had to parse GFM back out of prose.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

#[tokio::test]
async fn tables_stay_off_the_wire_unless_they_are_asked_for() {
    let harness = common::start().await;
    let events = harness.parse_ok(&common::table_pdf()).await;
    assert_eq!(common::shape(&events), ["info", "page", "status"]);
}

#[tokio::test]
async fn a_borderless_table_reaches_the_wire_as_a_grid_with_coordinates() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::table_pdf(),
            pb::PdfOptions {
                emit_tables: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    assert_eq!(
        common::shape(&events),
        ["info", "tables", "page", "status"],
        "the grid arrives before the markdown that flattens it"
    );

    let pages = common::tables(&events);
    assert_eq!(pages[0].page_no, 1);
    let table = &pages[0].tables[0];
    assert_eq!(table.kind, pb::TableKind::Data as i32);
    assert_eq!(
        table.column_boundaries.len(),
        3,
        "three columns of aligned text: {:?}",
        table.column_boundaries
    );
    assert_eq!(table.rows.len(), 4);
    assert_eq!(table.rows[0].cells, ["Year", "Engine", "Cards"]);

    let bbox = table.bbox.as_ref().expect("boundaries are an extent");
    assert!(bbox.width > 0.0 && bbox.height > 0.0, "{bbox:?}");

    // The same page's markdown can only say this much:
    let markdown = &common::pages(&events)[0].markdown;
    assert!(
        markdown.contains("|Year|"),
        "which is pipe characters: {markdown:?}"
    );
}

#[tokio::test]
async fn the_fold_puts_the_grid_in_the_documents_tables_not_in_a_paragraph() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::table_pdf(),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let document = common::documents(&events)[0];
    assert_eq!(document.tables.len(), 1, "one table, once");

    let table = &document.tables[0];
    assert_eq!(table.label, doc::DocItemLabel::Table as i32);
    assert_eq!(table.prov[0].page_no, 1);
    assert!(
        table.prov[0].bbox.is_some(),
        "a table detected from coordinates has coordinates"
    );

    let data = table.data.as_ref().expect("a grid");
    assert_eq!((data.num_rows, data.num_cols), (4, 3));
    assert_eq!(data.table_cells.len(), 12);
    assert_eq!(data.grid.len(), 4);
    assert_eq!(data.table_cells[0].text, "Year");
    assert!(data.table_cells[0].column_header);

    for cell in &data.table_cells {
        let bbox = cell.bbox.as_ref().expect("every cell is placed");
        assert!(bbox.r > bbox.l, "{bbox:?}");
        assert!(bbox.t > bbox.b, "{bbox:?}");
    }

    // And nothing anywhere in the fragment is still carrying the pipes.
    for item in &document.texts {
        let text = match item.item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => &text.base.as_ref().unwrap().text,
            Some(doc::base_text_item::Item::SectionHeader(header)) => {
                &header.base.as_ref().unwrap().text
            }
            other => panic!("unexpected item {other:?}"),
        };
        assert!(!text.contains('|'), "a table survived as prose: {text:?}");
    }
}
