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
    let mut detected = detect(page_no, &laid_out, rects, lines);
    for table in &mut detected {
        trim_rows_outside_the_rules(table, &laid_out, lines, page_no);
    }
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

/// How far a row boundary may sit past the end of the vertical rules and
/// still be inside the grid they draw.
const RULE_TOLERANCE: f32 = 3.0;

/// How far a vertical stroke may lean, in points end to end, and still be
/// a rule.
const RULE_LEAN: f32 = 1.0;

/// The shortest vertical stroke that counts as a rule of a grid.
const MIN_RULE_LENGTH: f32 = 10.0;

/// Drop the rows a ruled grid does not reach.
///
/// The line detector takes the nearest horizontal rule above a grid as the
/// grid's top, and a page that underlines its title puts a rule there: the
/// paragraph between the title and the table then reads as the table's
/// first row, with one cell of prose and the rest empty. The vertical
/// rules say where the grid is. A leading or trailing row whose boundary
/// lies beyond every vertical rule of the table, and whose cells hold
/// content in no column but the first, is outside the grid, and its runs
/// go back to the page: that is the shape a paragraph makes when it spills
/// one row past the rules, because prose starts flush with the left margin
/// and says nothing in the columns after it. A header set above an
/// open-edged grid is populated across its columns and stays — including a
/// row-header stub whose own leading column is blank and every other
/// column is filled, which is the shape the detector accepts a header for
/// even when it names no leading column of its own.
///
/// A table with no vertical rules inside its extent is left as detected;
/// there is no geometry to judge it by.
fn trim_rows_outside_the_rules(
    table: &mut Table,
    items: &[TextItem],
    lines: &[PdfLine],
    page_no: u32,
) {
    let (Some(&left), Some(&right)) = (table.columns.first(), table.columns.last()) else {
        return;
    };
    let mut top = f32::MIN;
    let mut bottom = f32::MAX;
    let mut ruled = false;
    for line in lines.iter().filter(|line| line.page == page_no) {
        if (line.x1 - line.x2).abs() > RULE_LEAN
            || (line.y1 - line.y2).abs() < MIN_RULE_LENGTH
            || line.x1 < left - RULE_TOLERANCE
            || line.x1 > right + RULE_TOLERANCE
        {
            continue;
        }
        ruled = true;
        top = top.max(line.y1.max(line.y2));
        bottom = bottom.min(line.y1.min(line.y2));
    }
    if !ruled {
        return;
    }
    // A row's content sits nowhere but the first column: that is what a
    // stray paragraph line looks like, since prose starts at the left
    // margin and never reaches into a table's later columns. A row-header
    // stub is the opposite shape — its own first column is the one left
    // blank — so this predicate leaves it alone, which is what the
    // detector's own acceptance of that header depends on.
    let sparse =
        |cells: &[String]| cells.len() > 1 && cells[1..].iter().all(|cell| cell.trim().is_empty());
    while table.rows.len() > 1
        && table.rows[0] > top + RULE_TOLERANCE
        && table.cells.first().is_some_and(|cells| sparse(cells))
    {
        table.rows.remove(0);
        table.cells.remove(0);
        let floor = table.rows[0] + RULE_TOLERANCE;
        table
            .item_indices
            .retain(|index| items.get(*index).is_none_or(|item| item.y <= floor));
    }
    while table.rows.len() > 1
        && table.rows[table.rows.len() - 1] < bottom - RULE_TOLERANCE
        && table.cells.last().is_some_and(|cells| sparse(cells))
    {
        table.rows.pop();
        table.cells.pop();
        // The ceiling below which a run is outside the grid is the new
        // trailing row's own boundary, not the one just discarded — using
        // the discarded row's y left runs sitting exactly on it (the usual
        // case: detectors report a row at its own text baseline) inside
        // the tolerance band and off the page they were sent back to.
        let ceiling = table.rows[table.rows.len() - 1] - RULE_TOLERANCE;
        table
            .item_indices
            .retain(|index| items.get(*index).is_none_or(|item| item.y >= ceiling));
    }
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

    /// A vertical rule at `x` from `bottom` up to `top`.
    fn vertical(x: f32, bottom: f32, top: f32) -> PdfLine {
        PdfLine {
            x1: x,
            y1: bottom,
            x2: x,
            y2: top,
            page: 1,
        }
    }

    /// A two-column grid ruled from y 620 down to y 434, and an intro
    /// paragraph above it that a title's underline at y 680 made the
    /// detector take for the grid's first row.
    fn grid_with_a_prose_row_above() -> (Table, Vec<TextItem>, Vec<PdfLine>) {
        let items = vec![
            placed("Complete every field.", 90.0, 652.0),
            placed("Field", 90.0, 607.0),
            placed("Value", 306.0, 607.0),
            placed("Request number", 90.0, 592.0),
            placed("EQ-2024-0117", 306.0, 592.0),
        ];
        let table = Table::new(
            vec![84.6, 300.6, 516.6],
            vec![680.0, 619.8, 604.3],
            vec![
                vec!["Complete every field.".to_owned(), String::new()],
                vec!["Field".to_owned(), "Value".to_owned()],
                vec!["Request number".to_owned(), "EQ-2024-0117".to_owned()],
            ],
            vec![0, 1, 2, 3, 4],
        );
        let lines = vec![
            vertical(84.6, 433.6, 620.1),
            vertical(300.6, 434.1, 619.6),
            vertical(516.6, 433.6, 620.1),
        ];
        (table, items, lines)
    }

    #[test]
    fn a_prose_row_above_the_vertical_rules_is_not_a_row_of_the_grid() {
        let (mut table, items, lines) = grid_with_a_prose_row_above();
        trim_rows_outside_the_rules(&mut table, &items, &lines, 1);
        assert_eq!(table.cells.len(), 2, "{:?}", table.cells);
        assert_eq!(table.cells[0], ["Field", "Value"]);
        assert!(
            (table.rows[0] - 619.8).abs() < f32::EPSILON,
            "the grid's top is the top rule: {:?}",
            table.rows
        );
        assert!(
            !table.item_indices.contains(&0),
            "the intro's run went back to the page: {:?}",
            table.item_indices
        );
        let bbox = region(&table, &items).bbox.expect("an extent");
        assert!(
            bbox.y + bbox.height < 640.0,
            "the extent stops at the grid: {bbox:?}"
        );
    }

    #[test]
    fn a_header_populated_across_its_columns_stays_above_an_open_grid() {
        // The detector accepts a header above the top rule when every
        // column of it is filled; that shape is a header, not prose.
        let (mut table, items, lines) = grid_with_a_prose_row_above();
        table.cells[0] = vec!["Name".to_owned(), "Amount".to_owned()];
        trim_rows_outside_the_rules(&mut table, &items, &lines, 1);
        assert_eq!(table.cells.len(), 3);
    }

    #[test]
    fn a_grid_without_vertical_rules_is_left_as_detected() {
        let (mut table, items, _) = grid_with_a_prose_row_above();
        trim_rows_outside_the_rules(&mut table, &items, &[], 1);
        assert_eq!(table.cells.len(), 3);
    }

    #[test]
    fn a_sparse_row_below_the_rules_is_trimmed_too() {
        let (mut table, mut items, lines) = grid_with_a_prose_row_above();
        table.cells.remove(0);
        table.rows.remove(0);
        items.push(placed("Signature: ____", 90.0, 400.0));
        table.rows.push(410.0);
        table
            .cells
            .push(vec!["Signature: ____".to_owned(), String::new()]);
        table.item_indices.push(5);
        trim_rows_outside_the_rules(&mut table, &items, &lines, 1);
        assert_eq!(table.cells.len(), 2, "{:?}", table.cells);
        assert!(!table.item_indices.contains(&5));
    }

    #[test]
    fn a_sparse_row_below_the_rules_gives_its_runs_back_when_rows_are_baselines() {
        // The text-anchor and alignment detectors report each row by the
        // baseline its text sits on, not by the rule above it. The trimmed
        // row's run then sits exactly on the boundary that was popped, and
        // it has to go back to the page all the same.
        let items = vec![
            placed("Field", 90.0, 607.0),
            placed("Value", 306.0, 607.0),
            placed("Request number", 90.0, 592.0),
            placed("EQ-2024-0117", 306.0, 592.0),
            placed("Signature: ____", 90.0, 400.0),
        ];
        let mut table = Table::new(
            vec![84.6, 300.6, 516.6],
            vec![607.0, 592.0, 400.0],
            vec![
                vec!["Field".to_owned(), "Value".to_owned()],
                vec!["Request number".to_owned(), "EQ-2024-0117".to_owned()],
                vec!["Signature: ____".to_owned(), String::new()],
            ],
            vec![0, 1, 2, 3, 4],
        );
        let lines = vec![
            vertical(84.6, 433.6, 620.1),
            vertical(300.6, 434.1, 619.6),
            vertical(516.6, 433.6, 620.1),
        ];
        trim_rows_outside_the_rules(&mut table, &items, &lines, 1);
        assert_eq!(table.cells.len(), 2, "{:?}", table.cells);
        assert!(
            !table.item_indices.contains(&4),
            "the signature line's run went back to the page: {:?}",
            table.item_indices
        );
        let bbox = region(&table, &items).bbox.expect("an extent");
        assert!(bbox.y > 430.0, "the extent stops at the grid: {bbox:?}");
    }

    /// A horizontal rule at `y` from `left` to `right`.
    fn horizontal(y: f32, left: f32, right: f32) -> PdfLine {
        PdfLine {
            x1: left,
            y1: y,
            x2: right,
            y2: y,
            page: 1,
        }
    }

    #[test]
    fn a_header_with_an_unlabelled_stub_column_stays_above_an_open_grid() {
        // The open-edge detector accepts a header above its top rule whose
        // first cell is empty: the stub column of a row-header grid has no
        // label of its own. That header has one filled cell, which is the
        // shape of a title row too, and it is not one.
        let items = vec![
            placed("Value", 360.0, 375.0),
            placed("Pack", 60.0, 320.0),
            placed("OCR body", 360.0, 320.0),
            placed("Application", 60.0, 220.0),
            placed("Search", 360.0, 220.0),
        ];
        let lines = vec![
            horizontal(360.0, 30.0, 630.0),
            horizontal(275.0, 30.0, 630.0),
            horizontal(170.0, 30.0, 630.0),
            vertical(330.0, 168.0, 362.0),
        ];
        let tables = page_tables(1, &items, &[], &lines).expect("the grid is detected");
        let rows: Vec<&[String]> = tables.tables[0]
            .rows
            .iter()
            .map(|row| row.cells.as_slice())
            .collect();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0], ["", "Value"], "the header is the first row");
    }
}
