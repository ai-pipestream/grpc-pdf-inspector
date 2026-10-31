// SPDX-License-Identifier: Apache-2.0

//! The rows of a table the renderer printed and no detector reported.
//!
//! The markdown renderer runs its own table cascade over the page's runs
//! (bands, struct tree, synthetic rules) and prints whatever it finds as
//! pipe syntax. The grids on the `tables` event come from the three public
//! detectors, so now and then the renderer prints a table that has no grid
//! beside it: Figure 1 of a paper whose code panels sit side by side is
//! one. The fold used to keep such a block as a paragraph, so a text item
//! carried `|---|---|` and every consumer that rendered it escaped the
//! pipes and printed a table nobody could read as one.
//!
//! The renderer's rows are the only account of that table there is, so
//! they become the grid: one row per printed line, one cell per unescaped
//! pipe-delimited field, and the lines above the separator as header rows,
//! which is the renderer's own convention. Nothing else is invented. The
//! renderer printed no cell boxes, so the cells carry none.

/// A pipe-syntax block read back into rows of cells.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenderedRows {
    /// Every printed row, separator excluded, in print order.
    pub rows: Vec<Vec<String>>,
    /// How many of `rows` sit above the separator line.
    pub header_rows: usize,
}

impl RenderedRows {
    /// Read a block the renderer printed as a table.
    ///
    /// `None` when the block has no row with a cell in it, which leaves
    /// the caller nothing to build a table from.
    #[must_use]
    pub fn parse(block: &str) -> Option<Self> {
        let mut parsed = Self::default();
        let mut separator_seen = false;
        for line in block.lines().map(str::trim).filter(|line| !line.is_empty()) {
            // Only the first separator-shaped line is the separator: a row
            // of `-` cells (a common "no value") prints the same way.
            if !separator_seen && is_separator(line) {
                parsed.header_rows = parsed.rows.len();
                separator_seen = true;
                continue;
            }
            parsed.rows.push(split_row(line));
        }
        parsed
            .rows
            .iter()
            .any(|row| row.iter().any(|cell| !cell.is_empty()))
            .then_some(parsed)
    }

    /// How many columns the widest row has.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// The non-empty cells, row by row.
    pub fn filled_cells(&self) -> impl Iterator<Item = &str> {
        self.rows
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|cell| !cell.is_empty())
    }
}

/// The `|---|:--:|` line between a header and the body.
fn is_separator(line: &str) -> bool {
    line.contains('-')
        && line
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' ' | '\t'))
}

/// One printed row's cells, with the renderer's `\|` escapes undone.
///
/// The leading and trailing pipes frame the row and are not cell
/// boundaries; a row printed without them is read the same way.
fn split_row(line: &str) -> Vec<String> {
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = match line.strip_suffix('|') {
        Some(rest) if !rest.ends_with('\\') => rest,
        _ => line,
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&'|') => {
                cell.push('|');
                characters.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            other => cell.push(other),
        }
    }
    cells.push(cell.trim().to_owned());
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_printed_table_reads_back_as_its_rows_with_the_header_counted() {
        let rows =
            RenderedRows::parse("|Year|Value|\n|---|---|\n|2024|10|\n|2025|12|").expect("a table");
        assert_eq!(rows.header_rows, 1);
        assert_eq!(
            rows.rows,
            [["Year", "Value"], ["2024", "10"], ["2025", "12"]]
        );
        assert_eq!(rows.columns(), 2);
    }

    #[test]
    fn an_escaped_pipe_is_a_character_of_its_cell() {
        let rows = RenderedRows::parse("|a\\|b|x|\n|---|---|").expect("a table");
        assert_eq!(rows.rows, [["a|b", "x"]]);
    }

    #[test]
    fn an_escaped_pipe_at_the_end_of_a_row_stays_in_the_last_cell() {
        let rows = RenderedRows::parse("|x|a\\|").expect("a table");
        assert_eq!(rows.rows, [["x", "a|"]]);
    }

    #[test]
    fn empty_cells_keep_their_column() {
        let rows =
            RenderedRows::parse("|A|B||D|\n|---|---|---|---|\n|one|two|three||").expect("a table");
        assert_eq!(rows.rows[0], ["A", "B", "", "D"]);
        assert_eq!(rows.rows[1], ["one", "two", "three", ""]);
        assert_eq!(rows.filled_cells().count(), 6);
    }

    #[test]
    fn a_row_of_dashes_below_the_separator_is_a_row() {
        let rows = RenderedRows::parse("|a|b|\n|---|---|\n|-|-|\n|1|2|").expect("a table");
        assert_eq!(rows.rows, [["a", "b"], ["-", "-"], ["1", "2"]]);
    }

    #[test]
    fn a_block_with_no_separator_has_no_header_rows() {
        let rows = RenderedRows::parse("|a|b|\n|c|d|").expect("a table");
        assert_eq!(rows.header_rows, 0);
        assert_eq!(rows.rows.len(), 2);
    }

    #[test]
    fn a_block_with_no_cell_text_is_no_table() {
        assert_eq!(RenderedRows::parse("|---|---|"), None);
        assert_eq!(RenderedRows::parse(""), None);
        assert_eq!(RenderedRows::parse("| | |"), None);
    }
}
