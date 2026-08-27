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
//! A search that fails is answered with nothing, never with a box. A wrong
//! box is worse than no box: it is a coordinate a downstream merge would
//! reconcile on.
//!
//! The same index answers the link question. A link annotation is a
//! rectangle and a target, and the runs whose boxes sit inside that
//! rectangle are its anchor text — so once a block's letters are matched to
//! its runs, the character range the link covers inside that block is
//! arithmetic rather than inference. That is the difference between
//! `InlineSpan.hyperlink` over the words someone actually linked and a
//! regular expression looking for URLs in the visible text, which finds a
//! bare URL nobody linked and misses a link whose anchor text is a word.
//!
//! The authored roles ride along on the same index. A tagged document
//! states what each marked-content region is, and every run carries the id
//! that names its region, so once a block is matched to its runs the
//! document's own word for it — `H2`, `LI`, `Code`, `Caption` — is in hand.
//! That word beats the number of `#` characters the markdown renderer
//! printed, because the renderer was guessing from type size and the
//! document was not.
//!
//! Internal links are anchored the same way and land on
//! `InlineSpan.target` instead, pointing at the page item the destination
//! resolves to. They reach this module from the metadata pass rather than
//! from the runs: the extractor reads `/A /URI` and nothing else, so a
//! table of contents, a cross-reference and a footnote jump are invisible
//! to it.
//!
//! Image placements ride it too. The content-stream walker emits a run for
//! every image XObject with the box the current transformation matrix put
//! it at, and the markdown renderer discards them by default, so
//! `Document.pictures` was structurally always empty. A run that is an
//! image is a picture with a box, and a link annotation over that box is
//! that picture's link.
//!
//! Emphasis rides the same machinery. Bold, italic, underline and strikeout
//! are per-run facts that markdown flattens into `**`, `*`, `<u>` and `<s>`
//! inside a paragraph's text; grouped by run and reported as spans they are
//! `Formatting` over the characters they actually cover, so a partially
//! bold paragraph stays recoverable. A run with nothing to say — no link,
//! no decoration — produces no span at all, which keeps "no span" meaning
//! "the item's own default".

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
    /// For each run, how it differs from plain body text: its link target
    /// and its decorations.
    styles: Vec<Style>,
    /// For each run, the role its marked-content region was tagged with,
    /// when the document is tagged and the run sits in one.
    roles: Vec<Option<(pb::StructureRole, String)>>,
    /// Which runs are image placements, in page order.
    images: Vec<usize>,
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
    pub fn new(
        spans: &pb::PageSpans,
        internal: &[pb::LinkTarget],
        structure: Option<&pb::PageStructure>,
    ) -> Self {
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
        let styles = styles(spans, &anchor_targets(spans, internal, &boxes));
        let roles = tagged_roles(spans, structure);
        let images = spans
            .spans
            .iter()
            .enumerate()
            .filter(|(_, span)| pb::SpanKind::try_from(span.kind) == Ok(pb::SpanKind::Image))
            .map(|(index, _)| index)
            .collect();
        Self {
            page_no: spans.page_no,
            letters,
            owners,
            boxes,
            styles,
            roles,
            images,
            cursor: 0,
        }
    }

    /// The page these runs came from.
    #[must_use]
    pub const fn page_no(&self) -> u32 {
        self.page_no
    }

    /// The image placements on this page, in the order the page draws
    /// them.
    #[must_use]
    pub fn pictures(&self) -> Vec<Picture> {
        self.images
            .iter()
            .map(|&index| {
                let anchor = self
                    .styles
                    .get(index)
                    .and_then(|style| style.anchor.as_ref());
                Picture {
                    bbox: self.boxes[index].clone(),
                    hyperlink: match anchor {
                        Some(Anchor::External(uri)) => Some(uri.clone()),
                        _ => None,
                    },
                    // An internal destination resolves the same way an
                    // anchored one does: onto the page item it lands on,
                    // which is an item of this fragment.
                    target: match anchor {
                        Some(Anchor::Internal(page_no)) => Some(doc::FineRef {
                            r#ref: page_ref(*page_no),
                            range: None,
                        }),
                        _ => None,
                    },
                }
            })
            .collect()
    }

    /// Where `run` sits among this page's runs, when it is one of them.
    ///
    /// Identity is the box: both this index and the run being looked up
    /// were built from the same extraction, so the same run has the same
    /// four numbers on both sides.
    #[must_use]
    pub fn index_of(&self, run: &pb::TextSpan) -> Option<usize> {
        let wanted = bounding_box(run.bbox.as_ref());
        self.boxes.iter().position(|box_| *box_ == wanted)
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
    pub fn locate(&mut self, text: &str) -> Located {
        // Which character of `text` each of its letters came from, so a
        // range measured in letters can be reported in the characters a
        // consumer will index `text` with.
        let mut needle = Vec::new();
        let mut columns = Vec::new();
        for (column, character) in text.chars().enumerate() {
            if !character.is_alphanumeric() {
                continue;
            }
            for lowered in character.to_lowercase() {
                needle.push(lowered);
                columns.push(column);
            }
        }
        if needle.is_empty() {
            return Located::default();
        }
        let Some(at) =
            find(&self.letters, &needle, self.cursor).or_else(|| find(&self.letters, &needle, 0))
        else {
            return Located::default();
        };
        let end = at + needle.len();
        self.cursor = self.cursor.max(end);
        let spans = self.inline_spans(at, end, &columns);
        Located {
            hyperlink: self.whole_block_link(at, end),
            bbox: self.union(at, end),
            spans,
            role: self.role(at, end),
            first_run: self.owners.get(at).copied(),
        }
    }

    /// The external link covering every letter of the block, when one
    /// does.
    ///
    /// A block that is entirely one link is a link in the upstream
    /// dialect's sense too, and saying so keeps the fragment readable to a
    /// consumer that has no inline spans. Decorations do not break this:
    /// a link whose second half is bold is still entirely a link.
    fn whole_block_link(&self, at: usize, end: usize) -> Option<String> {
        let mut target: Option<&str> = None;
        for &owner in &self.owners[at..end] {
            let uri = match self
                .styles
                .get(owner)
                .and_then(|style| style.anchor.as_ref())
            {
                Some(Anchor::External(uri)) => uri.as_str(),
                _ => return None,
            };
            match target {
                Some(known) if known != uri => return None,
                _ => target = Some(uri),
            }
        }
        target.map(ToOwned::to_owned)
    }

    /// The role the document gave the runs behind `letters[at..end]`.
    ///
    /// A block normally sits in one marked-content region and so has one
    /// role. When it straddles several, the one that covers the most
    /// letters wins, because that is the one the block mostly is.
    fn role(&self, at: usize, end: usize) -> Option<(pb::StructureRole, String)> {
        let mut tally: Vec<(&(pb::StructureRole, String), usize)> = Vec::new();
        for &owner in &self.owners[at..end] {
            let Some(role) = self.roles.get(owner).and_then(Option::as_ref) else {
                continue;
            };
            match tally.iter_mut().find(|(known, _)| *known == role) {
                Some((_, count)) => *count += 1,
                None => tally.push((role, 1)),
            }
        }
        tally
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(role, _)| role.clone())
    }

    /// The styled runs inside `letters[at..end]`, as character ranges into
    /// the block those letters came from.
    ///
    /// Consecutive letters sharing a style are one span. A style with
    /// nothing to say produces none, so a paragraph of plain body text
    /// carries no spans at all.
    fn inline_spans(&self, at: usize, end: usize, columns: &[usize]) -> Vec<doc::InlineSpan> {
        let mut spans: Vec<doc::InlineSpan> = Vec::new();
        let mut open: Option<(&Style, usize, usize)> = None;
        for (offset, &owner) in self.owners[at..end].iter().enumerate() {
            let style = self.styles.get(owner).filter(|style| !style.is_plain());
            match (&mut open, style) {
                (Some((known, _, last)), Some(style)) if *known == style => *last = offset,
                (open_run, style) => {
                    if let Some((style, first, last)) = open_run.take() {
                        spans.push(style.span(first, last, columns));
                    }
                    *open_run = style.map(|style| (style, offset, offset));
                }
            }
        }
        if let Some((style, first, last)) = open {
            spans.push(style.span(first, last, columns));
        }
        spans
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

/// One image placement on a page.
#[derive(Debug)]
pub struct Picture {
    /// Where the image was drawn.
    pub bbox: doc::BoundingBox,
    /// An external link annotation covering it, when one does.
    pub hyperlink: Option<String>,
    /// An internal destination covering it: a figure that jumps into the
    /// document rather than out of it.
    pub target: Option<doc::FineRef>,
}

/// One block of text, located among the runs that produced it.
#[derive(Debug, Default)]
pub struct Located {
    /// The box around those runs, when they were found.
    pub bbox: Option<doc::BoundingBox>,
    /// Link runs inside the block, as character ranges into its text.
    pub spans: Vec<doc::InlineSpan>,
    /// The link covering the whole block, when one does.
    pub hyperlink: Option<String>,
    /// The role the document gave the block, and the name it used, when the
    /// document is tagged.
    pub role: Option<(pb::StructureRole, String)>,
    /// Which of the page's runs the block starts on, which is where it
    /// stands in the order the page was drawn in.
    pub first_run: Option<usize>,
}

/// For each run, the role of the marked-content region it sits in.
fn tagged_roles(
    spans: &pb::PageSpans,
    structure: Option<&pb::PageStructure>,
) -> Vec<Option<(pb::StructureRole, String)>> {
    let Some(structure) = structure else {
        return vec![None; spans.spans.len()];
    };
    spans
        .spans
        .iter()
        .map(|span| {
            let mcid = span.mcid?;
            let element = structure
                .elements
                .iter()
                .find(|element| element.mcid == mcid)?;
            Some((
                pb::StructureRole::try_from(element.role).unwrap_or(pb::StructureRole::Unspecified),
                element.role_raw.clone(),
            ))
        })
        .collect()
}

/// Where a link annotation leads.
#[derive(Clone, PartialEq, Eq)]
enum Anchor {
    /// Out of the document, to a URI.
    External(String),
    /// Into the document, to a 1-indexed page.
    Internal(u32),
}

/// How one run differs from plain body text.
#[derive(Clone, Default, PartialEq)]
struct Style {
    /// The link annotation covering the run, when one does.
    anchor: Option<Anchor>,
    /// Whether the face is bold.
    bold: bool,
    /// Whether the face is italic.
    italic: bool,
    /// Whether a rule is drawn under the run.
    underline: bool,
    /// Whether a rule crosses it.
    strikeout: bool,
    /// The face the run is set in.
    font_family: String,
    /// Its type size in points.
    font_size: f32,
}

impl Style {
    /// Whether the run has nothing to say beyond being text.
    ///
    /// Font and size alone are not something to say: every run has them,
    /// and a span per run stating the body face would be noise the length
    /// of the document. They are reported on the spans that exist for
    /// another reason, where they qualify a run that already stands out.
    const fn is_plain(&self) -> bool {
        self.anchor.is_none() && !self.bold && !self.italic && !self.underline && !self.strikeout
    }

    /// This style over `columns[first..=last]` as an inline span.
    fn span(&self, first: usize, last: usize, columns: &[usize]) -> doc::InlineSpan {
        let start = columns.get(first).copied().unwrap_or(0);
        // The range is half-open and measured in characters, so it ends one
        // past the last character it covers.
        let end = columns.get(last).copied().unwrap_or(start) + 1;
        let decorated = self.bold || self.italic || self.underline || self.strikeout;
        let mut span = doc::InlineSpan {
            range: Some(doc::IntSpan {
                start: i32::try_from(start).unwrap_or(i32::MAX),
                end: i32::try_from(end).unwrap_or(i32::MAX),
            }),
            formatting: decorated.then(|| doc::Formatting {
                bold: self.bold,
                italic: self.italic,
                underline: self.underline,
                strikethrough: self.strikeout,
                ..doc::Formatting::default()
            }),
            font_family: (!self.font_family.is_empty()).then(|| self.font_family.clone()),
            font_size_pt: (self.font_size > 0.0).then(|| f64::from(self.font_size)),
            ..doc::InlineSpan::default()
        };
        match &self.anchor {
            Some(Anchor::External(uri)) => span.hyperlink = Some(uri.clone()),
            // An internal destination points at a page item, which is an
            // item of this fragment: `pages` is keyed by page number, so
            // the pointer resolves without knowing which text item happens
            // to sit there.
            Some(Anchor::Internal(page_no)) => {
                span.target = Some(doc::FineRef {
                    r#ref: page_ref(*page_no),
                    range: None,
                });
            }
            None => {}
        }
        span
    }
}

/// Each run's style: its link target, if any, and how it is set.
fn styles(spans: &pb::PageSpans, anchors: &[Option<Anchor>]) -> Vec<Style> {
    spans
        .spans
        .iter()
        .enumerate()
        .map(|(index, span)| Style {
            anchor: anchors.get(index).and_then(Clone::clone),
            bold: span.bold,
            italic: span.italic,
            underline: span.underline,
            strikeout: span.strikeout,
            font_family: span.font_family.clone(),
            font_size: span.font_size,
        })
        .collect()
}

/// The JSON-Pointer reference of one page item.
#[must_use]
pub fn page_ref(page_no: u32) -> String {
    format!("#/pages/{page_no}")
}

/// For each run, the target of the link annotation covering it.
///
/// A run is covered when its box's centre lies inside the annotation's
/// rectangle. Centres rather than edges because an annotation is drawn to
/// the anchor's visual extent, which routinely clips a glyph's box by a
/// fraction of a point at either end.
fn anchor_targets(
    spans: &pb::PageSpans,
    internal: &[pb::LinkTarget],
    boxes: &[doc::BoundingBox],
) -> Vec<Option<Anchor>> {
    // External annotations arrive as runs of their own; internal ones do
    // not reach the extractor at all and come from the metadata pass.
    let mut annotations: Vec<(doc::BoundingBox, Anchor)> = spans
        .spans
        .iter()
        .enumerate()
        .filter(|(_, span)| {
            pb::SpanKind::try_from(span.kind) == Ok(pb::SpanKind::Link) && !span.link_uri.is_empty()
        })
        .filter_map(|(index, span)| {
            Some((
                boxes.get(index)?.clone(),
                Anchor::External(span.link_uri.clone()),
            ))
        })
        .collect();
    annotations.extend(
        internal
            .iter()
            .filter(|link| link.uri.is_empty() && link.dest_page_no > 0)
            .map(|link| {
                (
                    bounding_box(link.rect.as_ref()),
                    Anchor::Internal(link.dest_page_no),
                )
            }),
    );
    if annotations.is_empty() {
        return vec![None; boxes.len()];
    }
    spans
        .spans
        .iter()
        .zip(boxes)
        .map(|(span, run)| {
            // An annotation does not anchor itself; anything else the page
            // drew inside its rectangle does, text and images alike.
            if pb::SpanKind::try_from(span.kind) == Ok(pb::SpanKind::Link) {
                return None;
            }
            let x = f64::midpoint(run.l, run.r);
            let y = f64::midpoint(run.b, run.t);
            annotations
                .iter()
                .find(|(rect, _)| x >= rect.l && x <= rect.r && y >= rect.b && y <= rect.t)
                .map(|(_, anchor)| anchor.clone())
        })
        .collect()
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

    /// One page's runs indexed with no internal links, which is every case
    /// but the one test that supplies them.
    fn index(spans: pb::PageSpans) -> PageRuns {
        PageRuns::new(&spans, &[], None)
    }

    #[test]
    fn a_block_spanning_two_runs_gets_the_box_around_both() {
        let mut runs = index(page(vec![
            span("Hello", 10.0, 700.0, pb::SpanKind::Text),
            span("world", 60.0, 700.0, pb::SpanKind::Text),
        ]));
        let bbox = runs
            .locate("Hello world")
            .bbox
            .expect("both runs are found");
        assert!((bbox.l - 10.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.r - 100.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.b - 700.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!((bbox.t - 710.0).abs() < f64::EPSILON, "{bbox:?}");
        assert_eq!(bbox.coord_origin, Some(doc::CoordOrigin::Bottomleft as i32));
    }

    #[test]
    fn markdown_decoration_does_not_move_the_match() {
        let mut runs = index(page(vec![span(
            "A Heading",
            10.0,
            700.0,
            pb::SpanKind::Text,
        )]));
        assert!(
            runs.locate("## **A Heading**").bbox.is_some(),
            "hashes and asterisks are not letters"
        );
    }

    #[test]
    fn two_blocks_match_their_own_runs_not_each_others() {
        let mut runs = index(page(vec![
            span("alpha", 10.0, 700.0, pb::SpanKind::Text),
            span("alpha", 10.0, 600.0, pb::SpanKind::Text),
        ]));
        let first = runs.locate("alpha").bbox.expect("the first run");
        let second = runs.locate("alpha").bbox.expect("the second run");
        assert!((first.b - 700.0).abs() < f64::EPSILON);
        assert!(
            (second.b - 600.0).abs() < f64::EPSILON,
            "the cursor moved past the first match: {second:?}"
        );
    }

    #[test]
    fn text_that_is_not_on_the_page_gets_no_box() {
        let mut runs = index(page(vec![span("alpha", 10.0, 700.0, pb::SpanKind::Text)]));
        assert!(
            runs.locate("beta").bbox.is_none(),
            "no box is better than a wrong one"
        );
        assert!(
            runs.locate("   ").bbox.is_none(),
            "whitespace locates nothing"
        );
    }

    /// A link annotation covering the box at `y`, one line tall.
    fn annotation(y: f64, uri: &str) -> pb::TextSpan {
        pb::TextSpan {
            text: uri.to_owned(),
            bbox: Some(pb::Rect {
                x: 5.0,
                y: y - 2.0,
                width: 60.0,
                height: 14.0,
            }),
            kind: pb::SpanKind::Link.into(),
            link_uri: uri.to_owned(),
            ..pb::TextSpan::default()
        }
    }

    #[test]
    fn a_link_covers_only_the_runs_under_its_rectangle() {
        let mut runs = index(page(vec![
            span("Please ", 10.0, 700.0, pb::SpanKind::Text),
            span("click here", 10.0, 690.0, pb::SpanKind::Text),
            span(" now", 10.0, 680.0, pb::SpanKind::Text),
            annotation(690.0, "https://example.invalid/x"),
        ]));
        let located = runs.locate("Please click here now");
        assert_eq!(located.spans.len(), 1, "{:?}", located.spans);
        let span = &located.spans[0];
        assert_eq!(span.hyperlink.as_deref(), Some("https://example.invalid/x"));
        let range = span.range.as_ref().expect("a range");
        let text: Vec<char> = "Please click here now".chars().collect();
        let linked: String = text[range.start as usize..range.end as usize]
            .iter()
            .collect();
        assert_eq!(linked, "click here");
        assert!(
            located.hyperlink.is_none(),
            "a partially linked block is not a link at item level"
        );
    }

    #[test]
    fn a_block_that_is_all_one_link_is_a_link_at_item_level() {
        let mut runs = index(page(vec![
            span("click here", 10.0, 690.0, pb::SpanKind::Text),
            annotation(690.0, "https://example.invalid/y"),
        ]));
        let located = runs.locate("click here");
        assert_eq!(
            located.hyperlink.as_deref(),
            Some("https://example.invalid/y")
        );
        assert_eq!(located.spans.len(), 1);
    }

    #[test]
    fn two_annotations_in_one_block_stay_two_runs() {
        let mut runs = index(page(vec![
            span("first", 10.0, 700.0, pb::SpanKind::Text),
            span("second", 10.0, 690.0, pb::SpanKind::Text),
            annotation(700.0, "https://example.invalid/1"),
            annotation(690.0, "https://example.invalid/2"),
        ]));
        let located = runs.locate("first second");
        let targets: Vec<&str> = located
            .spans
            .iter()
            .map(|span| span.hyperlink.as_deref().expect("a target"))
            .collect();
        assert_eq!(
            targets,
            ["https://example.invalid/1", "https://example.invalid/2"]
        );
    }

    /// One run set in a decorated face.
    fn decorated(text: &str, y: f64, bold: bool, italic: bool) -> pb::TextSpan {
        let mut span = span(text, 10.0, y, pb::SpanKind::Text);
        span.bold = bold;
        span.italic = italic;
        span.font_family = if bold { "Helvetica-Bold" } else { "Helvetica" }.to_owned();
        span.font_size = 11.0;
        span
    }

    #[test]
    fn a_decorated_run_becomes_a_formatting_span_over_its_own_characters() {
        let mut runs = index(page(vec![
            decorated("plain ", 700.0, false, false),
            decorated("bold", 690.0, true, false),
            decorated(" plain", 680.0, false, false),
        ]));
        let located = runs.locate("plain bold plain");
        assert_eq!(located.spans.len(), 1, "{:?}", located.spans);
        let span = &located.spans[0];
        let formatting = span.formatting.as_ref().expect("a decorated run says so");
        assert!(formatting.bold);
        assert_eq!(span.font_family.as_deref(), Some("Helvetica-Bold"));
        assert_eq!(span.font_size_pt, Some(11.0));

        let range = span.range.as_ref().expect("a range");
        let text: Vec<char> = "plain bold plain".chars().collect();
        let covered: String = text[range.start as usize..range.end as usize]
            .iter()
            .collect();
        assert_eq!(covered, "bold");
    }

    #[test]
    fn undecorated_unlinked_runs_produce_no_spans() {
        let mut runs = index(page(vec![
            decorated("all", 700.0, false, false),
            decorated(" plain", 690.0, false, false),
        ]));
        assert!(runs.locate("all plain").spans.is_empty());
    }

    #[test]
    fn a_link_that_is_also_bold_is_one_span_carrying_both() {
        let mut runs = index(page(vec![
            decorated("click here", 690.0, true, false),
            annotation(690.0, "https://example.invalid/z"),
        ]));
        let located = runs.locate("click here");
        assert_eq!(located.spans.len(), 1, "{:?}", located.spans);
        let span = &located.spans[0];
        assert_eq!(span.hyperlink.as_deref(), Some("https://example.invalid/z"));
        assert!(span.formatting.as_ref().expect("formatting").bold);
        assert_eq!(
            located.hyperlink.as_deref(),
            Some("https://example.invalid/z"),
            "a decoration inside a link does not stop the block being a link"
        );
    }

    #[test]
    fn a_link_whose_second_half_is_bold_is_still_wholly_a_link() {
        let mut runs = index(page(vec![
            decorated("click ", 700.0, false, false),
            decorated("here", 690.0, true, false),
            annotation(700.0, "https://example.invalid/w"),
            annotation(690.0, "https://example.invalid/w"),
        ]));
        let located = runs.locate("click here");
        assert_eq!(located.spans.len(), 2, "the decoration changes mid-link");
        assert_eq!(
            located.hyperlink.as_deref(),
            Some("https://example.invalid/w")
        );
    }

    #[test]
    fn an_internal_destination_becomes_a_target_not_a_hyperlink() {
        let spans = page(vec![span(
            "see chapter two",
            10.0,
            690.0,
            pb::SpanKind::Text,
        )]);
        let internal = [pb::LinkTarget {
            page_no: 1,
            rect: Some(pb::Rect {
                x: 5.0,
                y: 688.0,
                width: 60.0,
                height: 14.0,
            }),
            dest_page_no: 7,
            ..pb::LinkTarget::default()
        }];
        let mut runs = PageRuns::new(&spans, &internal, None);
        let located = runs.locate("see chapter two");
        assert_eq!(located.spans.len(), 1);
        let target = located.spans[0].target.as_ref().expect("a target");
        assert_eq!(target.r#ref, "#/pages/7");
        assert!(
            located.spans[0].hyperlink.is_none(),
            "an internal jump is not a URL"
        );
        assert!(located.hyperlink.is_none());
    }

    #[test]
    fn an_image_run_becomes_a_picture_with_its_box() {
        let mut image = span("[Image: Im1]", 72.0, 120.0, pb::SpanKind::Image);
        image.bbox = Some(pb::Rect {
            x: 72.0,
            y: 120.0,
            width: 120.0,
            height: 80.0,
        });
        let runs = index(page(vec![
            span("prose beside it", 10.0, 700.0, pb::SpanKind::Text),
            image,
        ]));
        let pictures = runs.pictures();
        assert_eq!(pictures.len(), 1, "one image, one picture");
        assert!((pictures[0].bbox.l - 72.0).abs() < f64::EPSILON);
        assert!((pictures[0].bbox.t - 200.0).abs() < f64::EPSILON);
        assert!(pictures[0].hyperlink.is_none(), "nothing links to it");
        assert!(pictures[0].target.is_none());
    }

    #[test]
    fn a_link_over_a_picture_lands_on_the_picture() {
        let mut image = span("[Image: Im1]", 72.0, 120.0, pb::SpanKind::Image);
        image.bbox = Some(pb::Rect {
            x: 72.0,
            y: 120.0,
            width: 120.0,
            height: 80.0,
        });
        let mut annotation = span("uri", 72.0, 120.0, pb::SpanKind::Link);
        annotation.bbox = Some(pb::Rect {
            x: 70.0,
            y: 118.0,
            width: 124.0,
            height: 84.0,
        });
        annotation.link_uri = "https://example.invalid/figure".to_owned();
        let runs = index(page(vec![image, annotation]));
        assert_eq!(
            runs.pictures()[0].hyperlink.as_deref(),
            Some("https://example.invalid/figure")
        );
        assert!(runs.pictures()[0].target.is_none(), "out, not in");
    }

    #[test]
    fn an_internal_destination_over_a_picture_lands_on_the_picture() {
        let mut image = span("[Image: Im1]", 72.0, 120.0, pb::SpanKind::Image);
        image.bbox = Some(pb::Rect {
            x: 72.0,
            y: 120.0,
            width: 120.0,
            height: 80.0,
        });
        let internal = [pb::LinkTarget {
            page_no: 1,
            rect: Some(pb::Rect {
                x: 70.0,
                y: 118.0,
                width: 124.0,
                height: 84.0,
            }),
            dest_page_no: 4,
            ..pb::LinkTarget::default()
        }];
        let runs = PageRuns::new(&page(vec![image]), &internal, None);
        let pictures = runs.pictures();
        assert_eq!(
            pictures[0].target.as_ref().expect("a target").r#ref,
            "#/pages/4"
        );
        assert!(
            pictures[0].hyperlink.is_none(),
            "a jump inside the document is not a URL"
        );
    }

    #[test]
    fn a_page_that_draws_nothing_has_no_pictures() {
        let runs = index(page(vec![span("prose", 10.0, 700.0, pb::SpanKind::Text)]));
        assert!(runs.pictures().is_empty());
    }

    #[test]
    fn a_link_annotations_url_does_not_shift_the_page_letters() {
        // The link run's text is its URL, which the markdown renderer does
        // not emit. Letting it into the index would push every later block
        // onto the wrong runs.
        let mut runs = index(page(vec![
            span("https://example.invalid", 10.0, 700.0, pb::SpanKind::Link),
            span("click here", 10.0, 700.0, pb::SpanKind::Text),
        ]));
        let bbox = runs.locate("click here").bbox.expect("the text run");
        assert!((bbox.b - 700.0).abs() < f64::EPSILON, "{bbox:?}");
    }
}
