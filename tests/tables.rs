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
async fn a_ruled_table_the_alignment_detector_misses_reaches_the_wire() {
    // The alignment detector finds nothing here: the cells are prose, of
    // every width, and nothing about the text says "table". What says it is
    // the rules the document drew, and until the vendored crate published
    // its vector geometry the detector that reads them was unreachable, so
    // a table with real rules came out worse than a borderless one.
    let ruled = common::ruled_table_pdf();
    let items = pdf_inspector::extract_text_with_positions_mem(&ruled).expect("runs");
    assert!(
        pdf_inspector::tables::detect_tables(
            &items,
            grpc_pdf_inspector::tables::body_font_size(&items),
            false,
        )
        .is_empty(),
        "the fixture is only interesting while alignment alone misses it"
    );

    let harness = common::start().await;
    let events = harness
        .parse(
            &ruled,
            pb::PdfOptions {
                emit_tables: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    let pages = common::tables(&events);
    assert_eq!(pages.len(), 1, "one page, one table event");
    let table = &pages[0].tables[0];
    assert_eq!(table.kind, pb::TableKind::Data as i32);
    assert_eq!(
        table.column_boundaries,
        [72.0, 220.0, 540.0],
        "the boundaries are the rules the document drew, to the point"
    );
    let cells: Vec<&str> = table
        .rows
        .iter()
        .flat_map(|row| row.cells.iter().map(String::as_str))
        .collect();
    assert!(
        cells.contains(&"Analytical Engine"),
        "the cells are the document's own: {cells:?}"
    );

    let layout = common::status(&events)
        .layout
        .as_ref()
        .expect("FULL reports layout");
    assert_eq!(
        layout.pages_with_tables,
        [1],
        "and the layout verdict now comes from the same detector that found it"
    );
    assert!(layout.is_complex);
}

#[tokio::test]
async fn the_layout_verdict_survives_the_analysis_pass_it_replaced() {
    // The borderless fixture reached `pages_with_tables` through a separate
    // analysis pass over the file. It reaches it through the page loop now,
    // and the answer is the same one.
    let harness = common::start().await;
    let events = harness.parse_ok(&common::table_pdf()).await;
    let layout = common::status(&events)
        .layout
        .as_ref()
        .expect("FULL reports layout");
    assert_eq!(layout.pages_with_tables, [1]);
    assert!(layout.pages_with_columns.is_empty());
    assert!(layout.is_complex);

    let plain = harness.parse_ok(&common::text_pdf(2, 60, "prose")).await;
    let layout = common::status(&plain)
        .layout
        .as_ref()
        .expect("FULL reports layout");
    assert!(!layout.is_complex, "and prose is still prose: {layout:?}");
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
