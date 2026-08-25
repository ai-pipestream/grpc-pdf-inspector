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
//! - **Pages are measured when the file was asked about itself.** `pages`
//!   carries one `PageItem` per page the `info` event reported, with
//!   `page_no` and `unit` always set and `size` set from the page's own
//!   visible box when the `metadata` event carried one. Nothing is
//!   invented: a page whose box was never read has no size.
//! - **Furniture is reported, not deleted.** Repeated headers, footers and
//!   folio numbers are stripped from the body by default and used to
//!   vanish. When the stream reports them they go into the furniture group
//!   under `CONTENT_LAYER_FURNITURE`, which is what that layer is for.
//! - **Metadata is the file's own.** `source_meta`, `outline`,
//!   `attachments` and `anchors` come from the document's dictionaries
//!   rather than from its text — an authored outline is better evidence of
//!   structure than heading levels guessed from type size, and a PDF
//!   carrying a spreadsheet inside it used to be invisible to the whole
//!   pipeline.
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

use std::collections::HashMap;

use crate::page_runs::{Located, PageRuns, page_ref};
use crate::proto::ai::pipestream::document::v1 as doc;
use crate::proto::v1 as pb;
use crate::structure;
use crate::{COLLECTOR, PARSER, VERSION};

/// Value of `Document.schema_name`: the identifier of the upstream schema
/// dialect this plane tracks, spelled as that dialect spells it.
pub const SCHEMA_NAME: &str = "docling_document_v2";

/// Value of `DocumentOrigin.mimetype`.
pub const MIMETYPE: &str = "application/pdf";

/// Prefix of a `SubDocumentRef.id` naming a file the PDF carries inside
/// itself, so a coordinator can tell one apart from a payload another
/// collector registered.
pub const ATTACHMENT_SCHEME: &str = "pdf-embedded-file:";

/// Value of `PageItem.unit`: every coordinate this fold writes is in PDF
/// user-space points, 1/72 inch, measured from the page's bottom-left.
pub const UNIT: &str = "pt";

/// Self ref of the body group: the parent of everything this fold makes
/// that is not under a section header.
const BODY_REF: &str = "#/body";

/// Self ref of the furniture group: where the lines the header, footer and
/// folio stripper removed go, when the stream reports them.
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
    /// Internal link annotations by the page they are drawn on, from the
    /// `metadata` event. The extractor reads external targets only, so
    /// these are the whole of the fold's knowledge of cross-references.
    internal_links: HashMap<u32, Vec<pb::LinkTarget>>,
    /// The authored roles of the page being folded, when the document is
    /// tagged and the stream carried them. Held like `runs`: the
    /// `structure` event arrives before the page it describes.
    structure: Option<pb::PageStructure>,
    /// The grids found on the page being folded, in reverse reading order
    /// so the next one is the last one. Each pipe-syntax block in the
    /// page's markdown takes one: both sequences are in reading order, so
    /// the nth flattened table is the nth detected grid.
    detected_tables: Vec<pb::TableRegion>,
    /// The list currently being accumulated: whether its markers count, and
    /// the self ref of the group its items hang off.
    open_list: Option<(bool, String)>,
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
                raw_score_samples: None,
            },
            headings: Vec::new(),
            runs: None,
            internal_links: HashMap::new(),
            structure: None,
            detected_tables: Vec::new(),
            open_list: None,
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
            Event::Metadata(metadata) => self.on_metadata(metadata),
            Event::Structure(structure) => self.structure = Some(structure.clone()),
            Event::Tables(tables) => {
                self.detected_tables = tables.tables.iter().rev().cloned().collect();
            }
            Event::Spans(spans) => {
                let internal = self
                    .internal_links
                    .get(&spans.page_no)
                    .map_or(&[][..], Vec::as_slice);
                let structure = self
                    .structure
                    .as_ref()
                    .filter(|structure| structure.page_no == spans.page_no);
                self.runs = Some(PageRuns::new(spans, internal, structure));
            }
            Event::Page(page) => self.on_page(page),
            Event::Status(_) | Event::Document(_) => {}
        }
    }

    /// Finish the fragment and take it. The fold is empty afterwards.
    pub fn take(&mut self) -> doc::Document {
        self.headings.clear();
        self.runs = None;
        self.structure = None;
        self.detected_tables.clear();
        self.open_list = None;
        self.internal_links.clear();
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

    /// `metadata` is what the file says about itself: the fold writes the
    /// parts of it the schema has homes for.
    ///
    /// What has no home yet stays on the event plane, which is typed and
    /// complete, rather than being flattened into a string map here. That
    /// list is short and is documented in `docs/capture-deferrals.md`.
    fn on_metadata(&mut self, metadata: &pb::PdfMetadata) {
        if let Some(info) = metadata.info.as_ref() {
            self.document.source_meta = Some(doc::DocumentMeta {
                title: non_empty(&info.title),
                authors: info.authors.clone(),
                created: info.created,
                modified: info.modified,
                created_raw: non_empty(&info.created_raw),
                modified_raw: non_empty(&info.modified_raw),
                language: non_empty(&metadata.language),
                // The document's producer is the software that wrote the
                // file. The authoring application it was converted from is
                // on the event plane as `creator_tool`; the schema has one
                // slot and this is the one that fills it.
                generator: non_empty(&info.producer),
                keywords: info.keywords.clone(),
                ..doc::DocumentMeta::default()
            });
            if self.document.name.is_empty() {
                self.document.name.clone_from(&info.title);
            }
        }

        // The document's own table of contents, which is authored evidence
        // rather than heading levels guessed from type size.
        self.document.outline = metadata
            .outline
            .iter()
            .map(|entry| doc::OutlineEntry {
                title: entry.title.clone(),
                level: i32::try_from(entry.level).unwrap_or(i32::MAX),
                page_no: (entry.page_no > 0)
                    .then(|| i32::try_from(entry.page_no).unwrap_or(i32::MAX)),
                target: (entry.page_no > 0).then(|| fine_ref(page_ref(entry.page_no))),
            })
            .collect();

        // A PDF is a container, and what it contains was invisible to the
        // whole pipeline.
        self.document.attachments = metadata
            .embedded_files
            .iter()
            .map(|file| doc::SubDocumentRef {
                id: format!("{ATTACHMENT_SCHEME}{}", file.name),
                name: file.name.clone(),
                media_type: file.media_type.clone(),
                size_bytes: file.size_bytes,
                item_ref: None,
                // A PDF attachment is a file, not an embedded object with a
                // container class behind it.
                class_id: None,
                kind: None,
            })
            .collect();

        // Named destinations are the positions the file's own
        // cross-references point at.
        self.document.anchors = metadata
            .destinations
            .iter()
            .filter(|destination| destination.page_no > 0)
            .map(|destination| doc::NamedAnchor {
                name: destination.name.clone(),
                target: Some(fine_ref(page_ref(destination.page_no))),
            })
            .collect();

        for page in &metadata.pages {
            let page_no = i32::try_from(page.page_no).unwrap_or(i32::MAX);
            let item = self.document.pages.entry(page_no).or_insert(doc::PageItem {
                page_no,
                unit: Some(UNIT.to_owned()),
                ..doc::PageItem::default()
            });
            // The visible box is the page as a reader sees it, which is the
            // frame every box on this wire is measured against.
            if let Some(size) = page.crop_box.as_ref() {
                item.size = Some(doc::Size {
                    width: size.width,
                    height: size.height,
                });
            }
            if page.rotation != 0 {
                item.quality = Some(doc::PageQuality {
                    rotation_degrees: Some(f64::from(page.rotation)),
                    ..doc::PageQuality::default()
                });
            }
        }

        self.internal_links.clear();
        for link in &metadata.links {
            if link.uri.is_empty() && link.dest_page_no > 0 {
                self.internal_links
                    .entry(link.page_no)
                    .or_default()
                    .push(link.clone());
            }
        }
    }

    /// Fold one page's markdown into items.
    fn on_page(&mut self, page: &pb::PageMarkdown) {
        self.on_page_quality(page);
        for line in &page.furniture {
            self.push_furniture(line, page.page_no);
        }
        for block in blocks(&page.markdown) {
            match block {
                Block::Table(text) => self.on_table(&text, page.page_no),
                Block::ListItem {
                    marker,
                    enumerated,
                    text,
                } => {
                    let located = self.locate(&text, page.page_no);
                    self.open_list(enumerated);
                    self.push_text(
                        &text,
                        page.page_no,
                        Kind::ListItem { marker, enumerated },
                        located,
                    );
                }
                Block::Code { language, text } => {
                    self.close_list();
                    let located = self.locate(&text, page.page_no);
                    self.push_code(&text, page.page_no, language.as_deref(), located);
                }
                block => {
                    self.close_list();
                    let (guessed, text) = match block {
                        Block::Heading { level, text } => (Some(level), text),
                        Block::Paragraph(text) => (None, text),
                        // The arms above handled every other variant.
                        _ => continue,
                    };
                    let located = self.locate(&text, page.page_no);
                    // The document's own word for the block beats the
                    // markdown renderer's guess at it. The renderer inferred
                    // heading depth from type size; a tagged document states
                    // it.
                    let authored = located
                        .role
                        .as_ref()
                        .and_then(|(role, _)| structure::heading_level(*role));
                    let level = authored.or(guessed);
                    if let Some(level) = level {
                        // A header opens a level on the ladder before it is
                        // placed, so it is parented to the header enclosing
                        // it rather than to the one it closes.
                        self.close_headings(level);
                    }
                    let kind = level.map_or(Kind::Paragraph, Kind::Heading);
                    let self_ref = self.push_text(&text, page.page_no, kind, located);
                    if let Some(level) = level {
                        self.headings.push((level, self_ref));
                    }
                }
            }
        }
        // A page ends whatever it was in the middle of.
        self.close_list();
    }

    /// Record what the reading pass measured about a page.
    ///
    /// These are measurements, not verdicts about the document: a page that
    /// decoded to mojibake says so here even when the document as a whole
    /// looked fine to the sampling detection.
    fn on_page_quality(&mut self, page: &pb::PageMarkdown) {
        if page.page_no == 0 || (!page.needs_ocr && page.replacement_runs == 0) {
            return;
        }
        let page_no = i32::try_from(page.page_no).unwrap_or(i32::MAX);
        let item = self.document.pages.entry(page_no).or_insert(doc::PageItem {
            page_no,
            unit: Some(UNIT.to_owned()),
            ..doc::PageItem::default()
        });
        let quality = item.quality.get_or_insert_default();
        quality.replacement_runs = Some(i32::try_from(page.replacement_runs).unwrap_or(i32::MAX));
        if page.needs_ocr {
            quality.ocr_recommended = Some(true);
        }
    }

    /// Put one stripped line into the furniture layer.
    ///
    /// The stripper identifies repeated headers, footers and folio numbers
    /// and deletes them. They are not body text and they do not go back
    /// into it; they go here, which is what `CONTENT_LAYER_FURNITURE` and
    /// the furniture group are for and why both existed empty.
    fn push_furniture(&mut self, text: &str, page_no: u32) {
        let self_ref = format!("#/texts/{}", self.document.texts.len());
        self.document.texts.push(doc::BaseTextItem {
            item: Some(doc::base_text_item::Item::Text(doc::TextItem {
                base: Some(doc::TextItemBase {
                    self_ref: self_ref.clone(),
                    parent: Some(reference(FURNITURE_REF)),
                    content_layer: doc::ContentLayer::Furniture as i32,
                    meta: Some(doc::BaseMeta::default()),
                    prov: provenance(page_no, None),
                    // Which of header, footer or folio this was is not
                    // reported by the stripper, so it is not claimed here.
                    label: doc::DocItemLabel::Text as i32,
                    orig: text.to_owned(),
                    text: text.to_owned(),
                    source: vec![doc::SourceType {
                        source: Some(doc::source_type::Source::Collector(self.source.clone())),
                    }],
                    ..doc::TextItemBase::default()
                }),
            })),
        });
        if let Some(furniture) = self.document.furniture.as_mut() {
            furniture.children.push(reference(&self_ref));
        }
    }

    /// Fold one flattened table back into a grid.
    ///
    /// The detector found the grid; the renderer printed pipe characters;
    /// this puts the grid back. When no grid was detected for this block —
    /// because the caller's stream carried none — the pipe characters are
    /// kept as a paragraph, which is what they were before.
    fn on_table(&mut self, text: &str, page_no: u32) {
        self.close_list();
        let located = self.locate(text, page_no);
        let Some(region) = self.detected_tables.pop() else {
            self.push_text(text, page_no, Kind::Paragraph, located);
            return;
        };
        let parent = self.current_parent();
        let self_ref = format!("#/tables/{}", self.document.tables.len());
        let bbox = region.bbox.as_ref().map(bounding_box).or(located.bbox);
        let kind = pb::TableKind::try_from(region.kind).unwrap_or(pb::TableKind::Unspecified);
        self.document.tables.push(doc::TableItem {
            self_ref: self_ref.clone(),
            parent: Some(reference(&parent)),
            content_layer: doc::ContentLayer::Body as i32,
            // A table of contents is navigation rather than data, and the
            // detector says which it found.
            label: if kind == pb::TableKind::Contents {
                doc::DocItemLabel::DocumentIndex
            } else {
                doc::DocItemLabel::Table
            } as i32,
            prov: provenance(page_no, bbox),
            data: Some(table_data(&region)),
            source: vec![doc::SourceType {
                source: Some(doc::source_type::Source::Collector(self.source.clone())),
            }],
            ..doc::TableItem::default()
        });
        self.link_child(&parent, &self_ref);
    }

    /// Append one code block. `CodeItem` inlines the base fields rather
    /// than wrapping them, so it is built here rather than in
    /// [`Self::push_text`].
    fn push_code(&mut self, text: &str, page_no: u32, language: Option<&str>, located: Located) {
        let parent = self.current_parent();
        let self_ref = format!("#/texts/{}", self.document.texts.len());
        self.document.texts.push(doc::BaseTextItem {
            item: Some(doc::base_text_item::Item::Code(doc::CodeItem {
                self_ref: self_ref.clone(),
                parent: Some(reference(&parent)),
                content_layer: doc::ContentLayer::Body as i32,
                label: doc::DocItemLabel::Code as i32,
                prov: provenance(page_no, located.bbox),
                orig: text.to_owned(),
                text: text.to_owned(),
                // The fence's own word for the language, kept verbatim
                // rather than mapped onto an enum that may not have it.
                code_language_raw: language.map(ToOwned::to_owned),
                source: vec![doc::SourceType {
                    source: Some(doc::source_type::Source::Collector(self.source.clone())),
                }],
                ..doc::CodeItem::default()
            })),
        });
        self.link_child(&parent, &self_ref);
    }

    /// Open a list group, or keep the open one when it is the same kind of
    /// list.
    fn open_list(&mut self, enumerated: bool) {
        if self
            .open_list
            .as_ref()
            .is_some_and(|(counted, _)| *counted == enumerated)
        {
            return;
        }
        self.close_list();
        let parent = self.current_parent();
        let self_ref = format!("#/groups/{}", self.document.groups.len());
        self.document.groups.push(doc::GroupItem {
            self_ref: self_ref.clone(),
            parent: Some(reference(&parent)),
            content_layer: doc::ContentLayer::Body as i32,
            label: if enumerated {
                doc::GroupLabel::OrderedList
            } else {
                doc::GroupLabel::List
            } as i32,
            ..doc::GroupItem::default()
        });
        self.link_child(&parent, &self_ref);
        self.open_list = Some((enumerated, self_ref));
    }

    /// Close the open list, if there is one.
    fn close_list(&mut self) {
        self.open_list = None;
    }

    /// Append one text item of the given kind and return its self ref.
    fn push_text(&mut self, text: &str, page_no: u32, kind: Kind, located: Located) -> String {
        let parent = match (&kind, self.open_list.as_ref()) {
            // A list item hangs off the group that opened for it.
            (Kind::ListItem { .. }, Some((_, group))) => group.clone(),
            _ => self.current_parent(),
        };
        let self_ref = format!("#/texts/{}", self.document.texts.len());
        let base = doc::TextItemBase {
            self_ref: self_ref.clone(),
            parent: Some(reference(&parent)),
            content_layer: doc::ContentLayer::Body as i32,
            meta: Some(doc::BaseMeta::default()),
            prov: provenance(page_no, located.bbox),
            hyperlink: located.hyperlink,
            spans: located.spans,
            // The source's own name for the item, verbatim. A consumer that
            // knows the tagged-PDF vocabulary reads more out of "BlockQuote"
            // or "Caption" than any label this fold could map it onto.
            style_name: located.role.map(|(_, name)| name),
            label: kind.label() as i32,
            orig: text.to_owned(),
            text: text.to_owned(),
            source: vec![doc::SourceType {
                source: Some(doc::source_type::Source::Collector(self.source.clone())),
            }],
            ..doc::TextItemBase::default()
        };
        let variant = match kind {
            Kind::Heading(level) => {
                doc::base_text_item::Item::SectionHeader(doc::SectionHeaderItem {
                    base: Some(base),
                    // Redundant with the nesting, and kept anyway: the
                    // upstream dialect populates both.
                    level,
                })
            }
            Kind::ListItem { marker, enumerated } => {
                doc::base_text_item::Item::ListItem(doc::ListItem {
                    base: Some(base),
                    enumerated,
                    marker: Some(marker),
                })
            }
            Kind::Paragraph => doc::base_text_item::Item::Text(doc::TextItem { base: Some(base) }),
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
        } else if let Some(index) = parent
            .strip_prefix("#/groups/")
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            if let Some(group) = self.document.groups.get_mut(index) {
                group.children.push(reference(child));
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

/// What kind of item a block of markdown becomes.
enum Kind {
    /// Prose.
    Paragraph,
    /// A section header at the given depth.
    Heading(i32),
    /// One item of a list.
    ListItem {
        /// The marker the source printed.
        marker: String,
        /// Whether that marker counts.
        enumerated: bool,
    },
}

impl Kind {
    /// The schema label for this kind of item.
    const fn label(&self) -> doc::DocItemLabel {
        match self {
            Self::Paragraph => doc::DocItemLabel::Paragraph,
            Self::Heading(_) => doc::DocItemLabel::SectionHeader,
            Self::ListItem { .. } => doc::DocItemLabel::ListItem,
        }
    }
}

/// One detected grid as the schema's table data.
///
/// Every cell keeps its own box, cut from the column and row boundaries the
/// detector measured. The first row is marked as the header because that is
/// the convention the renderer itself follows when it prints the grid as
/// markdown; the detector reports no header row of its own.
fn table_data(region: &pb::TableRegion) -> doc::TableData {
    let columns = region
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(0);
    let header_rows = usize::from(region.kind == pb::TableKind::Data as i32);
    let mut cells = Vec::new();
    let mut grid = Vec::new();
    for (row_index, row) in region.rows.iter().enumerate() {
        let mut row_cells = Vec::new();
        for (column_index, text) in row.cells.iter().enumerate() {
            let cell = doc::TableCell {
                bbox: cell_bbox(region, row_index, column_index),
                row_span: 1,
                col_span: 1,
                start_row_offset_idx: i32::try_from(row_index).unwrap_or(i32::MAX),
                end_row_offset_idx: i32::try_from(row_index + 1).unwrap_or(i32::MAX),
                start_col_offset_idx: i32::try_from(column_index).unwrap_or(i32::MAX),
                end_col_offset_idx: i32::try_from(column_index + 1).unwrap_or(i32::MAX),
                text: text.clone(),
                column_header: row_index < header_rows,
                ..doc::TableCell::default()
            };
            row_cells.push(cell.clone());
            cells.push(cell);
        }
        grid.push(doc::TableRow { cells: row_cells });
    }
    doc::TableData {
        table_cells: cells,
        num_rows: i32::try_from(region.rows.len()).unwrap_or(i32::MAX),
        num_cols: i32::try_from(columns).unwrap_or(i32::MAX),
        grid,
        ..doc::TableData::default()
    }
}

/// One cell's box, cut from the grid's own measurements.
///
/// The detector reports one x per column and one y per row, not fences. A
/// cell's right edge is therefore the next column's x, and its top edge the
/// previous row's y — page space grows upwards while a table is read
/// downwards. The outermost edges, which no boundary names, come from the
/// table's own extent. Nothing here is interpolated: every number is one
/// the detector or the extractor measured.
fn cell_bbox(region: &pb::TableRegion, row: usize, column: usize) -> Option<doc::BoundingBox> {
    let extent = region.bbox.as_ref()?;
    let left = *region.column_boundaries.get(column)?;
    let right = region
        .column_boundaries
        .get(column + 1)
        .copied()
        .unwrap_or(extent.x + extent.width);
    let bottom = *region.row_boundaries.get(row)?;
    let top = row
        .checked_sub(1)
        .and_then(|previous| region.row_boundaries.get(previous).copied())
        .unwrap_or(extent.y + extent.height);
    Some(doc::BoundingBox {
        l: left.min(right),
        r: left.max(right),
        t: top.max(bottom),
        b: top.min(bottom),
        coord_origin: Some(doc::CoordOrigin::Bottomleft as i32),
        coord_origin_raw: None,
    })
}

/// A wire rectangle as a schema bounding box, in the same space.
fn bounding_box(rect: &pb::Rect) -> doc::BoundingBox {
    doc::BoundingBox {
        l: rect.x,
        t: rect.y + rect.height,
        r: rect.x + rect.width,
        b: rect.y,
        coord_origin: Some(doc::CoordOrigin::Bottomleft as i32),
        coord_origin_raw: None,
    }
}

/// One block of a page's markdown: a heading, or a paragraph of prose.
enum Block {
    /// An ATX heading line.
    Heading { level: i32, text: String },
    /// One item of a bullet or numbered list.
    ListItem {
        /// The marker the renderer printed, without its trailing space.
        marker: String,
        /// Whether the marker is a number rather than a bullet.
        enumerated: bool,
        /// The item's text, marker removed.
        text: String,
    },
    /// A fenced code block.
    Code {
        /// The language on the opening fence, when it named one.
        language: Option<String>,
        /// The code, fences removed and indentation kept.
        text: String,
    },
    /// A run of pipe-syntax rows: a table the renderer flattened.
    Table(String),
    /// Prose.
    Paragraph(String),
}

/// Split page markdown into blocks.
///
/// This is a structural read of markdown, not a markdown parser, and it
/// reads exactly the constructs the extraction renderer emits: ATX
/// headings, fenced code, `-` and `1.` list markers, pipe rows, and
/// blank-line-separated prose. Anything else is prose, which is what
/// generated markdown mostly is.
fn blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut fence: Option<(Option<String>, Vec<String>)> = None;

    for line in markdown.lines() {
        // A fence swallows everything until it is closed, so nothing
        // inside a code block is read as markdown.
        if let Some((language, body)) = fence.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code {
                    language: language.clone(),
                    text: body.join("\n"),
                });
                fence = None;
            } else {
                body.push(line.to_owned());
            }
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            flush_paragraph(&mut paragraph, &mut blocks);
            let language = rest.trim();
            fence = Some((
                (!language.is_empty()).then(|| language.to_owned()),
                Vec::new(),
            ));
            continue;
        }

        if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some((level, text)) = atx_heading(line) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading { level, text });
            continue;
        }
        if let Some((marker, enumerated, text)) = list_item(line) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::ListItem {
                marker,
                enumerated,
                text,
            });
            continue;
        }
        if is_table_row(line) {
            // Pipe rows accumulate like a paragraph and are recognized as a
            // table when the run ends, because one row is not a table.
            paragraph.push(line.trim());
            continue;
        }
        paragraph.push(line.trim());
    }
    if let Some((language, body)) = fence {
        // An unterminated fence is still a code block; the renderer
        // produced it and the page simply ended.
        blocks.push(Block::Code {
            language,
            text: body.join("\n"),
        });
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    blocks
}

/// End the paragraph being accumulated, if there is one.
///
/// A run of lines that are all pipe rows is a table rather than prose.
fn flush_paragraph(lines: &mut Vec<&str>, blocks: &mut Vec<Block>) {
    if lines.is_empty() {
        return;
    }
    let text = lines.join("\n");
    let table = lines.len() > 1 && lines.iter().all(|line| is_table_row(line));
    blocks.push(if table {
        Block::Table(text)
    } else {
        Block::Paragraph(text)
    });
    lines.clear();
}

/// Whether a line is a pipe-syntax table row.
fn is_table_row(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|') && line.ends_with('|') && line.len() > 1
}

/// Parse a list line into its marker, whether the marker counts, and the
/// text after it.
///
/// Only the markers the extraction renderer emits are recognized: `- ` for
/// bullets and `N. ` for numbers. A line beginning with a dash and no space
/// is a sentence that starts with a dash.
fn list_item(line: &str) -> Option<(String, bool, String)> {
    let line = line.trim_start();
    if let Some(rest) = line.strip_prefix("- ") {
        let text = rest.trim();
        return (!text.is_empty()).then(|| ("-".to_owned(), false, text.to_owned()));
    }
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = line.get(digits.len()..)?.strip_prefix(". ")?;
    let text = rest.trim();
    (!text.is_empty()).then(|| (format!("{digits}."), true, text.to_owned()))
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

/// A value, unless it is empty.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// A JSON-Pointer reference with no character range.
fn fine_ref(target: String) -> doc::FineRef {
    doc::FineRef {
        r#ref: target,
        range: None,
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

    /// The base of any text item that has one. `CodeItem` does not: it
    /// inlines the base fields rather than wrapping them.
    fn base_of(item: &doc::BaseTextItem) -> &doc::TextItemBase {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Text(text) => text.base.as_ref().expect("a base"),
            doc::base_text_item::Item::SectionHeader(header) => {
                header.base.as_ref().expect("a base")
            }
            doc::base_text_item::Item::ListItem(list_item) => {
                list_item.base.as_ref().expect("a base")
            }
            other => panic!("this fold makes no {other:?}"),
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

    fn quality_of(document: &doc::Document, page_no: i32) -> &doc::PageQuality {
        document.pages[&page_no]
            .quality
            .as_ref()
            .expect("the page was measured")
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

        // Every self_ref is its position in its own arena.
        let mut refs: Vec<String> = Vec::new();
        for (index, item) in document.texts.iter().enumerate() {
            let self_ref = self_ref_of(item);
            assert_eq!(
                self_ref,
                format!("#/texts/{index}"),
                "self_ref matches its arena position"
            );
            refs.push(self_ref);
        }
        for (index, group) in document.groups.iter().enumerate() {
            assert_eq!(group.self_ref, format!("#/groups/{index}"));
            refs.push(group.self_ref.clone());
        }
        for (index, table) in document.tables.iter().enumerate() {
            assert_eq!(table.self_ref, format!("#/tables/{index}"));
            refs.push(table.self_ref.clone());
        }

        // Every parent resolves, and lists the item as its child.
        let parents: Vec<(String, String)> = document
            .texts
            .iter()
            .map(|item| (self_ref_of(item), parent_of(item)))
            .chain(document.groups.iter().map(|group| {
                (
                    group.self_ref.clone(),
                    group.parent.as_ref().expect("a parent").r#ref.clone(),
                )
            }))
            .chain(document.tables.iter().map(|table| {
                (
                    table.self_ref.clone(),
                    table.parent.as_ref().expect("a parent").r#ref.clone(),
                )
            }))
            .collect();
        for (self_ref, parent) in &parents {
            let children = children_of(document, parent);
            assert!(
                children.iter().any(|child| child.r#ref == *self_ref),
                "{parent} lists {self_ref}"
            );
        }

        // Everything listed as a child is an item, exactly once.
        let mut listed: Vec<String> = body
            .children
            .iter()
            .chain(
                document
                    .furniture
                    .as_ref()
                    .map(|furniture| furniture.children.iter())
                    .into_iter()
                    .flatten(),
            )
            .map(|child| child.r#ref.clone())
            .collect();
        for item in &document.texts {
            listed.extend(
                children_of_item(item)
                    .iter()
                    .map(|child| child.r#ref.clone()),
            );
        }
        for group in &document.groups {
            listed.extend(group.children.iter().map(|child| child.r#ref.clone()));
        }
        listed.sort();
        refs.sort();
        assert_eq!(listed, refs, "every item is listed exactly once");
    }

    /// The self ref of any text item, whichever variant it is.
    fn self_ref_of(item: &doc::BaseTextItem) -> String {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Code(code) => code.self_ref.clone(),
            other => base_of(&doc::BaseTextItem {
                item: Some(other.clone()),
            })
            .self_ref
            .clone(),
        }
    }

    /// The parent ref of any text item.
    fn parent_of(item: &doc::BaseTextItem) -> String {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Code(code) => {
                code.parent.as_ref().expect("a parent").r#ref.clone()
            }
            other => base_of(&doc::BaseTextItem {
                item: Some(other.clone()),
            })
            .parent
            .as_ref()
            .expect("a parent")
            .r#ref
            .clone(),
        }
    }

    /// The children of any text item.
    fn children_of_item(item: &doc::BaseTextItem) -> Vec<doc::RefItem> {
        match item.item.as_ref().expect("a variant") {
            doc::base_text_item::Item::Code(code) => code.children.clone(),
            other => base_of(&doc::BaseTextItem {
                item: Some(other.clone()),
            })
            .children
            .clone(),
        }
    }

    /// The children a ref names, whatever kind of item it is.
    fn children_of(document: &doc::Document, self_ref: &str) -> Vec<doc::RefItem> {
        if self_ref == BODY_REF {
            return document.body.as_ref().expect("a body").children.clone();
        }
        if self_ref == FURNITURE_REF {
            return document
                .furniture
                .as_ref()
                .expect("a furniture group")
                .children
                .clone();
        }
        if let Some(index) = self_ref
            .strip_prefix("#/groups/")
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            return document.groups[index].children.clone();
        }
        let index: usize = self_ref
            .strip_prefix("#/texts/")
            .and_then(|rest| rest.parse().ok())
            .unwrap_or_else(|| panic!("parent {self_ref} resolves"));
        children_of_item(&document.texts[index])
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
    fn a_bullet_list_becomes_list_items_inside_a_list_group() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "- first\n- second\n\nprose after\n"));
        let document = fold.take();

        assert_eq!(document.groups.len(), 1, "one list, one group");
        assert_eq!(
            document.groups[0].label,
            doc::GroupLabel::List as i32,
            "a bullet list is not an ordered one"
        );
        assert_eq!(document.groups[0].children.len(), 2);

        let markers: Vec<(&str, bool)> = document.texts[..2]
            .iter()
            .map(|item| match item.item.as_ref() {
                Some(doc::base_text_item::Item::ListItem(list_item)) => (
                    list_item.marker.as_deref().expect("a marker"),
                    list_item.enumerated,
                ),
                other => panic!("a list item, got {other:?}"),
            })
            .collect();
        assert_eq!(markers, [("-", false), ("-", false)]);
        assert_eq!(base_of(&document.texts[0]).text, "first", "marker removed");
        assert_eq!(
            base_of(&document.texts[0]).label,
            doc::DocItemLabel::ListItem as i32
        );

        // Prose after the list is not in it.
        assert_eq!(
            base_of(&document.texts[2]).parent.as_ref().unwrap().r#ref,
            BODY_REF
        );
        assert_sound(&document);
    }

    #[test]
    fn a_numbered_list_is_an_ordered_group_and_keeps_its_numbers() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "1. alpha\n2. beta\n"));
        let document = fold.take();
        assert_eq!(
            document.groups[0].label,
            doc::GroupLabel::OrderedList as i32
        );
        match document.texts[1].item.as_ref() {
            Some(doc::base_text_item::Item::ListItem(list_item)) => {
                assert!(list_item.enumerated);
                assert_eq!(list_item.marker.as_deref(), Some("2."));
            }
            other => panic!("a list item, got {other:?}"),
        }
        assert_sound(&document);
    }

    #[test]
    fn switching_list_kind_starts_a_new_group() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "- bullet\n1. number\n"));
        let document = fold.take();
        assert_eq!(document.groups.len(), 2, "two kinds of list, two groups");
        assert_sound(&document);
    }

    #[test]
    fn a_dash_that_is_not_a_marker_is_prose() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "-not a list\n"));
        let document = fold.take();
        assert!(document.groups.is_empty());
        assert_eq!(
            base_of(&document.texts[0]).label,
            doc::DocItemLabel::Paragraph as i32
        );
    }

    #[test]
    fn a_fenced_block_becomes_a_code_item_without_its_fences() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "```rust\nlet x = 1;\nlet y = 2;\n```\n"));
        let document = fold.take();
        match document.texts[0].item.as_ref() {
            Some(doc::base_text_item::Item::Code(code)) => {
                assert_eq!(code.text, "let x = 1;\nlet y = 2;");
                assert_eq!(code.code_language_raw.as_deref(), Some("rust"));
                assert_eq!(code.label, doc::DocItemLabel::Code as i32);
                assert!(!code.text.contains("```"), "the fences are syntax");
            }
            other => panic!("a code item, got {other:?}"),
        }
        assert_sound(&document);
    }

    #[test]
    fn a_hash_inside_a_fence_is_code_not_a_heading() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "```\n# not a heading\n```\n"));
        let document = fold.take();
        assert_eq!(document.texts.len(), 1);
        assert!(matches!(
            document.texts[0].item.as_ref(),
            Some(doc::base_text_item::Item::Code(_))
        ));
    }

    #[test]
    fn a_flattened_table_becomes_a_grid_again() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(1, "", 1.0));
        fold.consume(&pb::parse_pdf_response::Event::Tables(pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: 600.0,
                    width: 200.0,
                    height: 40.0,
                }),
                column_boundaries: vec![100.0, 200.0],
                row_boundaries: vec![640.0, 620.0],
                rows: vec![
                    pb::TableCells {
                        cells: vec!["Year".to_owned(), "Count".to_owned()],
                    },
                    pb::TableCells {
                        cells: vec!["1843".to_owned(), "7".to_owned()],
                    },
                ],
                kind: pb::TableKind::Data.into(),
            }],
        }));
        fold.consume(&page(1, "| Year | Count |\n| 1843 | 7 |\n"));
        let document = fold.take();

        assert!(
            document.texts.is_empty(),
            "the pipe characters did not become a paragraph"
        );
        assert_eq!(document.tables.len(), 1);
        let table = &document.tables[0];
        assert_eq!(table.label, doc::DocItemLabel::Table as i32);
        assert_eq!(table.prov[0].page_no, 1);

        let data = table.data.as_ref().expect("a grid");
        assert_eq!((data.num_rows, data.num_cols), (2, 2));
        assert_eq!(data.table_cells.len(), 4);
        assert_eq!(data.grid.len(), 2);
        assert_eq!(data.table_cells[0].text, "Year");
        assert!(data.table_cells[0].column_header, "the first row heads it");
        assert!(!data.table_cells[2].column_header);

        // Every cell is cut from the values the detector measured; the
        // outermost edges close on the table's own extent.
        let cell = data.table_cells[3].bbox.as_ref().expect("a cell box");
        assert!((cell.l - 200.0).abs() < f64::EPSILON, "{cell:?}");
        assert!((cell.r - 300.0).abs() < f64::EPSILON, "{cell:?}");
        assert!((cell.b - 620.0).abs() < f64::EPSILON, "{cell:?}");
        assert!((cell.t - 640.0).abs() < f64::EPSILON, "{cell:?}");
        assert_sound(&document);
    }

    #[test]
    fn a_table_of_contents_is_labelled_as_one() {
        let mut fold = DocumentFold::new();
        fold.consume(&pb::parse_pdf_response::Event::Tables(pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                rows: vec![pb::TableCells {
                    cells: vec!["Chapter One".to_owned(), "3".to_owned()],
                }],
                kind: pb::TableKind::Contents.into(),
                ..pb::TableRegion::default()
            }],
        }));
        fold.consume(&page(1, "| Chapter One | 3 |\n| Chapter Two | 9 |\n"));
        let document = fold.take();
        assert_eq!(
            document.tables[0].label,
            doc::DocItemLabel::DocumentIndex as i32,
            "a contents table is navigation, not data"
        );
        let data = document.tables[0].data.as_ref().expect("a grid");
        assert!(
            !data.table_cells[0].column_header,
            "a contents table has no header row"
        );
    }

    #[test]
    fn pipe_characters_with_no_detected_grid_stay_a_paragraph() {
        let mut fold = DocumentFold::new();
        fold.consume(&page(1, "| Year | Count |\n| 1843 | 7 |\n"));
        let document = fold.take();
        assert!(document.tables.is_empty(), "nothing was detected");
        assert_eq!(document.texts.len(), 1, "so nothing was lost either");
        assert!(base_of(&document.texts[0]).text.contains('|'));
        assert_sound(&document);
    }

    #[test]
    fn stripped_furniture_lands_in_the_furniture_layer() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(1, "", 1.0));
        fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
            page_no: 1,
            markdown: "the body of the page".to_owned(),
            furniture: vec!["A Running Head".to_owned(), "12".to_owned()],
            ..pb::PageMarkdown::default()
        }));
        let document = fold.take();

        let furniture = document.furniture.as_ref().expect("a furniture group");
        assert_eq!(furniture.children.len(), 2);
        let head = base_of(&document.texts[0]);
        assert_eq!(head.text, "A Running Head");
        assert_eq!(head.content_layer, doc::ContentLayer::Furniture as i32);
        assert_eq!(head.prov[0].page_no, 1);
        assert_eq!(head.parent.as_ref().unwrap().r#ref, FURNITURE_REF);

        // And the body is still the body.
        let body = base_of(&document.texts[2]);
        assert_eq!(body.content_layer, doc::ContentLayer::Body as i32);
        assert_sound(&document);
    }

    #[test]
    fn a_pages_text_quality_is_measured_not_asserted() {
        let mut fold = DocumentFold::new();
        fold.consume(&info(2, "", 1.0));
        fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
            page_no: 1,
            markdown: "clean text".to_owned(),
            ..pb::PageMarkdown::default()
        }));
        fold.consume(&pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
            page_no: 2,
            markdown: "garbled text".to_owned(),
            needs_ocr: true,
            ocr_reason: pb::OcrReason::SuspectedGarbled.into(),
            replacement_runs: 7,
            ..pb::PageMarkdown::default()
        }));
        let document = fold.take();

        assert!(
            document.pages[&1].quality.is_none(),
            "a clean page has nothing to report"
        );
        let quality = quality_of(&document, 2);
        assert_eq!(quality.replacement_runs, Some(7));
        assert_eq!(quality.ocr_recommended, Some(true));
        assert!(
            quality.garble_score.is_none(),
            "the letter-frequency score is not this pipeline's to give"
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
