// SPDX-License-Identifier: Apache-2.0

//! A table two detectors agree on, and disagree about the length of.
//!
//! The markdown renderer prints a table its own finder saw as pipe rows,
//! and the fold swaps that block for the grid the table detectors found,
//! which carries cell boxes. The two are matched on half of either side.
//! When the grid had fewer rows than the block, the rows only the block
//! had were in the `page` markdown and nowhere in the Document.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

#[tokio::test]
async fn rows_the_grid_lacks_are_kept_and_counted() {
    let events = common::start()
        .await
        .parse(
            &common::partly_ruled_table_pdf(8, 3),
            pb::PdfOptions {
                emit_document: true,
                emit_tables: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");

    // The controls: the grid has the three ruled rows, the markdown all
    // eight.
    let grids: Vec<&pb::TableRegion> = events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Tables(tables) => Some(&tables.tables),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(grids.len(), 1);
    assert_eq!(grids[0].rows.len(), 3);
    let markdown = &common::pages(&events)[0].markdown;
    assert!(markdown.contains("Entry7"), "{markdown}");

    let documents = common::documents(&events);
    let document = documents.first().expect("a document event");
    let cells: Vec<&str> = document
        .tables
        .iter()
        .filter_map(|table| table.data.as_ref())
        .flat_map(|data| data.table_cells.iter().map(|cell| cell.text.as_str()))
        .collect();
    for row in 0..8 {
        let entry = format!("Entry{row}");
        assert!(
            cells.contains(&entry.as_str()),
            "{entry} is on the page and in the markdown, and missing from the Document: \
             {cells:?}"
        );
    }
    assert_eq!(
        document.tables.len(),
        2,
        "the grid, and the rows it lacks beside it"
    );
    let reachable = common::reachable_from_body(document);
    for table in &document.tables {
        assert!(reachable.contains(&table.self_ref), "{}", table.self_ref);
    }

    let split: Vec<&pb::ParseWarning> = common::status(&events)
        .warnings
        .iter()
        .filter(|warning| warning.code == pb::ParseWarningCode::TableRowsOutsideGrid as i32)
        .collect();
    assert_eq!(split.len(), 1, "{:?}", common::status(&events).warnings);
    assert!(
        split[0].message.contains("page(s) 1"),
        "{:?}",
        split[0].message
    );
}

#[tokio::test]
async fn a_fully_ruled_table_is_one_table_and_no_warning() {
    let events = common::start()
        .await
        .parse(
            &common::partly_ruled_table_pdf(4, 4),
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the fixture should parse");
    let documents = common::documents(&events);
    assert_eq!(documents[0].tables.len(), 1);
    assert!(
        common::status(&events)
            .warnings
            .iter()
            .all(|warning| warning.code != pb::ParseWarningCode::TableRowsOutsideGrid as i32)
    );
}
