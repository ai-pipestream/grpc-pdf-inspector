// SPDX-License-Identifier: Apache-2.0

//! Positioned text runs, from the library's items onto the wire.
//!
//! The extractor hands back one flat `Vec<TextItem>` for the whole
//! document, in page order, and each item knows its 1-indexed page. The
//! stream is per page, so the items are grouped here and nowhere else, and
//! the grouping is the only place that decides what "in order" means: the
//! extractor's own order, which is content-stream order plus the page's
//! link annotations and the document's form fields appended after them.
//!
//! Nothing in this module interprets a run. It converts, it groups, and it
//! leaves every judgement about what a run *means* to the markdown
//! renderer and the Document fold.

use std::collections::{BTreeMap, HashMap};

use pdf_inspector::TextItem;
use pdf_inspector::types::ItemType;

use crate::proto::v1 as pb;

/// Group the document's items by their 1-indexed page.
///
/// A `BTreeMap` rather than a `HashMap`: pages are emitted in ascending
/// order for the whole-document case, and a map that already knows its own
/// order is one less thing to sort.
#[must_use]
pub fn by_page(items: Vec<TextItem>) -> BTreeMap<u32, Vec<TextItem>> {
    let mut pages: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for item in items {
        pages.entry(item.page).or_default().push(item);
    }
    pages
}

/// The runs an invisible-inclusive extraction produced that the visible one
/// did not.
///
/// The parser has one switch for the invisible layer and it governs the
/// whole walk, so the only way to separate hidden runs from visible ones is
/// to hold both walks side by side and take the difference. It is a
/// multiset difference, keyed on the page, the text and the box rounded to
/// a hundredth of a point: a page that draws the same word twice keeps both
/// copies on the visible side and neither is reported.
///
/// Both walks read the same operators in the same order and the skip branch
/// advances the text matrix exactly as the keep branch does, so a visible
/// run has the same text at the same coordinates in both. The exception is
/// a hidden run close enough to a visible one for the extractor to join
/// them into a single item: the joined item is not in the visible walk, so
/// it reads as invisible. Its box and its text are still exactly what the
/// page drew, and reporting it is the conservative direction.
///
/// Image placeholders, link rectangles and form values are dropped: they
/// are not text the page drew, so "invisible" says nothing about them.
#[must_use]
pub fn only_invisible(visible: &[TextItem], with_invisible: Vec<TextItem>) -> Vec<TextItem> {
    let mut seen: HashMap<Key, usize> = HashMap::new();
    for item in visible {
        *seen.entry(key(item)).or_default() += 1;
    }
    with_invisible
        .into_iter()
        .filter(|item| matches!(item.item_type, ItemType::Text) && !item.text.trim().is_empty())
        .filter(|item| match seen.get_mut(&key(item)) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// What makes two runs of two walks the same run.
type Key = (u32, String, i64, i64);

/// A run's identity for [`only_invisible`].
fn key(item: &TextItem) -> Key {
    let hundredths = |value: f32| (f64::from(value) * 100.0).round() as i64;
    (
        item.page,
        item.text.clone(),
        hundredths(item.x),
        hundredths(item.y),
    )
}

/// One page's runs as the wire message.
#[must_use]
pub fn page_spans(page_no: u32, items: &[TextItem]) -> pb::PageSpans {
    page_spans_marking(page_no, items, |_| false)
}

/// One page's runs as the wire message, the ones `is_chrome` names flagged
/// as page chrome. Every run goes out, chrome included: the event is the
/// page as drawn, and the flag is what says which runs the rendering was
/// not given.
pub fn page_spans_marking(
    page_no: u32,
    items: &[TextItem],
    is_chrome: impl Fn(&TextItem) -> bool,
) -> pb::PageSpans {
    pb::PageSpans {
        page_no,
        spans: items
            .iter()
            .map(|item| pb::TextSpan {
                chrome: is_chrome(item),
                ..span(item)
            })
            .collect(),
    }
}

/// One run as the wire message.
#[must_use]
pub fn span(item: &TextItem) -> pb::TextSpan {
    // Width and height are extents, and a right-to-left or upward run can
    // hand back a negative one. The box is normalized here so a consumer
    // never has to wonder which corner `x`/`y` names.
    let (x, width) = normalize(item.x, item.width);
    let (y, height) = normalize(item.y, item.height);
    pb::TextSpan {
        text: item.text.clone(),
        bbox: Some(pb::Rect {
            x: f64::from(x),
            y: f64::from(y),
            width: f64::from(width),
            height: f64::from(height),
        }),
        font_family: item.font.clone(),
        font_tag: item.font_tag.clone(),
        font_size: item.font_size,
        bold: item.is_bold,
        italic: item.is_italic,
        underline: item.is_underline,
        strikeout: item.is_strikeout,
        mcid: item.mcid,
        kind: kind(&item.item_type).into(),
        link_uri: match &item.item_type {
            ItemType::Link(uri) => uri.clone(),
            _ => String::new(),
        },
        // Chrome is a verdict over the whole document, taken by the caller
        // that has it; one run on its own is never chrome.
        chrome: false,
    }
}

/// Map the library's item type onto the wire enum.
fn kind(item_type: &ItemType) -> pb::SpanKind {
    match item_type {
        ItemType::Text => pb::SpanKind::Text,
        ItemType::Image => pb::SpanKind::Image,
        ItemType::Link(_) => pb::SpanKind::Link,
        ItemType::FormField => pb::SpanKind::FormField,
    }
}

/// Turn an origin and a possibly-negative extent into an origin and a
/// non-negative extent.
fn normalize(origin: f32, extent: f32) -> (f32, f32) {
    if extent < 0.0 {
        (origin + extent, -extent)
    } else {
        (origin, extent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(page: u32, text: &str) -> TextItem {
        TextItem {
            text: text.to_owned(),
            x: 10.0,
            y: 700.0,
            width: 40.0,
            height: 12.0,
            font: "ABCDEF+Helvetica".to_owned(),
            font_tag: "F1".to_owned(),
            font_size: 12.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: Some(3),
        }
    }

    #[test]
    fn items_group_by_page_in_ascending_page_order() {
        let pages = by_page(vec![item(2, "b"), item(1, "a"), item(2, "c")]);
        let order: Vec<u32> = pages.keys().copied().collect();
        assert_eq!(order, [1, 2]);
        assert_eq!(pages[&2].len(), 2, "a page keeps every one of its runs");
        assert_eq!(pages[&2][0].text, "b", "extraction order is preserved");
    }

    #[test]
    fn a_run_carries_its_box_font_and_join_key() {
        let spans = page_spans(1, &[item(1, "hello")]);
        assert_eq!(spans.page_no, 1);
        let span = &spans.spans[0];
        assert_eq!(span.text, "hello");
        let bbox = span.bbox.as_ref().expect("a box");
        assert!((bbox.x - 10.0).abs() < f64::EPSILON);
        assert!((bbox.y - 700.0).abs() < f64::EPSILON);
        assert!((bbox.width - 40.0).abs() < f64::EPSILON);
        assert_eq!(span.font_family, "ABCDEF+Helvetica");
        assert_eq!(span.font_tag, "F1");
        assert_eq!(span.mcid, Some(3));
        assert_eq!(span.kind, pb::SpanKind::Text as i32);
    }

    #[test]
    fn a_link_run_carries_its_target_and_kind() {
        let mut link = item(1, "https://example.invalid/x");
        link.item_type = ItemType::Link("https://example.invalid/x".to_owned());
        let span = span(&link);
        assert_eq!(span.kind, pb::SpanKind::Link as i32);
        assert_eq!(span.link_uri, "https://example.invalid/x");
    }

    #[test]
    fn a_negative_extent_becomes_an_origin_and_a_positive_extent() {
        let mut backwards = item(1, "x");
        backwards.width = -20.0;
        let span = span(&backwards);
        let bbox = span.bbox.as_ref().expect("a box");
        assert!((bbox.x - -10.0).abs() < f64::EPSILON);
        assert!((bbox.width - 20.0).abs() < f64::EPSILON);
    }
}
