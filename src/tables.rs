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
//! Nothing here re-detects anything. The detector is public and takes the
//! same positioned runs the extraction pass already produced; this module
//! calls it and maps what it returns.

use pdf_inspector::TextItem;
use pdf_inspector::tables::{Table, TableKind, detect_tables};

use crate::proto::v1 as pb;

/// The tables on one page, or `None` when the page has none.
///
/// `base_font_size` is the page's body size, which the detector uses to
/// tell a table's cells from surrounding prose.
#[must_use]
pub fn page_tables(page_no: u32, items: &[TextItem]) -> Option<pb::PageTables> {
    let tables: Vec<pb::TableRegion> = detect_tables(items, body_font_size(items), false)
        .iter()
        .map(|table| region(table, items))
        .collect();
    (!tables.is_empty()).then_some(pb::PageTables { page_no, tables })
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
/// The detector's column and row values are where each column and row
/// *starts*, so they cannot say where the last column ends or how far the
/// last row descends. The runs can: the table's extent is the hull of the
/// items it took. A table that claims no run has no extent, and says so
/// rather than reporting a point at the origin.
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
