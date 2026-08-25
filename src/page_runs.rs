// SPDX-License-Identifier: Apache-2.0

//! Locating a block of rendered markdown back among the runs it came from.
//!
//! The Document fold reads markdown, and the boxes live on the positioned
//! runs the markdown was rendered from. Both describe the same page in the
//! same order, so the join is a search rather than a guess: normalize both
//! sides to their letters and digits, find where the block's letters occur
//! in the page's letters, and the runs that contributed those letters are
//! the runs the block came from.
//!
//! Normalizing away everything that is not alphanumeric is what makes this
//! survive the rendering. Markdown adds `#`, `**`, `- ` and pipe
//! characters; the renderer joins runs with spaces, fixes hyphenation
//! across line ends, and drops folios. None of that changes the letters, so
//! none of it moves the match.
//!
//! A search that fails is answered with `None`, never with a box. A wrong
//! box is worse than no box: it is a coordinate a downstream merge would
//! reconcile on.

use crate::proto::ai::pipestream::document::v1 as doc;
use crate::proto::v1 as pb;

/// One page's positioned runs, indexed for lookup by text.
pub struct PageRuns {
    /// The 1-indexed page these runs came from.
    page_no: u32,
    /// Every alphanumeric character of the page, runs concatenated in
    /// extraction order, lower-cased.
    letters: Vec<char>,
    /// For each character in `letters`, which run contributed it.
    owners: Vec<usize>,
    /// Each run's box, in the same order the runs arrived.
    boxes: Vec<doc::BoundingBox>,
    /// How far into `letters` the fold has already matched. Blocks are
    /// folded in reading order, so a search normally succeeds at the
    /// cursor and never revisits the page.
    cursor: usize,
}

impl PageRuns {
    /// Index one page's runs.
    ///
    /// Image placeholders and link annotations are indexed for their boxes
    /// but contribute no letters: their `text` is a placeholder or a URL
    /// that the markdown renderer does not emit, so letting it into the
    /// page's letters would shift every match after it.
    #[must_use]
    pub fn new(spans: &pb::PageSpans) -> Self {
        let mut letters = Vec::new();
        let mut owners = Vec::new();
        let mut boxes = Vec::with_capacity(spans.spans.len());
        for (index, span) in spans.spans.iter().enumerate() {
            boxes.push(bounding_box(span.bbox.as_ref()));
            if !contributes_text(span) {
                continue;
            }
            for character in span.text.chars().filter(|c| c.is_alphanumeric()) {
                for lowered in character.to_lowercase() {
                    letters.push(lowered);
                    owners.push(index);
                }
            }
        }
        Self {
            page_no: spans.page_no,
            letters,
            owners,
            boxes,
            cursor: 0,
        }
    }

    /// The page these runs came from.
    #[must_use]
    pub const fn page_no(&self) -> u32 {
        self.page_no
    }

    /// The box enclosing the runs that produced `text`, when they can be
    /// found.
    ///
    /// The search starts at the cursor, which is where the previous block
    /// ended, and falls back to the whole page. The fallback matters for
    /// tables and multi-column pages, where the renderer reorders runs; it
    /// does not move the cursor backwards, because a block found behind the
    /// cursor is evidence of reordering rather than a reason to re-read the
    /// page.
    pub fn locate(&mut self, text: &str) -> Option<doc::BoundingBox> {
        let needle: Vec<char> = text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if needle.is_empty() {
            return None;
        }
        let at = find(&self.letters, &needle, self.cursor)
            .or_else(|| find(&self.letters, &needle, 0))?;
        let end = at + needle.len();
        self.cursor = self.cursor.max(end);
        self.union(at, end)
    }

    /// The union of the boxes of every run contributing to `letters[at..end]`.
    fn union(&self, at: usize, end: usize) -> Option<doc::BoundingBox> {
        let mut hull: Option<doc::BoundingBox> = None;
        let mut last = usize::MAX;
        for &owner in &self.owners[at..end] {
            if owner == last {
                continue;
            }
            last = owner;
            let run = self.boxes.get(owner)?;
            hull = Some(match hull {
                None => run.clone(),
                Some(hull) => merge(&hull, run),
            });
        }
        hull
    }
}

/// Whether a run's text is part of what the markdown renderer emitted.
fn contributes_text(span: &pb::TextSpan) -> bool {
    matches!(
        pb::SpanKind::try_from(span.kind),
        Ok(pb::SpanKind::Text | pb::SpanKind::FormField)
    )
}

/// A wire rectangle as a schema bounding box.
///
/// The wire's rectangle is an origin and two extents in PDF user space; the
/// schema's is four edges plus the corner they are measured from. `t` is
/// the top edge and `b` the bottom, which with a bottom-left origin means
/// `t` is the larger number.
fn bounding_box(rect: Option<&pb::Rect>) -> doc::BoundingBox {
    let rect = rect.cloned().unwrap_or_default();
    doc::BoundingBox {
        l: rect.x,
        t: rect.y + rect.height,
        r: rect.x + rect.width,
        b: rect.y,
        coord_origin: Some(doc::CoordOrigin::Bottomleft as i32),
        coord_origin_raw: None,
    }
}

/// The smallest box containing both.
fn merge(left: &doc::BoundingBox, right: &doc::BoundingBox) -> doc::BoundingBox {
    doc::BoundingBox {
        l: left.l.min(right.l),
        t: left.t.max(right.t),
        r: left.r.max(right.r),
        b: left.b.min(right.b),
        coord_origin: left.coord_origin,
        coord_origin_raw: left.coord_origin_raw.clone(),
    }
}

/// Index of `needle` in `haystack` at or after `from`.
fn find(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&at| haystack[at..at + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x: f64, y: f64, kind: pb::SpanKind) -> pb::TextSpan {
        pb::TextSpan {
            text: text.to_owned(),
            bbox: Some(pb::Rect {
                x,
                y,
                width: 40.0,
                height: 10.0,
            }),
            kind: kind.into(),
            ..pb::TextSpan::default()
        }
    }

    fn page(spans: Vec<pb::TextSpan>) -> pb::PageSpans {
        pb::PageSpans { page_no: 1, spans }
    }

    #[test]
    fn a_block_spanning_two_runs_gets_the_box_around_both() {
        let mut runs = PageRuns::new(&page(vec![
            span("Hello", 10.0, 700.0, pb::SpanKind::Text),
            span("world", 60.0, 700.0, pb::SpanKind::Text),
        ]));
        let bbox = runs.locate("Hello world").expect("both runs are found");
        assert!((bbox.l - 10.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.r - 100.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.b - 700.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.t - 710.0).abs() < f64::EPSILON, "{bbox:?}");
        assert_eq!(bbox.coord_origin, Some(doc::CoordOrigin::Bottomleft as i32));
    }

    #[test]
    fn markdown_decoration_does_not_move_the_match() {
        let mut runs = PageRuns::new(&page(vec![span(
            "A Heading",
            10.0,
            700.0,
            pb::SpanKind::Text,
        )]));
        assert!(
            runs.locate("## **A Heading**").is_some(),
            "hashes and asterisks are not letters"
        );
    }

    #[test]
    fn two_blocks_match_their_own_runs_not_each_others() {
        let mut runs = PageRuns::new(&page(vec![
            span("alpha", 10.0, 700.0, pb::SpanKind::Text),
            span("alpha", 10.0, 600.0, pb::SpanKind::Text),
        ]));
        let first = runs.locate("alpha").expect("the first run");
        let second = runs.locate("alpha").expect("the second run");
        assert!((first.b - 700.0).abs() < f64::EPSILON);
        assert!(
            (second.b - 600.0).abs() < f64::EPSILON,
            "the cursor moved past the first match: {second:?}"
        );
    }

    #[test]
    fn text_that_is_not_on_the_page_gets_no_box() {
        let mut runs = PageRuns::new(&page(vec![span("alpha", 10.0, 700.0, pb::SpanKind::Text)]));
        assert!(
            runs.locate("beta").is_none(),
            "no box is better than a wrong one"
        );
        assert!(runs.locate("   ").is_none(), "whitespace locates nothing");
    }

    #[test]
    fn a_link_annotations_url_does_not_shift_the_page_letters() {
        // The link run's text is its URL, which the markdown renderer does
        // not emit. Letting it into the index would push every later block
        // onto the wrong runs.
        let mut runs = PageRuns::new(&page(vec![
            span("https://example.invalid", 10.0, 700.0, pb::SpanKind::Link),
            span("click here", 10.0, 700.0, pb::SpanKind::Text),
        ]));
        let bbox = runs.locate("click here").expect("the text run");
        assert!((bbox.b - 700.0).abs() < f64::EPSILON, "{bbox:?}");
    }
}
