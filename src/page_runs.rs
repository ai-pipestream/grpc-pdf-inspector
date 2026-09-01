// SPDX-License-Identifier: Apache-2.0

//! Locating a block of rendered markdown back among the runs it came from.
//!
//! The Document fold reads markdown, and the boxes live on the positioned
//! runs the markdown was rendered from. Both describe the same page, so
//! the join is a search rather than a guess: normalize both sides to their
//! letters and digits, follow the block's letters from run to run, and the
//! runs that spelled them are the runs the block came from.
//!
//! It is a chain of runs and not one stretch of the page, because the
//! extractor does not deliver a page in reading order. It delivers it line
//! by line across every column at once, so on a two-column page the lines
//! of one paragraph arrive interleaved with the lines of the paragraph
//! beside it, and no paragraph of more than one line is a contiguous
//! stretch of anything. Followed run by run it is still a chain, and the
//! chain is what carries the box.
//!
//! The same chain says which runs each letter of the block came from,
//! which is how a block the renderer assembled out of two blocks set side
//! by side is taken apart again ([`PageRuns::side_by_side`]).
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

use std::collections::HashSet;

use crate::proto::ai::pipestream::document::v1 as doc;
use crate::proto::v1 as pb;

/// The most candidate runs one block's search may examine before it gives
/// up. A block the rendering has diverged from is answered with nothing
/// rather than with a page-sized backtracking search.
const SEARCH_BUDGET: usize = 2_000_000;

/// How wide a gap between two groups of a block's runs must be, in points,
/// before it is a gutter between two blocks set side by side rather than
/// the space between two words.
const MIN_GUTTER: f64 = 6.0;

/// How far one run's centre must sit above another's, in points, before the
/// two are on different lines of the page.
const LINE_SEPARATION: f64 = 1.0;

/// The smallest Form XObject placement, in square points, that is a
/// figure rather than a glyph, a rule or a decoration drawn as a form.
const MIN_FIGURE_AREA: f64 = 400.0;

/// The share of the page a form may cover before it is the page's own
/// wrapper, the shape print-to-PDF producers give a page whose whole
/// content is one form, rather than a figure on it.
const PAGE_WRAPPER_SHARE: f64 = 0.9;

/// The share of a box that must lie inside another for the first to be
/// inside the second.
const INSIDE_SHARE: f64 = 0.9;

/// One page's positioned runs, indexed for lookup by text.
pub struct PageRuns {
    /// The 1-indexed page these runs came from.
    page_no: u32,
    /// Each run's alphanumeric characters, lower-cased, in the order the
    /// runs arrived. A run that contributes no text has none.
    letters: Vec<Vec<char>>,
    /// How many of each run's letters the blocks folded so far have
    /// claimed, counted from the run's start. A block never takes a letter
    /// another block already has, which is what keeps two identical lines
    /// apart.
    claimed: Vec<usize>,
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
    /// Which runs are Form XObject placements, in page order.
    forms: Vec<usize>,
    /// The runs the page's chrome report claimed, in page order, each with
    /// its trimmed text: the furniture lines of the page event are these
    /// runs' texts in this order.
    chrome: Vec<(String, usize)>,
    /// The run the last block ended on. Blocks are folded in reading
    /// order, so the next block normally begins at or shortly after it and
    /// the search starts there before it looks at the rest of the page.
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
        let mut letters = Vec::with_capacity(spans.spans.len());
        let mut boxes = Vec::with_capacity(spans.spans.len());
        for span in &spans.spans {
            boxes.push(bounding_box(span.bbox.as_ref()));
            letters.push(if contributes_text(span) {
                alphanumeric(&span.text).collect()
            } else {
                Vec::new()
            });
        }
        let styles = styles(spans, &anchor_targets(spans, internal, &boxes));
        let roles = tagged_roles(spans, structure);
        let of_kind = |kind: pb::SpanKind| -> Vec<usize> {
            spans
                .spans
                .iter()
                .enumerate()
                .filter(|(_, span)| pb::SpanKind::try_from(span.kind) == Ok(kind))
                .map(|(index, _)| index)
                .collect()
        };
        let images = of_kind(pb::SpanKind::Image);
        let forms = of_kind(pb::SpanKind::Form);
        let chrome = spans
            .spans
            .iter()
            .enumerate()
            .filter(|(_, span)| span.chrome)
            .map(|(index, span)| (span.text.trim().to_owned(), index))
            .collect();
        Self {
            page_no: spans.page_no,
            claimed: vec![0; letters.len()],
            letters,
            boxes,
            styles,
            roles,
            images,
            forms,
            chrome,
            cursor: 0,
        }
    }

    /// The `ordinal`-th chrome run of the page: its trimmed text and its
    /// box. The page event lists its furniture lines in the order the
    /// chrome runs were drawn, so the line and the run pair by position;
    /// the text is returned so the caller can check that they do.
    #[must_use]
    pub fn chrome_run(&self, ordinal: usize) -> Option<(&str, doc::BoundingBox)> {
        self.chrome
            .get(ordinal)
            .map(|(text, index)| (text.as_str(), self.boxes[*index].clone()))
    }

    /// The page these runs came from.
    #[must_use]
    pub const fn page_no(&self) -> u32 {
        self.page_no
    }

    /// The pictures on this page, in the order the page draws them: every
    /// image placement, and every Form XObject placement that is a figure.
    ///
    /// A form is a figure when it is a region of the page and not the
    /// page: not the wrapper a print-to-PDF producer draws a whole page
    /// through, which covers the page (`page` is its size, when the
    /// metadata pass measured it); not a glyph or a rule drawn as a form;
    /// not a form drawn inside another form that already counts; and not
    /// a form whose content is an image the page already reports, which
    /// would be the same picture twice.
    #[must_use]
    pub fn pictures(&self, page: Option<&doc::Size>) -> Vec<Picture> {
        let page_area = page.map(|size| size.width * size.height);
        let mut kept: Vec<usize> = Vec::new();
        let mut by_area: Vec<usize> = self.forms.clone();
        by_area
            .sort_by(|left, right| area(&self.boxes[*right]).total_cmp(&area(&self.boxes[*left])));
        for index in by_area {
            let bbox = &self.boxes[index];
            let extent = area(bbox);
            if extent < MIN_FIGURE_AREA
                || page_area.is_some_and(|page| extent >= page * PAGE_WRAPPER_SHARE)
                || kept.iter().any(|&outer| inside(bbox, &self.boxes[outer]))
                || self
                    .images
                    .iter()
                    .any(|&image| inside(&self.boxes[image], bbox))
            {
                continue;
            }
            kept.push(index);
        }
        let mut placements: Vec<usize> = self.images.iter().copied().chain(kept).collect();
        placements.sort_unstable();
        placements
            .into_iter()
            .map(|index| {
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

    /// The box of the run at `index`, when the page has one there.
    #[must_use]
    pub fn box_of(&self, index: usize) -> Option<&doc::BoundingBox> {
        self.boxes.get(index)
    }

    /// The box enclosing the runs that produced `text`, when they can be
    /// found.
    ///
    /// A block is matched as a chain of runs read in order, not as one
    /// stretch of the page's letters. The two differ on any page whose
    /// runs are not in reading order, which is every page set in columns:
    /// the extractor delivers a page line by line across all its columns,
    /// so the lines of one paragraph arrive interleaved with the lines of
    /// the paragraph beside it, and a paragraph of more than one line is
    /// never one stretch of the page. Followed run by run, it is still a
    /// chain, and the chain is what is looked for.
    ///
    /// The search starts at the run the previous block ended on and falls
    /// back to the whole page. Runs a previous block claimed are not
    /// offered again, so the second of two identical lines matches its own
    /// run rather than the first line's.
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
        let Some(pieces) = self.chain(&needle) else {
            return Located::default();
        };
        let mut owners = Vec::with_capacity(needle.len());
        for piece in &pieces {
            self.claimed[piece.run] = piece.start + piece.len;
            owners.extend(std::iter::repeat_n(piece.run, piece.len));
        }
        if let Some(last) = pieces.last() {
            self.cursor = last.run;
        }
        self.located(owners, columns)
    }

    /// The chain of runs whose letters, read in order, spell `needle`.
    ///
    /// The first piece may begin inside its run, because the rendering
    /// drops list markers and folios that the run still carries; the last
    /// piece may end inside its run, because the rendering splits a line
    /// where the page did not. Every piece between takes a whole run.
    ///
    /// Runs are tried from their unclaimed beginning first, all of them,
    /// before any run is tried from the middle. A short block, a figure's
    /// one-word label, occurs inside some longer run of most pages by
    /// accident, and a match that starts there takes letters the run's
    /// own block needs later; the run that begins with the block is the
    /// one it came from.
    fn chain(&self, needle: &[char]) -> Option<Vec<Piece>> {
        let count = self.letters.len();
        let cursor = self.cursor.min(count);
        let mut budget = SEARCH_BUDGET;
        let mut failed = HashSet::new();
        for mid_run in [false, true] {
            for run in (cursor..count).chain(0..cursor) {
                let letters = &self.letters[run];
                let from = self.claimed[run];
                let starts = if mid_run {
                    from + 1..letters.len()
                } else {
                    from..(from + 1).min(letters.len())
                };
                for start in starts {
                    budget = budget.checked_sub(1)?;
                    if letters[start] != needle[0] {
                        continue;
                    }
                    let mut search = Search {
                        needle,
                        budget: &mut budget,
                        used: vec![run],
                        failed: &mut failed,
                    };
                    if let Some(pieces) = self.extend(&mut search, 0, run, start) {
                        return Some(pieces);
                    }
                }
            }
        }
        None
    }

    /// Match `needle[offset..]` against `letters[run][start..]` and, when
    /// the needle outlasts the run, against further runs.
    ///
    /// The next run is looked for after this one in page order first and
    /// before it second: the next line of a paragraph is a line or two
    /// further down the page, however many lines of the neighbouring
    /// column the extractor put between them. A continuation that failed
    /// once is not tried again from another path: a page of repeated
    /// tokens, a code listing or a formula, would otherwise be searched
    /// once per way of reaching each of them.
    fn extend(
        &self,
        search: &mut Search<'_>,
        offset: usize,
        run: usize,
        start: usize,
    ) -> Option<Vec<Piece>> {
        let available = &self.letters[run][start..];
        let rest = &search.needle[offset..];
        if rest.len() <= available.len() {
            return available.starts_with(rest).then(|| {
                vec![Piece {
                    run,
                    start,
                    len: rest.len(),
                }]
            });
        }
        if !rest.starts_with(available) || search.failed.contains(&(offset, run, start)) {
            return None;
        }
        let next = offset + available.len();
        let count = self.letters.len();
        for candidate in (run + 1..count).chain((0..run).rev()) {
            *search.budget = search.budget.checked_sub(1)?;
            let from = self.claimed[candidate];
            let letters = &self.letters[candidate];
            if from >= letters.len()
                || letters[from] != search.needle[next]
                || search.used.contains(&candidate)
            {
                continue;
            }
            search.used.push(candidate);
            if let Some(mut tail) = self.extend(search, next, candidate, from) {
                tail.insert(
                    0,
                    Piece {
                        run,
                        start,
                        len: available.len(),
                    },
                );
                return Some(tail);
            }
            search.used.pop();
        }
        search.failed.insert((offset, run, start));
        None
    }

    /// Everything a match says about a block, from the run each of its
    /// letters came from.
    fn located(&self, owners: Vec<usize>, columns: Vec<usize>) -> Located {
        Located {
            hyperlink: self.whole_block_link(&owners),
            bbox: self.union(&owners),
            spans: self.inline_spans(&owners, &columns),
            role: self.role(&owners),
            first_run: owners.first().copied(),
            owners,
            columns,
        }
    }

    /// The blocks set side by side inside one located block, in column
    /// order, when there is more than one.
    ///
    /// The renderer assembles a line from the runs that share a baseline.
    /// Two blocks set beside each other, a figure caption beside the prose
    /// wrapped around it or the two halves of a two-column region it did
    /// not recognise, therefore come out of it as one block whose words
    /// alternate between the two. The runs still know which side of the
    /// gutter they were drawn on. A gutter is a vertical gap no run of the
    /// block crosses, with runs on both sides whose lines alternate: a
    /// run of one side sits strictly between two runs of the other. A
    /// marker beside the first line of its item and a label beside a
    /// value do not alternate, and stay one block.
    ///
    /// Each side keeps the characters of `text` its own runs produced, in
    /// the order the text had them, so nothing is respelled: the words are
    /// the renderer's, only sorted back onto the runs that drew them.
    #[must_use]
    pub fn side_by_side(&self, located: &Located, text: &str) -> Option<Vec<(String, Located)>> {
        let mut runs: Vec<usize> = Vec::new();
        for &owner in &located.owners {
            if !runs.contains(&owner) {
                runs.push(owner);
            }
        }
        if runs.len() < 2 {
            return None;
        }
        let regions = regions(&runs, &self.boxes)?;

        let characters: Vec<char> = text.chars().collect();
        // Which run each character of the text belongs to: a letter to the
        // run that drew it; an opening quote or bracket to the letter after
        // it; any other punctuation, and every space, to the letter before
        // it; and anything before the first letter to the first letter.
        let mut owner_of = vec![usize::MAX; characters.len()];
        for (letter, &column) in located.columns.iter().enumerate() {
            if let Some(slot) = owner_of.get_mut(column) {
                *slot = located.owners[letter];
            }
        }
        let mut following = located.owners.first().copied().unwrap_or(usize::MAX);
        for index in (0..characters.len()).rev() {
            if owner_of[index] == usize::MAX {
                if opens(characters[index]) {
                    owner_of[index] = following;
                }
            } else {
                following = owner_of[index];
            }
        }
        let mut preceding = located.owners.first().copied().unwrap_or(usize::MAX);
        for slot in &mut owner_of {
            if *slot == usize::MAX {
                *slot = preceding;
            } else {
                preceding = *slot;
            }
        }

        let mut parts = Vec::with_capacity(regions.len());
        for region in regions {
            let members: HashSet<usize> = region.iter().copied().collect();
            let mut kept: Vec<char> = Vec::new();
            let mut index_of = vec![usize::MAX; characters.len()];
            for (index, &character) in characters.iter().enumerate() {
                if !members.contains(&owner_of[index]) {
                    continue;
                }
                // Whitespace between characters of the other side collapses
                // to one space, and none is kept at the start.
                if character.is_whitespace() && kept.last().is_none_or(|last| last.is_whitespace())
                {
                    continue;
                }
                index_of[index] = kept.len();
                kept.push(character);
            }
            while kept.last().is_some_and(|last| last.is_whitespace()) {
                kept.pop();
            }
            let mut owners = Vec::new();
            let mut columns = Vec::new();
            for (letter, &owner) in located.owners.iter().enumerate() {
                if members.contains(&owner) {
                    owners.push(owner);
                    columns.push(index_of[located.columns[letter]]);
                }
            }
            parts.push((kept.into_iter().collect(), self.located(owners, columns)));
        }
        Some(parts)
    }

    /// The external link covering every letter of the block, when one
    /// does.
    ///
    /// A block that is entirely one link is a link in the upstream
    /// dialect's sense too, and saying so keeps the fragment readable to a
    /// consumer that has no inline spans. Decorations do not break this:
    /// a link whose second half is bold is still entirely a link.
    fn whole_block_link(&self, owners: &[usize]) -> Option<String> {
        let mut target: Option<&str> = None;
        for &owner in owners {
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

    /// The role the document gave the runs behind the block.
    ///
    /// A block normally sits in one marked-content region and so has one
    /// role. When it straddles several, the one that covers the most
    /// letters wins, because that is the one the block mostly is.
    fn role(&self, owners: &[usize]) -> Option<(pb::StructureRole, String)> {
        let mut tally: Vec<(&(pb::StructureRole, String), usize)> = Vec::new();
        for &owner in owners {
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

    /// The styled runs inside the block, as character ranges into the
    /// block those letters came from.
    ///
    /// Consecutive letters sharing a style are one span. A style with
    /// nothing to say produces none, so a paragraph of plain body text
    /// carries no spans at all.
    fn inline_spans(&self, owners: &[usize], columns: &[usize]) -> Vec<doc::InlineSpan> {
        let mut spans: Vec<doc::InlineSpan> = Vec::new();
        let mut open: Option<(&Style, usize, usize)> = None;
        for (offset, &owner) in owners.iter().enumerate() {
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

    /// The union of the boxes of every run contributing to the block.
    fn union(&self, owners: &[usize]) -> Option<doc::BoundingBox> {
        let mut hull: Option<doc::BoundingBox> = None;
        let mut last = usize::MAX;
        for &owner in owners {
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

/// The state of one block's search through the page's runs.
struct Search<'a> {
    /// The block's letters.
    needle: &'a [char],
    /// How many more candidate runs the search may examine.
    budget: &'a mut usize,
    /// The runs on the chain being followed, so no run is taken twice.
    used: Vec<usize>,
    /// Continuations already shown to lead nowhere: the needle offset, the
    /// run and the letter of it the continuation started from.
    failed: &'a mut HashSet<(usize, usize, usize)>,
}

/// One stretch of one run's letters that a block took.
struct Piece {
    /// Which run.
    run: usize,
    /// The first letter of the run the block took.
    start: usize,
    /// How many letters it took from there.
    len: usize,
}

/// Cut `runs` into the groups that sit side by side, left to right, when a
/// gutter separates them; `None` when they are one block.
///
/// Sorted by their right edges, the runs left of a gutter are a prefix and
/// the runs right of it are the rest, so every cut of that order is tried
/// and the leftmost gutter wins. The right-hand side is cut again for a
/// third column. Each group keeps the order the runs had in the block.
fn regions(runs: &[usize], boxes: &[doc::BoundingBox]) -> Option<Vec<Vec<usize>>> {
    let mut by_right = runs.to_vec();
    by_right.sort_by(|left, right| boxes[*left].r.total_cmp(&boxes[*right].r));
    for cut in 1..by_right.len() {
        let gutter_left = boxes[by_right[cut - 1]].r;
        let gutter_right = by_right[cut..]
            .iter()
            .map(|run| boxes[*run].l)
            .fold(f64::MAX, f64::min);
        if gutter_right - gutter_left < MIN_GUTTER {
            continue;
        }
        let (left, right) = by_right.split_at(cut);
        if !alternate(left, right, boxes) && !alternate(right, left, boxes) {
            continue;
        }
        let in_order = |side: &[usize]| -> Vec<usize> {
            runs.iter()
                .copied()
                .filter(|run| side.contains(run))
                .collect()
        };
        let mut groups = vec![in_order(left)];
        let rest = in_order(right);
        match regions(&rest, boxes) {
            Some(more) => groups.extend(more),
            None => groups.push(rest),
        }
        return Some(groups);
    }
    None
}

/// The area of a box, in square points.
fn area(bbox: &doc::BoundingBox) -> f64 {
    (bbox.r - bbox.l).abs() * (bbox.t - bbox.b).abs()
}

/// Whether most of `inner` lies inside `outer`.
fn inside(inner: &doc::BoundingBox, outer: &doc::BoundingBox) -> bool {
    let width = (inner.r.min(outer.r) - inner.l.max(outer.l)).max(0.0);
    let height = (inner.t.min(outer.t) - inner.b.max(outer.b)).max(0.0);
    let own = area(inner);
    own > 0.0 && width * height >= own * INSIDE_SHARE
}

/// Whether a character opens what follows it: a quotation mark or a
/// bracket that belongs with the word after it rather than the word
/// before.
fn opens(character: char) -> bool {
    matches!(
        character,
        '"' | '\''
            | '('
            | '['
            | '{'
            | '\u{2018}'
            | '\u{201C}'
            | '\u{00AB}'
            | '\u{2039}'
            | '\u{201A}'
            | '\u{201E}'
    )
}

/// Whether some run of `inner` sits strictly between two runs of `outer`
/// on the page: below one and above another.
fn alternate(inner: &[usize], outer: &[usize], boxes: &[doc::BoundingBox]) -> bool {
    let centre = |run: usize| f64::midpoint(boxes[run].b, boxes[run].t);
    inner.iter().any(|&run| {
        let y = centre(run);
        outer
            .iter()
            .any(|&other| centre(other) > y + LINE_SEPARATION)
            && outer
                .iter()
                .any(|&other| centre(other) < y - LINE_SEPARATION)
    })
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
    /// For each letter of the block, in order, the run that drew it. Empty
    /// when the block was not found.
    pub owners: Vec<usize>,
    /// For each letter of the block, in order, the character of the
    /// block's text it is.
    pub columns: Vec<usize>,
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

/// Whether a run's letters are part of the page's rendering. Image
/// placeholders and link annotations never are; neither is chrome, which
/// was taken out of the page before the renderer saw it.
fn contributes_text(span: &pb::TextSpan) -> bool {
    !span.chrome
        && matches!(
            pb::SpanKind::try_from(span.kind),
            Ok(pb::SpanKind::Text | pb::SpanKind::FormField)
        )
}

/// The lower-case letters and digits of a string.
fn alphanumeric(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
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

    fn chrome_run(text: &str, y: f64) -> pb::TextSpan {
        pb::TextSpan {
            chrome: true,
            ..span(text, 20.0, y, pb::SpanKind::Text)
        }
    }

    /// A margin number drawn on a body line's baseline sits between that
    /// line's letters and the next line's in page order. The chrome flag
    /// keeps it out of the letters, so the block the renderer made of the
    /// two lines still matches, and its box is the two lines' box, not the
    /// margin's.
    #[test]
    fn chrome_runs_contribute_no_letters() {
        let mut runs = PageRuns::new(
            &page(vec![
                chrome_run("000", 700.0),
                span("Diffusion for", 72.0, 700.0, pb::SpanKind::Text),
                chrome_run("001", 680.0),
                span("code generates", 72.0, 680.0, pb::SpanKind::Text),
            ]),
            &[],
            None,
        );
        let located = runs.locate("Diffusion for code generates");
        let bbox = located
            .bbox
            .expect("the block is found across the margin numbers");
        assert!(
            bbox.l >= 72.0,
            "the margin number's box is not in the union: {bbox:?}"
        );
        assert_eq!(runs.chrome_run(0).map(|(text, _)| text), Some("000"));
        assert_eq!(runs.chrome_run(1).map(|(text, _)| text), Some("001"));
        assert!(runs.chrome_run(2).is_none());
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
        let pictures = runs.pictures(None);
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
            runs.pictures(None)[0].hyperlink.as_deref(),
            Some("https://example.invalid/figure")
        );
        assert!(runs.pictures(None)[0].target.is_none(), "out, not in");
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
        let pictures = runs.pictures(None);
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
        assert!(runs.pictures(None).is_empty());
    }

    /// A Form XObject placement at an explicit box.
    fn form_at(name: &str, x: f64, y: f64, width: f64, height: f64) -> pb::TextSpan {
        pb::TextSpan {
            text: format!("[Form: {name}]"),
            bbox: Some(pb::Rect {
                x,
                y,
                width,
                height,
            }),
            kind: pb::SpanKind::Form.into(),
            ..pb::TextSpan::default()
        }
    }

    fn letter() -> doc::Size {
        doc::Size {
            width: 612.0,
            height: 792.0,
        }
    }

    #[test]
    fn a_form_drawn_as_a_figure_is_a_picture_with_its_placed_box() {
        let runs = index(page(vec![
            span("prose above", 108.0, 700.0, pb::SpanKind::Text),
            form_at("Im3", 345.6, 447.9, 158.4, 116.1),
            span("prose below", 108.0, 400.0, pb::SpanKind::Text),
        ]));
        let pictures = runs.pictures(Some(&letter()));
        assert_eq!(pictures.len(), 1);
        assert!(
            (pictures[0].bbox.l - 345.6).abs() < 1e-3,
            "{:?}",
            pictures[0].bbox
        );
        assert!(
            (pictures[0].bbox.t - 564.0).abs() < 1e-3,
            "{:?}",
            pictures[0].bbox
        );
    }

    #[test]
    fn a_form_that_wraps_the_whole_page_is_not_a_picture() {
        // Print-to-PDF producers draw the page's whole content through one
        // form; the figure inside it is the picture, the wrapper is not.
        let runs = index(page(vec![
            form_at("X1", 0.0, 0.0, 612.0, 792.0),
            form_at("X2", 72.0, 500.0, 200.0, 100.0),
            span("prose", 72.0, 400.0, pb::SpanKind::Text),
        ]));
        let pictures = runs.pictures(Some(&letter()));
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        assert!((pictures[0].bbox.l - 72.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_form_inside_a_figure_is_the_figure_not_a_second_picture() {
        let runs = index(page(vec![
            form_at("Fig", 72.0, 400.0, 300.0, 200.0),
            form_at("Panel", 80.0, 410.0, 100.0, 100.0),
        ]));
        let pictures = runs.pictures(Some(&letter()));
        assert_eq!(pictures.len(), 1);
        assert!(
            (pictures[0].bbox.r - 372.0).abs() < f64::EPSILON,
            "the outer form"
        );
    }

    #[test]
    fn a_form_that_only_holds_an_image_defers_to_the_image() {
        let mut image = span("[Image: Im1]", 100.0, 420.0, pb::SpanKind::Image);
        image.bbox = Some(pb::Rect {
            x: 100.0,
            y: 420.0,
            width: 150.0,
            height: 120.0,
        });
        let runs = index(page(vec![form_at("Fig", 72.0, 400.0, 300.0, 200.0), image]));
        let pictures = runs.pictures(Some(&letter()));
        assert_eq!(pictures.len(), 1);
        assert!(
            (pictures[0].bbox.l - 100.0).abs() < f64::EPSILON,
            "the image"
        );
    }

    #[test]
    fn a_form_too_small_to_be_a_figure_is_not_one() {
        let runs = index(page(vec![form_at("Glyph", 72.0, 400.0, 8.0, 8.0)]));
        assert!(runs.pictures(Some(&letter())).is_empty());
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

    /// One run at an explicit box.
    fn run_at(text: &str, x: f64, y: f64, width: f64) -> pb::TextSpan {
        pb::TextSpan {
            text: text.to_owned(),
            bbox: Some(pb::Rect {
                x,
                y,
                width,
                height: 10.0,
            }),
            kind: pb::SpanKind::Text.into(),
            ..pb::TextSpan::default()
        }
    }

    #[test]
    fn a_paragraph_interleaved_with_the_other_column_is_found_run_by_run() {
        // The extractor delivers a two-column page a line at a time across
        // both columns, so the left paragraph's lines arrive with the right
        // paragraph's lines between them.
        let mut runs = index(page(vec![
            run_at("left one", 57.0, 700.0, 200.0),
            run_at("right one", 316.0, 700.0, 200.0),
            run_at("left two", 57.0, 688.0, 200.0),
            run_at("right two", 316.0, 688.0, 200.0),
            run_at("left three", 57.0, 676.0, 100.0),
            run_at("right three", 316.0, 676.0, 100.0),
        ]));
        let left = runs.locate("left one left two left three");
        let bbox = left.bbox.expect("the left paragraph is a chain of runs");
        assert!((bbox.l - 57.0).abs() < f64::EPSILON, "{bbox:?}");
        assert!(
            (bbox.r - 257.0).abs() < f64::EPSILON,
            "the right column is not in the box: {bbox:?}"
        );
        assert!((bbox.b - 676.0).abs() < f64::EPSILON, "{bbox:?}");
        assert_eq!(left.first_run, Some(0));
        let right = runs.locate("right one right two right three");
        let bbox = right.bbox.expect("the right paragraph too");
        assert!((bbox.l - 316.0).abs() < f64::EPSILON, "{bbox:?}");
        assert_eq!(right.first_run, Some(1));
    }

    #[test]
    fn a_block_may_start_after_a_marker_the_rendering_dropped() {
        // The list marker is in the run and not in the item's text.
        let mut runs = index(page(vec![
            run_at("1. First point", 57.0, 700.0, 200.0),
            run_at("continued here", 57.0, 688.0, 200.0),
        ]));
        let located = runs.locate("First point continued here");
        assert!(located.bbox.is_some());
        assert_eq!(located.first_run, Some(0));
    }

    #[test]
    fn a_block_the_rendering_split_takes_the_rest_of_the_run_next() {
        // One run, two blocks: the first takes the run's head and the
        // second its tail, and neither takes the other's letters.
        let mut runs = index(page(vec![
            run_at("Heading words then prose", 57.0, 700.0, 200.0),
            run_at("more prose", 57.0, 688.0, 200.0),
        ]));
        assert!(runs.locate("Heading words").bbox.is_some());
        let rest = runs.locate("then prose more prose");
        assert!(rest.bbox.is_some());
        assert_eq!(rest.owners.first(), Some(&0));
        assert_eq!(rest.owners.last(), Some(&1));
    }

    #[test]
    fn a_caption_beside_wrapped_prose_is_cut_back_onto_its_own_runs() {
        // The renderer read each baseline across both blocks, so the block
        // it produced alternates a line of prose with a line of caption.
        let mut runs = index(page(vec![
            run_at("prose line one", 108.0, 420.0, 228.0),
            run_at("Figure 3: Overlap", 345.0, 419.0, 159.0),
            run_at("prose line two", 108.0, 409.0, 228.0),
            run_at("between the sets", 345.0, 408.0, 159.0),
            run_at("prose line three", 108.0, 398.0, 228.0),
            run_at("is small", 345.0, 397.0, 159.0),
        ]));
        let text = "prose line one Figure 3: Overlap prose line two between the sets prose \
                    line three is small";
        let located = runs.locate(text);
        assert!(located.bbox.is_some());
        let parts = runs
            .side_by_side(&located, text)
            .expect("two blocks sit side by side");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "prose line one prose line two prose line three");
        assert_eq!(parts[1].0, "Figure 3: Overlap between the sets is small");
        let prose = parts[0].1.bbox.as_ref().expect("a box");
        assert!((prose.r - 336.0).abs() < f64::EPSILON, "{prose:?}");
        let caption = parts[1].1.bbox.as_ref().expect("a box");
        assert!((caption.l - 345.0).abs() < f64::EPSILON, "{caption:?}");
        assert_eq!(parts[0].1.first_run, Some(0));
        assert_eq!(parts[1].1.first_run, Some(1));
        // The letters of both parts are the letters of the whole.
        assert_eq!(
            parts[0].1.owners.len() + parts[1].1.owners.len(),
            located.owners.len()
        );
    }

    #[test]
    fn a_label_between_two_lines_of_prose_is_its_own_block() {
        // A figure's stray label shares no baseline with the prose beside
        // it, and the renderer made it a line of the paragraph.
        let mut runs = index(page(vec![
            run_at("3.2 A heading line", 108.0, 561.0, 200.0),
            run_at("0", 387.0, 547.0, 5.0),
            run_at("We exploit the property", 108.0, 541.0, 228.0),
            run_at("of the process", 108.0, 530.0, 228.0),
        ]));
        let text = "3.2 A heading line 0 We exploit the property of the process";
        let located = runs.locate(text);
        let parts = runs
            .side_by_side(&located, text)
            .expect("the label is beside the prose");
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0].0,
            "3.2 A heading line We exploit the property of the process"
        );
        assert_eq!(parts[1].0, "0");
    }

    #[test]
    fn a_marker_beside_the_first_line_is_not_a_second_block() {
        // The marker and the first line share a baseline, so nothing of
        // one side sits between two lines of the other.
        let mut runs = index(page(vec![
            run_at("[1]", 57.0, 700.0, 12.0),
            run_at("Author, Title of the work,", 80.0, 700.0, 200.0),
            run_at("in the proceedings.", 80.0, 688.0, 200.0),
        ]));
        let text = "[1] Author, Title of the work, in the proceedings.";
        let located = runs.locate(text);
        assert!(located.bbox.is_some());
        assert!(runs.side_by_side(&located, text).is_none());
    }

    #[test]
    fn a_split_block_keeps_its_formatting_spans_on_the_right_characters() {
        let mut bold = run_at("bold caption", 345.0, 419.0, 100.0);
        bold.bold = true;
        let mut runs = index(page(vec![
            run_at("prose one", 108.0, 420.0, 200.0),
            bold,
            run_at("prose two", 108.0, 409.0, 200.0),
            run_at("plain caption", 345.0, 408.0, 100.0),
            run_at("prose three", 108.0, 398.0, 200.0),
        ]));
        let text = "prose one bold caption prose two plain caption prose three";
        let located = runs.locate(text);
        let parts = runs.side_by_side(&located, text).expect("two blocks");
        let (caption, located) = &parts[1];
        assert_eq!(caption, "bold caption plain caption");
        assert_eq!(located.spans.len(), 1, "{:?}", located.spans);
        let range = located.spans[0].range.as_ref().expect("a range");
        let covered: String = caption
            .chars()
            .skip(range.start as usize)
            .take((range.end - range.start) as usize)
            .collect();
        assert_eq!(covered, "bold caption");
    }

    #[test]
    fn a_short_block_takes_the_run_that_begins_with_it_not_a_run_that_contains_it() {
        // "Discrete" is a figure label of its own, drawn before the cursor;
        // a paragraph after the cursor happens to contain the word. The
        // label's run is the block's; a match inside the paragraph would
        // take letters the paragraph needs.
        let mut runs = index(page(vec![
            run_at("Discrete", 186.0, 700.0, 40.0),
            run_at("a code listing", 108.0, 650.0, 200.0),
            run_at("the discrete nature of later steps", 108.0, 600.0, 300.0),
        ]));
        assert!(runs.locate("a code listing").bbox.is_some());
        let label = runs.locate("Discrete");
        assert_eq!(label.first_run, Some(0), "the run that is the label");
        let paragraph = runs.locate("the discrete nature of later steps");
        assert!(
            paragraph.bbox.is_some(),
            "the paragraph still has every letter of its run"
        );
    }

    #[test]
    fn an_opening_quote_goes_with_the_words_it_opens() {
        // The renderer put the right block's line after the left block's
        // on each baseline, so the quote that opens the right block's
        // sentence follows the left block's full stop in the text.
        let mut runs = index(page(vec![
            run_at("forced me from the car.", 57.0, 700.0, 228.0),
            run_at("\u{201C}Get some chairs, why", 316.0, 699.0, 228.0),
            run_at("out of the room quickly.", 57.0, 689.0, 228.0),
            run_at("don\u{2019}t you,\u{201D} he said.", 316.0, 688.0, 228.0),
            run_at("and then we sat.", 57.0, 678.0, 228.0),
        ]));
        let text = "forced me from the car. \u{201C}Get some chairs, why out of the room quickly. \
                    don\u{2019}t you,\u{201D} he said. and then we sat.";
        let located = runs.locate(text);
        let parts = runs.side_by_side(&located, text).expect("two columns");
        assert_eq!(
            parts[0].0,
            "forced me from the car. out of the room quickly. and then we sat."
        );
        assert_eq!(
            parts[1].0,
            "\u{201C}Get some chairs, why don\u{2019}t you,\u{201D} he said."
        );
    }

    #[test]
    fn a_page_of_repeated_tokens_is_searched_once_not_once_per_path() {
        // A code listing: the same few tokens over and over, so every
        // continuation has many candidates and a search that revisited
        // each of them from every path would not finish.
        let mut spans = Vec::new();
        for row in 0..60 {
            for column in 0..4 {
                spans.push(run_at(
                    ["Orders", "Total", "Amount", "Region"][column],
                    100.0 + 60.0 * column as f64,
                    700.0 - 10.0 * row as f64,
                    50.0,
                ));
            }
        }
        // The block is the listing plus one word the page never drew, so
        // no complete chain exists and every path has to fail.
        let mut words: Vec<&str> = Vec::new();
        for _ in 0..60 {
            words.extend(["Orders", "Total", "Amount", "Region"]);
        }
        words.push("Zebra");
        let text = words.join(" ");
        let mut runs = index(page(spans));
        let started = std::time::Instant::now();
        assert!(runs.locate(&text).bbox.is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the search gave up in {:?}",
            started.elapsed()
        );
    }
}
