// SPDX-License-Identifier: Apache-2.0

//! The optional fold of one parse's event stream into a single
//! `ai.pipestream.document.v1.Document`.
//!
//! The typed event stream (`info`, `page`, `status`) is the primary,
//! lossless wire. This module is the lossy structural projection of it,
//! offered because a coordinator that only wants the Document plane should
//! not have to reimplement the mapping — the canonical fold lives next to
//! the collector that knows what the events mean. It runs only when the
//! caller sets `PdfOptions.emit_document`, and it sees exactly the events
//! the service sends: [`parse`](crate::parse) feeds each one to
//! [`DocumentFold::consume`] on its way to the socket.
//!
//! The projection is deliberately coarse, and honest about it:
//!
//! - **Structure is markdown-derived.** The `page` events carry markdown,
//!   and that markdown is all the fold parses: ATX headings (`#` through
//!   `####`) become section headers with their level, blank-line-separated
//!   blocks become paragraphs. Lists, emphasis and tables inside a block
//!   stay as their markdown source in `text`; the extraction markdown has
//!   already flattened the layout, so there is little more to recover, and
//!   parsing deeper would pretend to structure the source does not have.
//! - **Pages are named, not measured.** `pages` carries one `PageItem` per
//!   page the `info` event reported, with `page_no` and `unit` set and
//!   `size` and `image` omitted: nothing on the event stream yet carries a
//!   page box, and an invented size would outlive the honesty of this
//!   comment. `unit` is set anyway, because the runs' boxes *are* measured
//!   and a box whose unit a reader has to guess is barely a box.
//! - **Provenance is typed.** Every item carries a `ProvenanceItem` naming
//!   its page, and — when the `spans` event for that page located the
//!   item's text among the positioned runs — the union of those runs'
//!   boxes as its `bbox`. This used to be a `meta.custom_fields["pdf.page"]`
//!   number on the side, which is the untyped shape of the same fact;
//!   `prov[].page_no` is the typed one and costs nothing.
//! - **A self-contained fragment.** Refs are dense and local (`#/texts/0`),
//!   every item's `parent` is the section header it sits under (or
//!   `#/body`), and every parent lists the item in its `children`, so the
//!   coordinator's additive merge can renumber the fragment mechanically.
//!
//! Every item's `CollectorSource` names this service ([`COLLECTOR`]), the
//! parser and its version ([`PARSER`]) as `model`, this build's version
//! ([`VERSION`]) as `version`, and the detection confidence from `info` —
//! the only confidence the pipeline computes — as `confidence`.

use crate::page_runs::{Located, PageRuns};
use crate::proto::ai::pipestream::document::v1 as doc;
use crate::proto::v1 as pb;
use crate::{COLLECTOR, PARSER, VERSION};

/// Value of `Document.schema_name`: the upstream docling schema this plane
/// tracks.
pub const SCHEMA_NAME: &str = "docling_document_v2";

/// Value of `DocumentOrigin.mimetype`.
pub const MIMETYPE: &str = "application/pdf";

/// Value of `PageItem.unit`: every coordinate this fold writes is in PDF
/// user-space points, 1/72 inch, measured from the page's bottom-left.
pub const UNIT: &str = "pt";

/// Self ref of the body group: the parent of everything this fold makes
/// that is not under a section header.
const BODY_REF: &str = "#/body";

/// Self ref of the furniture group. Nothing is put in it: page chrome does
/// not survive into extraction markdown.
const FURNITURE_REF: &str = "#/furniture";

/// The deepest heading level the fold recognizes. `#` through `####` map
/// to levels 1 through 4; a line with more hashes is prose.
const MAX_HEADING_LEVEL: usize = 4;

/// A fold of one parse's events into one Document.
///
/// Feed it every event of one `ParsePdf` response stream in order, then
/// call [`take`](Self::take). Events from two different parses must not be
/// mixed into one fold.
pub struct DocumentFold {
    document: doc::Document,
    /// Attribution every item carries. `confidence` arrives with `info`.
    source: doc::CollectorSource,
    /// The open section headers, outermost first: the level of each and the
    /// self ref content under it names as its parent. Empty means the body
    /// is the parent.
    headings: Vec<(i32, String)>,
    /// The positioned runs of the page being folded, when the stream
    /// carried them. They arrive on the `spans` event immediately before
    /// the `page` event they belong to, and are dropped when the next one
    /// arrives: a page's boxes are of no use to any other page.
    runs: Option<PageRuns>,
}

impl Default for DocumentFold {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentFold {
    /// An empty fold, with the two root groups already in place.
    #[must_use]
    pub fn new() -> Self {
        Self {
            document: doc::Document {
                schema_name: Some(SCHEMA_NAME.to_owned()),
                origin: Some(doc::DocumentOrigin {
                    mimetype: MIMETYPE.to_owned(),
                    // No filename: the request stream carries bytes, not a
                    // name. `binary_hash` stays zero for the same reason the
                    // fold exists at all — it is not this stream's job to
                    // re-derive what the caller already has.
                    ..doc::DocumentOrigin::default()
                }),
                body: Some(group(BODY_REF, doc::ContentLayer::Body)),
                furniture: Some(group(FURNITURE_REF, doc::ContentLayer::Furniture)),
                ..doc::Document::default()
            },
            source: doc::CollectorSource {
                collector: COLLECTOR.to_owned(),
                model: Some(PARSER.to_owned()),
                version: Some(VERSION.to_owned()),
                confidence: None,
                // The detection score is the only number this pipeline
                // computes and it is already on the 0-to-1 scale
                // `confidence` names. There is no second, uncalibrated
                // signal behind it to report.
                raw_score: None,
                raw_score_kind: None,
            },
            headings: Vec::new(),
            runs: None,
        }
    }

    /// Fold one event of the stream.
    ///
    /// Events the projection has no slot for are ignored rather than
    /// refused: the fold is a projection, and an event it has no slot for
    /// is a gap in the projection, not a parse failure. `status` is counts
    /// and warnings, which describe the stream rather than the document; a
    /// `document` event is a fold's own output, and folding one back in
    /// would double the fragment.
    pub fn consume(&mut self, event: &pb::parse_pdf_response::Event) {
        use pb::parse_pdf_response::Event;
        match event {
            Event::Info(info) => self.on_info(info),
            Event::Spans(spans) => self.runs = Some(PageRuns::new(spans)),
            Event::Page(page) => self.on_page(page),
            Event::Status(_) | Event::Document(_) => {}
        }
    }

    /// Finish the fragment and take it. The fold is empty afterwards.
    pub fn take(&mut self) -> doc::Document {
        self.headings.clear();
        self.runs = None;
        std::mem::replace(&mut self.document, Self::new().document)
    }

    /// `info` names the document, counts its pages, and carries the
    /// detection confidence every item inherits.
    fn on_info(&mut self, info: &pb::PdfInfo) {
        self.source.confidence = Some(f64::from(info.confidence));
        if !info.title.is_empty() {
            self.document.name.clone_from(&info.title);
        }
        // One PageItem per reported page: the number is known, the size is
        // not, and only what is known is written down. `unit` is known
        // regardless — every box this fold writes is in PDF points — and
        // saying so is what makes those boxes readable.
        for page in 1..=info.page_count {
            let page_no = i32::try_from(page).unwrap_or(i32::MAX);
            self.document.pages.insert(
                page_no,
                doc::PageItem {
                    page_no,
                    unit: Some(UNIT.to_owned()),
                    ..doc::PageItem::default()
                },
            );
        }
    }

    /// Fold one page's markdown into text items.
    fn on_page(&mut self, page: &pb::PageMarkdown) {
        for block in blocks(&page.markdown) {
            match block {
                Block::Heading { level, text } => {
                    // A header opens a level on the ladder before it is
                    // placed, so it is parented to the header enclosing it
                    // rather than to the one it closes.
                    self.close_headings(level);
                    let self_ref = self.push_text(&text, page.page_no, Some(level));
                    self.headings.push((level, self_ref));
                }
                Block::Paragraph(text) => {
                    self.push_text(&text, page.page_no, None);
                }
            }
        }
    }

    /// Append one text item — a paragraph, or a section header when `level`
    /// is set — and return its self ref.
    fn push_text(&mut self, text: &str, page_no: u32, level: Option<i32>) -> String {
        let parent = self.current_parent();
        let self_ref = format!("#/texts/{}", self.document.texts.len());
        let located = self.locate(text, page_no);
        let base = doc::TextItemBase {
            self_ref: self_ref.clone(),
            parent: Some(reference(&parent)),
            content_layer: doc::ContentLayer::Body as i32,
            meta: Some(doc::BaseMeta::default()),
            prov: provenance(page_no, located.bbox),
            hyperlink: located.hyperlink,
            spans: located.spans,
            label: level.map_or(doc::DocItemLabel::Paragraph, |_| {
                doc::DocItemLabel::SectionHeader
            }) as i32,
            orig: text.to_owned(),
            text: text.to_owned(),
            source: vec![doc::SourceType {
                source: Some(doc::source_type::Source::Collector(self.source.clone())),
            }],
            ..doc::TextItemBase::default()
        };
        let variant = match level {
            Some(level) => doc::base_text_item::Item::SectionHeader(doc::SectionHeaderItem {
                base: Some(base),
                // Redundant with the nesting, and kept anyway: docling
                // populates both.
                level,
            }),
            None => doc::base_text_item::Item::Text(doc::TextItem { base: Some(base) }),
        };
        self.document.texts.push(doc::BaseTextItem {
            item: Some(variant),
        });
        self.link_child(&parent, &self_ref);
        self_ref
    }

    /// Find one block of text among the page's runs.
    ///
    /// Nothing is found when the runs did not arrive, when they belong to
    /// another page, or when the block's letters are not among them — and
    /// nothing found means nothing claimed.
    fn locate(&mut self, text: &str, page_no: u32) -> Located {
        if page_no == 0 {
            return Located::default();
        }
        self.runs
            .as_mut()
            .filter(|runs| runs.page_no() == page_no)
            .map_or_else(Located::default, |runs| runs.locate(text))
    }

    /// The ref new content parents to: the innermost open section header,
    /// or the body when no header has opened yet. Content before the first
    /// heading sits on the body, as it does upstream.
    fn current_parent(&self) -> String {
        self.headings
            .last()
            .map_or_else(|| BODY_REF.to_owned(), |(_, self_ref)| self_ref.clone())
    }

    /// Close every open header a level-`level` header ends, so that the
    /// next heading is nested under the nearest header of a lower level. A
    /// header of the same level closes the one before it: siblings, not
    /// parent and child.
    fn close_headings(&mut self, level: i32) {
        while self.headings.last().is_some_and(|(open, _)| *open >= level) {
            self.headings.pop();
        }
    }

    /// Both halves of the parent link: the item names its parent, and the
    /// parent lists the item.
    ///
    /// The only parents this fold makes are the body and section headers,
    /// so a ref that is neither is a bug in the caller rather than
    /// something to resolve generically.
    fn link_child(&mut self, parent: &str, child: &str) {
        if parent == BODY_REF {
            if let Some(body) = self.document.body.as_mut() {
                body.children.push(reference(child));
            }
        } else if let Some(base) = self.heading_base(parent) {
            base.children.push(reference(child));
        }
    }

    /// The base of the section header at a `#/texts/N` ref.
    fn heading_base(&mut self, self_ref: &str) -> Option<&mut doc::TextItemBase> {
        let index: usize = self_ref.strip_prefix("#/texts/")?.parse().ok()?;
        match self.document.texts.get_mut(index)?.item.as_mut()? {
            doc::base_text_item::Item::SectionHeader(header) => header.base.as_mut(),
            _ => None,
        }
    }
}

/// One block of a page's markdown: a heading, or a paragraph of prose.
enum Block {
    Heading { level: i32, text: String },
    Paragraph(String),
}

/// Split page markdown into blocks: ATX headings on their own lines,
/// paragraphs between blank lines.
///
/// This is a structural read of markdown, not a markdown parser: a
/// `#`-leading line inside a fenced code block would be misread as a
/// heading. That is accepted — extraction markdown is generated, not
/// authored, and the fold's contract is coarseness, not fidelity.
fn blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some((level, text)) = atx_heading(line) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading { level, text });
        } else {
            paragraph.push(line.trim());
        }
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    blocks
}

/// End the paragraph being accumulated, if there is one.
fn flush_paragraph(lines: &mut Vec<&str>, blocks: &mut Vec<Block>) {
    if lines.is_empty() {
        return;
    }
    blocks.push(Block::Paragraph(lines.join("\n")));
    lines.clear();
}

/// Parse an ATX heading line into its level and text, or `None` when the
/// line is prose.
///
/// Only `#` through `####` are headings here: the fold's contract names
/// four levels, and a run of five or more hashes is more likely prose
/// about hashes than a fifth-level section.
fn atx_heading(line: &str) -> Option<(i32, String)> {
    let line = line.trim_start();
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > MAX_HEADING_LEVEL {
        return None;
    }
    let rest = &line[hashes..];
    // A heading marker is followed by whitespace (or ends the line);
    // "#hashtag" is prose.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // An optional closing sequence of hashes is decoration, not text.
    let text = rest.trim().trim_end_matches('#').trim_end();
    if text.is_empty() {
        return None;
    }
    Some((i32::try_from(hashes).unwrap_or(i32::MAX), text.to_owned()))
}

/// Where one block of text came from.
///
/// The page alone is always a claim this fold can make, and it makes it: a
/// provenance entry naming only a page is strictly more than the nothing
/// that used to be there. The box is added when the block was located among
/// the page's runs.
///
/// Page 0 is the password fallback's whole-document event, which has no
/// page to name and therefore no provenance to give.
fn provenance(page_no: u32, bbox: Option<doc::BoundingBox>) -> Vec<doc::ProvenanceItem> {
    if page_no == 0 {
        return Vec::new();
    }
    vec![doc::ProvenanceItem {
        page_no: i32::try_from(page_no).unwrap_or(i32::MAX),
        bbox,
        ..doc::ProvenanceItem::default()
    }]
}

/// A root group with nothing in it yet.
fn group(self_ref: &str, layer: doc::ContentLayer) -> doc::GroupItem {
    doc::GroupItem {
        self_ref: self_ref.to_owned(),
        content_layer: layer as i32,
        ..doc::GroupItem::default()
    }
}

/// A JSON-Pointer reference to another item.
fn reference(target: &str) -> doc::RefItem {
    doc::RefItem {
        r#ref: target.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base of any text item, whichever variant it is.
    fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
            doc::base_text_item::Item::SectionHeader(header) => {
                header.base.as_ref().expect("a base")
            }
            other => panic!("the fold makes paragraphs and section headers, got {other:?}"),
        }
    }

    fn info(page_count: u32, title: &str, confidence: f32) -> pb::parse_pdf_response::Event {
        pb::parse_pdf_response::Event::Info(pb::PdfInfo {
            pdf_type: pb::PdfType::TextBased as i32,
            confidence,
            page_count,
            title: title.to_owned(),
            ..pb::PdfInfo::default()
        })
    }

    fn page(page_no: u32, markdown: &str) -> pb::parse_pdf_response::Event {
        pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
            page_no,
            markdown: markdown.to_owned(),
            ..pb::PageMarkdown::default()
        })
    }

    /// One page's runs, one run per line of `lines`, stacked down the page
    /// so a test can tell them apart by their box.
    fn spans(page_no: u32, lines: &[&str]) -> pb::parse_pdf_response::Event {
        pb::parse_pdf_response::Event::Spans(pb::PageSpans {
            page_no,
            spans: lines
                .iter()
                .enumerate()
                .map(|(index, line)| pb::TextSpan {
                    text: (*line).to_owned(),
                    bbox: Some(pb::Rect {
                        x: 72.0,
                        y: 700.0 - 20.0 * index as f64,
                        width: 400.0,
                        height: 12.0,
                    }),
                    kind: pb::SpanKind::Text.into(),
                    ..pb::TextSpan::default()
                })
                .collect(),
        })
    }

    /// The merge contract as a check: every ref dense, at its arena
    /// position, resolving, and reciprocated between parent and child.
    fn assert_sound(document: &doc::Document) {
        let body = document.body.as_ref().expect("a body");

        // Every self_ref is its position in the arena.
        let refs: Vec<String> = document
            .texts
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let base = base_of(item);
                assert_eq!(
                    base.self_ref,
                    format!("#/texts/{index}"),
                    "self_ref matches its arena position"
                );
                base.self_ref.clone()
            })
            .collect();

        // Every parent resolves, and lists the item as its child.
        for item in &document.texts {
            let base = base_of(item);
            let parent = &base.parent.as_ref().expect("a parent").r#ref;
            let children = if parent == BODY_REF {
                &body.children
            } else {
                let index: usize = parent
                    .strip_prefix("#/texts/")
                    .and_then(|rest| rest.parse().ok())
                    .unwrap_or_else(|| panic!("parent {parent} resolves"));
                &base_of(&document.texts[index]).children
            };
            assert!(
                children.iter().any(|child| child.r#ref == base.self_ref),
                "{parent} lists {}",
                base.self_ref
            );
        }

        // Everything listed as a child is an item, exactly once.
        let mut listed: Vec<&str> = body
            .children
            .iter()
            .map(|child| child.r#ref.as_str())
            .collect();
        for item in &document.texts {
            listed.extend(
                base_of(item)
                    .children
                    .iter()
                    .map(|child| child.r#ref.as_str()),
            );
        }
        let mut refs: Vec<&str> = refs.iter().map(String::as_str).collect();
        listed.sort_unstable();
        refs.sort_unstable();
        assert_eq!(listed, refs, "every item is listed exactly once");
    }

    #[test]
    fn an_empty_fold_is_a_sound_empty_fragment() {
        let mut fold = DocumentFold::new();
        let document = fold.take();
        assert_eq!(document.schema_name.as_deref(), Some(SCHEMA_NAME));
        assert_eq!(
            document
                .origin
                .as_ref()
                .map(|origin| origin.mimetype.as_str()),
            Some(MIMETYPE)
        );
        assert!(document.texts.is_empty());
        assert_sound(&document);
    }

    #[test]
    fn info_names_the_document_and_its_pages_without_measuring_them() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(3, "A Title", 0.9));
        let document = fold.take();
        assert_eq!(document.name, "A Title");
        assert_eq!(document.pages.len(), 3);
        for (page_no, item) in &document.pages {
            assert_eq!(item.page_no, *page_no);
            assert!(item.size.is_none(), "no fabricated page geometry");
            assert!(item.image.is_none(), "no fabricated page image");
        }
    }

    #[test]
    fn markdown_folds_into_headings_and_paragraphs() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(1, "", 1.0));
        fold.consume(&page(1, "# One\n\nfirst para\n\n## Two\n\nsecond para\n"));
        let document = fold.take();
        assert_eq!(document.texts.len(), 4);

        let labels: Vec<doc::DocItemLabel> = document
            .texts
            .iter()
            .map(|item| doc::DocItemLabel::try_from(base_of(item).label).expect("a known label"))
            .collect();
        assert_eq!(
            labels,
            [
                doc::DocItemLabel::SectionHeader,
                doc::DocItemLabel::Paragraph,
                doc::DocItemLabel::SectionHeader,
                doc::DocItemLabel::Paragraph,
            ]
        );

        match document.texts[2].item.as_ref() {
            Some(doc::base_text_item::Item::SectionHeader(header)) => {
                assert_eq!(header.level, 2);
            }
            other => panic!("the second heading is a section header: {other:?}"),
        }

        // The heading ladder: "second para" hangs off "Two", which hangs
        // off "One", which hangs off the body.
        assert_eq!(
            base_of(&document.texts[0]).parent.as_ref().unwrap().r#ref,
            BODY_REF
        );
        assert_eq!(
            base_of(&document.texts[2]).parent.as_ref().unwrap().r#ref,
            "#/texts/0"
        );
        assert_eq!(
            base_of(&document.texts[3]).parent.as_ref().unwrap().r#ref,
            "#/texts/2"
        );
        assert_sound(&document);
    }

    #[test]
    fn deep_hashes_and_hashtags_are_prose() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "##### five deep\n\n#hashtag\n"));
        let document = fold.take();
        assert_eq!(document.texts.len(), 2);
        for item in &document.texts {
            assert_eq!(base_of(item).label, doc::DocItemLabel::Paragraph as i32);
        }
    }

    #[test]
    fn every_item_carries_the_collector_source_convention() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(1, "", 0.75));
        fold.consume(&page(1, "some prose\n"));
        let document = fold.take();
        let base = base_of(&document.texts[0]);
        assert_eq!(base.source.len(), 1);
        let Some(doc::source_type::Source::Collector(source)) = base.source[0].source.as_ref()
        else {
            panic!("the source is a collector");
        };
        assert_eq!(source.collector, COLLECTOR);
        assert_eq!(source.model.as_deref(), Some(PARSER));
        assert_eq!(source.version.as_deref(), Some(VERSION));
        assert_eq!(source.confidence, Some(0.75));
    }

    #[test]
    fn an_items_page_is_provenance_not_a_custom_field() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(2, "text of page two\n"));
        let document = fold.take();
        let base = base_of(&document.texts[0]);
        assert_eq!(base.prov.len(), 1, "one provenance entry per item");
        assert_eq!(base.prov[0].page_no, 2);
        assert!(
            base.prov[0].bbox.is_none(),
            "no runs arrived, so no box is claimed"
        );
        let meta = base.meta.as_ref().expect("meta");
        assert!(
            meta.custom_fields.is_empty(),
            "the untyped side channel is gone: {:?}",
            meta.custom_fields
        );
    }

    #[test]
    fn runs_arriving_before_a_page_put_boxes_on_its_items() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(1, "", 1.0));
        fold.consume(&spans(1, &["A Heading", "some prose here"]));
        fold.consume(&page(1, "# A Heading\n\nsome prose here\n"));
        let document = fold.take();

        let heading = base_of(&document.texts[0]);
        let heading_box = heading.prov[0].bbox.as_ref().expect("the heading's box");
        assert!(
            (heading_box.b - 700.0).abs() < f64::EPSILON,
            "{heading_box:?}"
        );
        assert_eq!(
            heading_box.coord_origin,
            Some(doc::CoordOrigin::Bottomleft as i32)
        );

        let prose = base_of(&document.texts[1]);
        let prose_box = prose.prov[0].bbox.as_ref().expect("the prose box");
        assert!(
            (prose_box.b - 680.0).abs() < f64::EPSILON,
            "each block gets its own run, not the first one: {prose_box:?}"
        );
    }

    #[test]
    fn runs_from_another_page_are_not_borrowed() {
        let mut fold = DocumentFold::new();
        fold.consume(&spans(1, &["page one text"]));
        fold.consume(&page(2, "page one text\n"));
        let document = fold.take();
        let base = base_of(&document.texts[0]);
        assert_eq!(base.prov[0].page_no, 2);
        assert!(
            base.prov[0].bbox.is_none(),
            "a box from page 1 is not evidence about page 2"
        );
    }

    #[test]
    fn every_page_declares_the_unit_its_boxes_are_measured_in() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(2, "", 1.0));
        let document = fold.take();
        for item in document.pages.values() {
            assert_eq!(item.unit.as_deref(), Some(UNIT));
        }
    }

    #[test]
    fn the_whole_document_fallback_claims_no_page() {
        let mut fold = DocumentFold::new();
        // `page_no` 0 is the password fallback's whole-document event.
        fold.consume(&page(0, "all of it\n"));
        let document = fold.take();
        let base = base_of(&document.texts[0]);
        assert!(
            base.prov.is_empty(),
            "there is no page to name, so nothing is claimed"
        );
    }

    #[test]
    fn taking_twice_does_not_repeat_the_first_fragment() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "one\n"));
        assert_eq!(fold.take().texts.len(), 1);
        assert_eq!(fold.take().texts.len(), 0);
    }
}
