// SPDX-License-Identifier: Apache-2.0

//! Which of a document's runs are page chrome.
//!
//! Chrome is what the page carries because it is a page: the running head,
//! the folio, the line numbers ruled down the margin of a transcript. It is
//! not what the markdown renderer happened to leave out. Those are two
//! different facts, and conflating them starves the body: a renderer that
//! reads a two-column page column by column emits plenty of runs in an
//! order no forward scan of its own output can follow, and every one of
//! them reads as missing. Missing is not evidence of chrome, so nothing
//! here asks the markdown anything.
//!
//! What it asks instead is what chrome actually looks like, and every
//! answer is evidence a page carries whether or not anything was rendered:
//!
//! - **The parser's own verdict.** The crate identifies running headers and
//!   footers and strips them, by cross-page repetition on documents long
//!   enough to have it and by per-page position and type size on documents
//!   that are not. Its judgement is the first source, because deciding what
//!   a page's furniture is belongs to the parser rather than to this
//!   service. It has to be shown the whole document to make it: the service
//!   renders page by page, and one page's lines carry no repetition
//!   evidence at all.
//! - **Repetition at a page edge.** A line that sits in the isolated block
//!   at the top or the bottom of the page, says the same thing on page
//!   after page once its digits are read as a shape rather than a number,
//!   and sits at the same height every time, is a running head or a folio.
//!   `Chapter 3, page 5` and `Chapter 3, page 6` are the same running head.
//! - **Numbers in the margin.** A column of short numbers standing outside
//!   the block the page's text is set in, on page after page, is a line
//!   numbering. Inside the text block the same digits are a list marker or
//!   a table cell, so the margin is the whole of the evidence.
//!
//! Everything else is content, including content a renderer dropped.

use std::collections::{BTreeMap, HashMap, HashSet};

use pdf_inspector::TextItem;
use pdf_inspector::types::ItemType;

/// How far a run's block must sit from the rest of the page, in multiples
/// of the page's own line leading, before it counts as an edge block. The
/// parser's short-document policy uses the same factor for the same
/// judgement.
const ISOLATION_FACTOR: f32 = 1.8;

/// The most lines an edge block can have. A run of three is body-sized
/// content that happens to start at the top of the page.
const MAX_EDGE_LINES: usize = 2;

/// How many lines a page needs before any of them can be judged to be at
/// its edge. A page with four lines on it has no body for anything to be
/// outside of.
const MIN_PAGE_LINES: usize = 5;

/// How far apart two heights can be and still be the same line of the page.
const SAME_LINE: f32 = 0.5;

/// How far a repeated edge line's height may wander across the pages it
/// appears on. A running head is typeset at one height; a table row that
/// happens to reach the page edge is not.
const EDGE_DRIFT: f32 = 4.0;

/// How far outside the text block a run must sit to be in the margin.
const MARGIN_GAP: f32 = 2.0;

/// The most characters a margin line number can have.
const MAX_MARGIN_NUMBER: usize = 4;

/// How many numbers a page needs in one margin before they are a
/// numbering rather than a coincidence.
const MIN_MARGIN_NUMBERS: usize = 3;

/// How many characters a run needs before its extent counts towards the
/// block the page's text is set in.
const BODY_RUN_CHARS: usize = 4;

/// The verdict on one document's runs: which of them are page chrome.
pub struct Chrome {
    /// The runs the evidence convicts, by their identity on the page.
    convicted: HashSet<Key>,
}

impl Chrome {
    /// No run is chrome. What a call that asked for no furniture report
    /// gets, and what a document with no evidence of chrome gets too.
    #[must_use]
    pub fn none() -> Self {
        Self {
            convicted: HashSet::new(),
        }
    }

    /// Weigh the evidence over a whole document's runs.
    ///
    /// `items` is the extraction's own flat list, every page of it: the
    /// repetition evidence is a document-wide fact and cannot be recovered
    /// from one page at a time.
    #[must_use]
    pub fn detect(items: &[TextItem], page_count: u32) -> Self {
        let mut convicted = HashSet::new();
        stripped_by_the_parser(items, page_count, &mut convicted);
        let pages = drawn_text(items);
        repeated_edge_lines(&pages, page_count, &mut convicted);
        margin_numbers(&pages, page_count, &mut convicted);
        Self { convicted }
    }

    /// Whether the evidence convicts this run.
    #[must_use]
    pub fn convicts(&self, item: &TextItem) -> bool {
        self.convicted.contains(&key(item))
    }
}

/// A run's identity: which page drew it, where, and what it says.
type Key = (u32, i64, i64, String);

/// One sighting of a shape at a page edge: the page, the height it sat at,
/// and the run that said it.
type Sighting = (u32, f32, Key);

/// One run's identity.
fn key(item: &TextItem) -> Key {
    (
        item.page,
        hundredths(item.x),
        hundredths(item.y),
        item.text.clone(),
    )
}

/// A coordinate as a whole number of hundredths of a point, so two floats
/// that came out of the same measurement compare equal.
fn hundredths(value: f32) -> i64 {
    (f64::from(value) * 100.0).round() as i64
}

/// The runs the page drew as text, by page, in extraction order.
///
/// Image placements, link rectangles and empty runs are not text a reader
/// saw, so nothing about chrome is claimed for them.
fn drawn_text(items: &[TextItem]) -> BTreeMap<u32, Vec<&TextItem>> {
    let mut pages: BTreeMap<u32, Vec<&TextItem>> = BTreeMap::new();
    for item in items {
        if matches!(item.item_type, ItemType::Text | ItemType::FormField)
            && !item.text.trim().is_empty()
        {
            pages.entry(item.page).or_default().push(item);
        }
    }
    pages
}

/// The runs the parser's own header, footer and folio stripper removes.
///
/// It is shown the whole document, which is the only way it can weigh
/// repetition: the service renders one page at a time, and on a single
/// page's lines the document-wide classifier has nothing to count.
fn stripped_by_the_parser(items: &[TextItem], page_count: u32, convicted: &mut HashSet<Key>) {
    let lines = pdf_inspector::extractor::group_into_lines(items.to_vec());
    let grouped: HashSet<Key> = lines
        .iter()
        .flat_map(|line| line.items.iter())
        .map(key)
        .collect();
    let kept: HashSet<Key> =
        pdf_inspector::markdown::strip_repeated_header_footer_lines(lines, page_count)
            .iter()
            .flat_map(|line| line.items.iter())
            .map(key)
            .collect();
    // Only runs the grouping actually saw: a run it dropped on its way is
    // not a run the stripper decided anything about.
    convicted.extend(grouped.difference(&kept).cloned());
}

/// How many distinct pages a repetition needs before it is evidence.
///
/// Two, or three tenths of a longer document, which is the proportion the
/// parser's own classifier uses. A one-page document has no repetition to
/// measure and nothing here can convict on it.
fn threshold(page_count: u32) -> usize {
    2.max(page_count as usize * 30 / 100)
}

/// Convict the lines that repeat, page after page, in the isolated block at
/// a page edge.
fn repeated_edge_lines(
    pages: &BTreeMap<u32, Vec<&TextItem>>,
    page_count: u32,
    convicted: &mut HashSet<Key>,
) {
    // Each shape seen at an edge, with the pages it was seen on, the
    // heights it was seen at, and the runs that said it.
    let mut seen: HashMap<(Edge, String), Vec<Sighting>> = HashMap::new();
    for (page, items) in pages {
        for (edge, heights) in edges(items) {
            for item in items
                .iter()
                .filter(|item| heights.iter().any(|y| (item.y - y).abs() < SAME_LINE))
            {
                seen.entry((edge, shape(&item.text)))
                    .or_default()
                    .push((*page, item.y, key(item)));
            }
        }
    }

    let threshold = threshold(page_count);
    for occurrences in seen.values() {
        let distinct: HashSet<u32> = occurrences.iter().map(|(page, _, _)| *page).collect();
        if distinct.len() < threshold {
            continue;
        }
        let highest = occurrences
            .iter()
            .map(|(_, y, _)| *y)
            .fold(f32::MIN, f32::max);
        let lowest = occurrences
            .iter()
            .map(|(_, y, _)| *y)
            .fold(f32::MAX, f32::min);
        if highest - lowest > EDGE_DRIFT {
            continue;
        }
        convicted.extend(occurrences.iter().map(|(_, _, key)| key.clone()));
    }
}

/// Which edge of the page a block sits at.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Edge {
    /// Above everything else on the page.
    Top,
    /// Below everything else on the page.
    Bottom,
}

/// The heights of the isolated block at each edge of a page.
///
/// A page's furniture is set off from its body by white space, so the block
/// at each edge qualifies only when a clear gap separates it from the next
/// line inward. A block that runs on into the body is the body starting at
/// the top of the page, which is what a page of prose looks like.
fn edges(items: &[&TextItem]) -> Vec<(Edge, Vec<f32>)> {
    let mut heights: Vec<f32> = Vec::new();
    for item in items {
        if !heights.iter().any(|y| (y - item.y).abs() < SAME_LINE) {
            heights.push(item.y);
        }
    }
    if heights.len() < MIN_PAGE_LINES {
        return Vec::new();
    }
    heights.sort_by(|left, right| right.total_cmp(left));

    let mut steps: Vec<f32> = heights
        .windows(2)
        .map(|pair| pair[0] - pair[1])
        .filter(|step| *step > 1.0)
        .collect();
    steps.sort_by(f32::total_cmp);
    let leading = steps.get(steps.len() / 2).copied().unwrap_or(12.0);
    let isolation = leading * ISOLATION_FACTOR;

    let mut bands = Vec::new();
    if let Some(band) = edge_block(&heights, isolation) {
        bands.push((Edge::Top, band));
    }
    let from_bottom: Vec<f32> = heights.iter().rev().copied().collect();
    if let Some(band) = edge_block(&from_bottom, isolation) {
        bands.push((Edge::Bottom, band));
    }
    bands
}

/// The block at the near end of `heights`, when a gap of `isolation` or
/// more separates it from the rest of the page.
fn edge_block(heights: &[f32], isolation: f32) -> Option<Vec<f32>> {
    let mut lines = 1usize;
    while lines < heights.len()
        && lines <= MAX_EDGE_LINES
        && (heights[lines - 1] - heights[lines]).abs() < isolation
    {
        lines += 1;
    }
    if lines > MAX_EDGE_LINES || lines >= heights.len() {
        return None;
    }
    let gap = (heights[lines - 1] - heights[lines]).abs();
    (gap >= isolation).then(|| heights[..lines].to_vec())
}

/// A run's text as a shape: what it says with its numbers read as numbers
/// rather than as their values.
///
/// `page 5 of 12` and `page 6 of 12` are one running head printing its
/// own position, and they are the same shape.
fn shape(text: &str) -> String {
    let mut shaped = String::new();
    let mut in_number = false;
    for character in text.trim().chars() {
        if character.is_numeric() {
            if !in_number {
                shaped.push('#');
                in_number = true;
            }
            continue;
        }
        in_number = false;
        if character.is_whitespace() {
            if !shaped.ends_with(' ') {
                shaped.push(' ');
            }
            continue;
        }
        shaped.extend(character.to_lowercase());
    }
    shaped.trim().to_owned()
}

/// Convict the short numbers standing in a page's margin, when the same
/// margin carries them page after page.
fn margin_numbers(
    pages: &BTreeMap<u32, Vec<&TextItem>>,
    page_count: u32,
    convicted: &mut HashSet<Key>,
) {
    let mut numbered: Vec<Vec<Key>> = Vec::new();
    for items in pages.values() {
        let Some((left, right)) = text_block(items) else {
            continue;
        };
        let page: Vec<Key> = items
            .iter()
            .filter(|item| is_short_number(&item.text))
            .filter(|item| {
                let (start, end) = extent(item);
                end <= left - MARGIN_GAP || start >= right + MARGIN_GAP
            })
            .map(|item| key(item))
            .collect();
        if page.len() >= MIN_MARGIN_NUMBERS {
            numbered.push(page);
        }
    }
    if numbered.len() >= threshold(page_count) {
        convicted.extend(numbered.into_iter().flatten());
    }
}

/// The horizontal extent of the block a page's text is set in.
///
/// Measured over the runs long enough to be prose: a page whose only runs
/// are numbers has no text block, and neither has an empty one.
fn text_block(items: &[&TextItem]) -> Option<(f32, f32)> {
    let mut left = f32::MAX;
    let mut right = f32::MIN;
    let mut counted = 0usize;
    for item in items
        .iter()
        .filter(|item| {
            item.text
                .trim()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .count()
                >= BODY_RUN_CHARS
        })
        .filter(|item| !is_short_number(&item.text))
    {
        let (start, end) = extent(item);
        left = left.min(start);
        right = right.max(end);
        counted += 1;
    }
    (counted >= BODY_RUN_CHARS).then_some((left, right))
}

/// A run's left and right edges, whichever way round it was measured.
fn extent(item: &TextItem) -> (f32, f32) {
    let far = item.x + item.width;
    (item.x.min(far), item.x.max(far))
}

/// Whether a run is a bare number short enough to be a line number.
///
/// Trailing punctuation is part of the shape a numbering is printed in
/// (`12.`, `12)`), and a run with a letter in it is not a number at all.
fn is_short_number(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().count() <= MAX_MARGIN_NUMBER
        && text.chars().any(char::is_numeric)
        && text
            .chars()
            .all(|character| character.is_numeric() || matches!(character, '.' | ')' | '-' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(page: u32, text: &str, x: f32, y: f32) -> TextItem {
        TextItem {
            text: text.to_owned(),
            x,
            y,
            width: 4.0 * text.chars().count() as f32,
            height: 10.0,
            font: "Helvetica".to_owned(),
            font_tag: "F1".to_owned(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: None,
        }
    }

    /// A page of prose with a running head above it and a folio below it.
    fn sheet(page_no: u32, head: &str) -> Vec<TextItem> {
        let mut items = vec![item(page_no, head, 72.0, 760.0)];
        for line in 0..10u8 {
            items.push(item(
                page_no,
                &format!("body line {line} of page {page_no} with ordinary words"),
                72.0,
                700.0 - 16.0 * f32::from(line),
            ));
        }
        items.push(item(page_no, &page_no.to_string(), 300.0, 40.0));
        items
    }

    #[test]
    fn a_repeated_running_head_and_its_folio_are_chrome() {
        let items: Vec<TextItem> = (1..=4)
            .flat_map(|page| sheet(page, "A Running Head"))
            .collect();
        let chrome = Chrome::detect(&items, 4);
        for run in &items {
            let convicted = chrome.convicts(run);
            if run.text == "A Running Head" || run.text.parse::<u32>().is_ok() {
                assert!(convicted, "chrome: {:?} on page {}", run.text, run.page);
            } else {
                assert!(!convicted, "body: {:?} on page {}", run.text, run.page);
            }
        }
    }

    #[test]
    fn a_line_the_renderer_would_reorder_is_not_chrome() {
        // Nothing here asks the markdown anything, so a page whose runs a
        // renderer emits out of order has no furniture at all.
        let mut items = Vec::new();
        for page in 1..=3u32 {
            for row in 0..8u8 {
                let y = 700.0 - 16.0 * f32::from(row);
                items.push(item(
                    page,
                    &format!("left column row {row} page {page} of prose"),
                    72.0,
                    y,
                ));
                items.push(item(
                    page,
                    &format!("right column row {row} page {page} of prose"),
                    320.0,
                    y,
                ));
            }
        }
        let chrome = Chrome::detect(&items, 3);
        assert!(
            !items.iter().any(|run| chrome.convicts(run)),
            "two columns of prose carry no chrome"
        );
    }

    #[test]
    fn numbers_in_the_margin_are_chrome_and_the_same_numbers_inside_it_are_not() {
        let mut items = Vec::new();
        for page in 1..=3u32 {
            for row in 1..=10u8 {
                let y = 700.0 - 16.0 * f32::from(row);
                items.push(item(page, &row.to_string(), 40.0, y));
                items.push(item(
                    page,
                    &format!("body line {row} of page {page} with ordinary words"),
                    72.0,
                    y,
                ));
                // The same digits inside the text block: a cell, a marker,
                // a footnote reference. Not a margin numbering.
                items.push(item(page, &row.to_string(), 200.0, y));
            }
        }
        let chrome = Chrome::detect(&items, 3);
        for run in &items {
            let expected = run.x < 72.0;
            assert_eq!(
                chrome.convicts(run),
                expected,
                "{:?} at x{} on page {}",
                run.text,
                run.x,
                run.page
            );
        }
    }

    #[test]
    fn a_single_page_has_no_repetition_to_convict_on() {
        let items = sheet(1, "A Running Head");
        let chrome = Chrome::detect(&items, 1);
        assert!(
            !items
                .iter()
                .any(|run| chrome.convicts(run) && run.text.starts_with("body")),
            "no body line is ever chrome"
        );
    }

    #[test]
    fn a_folio_that_counts_up_is_one_shape() {
        assert_eq!(shape("Chapter 3, page 12"), shape("Chapter 3, page 13"));
        assert_eq!(shape("7"), "#");
        assert_ne!(shape("Results"), shape("Methods"));
    }

    #[test]
    fn nothing_is_chrome_without_the_report() {
        let items = sheet(1, "A Running Head");
        let chrome = Chrome::none();
        assert!(!items.iter().any(|run| chrome.convicts(run)));
    }
}
