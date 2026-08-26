// SPDX-License-Identifier: Apache-2.0

//! Tables as grids, not as pipe characters.
//!
//! The detector produces a real grid — column boundaries and row boundaries
//! as coordinates in page points, plus the cell contents indexed by row and
//! column, plus its own judgement of whether the thing is data or a table of
//! contents. The markdown renderer then flattens all of that into `|`
//! characters, and a consumer that wants a table has to parse GFM back out
//! of a paragraph.
//!
//! Nothing here re-detects anything. The detectors are public and take the
//! same positioned runs, rectangles and line segments the extraction pass
//! already produced; this module calls them and maps what they return.
//!
//! There are three of them and the order matters. A table drawn with real
//! rules says where its cells are, so the rectangle detector is asked
//! first and the line detector second; only a table with no rules at all
//! falls through to the heuristic that infers columns from alignment. That
//! is the cascade the library runs internally for its own layout verdict,
//! and running it here is what makes a ruled table detect at least as well
//! as a borderless one instead of worse.

use pdf_inspector::tables::{
    Table, TableKind, detect_tables, detect_tables_from_lines, detect_tables_from_rects,
};
use pdf_inspector::types::ItemType;
use pdf_inspector::{PdfLine, PdfRect, TextItem};

use crate::proto::v1 as pb;

/// The tables on one page, or `None` when the page has none.
///
/// `items` are the page's runs; `rects` and `lines` may be the whole
/// document's, because every detector filters them to `page_no` itself.
#[must_use]
pub fn page_tables(
    page_no: u32,
    items: &[TextItem],
    rects: &[PdfRect],
    lines: &[PdfLine],
) -> Option<pb::PageTables> {
    // The rectangle detector strips image placeholders before it clusters,
    // because an image's left edge is not a column edge, and the indices it
    // reports are into what is left. Doing that filtering here, once, keeps
    // one slice of runs for the detectors and for the extents below; a
    // detector handed one slice and an extent read out of another is how a
    // box ends up naming the wrong runs.
    let laid_out: Vec<TextItem> = items
        .iter()
        .filter(|item| !matches!(item.item_type, ItemType::Image))
        .cloned()
        .collect();
    let detected = detect(page_no, &laid_out, rects, lines);
    let tables: Vec<pb::TableRegion> = detected
        .iter()
        .map(|table| region(table, &laid_out))
        .collect();
    (!tables.is_empty()).then_some(pb::PageTables { page_no, tables })
}

/// Run the three detectors in the order the library runs them, and take the
/// first that finds a data table.
///
/// A table of contents is not the answer this cascade is looking for: it
/// has rows and columns, so any detector can report one, and reporting it
/// would stop the search before the ruled data table below it was found.
/// The last detector's answer is kept whatever it is, so a page whose only
/// table is a table of contents still reports it.
fn detect(page_no: u32, items: &[TextItem], rects: &[PdfRect], lines: &[PdfLine]) -> Vec<Table> {
    let has_data = |tables: &[Table]| tables.iter().any(|table| table.kind == TableKind::Data);

    let (from_rects, _hints) = detect_tables_from_rects(items, rects, page_no);
    if has_data(&from_rects) {
        return from_rects;
    }
    let from_lines = detect_tables_from_lines(items, lines, page_no);
    if has_data(&from_lines) {
        return from_lines;
    }
    let heuristic = detect_tables(items, body_font_size(items), false);
    if has_data(&heuristic) {
        return heuristic;
    }
    // No detector found data. Whichever of them found a table of contents
    // found the only table on the page.
    [from_rects, from_lines, heuristic]
        .into_iter()
        .find(|tables| !tables.is_empty())
        .unwrap_or_default()
}

/// Whether a page's detected tables include real data, which is what
/// `LayoutComplexity.pages_with_tables` counts.
#[must_use]
pub fn has_data_table(tables: &pb::PageTables) -> bool {
    tables
        .tables
        .iter()
        .any(|table| table.kind == pb::TableKind::Data as i32)
}

/// One detected table as the wire message.
fn region(table: &Table, items: &[TextItem]) -> pb::TableRegion {
    pb::TableRegion {
        bbox: bbox(table, items),
        column_boundaries: table.columns.iter().map(|x| f64::from(*x)).collect(),
        row_boundaries: table.rows.iter().map(|y| f64::from(*y)).collect(),
        rows: table
            .cells
            .iter()
            .map(|row| pb::TableCells { cells: row.clone() })
            .collect(),
        kind: match table.kind {
            TableKind::Data => pb::TableKind::Data,
            TableKind::Toc => pb::TableKind::Contents,
        }
        .into(),
    }
}

/// The table's extent, measured from the runs it claims.
///
/// What a detector's column and row values mean varies with the detector:
/// the alignment one reports where each column and row starts, the ruled
/// ones report the rules themselves. Neither can be relied on to say where
/// the table's own edges are. The runs can: the table's extent is the hull
/// of the items it took. A table that claims no run has no extent, and says
/// so rather than reporting a point at the origin.
fn bbox(table: &Table, items: &[TextItem]) -> Option<pb::Rect> {
    let claimed = || {
        table
            .item_indices
            .iter()
            .filter_map(|index| items.get(*index))
    };
    let left = claimed().map(|item| item.x).reduce(f32::min)?;
    let right = claimed().map(|item| item.x + item.width).reduce(f32::max)?;
    let bottom = claimed().map(|item| item.y).reduce(f32::min)?;
    let top = claimed()
        .map(|item| item.y + item.height)
        .reduce(f32::max)?;
    Some(pb::Rect {
        x: f64::from(left),
        y: f64::from(bottom),
        width: f64::from(right - left),
        height: f64::from(top - bottom),
    })
}

/// The page's body type size: the size the most characters are set in.
///
/// The library computes this internally with machinery it does not export,
/// so it is computed here the same way it is computed everywhere — by
/// weight of text rather than by number of runs, because one enormous
/// heading is not a body size and forty words of prose are.
#[must_use]
pub fn body_font_size(items: &[TextItem]) -> f32 {
    // Quarter-point buckets: type sizes are rarely exact and two runs set
    // at 10.0 and 10.02 are the same size to any reader.
    let mut weights: Vec<(i32, usize)> = Vec::new();
    for item in items {
        if item.font_size <= 0.0 || item.text.trim().is_empty() {
            continue;
        }
        let bucket = (item.font_size * 4.0).round() as i32;
        let characters = item.text.chars().count();
        match weights.iter_mut().find(|(size, _)| *size == bucket) {
            Some((_, weight)) => *weight += characters,
            None => weights.push((bucket, characters)),
        }
    }
    weights
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map_or(12.0, |(bucket, _)| bucket as f32 / 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_inspector::types::ItemType;

    fn item(text: &str, font_size: f32) -> TextItem {
        TextItem {
            text: text.to_owned(),
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: font_size,
            font: "Helvetica".to_owned(),
            font_tag: "F1".to_owned(),
            font_size,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn the_body_size_is_the_size_most_of_the_text_is_set_in() {
        let items = vec![
            item("A Very Short Heading", 24.0),
            item("a much longer stretch of ordinary prose", 10.0),
            item("and another stretch of ordinary prose", 10.0),
        ];
        assert!((body_font_size(&items) - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn nearly_equal_sizes_are_one_size() {
        let items = vec![item("first half of the prose", 10.0), item("second", 10.02)];
        assert!((body_font_size(&items) - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_page_with_no_measurable_text_falls_back_to_a_default() {
        assert!((body_font_size(&[]) - 12.0).abs() < f32::EPSILON);
        assert!((body_font_size(&[item("   ", 10.0)]) - 12.0).abs() < f32::EPSILON);
    }

    /// One run placed on the page.
    fn placed(text: &str, x: f32, y: f32) -> TextItem {
        let mut placed = item(text, 10.0);
        placed.x = x;
        placed.y = y;
        placed.width = 40.0;
        placed.height = 10.0;
        placed
    }

    #[test]
    fn a_grid_takes_its_extent_from_the_runs_it_claims() {
        let items = vec![placed("a", 100.0, 700.0), placed("b", 200.0, 660.0)];
        let table = Table::new(
            vec![100.0, 200.0],
            vec![700.0, 660.0],
            vec![vec!["a".to_owned(), "b".to_owned()]],
            vec![0, 1],
        );
        let region = region(&table, &items);
        let bbox = region.bbox.expect("claimed runs are an extent");
        assert!((bbox.x - 100.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!(
            (bbox.width - 140.0).abs() < f64::EPSILON,
            "the last column ends where its text ends, not where it starts: {bbox:?}"
        );
        assert!((bbox.y - 660.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.height - 50.0).abs() < f64::EPSILON, "{bbox:?}");
        assert_eq!(region.rows[0].cells, ["a", "b"]);
        assert_eq!(region.kind, pb::TableKind::Data as i32);
    }

    #[test]
    fn a_grid_that_claims_no_run_claims_no_extent() {
        let table = Table::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());
        assert!(region(&table, &[]).bbox.is_none());
    }
}
