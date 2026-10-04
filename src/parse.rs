// SPDX-License-Identifier: Apache-2.0

//! The parse driver: one PDF in, a live event stream out.
//!
//! # Why the upload is buffered and the output still is not
//!
//! A PDF's cross-reference table is at the *end* of the file, so no page can
//! be located until the last byte has arrived. Buffering the upload is
//! forced by the format, and no amount of protocol design removes it.
//!
//! What is not forced is buffering the *output*. Detection runs first and
//! `info` goes out the moment it returns — the ~10-50ms routing answer —
//! before a single page has been extracted. `tests/streaming.rs` holds the
//! test that fails if someone turns this back into a batch.
//!
//! Past `info` the stream is incremental in rendering, not in extraction.
//! The extraction pass below is one library call that reads every selected
//! page before it returns; only then are the pages rendered, one at a time,
//! each `page` going out as its markdown is rendered. Between `info` and
//! the first page there is therefore a stretch that sends nothing, which is
//! why the parse checks its deadline and its caller's presence inside that
//! pass (see [`pdf_inspector::ParseGuard`]) rather than relying on the next
//! send to notice that nobody is listening.
//!
//! # Two library passes in FULL, and not three
//!
//! Detection (`process_pdf_mem_with_options` in detect-only mode) runs
//! first and alone, because `info` is the product's front door and it has
//! to go out before anything expensive starts. It is cheap: it reads
//! content streams, not glyphs.
//!
//! FULL then runs exactly one more:
//! `extract_text_with_positions_rects_and_forms_mem_with_ocr_layer`, whose
//! runs, rectangles, line segments and form placements answer everything
//! the rest of the mode needs. The markdown is rendered from those runs here with
//! [`to_markdown_from_items_with_rects_and_page_count`]; the tables come
//! from the same runs plus the vector geometry; the layout verdict comes
//! from the runs and the tables; the text-quality verdicts and the garble
//! score come from `analyze_text_quality` over the runs.
//!
//! There used to be a third pass. Layout complexity and the per-page
//! text-quality verdicts were fetched with a whole separate analysis pass
//! over the file, because the column detector and the quality module were
//! both private to the parser crate and neither could be reached from items
//! a caller held. The crate is vendored now and both are public
//! (`vendor/pdf-inspector/README.md`), so the answer is computed from the
//! runs already in hand and the pass is gone.
//!
//! Rendering from runs rather than calling `extract_pages_markdown_mem` is
//! the other choice worth stating. That entry point takes no options, it
//! hardcodes `MarkdownOptions::default()`, and it throws the runs away, and
//! the runs are where every coordinate, every font, every marked-content id
//! and the entire link layer live.
//!
//! Two optional passes remain, each paid for only by the call that asks:
//! `emit_structure` reads the tagged structure tree, `emit_metadata` reads
//! the document's own dictionaries, and `report_invisible` re-walks the
//! content streams with the invisible layer kept, but only for a document
//! whose first walk said there was an invisible layer to keep.
//! [`Metrics::parser_pass`](crate::metrics::Metrics::parser_pass) counts
//! them, so the count is a thing tests hold rather than a claim in a
//! comment.
//!
//! [`to_markdown_from_items_with_rects_and_page_count`]: pdf_inspector::to_markdown_from_items_with_rects_and_page_count
//!
//! # Page indexing
//!
//! The wire is 1-indexed everywhere, as PDF viewers are, and so are the
//! positioned items (`TextItem::page`) and `PdfOptions::pages`. Any
//! conversion happens here, at the library boundary, and nowhere else.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use pdf_inspector::{Interrupt, MarkdownOptions, ParseGuard, PdfOptions, PdfType, ProcessMode};
use tokio::sync::mpsc;
use tonic::Status;

use crate::document_fold::DocumentFold;
use crate::furniture;
use crate::metrics::Metrics;
use crate::proto::v1 as pb;
use crate::spans;
use crate::structure;
use crate::tables;

/// How the call ended.
#[derive(Debug)]
pub enum Outcome {
    /// A `status` trailer was delivered. The stream ends cleanly.
    Complete,
    /// The client stopped reading. Nothing further can be said to it.
    Abandoned,
    /// The call must end with this gRPC status.
    Failed(Box<Status>),
}

/// Internal control flow: either the client is gone or the call has failed.
enum Abort {
    /// The client stopped reading.
    Gone,
    /// The call must end with this status.
    Failed(Box<Status>),
}

impl From<Status> for Abort {
    fn from(status: Status) -> Self {
        Self::Failed(Box::new(status))
    }
}

/// The outbound half of a response stream, usable from a blocking thread.
///
/// Backpressure lives here. The fast path is a non-blocking `try_send`; only a
/// consumer that is genuinely behind reaches the waiting path, and that wait
/// is bounded, because a client that has stopped reading altogether would
/// otherwise pin a blocking-pool thread forever.
pub struct Sink {
    /// The channel feeding the tonic response stream.
    tx: mpsc::Sender<Result<pb::ParsePdfResponse, Status>>,
    /// How long to wait on a full channel before giving the call up.
    stall: Duration,
}

impl Sink {
    /// Wrap a response channel.
    #[must_use]
    pub fn new(tx: mpsc::Sender<Result<pb::ParsePdfResponse, Status>>, stall: Duration) -> Self {
        Self { tx, stall }
    }

    /// Put one event on the wire, waiting for the client if the channel is
    /// full.
    fn send(&self, event: pb::parse_pdf_response::Event) -> Result<(), Abort> {
        let message = Ok(pb::ParsePdfResponse { event: Some(event) });
        let message = match self.tx.try_send(message) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(Abort::Gone),
            Err(mpsc::error::TrySendError::Full(message)) => message,
        };
        // Inside `spawn_blocking` there is always a runtime handle; be
        // defensive anyway, because blocking without one panics.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Err(Abort::Gone);
        };
        match runtime.block_on(self.tx.send_timeout(message, self.stall)) {
            Ok(()) => Ok(()),
            // A client that has read nothing for the whole stall window has
            // abandoned the call. Reporting a status to it would mean sending
            // on the channel it is not draining, so there is nothing useful
            // left to do but free the thread.
            Err(_) => Err(Abort::Gone),
        }
    }
}

/// The two consumers of one parse's events.
///
/// The wire is one; the Document fold is the other, when the caller asked
/// for a Document. Every event goes to the fold, including the classes the
/// caller kept off the wire — the Document is a projection of what this
/// parse *found*, not of what the caller chose to be sent, and a
/// coordinator that wants boxes on its items should not have to pay for the
/// span events as well.
struct Events<'a> {
    /// The outbound half of the response stream.
    sink: &'a Sink,
    /// The fold, when `options.emit_document` was set.
    fold: Option<DocumentFold>,
}

impl<'a> Events<'a> {
    /// A router for one call.
    fn new(sink: &'a Sink, emit_document: bool) -> Self {
        Self {
            sink,
            fold: emit_document.then(DocumentFold::new),
        }
    }

    /// Put an event on the wire, and through the fold on its way.
    fn send(&mut self, event: pb::parse_pdf_response::Event) -> Result<(), Abort> {
        self.route(event, true)
    }

    /// Fold an event, and put it on the wire only when `on_wire`.
    fn route(&mut self, event: pb::parse_pdf_response::Event, on_wire: bool) -> Result<(), Abort> {
        if let Some(fold) = self.fold.as_mut() {
            fold.consume(&event);
        }
        if on_wire {
            self.sink.send(event)
        } else {
            Ok(())
        }
    }

    /// Whether an optional event class has any consumer, and is therefore
    /// worth building.
    const fn wanted(&self, on_wire: bool) -> bool {
        on_wire || self.fold.is_some()
    }
}

/// Classify `bytes` and stream the events the mode calls for into `sink`,
/// under the default limits and with no deadline.
///
/// Synchronous on purpose: extraction is CPU-bound and parallelizes with
/// rayon internally, so the caller runs this on
/// [`tokio::task::spawn_blocking`] and [`Sink`] bridges back to the async
/// channel. Everything stays in memory; nothing is written anywhere.
#[must_use]
pub fn run(bytes: &[u8], options: &pb::PdfOptions, metrics: &Metrics, sink: &Sink) -> Outcome {
    run_guarded(
        bytes,
        options,
        metrics,
        sink,
        &crate::Limits::default().parse_guard(),
    )
}

/// [`run`] under `guard`: every parser pass decodes within its
/// decompression bounds, and the parse stops at the next page once its
/// deadline passes or its caller cancels.
#[must_use]
pub fn run_guarded(
    bytes: &[u8],
    options: &pb::PdfOptions,
    metrics: &Metrics,
    sink: &Sink,
    guard: &ParseGuard,
) -> Outcome {
    match parse(bytes, options, metrics, sink, guard) {
        Ok(()) => Outcome::Complete,
        Err(Abort::Gone) => Outcome::Abandoned,
        Err(Abort::Failed(status)) => Outcome::Failed(status),
    }
}

/// The parse itself; every failure path funnels through [`Abort`].
fn parse(
    bytes: &[u8],
    options: &pb::PdfOptions,
    metrics: &Metrics,
    sink: &Sink,
    guard: &ParseGuard,
) -> Result<(), Abort> {
    let started = Instant::now();
    // The upload may already have spent the call's time.
    check(guard)?;
    let mode = pb::ProcessMode::try_from(options.mode)
        .map_err(|_| Status::invalid_argument(format!("unknown process mode {}", options.mode)))?;
    let mode = match mode {
        pb::ProcessMode::Unspecified => pb::ProcessMode::Full,
        known => known,
    };

    // What is wrong with the page selection whatever the document is, said
    // before the document is read.
    check_selection(options)?;

    // Pass one: classification. Cheap, and the reason this stream opens with
    // an answer instead of with work.
    let mut detect = PdfOptions::detect_only();
    if !options.password.is_empty() {
        detect = detect.password(options.password.clone());
    }
    metrics.parser_pass();
    let detected = guarded(guard, || {
        pdf_inspector::process_pdf_mem_with_options(bytes, detect)
    })?;
    // Detection's own per-page verdicts, kept because the trailer's
    // `extraction_ocr_reasons` is these merged with what reading the text
    // layer concludes, exactly as the analysis pass used to merge them.
    let detection_reasons = detected.ocr_reasons_by_page.clone();

    // The pages the call selected, resolved against the document before
    // anything is sent: a selection with no page in the document is a
    // caller mistake, not an empty success.
    let mut warnings = Vec::new();
    let selection = select(options, detected.page_count, &mut warnings)?;

    // The optional second consumer of this stream: when the caller asked for
    // a Document, every event is folded on its way out and the folded
    // Document goes out after the last `page`, before the `status` trailer.
    // With the flag off no fold is built and the path is what it was.
    let mut events = Events::new(sink, options.emit_document);

    events.send(pb::parse_pdf_response::Event::Info(pb::PdfInfo {
        pdf_type: pdf_type(detected.pdf_type).into(),
        confidence: detected.confidence,
        page_count: detected.page_count,
        title: detected.title.unwrap_or_default(),
        pages_needing_ocr: detected.pages_needing_ocr,
        ocr_reasons: detected
            .ocr_reasons_by_page
            .into_iter()
            .map(|page| pb::PageOcrReasons {
                page: page.page,
                reasons: page
                    .reasons
                    .iter()
                    .map(|reason| ocr_reason(reason).into())
                    .collect(),
            })
            .collect(),
        detection_time_ms: detected.processing_time_ms,
        ocr_recommended: detected.ocr_recommended,
    }))?;

    // What the file says about itself, read from its own dictionaries by a
    // second reader. It goes out immediately after `info` so that a
    // consumer knows the page boxes, the rotation and the link
    // destinations before any content arrives, and so the fold can measure
    // its pages instead of only naming them.
    if events.wanted(options.emit_metadata) {
        let password = (!options.password.is_empty()).then_some(options.password.as_str());
        // Not `guarded`: a metadata dictionary this reader cannot make
        // sense of is a gap in the metadata, not a failed parse. The text
        // extraction runs off its own reader and is unaffected.
        metrics.parser_pass();
        let metadata = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::metadata::read(bytes, password, guard.max_stream_bytes)
        }))
        .ok()
        .flatten();
        match metadata {
            Some(metadata) => {
                events.route(
                    pb::parse_pdf_response::Event::Metadata(metadata),
                    options.emit_metadata,
                )?;
            }
            None if options.emit_metadata => warnings.push(pb::ParseWarning {
                code: pb::ParseWarningCode::MetadataUnavailable.into(),
                message: "the document's own dictionaries could not be read, so no \
                          metadata event was sent; the text extraction is unaffected"
                    .to_owned(),
            }),
            None => {}
        }
    }

    let mut pages_extracted = 0u32;
    let mut layout = None;
    let mut has_encoding_issues = false;
    let mut extraction_ocr_reasons = Vec::new();
    let mut has_invisible_text = false;

    // What happens after `info` depends on the mode and on what detection
    // found. Scanned and image-based documents have no text layer at all, so
    // there is nothing to analyze or extract at any mode.
    let text_bearing = matches!(detected.pdf_type, PdfType::TextBased | PdfType::Mixed);
    match mode {
        pb::ProcessMode::DetectOnly | pb::ProcessMode::Unspecified => {}
        _ if !text_bearing => {}
        pb::ProcessMode::Analyze => {
            metrics.parser_pass();
            let analyzed = guarded(guard, || {
                pdf_inspector::process_pdf_mem_with_options(
                    bytes,
                    analyze_options(options, &selection),
                )
            })?;
            layout = Some(layout_proto(&analyzed.layout));
            has_encoding_issues = analyzed.has_encoding_issues;
            extraction_ocr_reasons = ocr_reasons_proto(&analyzed.ocr_reasons_by_page);
        }
        pb::ProcessMode::Full if !options.password.is_empty() => {
            // The per-page extraction API takes no password, so an encrypted
            // document is extracted whole and delivered as one event. The
            // trailer says so; `page_no` 0 means "the document".
            let mut full = PdfOptions::new();
            if let Selection::Pages(pages) = &selection {
                full = full.pages(pages.iter().copied());
            }
            full = full.password(options.password.clone());
            metrics.parser_pass();
            let processed = guarded(guard, || {
                pdf_inspector::process_pdf_mem_with_options(bytes, full)
            })?;
            if let Some(markdown) = processed.markdown.filter(|md| !md.is_empty()) {
                let markdown_bytes = markdown.len() as u64;
                let replacement_runs = replacement_runs(&markdown);
                events.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no: 0,
                    markdown,
                    // Whole-document extraction reports its OCR verdicts by
                    // page, and this event is not a page; the trailer's
                    // `extraction_ocr_reasons` carries them instead.
                    needs_ocr: false,
                    ocr_reason: pb::OcrReason::Unspecified.into(),
                    replacement_runs,
                    // The whole-document API takes no markdown options, so
                    // there is no second rendering to difference against.
                    furniture: Vec::new(),
                    // Neither the invisible layer nor a per-page score is
                    // reachable through the whole-document API, and this
                    // event is not a page anyway.
                    invisible: Vec::new(),
                    garble_score: None,
                    // No per-page runs came back either, so there is
                    // nothing to say a rendering left out.
                    dropped: Vec::new(),
                }))?;
                metrics.page_emitted(markdown_bytes);
                pages_extracted = 1;
            }
            warnings.push(pb::ParseWarning {
                code: pb::ParseWarningCode::PasswordFallback.into(),
                message: "a password was supplied, so the markdown was extracted \
                          whole-document and delivered as a single page event"
                    .to_owned(),
            });
            layout = Some(layout_proto(&processed.layout));
            has_encoding_issues = processed.has_encoding_issues;
            extraction_ocr_reasons = ocr_reasons_proto(&processed.ocr_reasons_by_page);
        }
        pb::ProcessMode::Full => {
            // The extraction pass, and the only one this mode needs.
            // Positioned runs rather than markdown, so that the boxes, the
            // fonts, the marked-content ids and the link annotations stay
            // in hand instead of being rendered away inside the library;
            // and the vector geometry with them, so the ruled-table
            // detectors are reachable at all.
            let filter = selection.filter();
            metrics.parser_pass();
            // With the library's OCR-layer fallback: a scanned page made
            // searchable draws no visible text, and its runs are its
            // invisible OCR layer instead of nothing, exactly as the
            // library's own region and whole-document pipelines read it.
            let pdf_inspector::OcrLayerExtraction {
                extraction: (items, rects, lines),
                mut forms,
                skipped_invisible,
                ocr_layer_pages,
                rotated_pages,
            } = guarded(guard, || {
                pdf_inspector::extract_text_with_positions_rects_and_forms_mem_with_ocr_layer(
                    bytes,
                    filter.as_ref(),
                )
            })?;
            // A landscape page drawn turned comes back in a frame of the
            // library's own, off the page. The page boxes are read only when
            // there is such a page, and what goes on the wire is moved onto
            // the page a reader sees as it is emitted (`crate::frame`).
            let moved = if rotated_pages.is_empty() {
                BTreeMap::new()
            } else {
                let password = (!options.password.is_empty()).then_some(options.password.as_str());
                let geometry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::metadata::read(bytes, password, guard.max_stream_bytes)
                }))
                .ok()
                .flatten()
                .map(|metadata| metadata.pages)
                .unwrap_or_default();
                crate::frame::movable_pages(&rotated_pages, &geometry)
            };
            crate::frame::place_forms(&moved, &mut forms);
            if let Some(fold) = events.fold.as_mut() {
                fold.turn_pages(moved.keys().copied());
            }
            // Where the page invoked Form XObjects. A vector figure is one,
            // and it draws no image run, so this is the only record of
            // where it sits. They ride the spans event, after the runs.
            let mut forms = spans::forms_by_page(forms);
            // Whether any page drew invisible text, said whether or not
            // anyone asked for the runs themselves: the layer the walk left
            // out, and the OCR layers it adopted as a scan's text. A text
            // layer nobody is told about is what this reports.
            has_invisible_text = skipped_invisible || !ocr_layer_pages.is_empty();

            // The text-quality verdicts, from the runs this call already
            // holds rather than from a second read of the file.
            let quality = pdf_inspector::analyze_text_quality(&items);
            // An OCR layer's misreadings are not a broken font encoding, and
            // its page needs OCR already; letting them set the document's
            // flag would send every page of a mostly born-digital document
            // to recognition for one scanned page.
            has_encoding_issues = quality
                .pages_needing_ocr
                .iter()
                .any(|page| !ocr_layer_pages.contains(page));
            let mut verdicts = page_verdicts(&detection_reasons, &quality);
            // A page whose text is its OCR layer is a scan. The layer is
            // what the markdown carries, so a caller without OCR still gets
            // the words, but no reader saw them and a caller with OCR
            // should read the page again.
            for page in &ocr_layer_pages {
                add_verdict(&mut verdicts, *page, pb::OcrReason::Scanned);
            }

            // The invisible layer's own runs, which need the walk run again
            // with the layer kept. Taken only when someone is listening and
            // only when the first walk said it left something out; an
            // adopted OCR layer is in the runs already, so the difference
            // the second walk is read against has none of it.
            let mut invisible = if events.wanted(options.report_invisible) && skipped_invisible {
                metrics.parser_pass();
                let (kept, _) = guarded(guard, || {
                    pdf_inspector::extract_text_with_positions_mem_pages_with_invisible(
                        bytes,
                        filter.as_ref(),
                        true,
                    )
                })?;
                spans::by_page(spans::only_invisible(&items, kept))
            } else {
                BTreeMap::new()
            };

            // Which runs are page chrome, weighed over the whole document
            // before it is split into pages. Repetition is most of the
            // evidence and no single page carries any.
            let chrome = if events.wanted(options.report_furniture) {
                furniture::Chrome::detect(&items, detected.page_count)
            } else {
                furniture::Chrome::none()
            };

            let mut by_page = spans::by_page(items);

            // The document's own structure tree, when anyone wants it.
            // Untagged documents return an empty list, which is the honest
            // answer rather than a failure.
            let mut structure = if events.wanted(options.emit_structure) {
                let selected = selection.listed();
                metrics.parser_pass();
                let elements = guarded(guard, || {
                    pdf_inspector::extract_structure_elements_mem(bytes, selected)
                })?;
                structure::by_page(elements)
            } else {
                BTreeMap::new()
            };

            // The layout verdict, accumulated page by page below out of the
            // detectors the pages run through anyway.
            let mut pages_with_tables = Vec::new();
            let mut pages_with_columns = Vec::new();

            for page_no in selection.pages(detected.page_count) {
                // Rendering, tables and the fold run here rather than in
                // the parser, so the call's time is checked here too.
                check(guard)?;
                let page_items = by_page.remove(&page_no).unwrap_or_default();

                // A page that drew a picture and no text at all is a scan,
                // or a page whose words are in a figure, and it needs OCR
                // either way. Sampling detection may never have looked at
                // it and the quality pass only scores pages that have text,
                // so this is the check that reads every page it extracts.
                if let Some(reason) = untexted_page_reason(&page_items, forms.get(&page_no)) {
                    add_verdict(&mut verdicts, page_no, reason);
                }

                // The roles go out before the runs they describe, so a
                // consumer reading the whole stream never has to look
                // ahead.
                if let Some(roles) = structure.remove(&page_no) {
                    events.route(
                        pb::parse_pdf_response::Event::Structure(roles),
                        options.emit_structure,
                    )?;
                }

                // The grids. They are detected on every page whether or not
                // the caller wants the event, because the layout verdict is
                // "which pages have a data table" and that is this
                // detector's answer; the analysis pass that used to be
                // asked for it ran the same three detectors over the same
                // runs, one whole read of the file later. They go out
                // before the markdown that flattens them into pipe
                // characters.
                let page_tables = tables::page_tables(page_no, &page_items, &rects, &lines);
                if page_tables.as_ref().is_some_and(tables::has_data_table) {
                    pages_with_tables.push(page_no);
                }
                // Columns are counted after the tables, because a table's
                // own column spacing looks like a gutter and the detector
                // needs telling that this page has one.
                let columns = pdf_inspector::extractor::detect_columns(
                    &page_items,
                    page_no,
                    pages_with_tables.last() == Some(&page_no),
                );
                if columns.len() >= 2 {
                    pages_with_columns.push(page_no);
                }
                if let Some(mut tables) = page_tables {
                    if let Some(crop) = moved.get(&page_no) {
                        crate::frame::place_tables(*crop, &mut tables);
                    }
                    events.route(
                        pb::parse_pdf_response::Event::Tables(tables),
                        options.emit_tables,
                    )?;
                }

                // The runs go out before the rendering they produced, so a
                // consumer reading both never has to buffer one to
                // interpret the other.
                // The chrome verdicts ride the runs: the fold joins the
                // rendering back to the runs by their letters, and a margin
                // number sharing a baseline with a body line would otherwise
                // sit in the middle of that line's letters, where no block
                // of the rendering can match across it.
                if events.wanted(options.emit_spans) {
                    let mut spans = spans::page_spans_marking(page_no, &page_items, |item| {
                        chrome.convicts(item)
                    });
                    if let Some(crop) = moved.get(&page_no) {
                        crate::frame::place_spans(*crop, &mut spans.spans);
                    }
                    spans.spans.extend(
                        forms
                            .remove(&page_no)
                            .unwrap_or_default()
                            .iter()
                            .map(spans::form_span),
                    );
                    events.route(
                        pb::parse_pdf_response::Event::Spans(spans),
                        options.emit_spans,
                    )?;
                }

                // The page's chrome comes out of the page before the
                // renderer is given it, and this is the only order that
                // works. The renderer assembles a line from the runs
                // sharing its baseline, and a margin line number shares the
                // baseline of the line it stands beside, so a number left
                // in the input is not a run the rendering omits or keeps:
                // it is fused into the middle of the body text, arriving
                // glued to a word with no space. No verdict taken on the
                // output can separate them again. Taking the runs out of
                // the input is what keeps a page's body text the body's,
                // in the markdown and in every item folded from it.
                let (chrome_runs, page_items): (Vec<_>, Vec<_>) = page_items
                    .into_iter()
                    .partition(|item| chrome.convicts(item));
                let furniture: Vec<String> = chrome_runs
                    .iter()
                    .map(|item| item.text.trim().to_owned())
                    .collect();
                // What is left is content, and what the rendering does with
                // it is what `dropped` answers for.
                let content: Vec<pb::TextSpan> = if events.wanted(options.report_furniture) {
                    page_items
                        .iter()
                        .filter(|item| {
                            matches!(
                                item.item_type,
                                pdf_inspector::types::ItemType::Text
                                    | pdf_inspector::types::ItemType::FormField
                            )
                        })
                        .map(spans::span)
                        .collect()
                } else {
                    Vec::new()
                };

                let markdown = pdf_inspector::to_markdown_from_items_with_rects_and_page_count(
                    page_items,
                    MarkdownOptions::default(),
                    &[],
                    detected.page_count,
                );
                let markdown = markdown.trim().to_owned();
                let mut dropped = absent_from(content, &markdown);
                let markdown_bytes = markdown.len() as u64;
                let reasons = verdicts.get(&page_no);
                let needs_ocr = reasons.is_some();
                let reason = reasons
                    .and_then(|reasons| reasons.first())
                    .copied()
                    .unwrap_or(pb::OcrReason::Unspecified);
                let replacement_runs = replacement_runs(&markdown);
                // The rendering is the last thing the encoding backstop can
                // look at, and it catches a page whose runs were each
                // individually unremarkable.
                if !ocr_layer_pages.contains(&page_no) {
                    has_encoding_issues |= pdf_inspector::detect_encoding_issues(&markdown);
                }
                let mut page_invisible: Vec<pb::TextSpan> = invisible
                    .remove(&page_no)
                    .unwrap_or_default()
                    .iter()
                    .map(spans::span)
                    .collect();
                if let Some(crop) = moved.get(&page_no) {
                    crate::frame::place_spans(*crop, &mut dropped);
                    crate::frame::place_spans(*crop, &mut page_invisible);
                }
                events.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no,
                    markdown,
                    needs_ocr,
                    ocr_reason: reason.into(),
                    replacement_runs,
                    furniture,
                    invisible: page_invisible,
                    garble_score: garble_score(&quality, page_no),
                    dropped,
                }))?;
                // Counted after the send: "emitted" means on the wire, and a
                // counter that runs ahead of a blocked send is how a batch
                // hides from the streaming tests.
                metrics.page_emitted(markdown_bytes);
                pages_extracted += 1;
            }

            layout = Some(pb::LayoutComplexity {
                is_complex: !pages_with_tables.is_empty() || !pages_with_columns.is_empty(),
                pages_with_tables,
                pages_with_columns,
            });
            extraction_ocr_reasons = verdicts
                .iter()
                .map(|(page, reasons)| pb::PageOcrReasons {
                    page: *page,
                    reasons: reasons.iter().map(|reason| (*reason).into()).collect(),
                })
                .collect();
        }
    }

    // The trailer answers for the pages that were read. Detection's verdicts
    // cover the whole document and went out on `info`.
    if let Some(selected) = selection.filter() {
        extraction_ocr_reasons.retain(|reasons| selected.contains(&reasons.page));
    }

    // The fold has seen every content event now, so its Document goes out
    // here — after the last `page`, before the `status` trailer that closes
    // the stream.
    check(guard)?;
    if let Some(fold) = events.fold.as_mut() {
        sink.send(pb::parse_pdf_response::Event::Document(fold.take()))?;
    }
    sink.send(pb::parse_pdf_response::Event::Status(pb::ParseStatus {
        pages_extracted,
        warnings,
        layout,
        has_encoding_issues,
        processing_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        extraction_ocr_reasons,
        has_invisible_text,
    }))?;
    Ok(())
}

/// The content runs the rendering left out.
///
/// The comparison is on letters and digits only, because the renderer joins
/// runs with spaces, repairs hyphenation across line ends and adds markdown
/// punctuation, none of which changes a letter.
///
/// A run is looked for in the whole rendering rather than forwards from
/// where the last one was found. Reading order is not what a rendering is
/// in: the renderer reads a multi-column page one column at a time, so runs
/// the page drew side by side come out pages apart in its output, and a
/// forward scan reads every one of them as missing. That is exactly the
/// mistake that filed a two-column paper's second column as page
/// furniture. Searching the whole rendering costs a page-sized scan per run
/// and answers the question that was actually asked: is this text in the
/// output at all.
///
/// Short runs are not looked for. A run of one or two letters occurs
/// somewhere in any page of prose by accident, so reporting on it would be
/// noise in whichever direction the accident fell.
fn absent_from(runs: Vec<pb::TextSpan>, markdown: &str) -> Vec<pb::TextSpan> {
    if runs.is_empty() {
        return Vec::new();
    }
    let rendered = letters(markdown);
    runs.into_iter()
        .filter(|run| {
            let needle = letters(&run.text);
            needle.chars().count() >= MIN_DROPPED_LETTERS && !rendered.contains(&needle)
        })
        .collect()
}

/// How many letters a run needs before its absence from the rendering
/// means anything.
const MIN_DROPPED_LETTERS: usize = 3;

/// A string reduced to its lower-case letters and digits.
fn letters(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How many runs of U+FFFD the text decoded to, a consecutive run counting
/// once.
///
/// One replacement character is a glyph the font could not map; a run of
/// forty is a page whose encoding is gone. Counting runs rather than
/// characters keeps those two apart without letting a long word of garble
/// outweigh a page of scattered failures.
fn replacement_runs(text: &str) -> u32 {
    let mut runs = 0;
    let mut inside = false;
    for character in text.chars() {
        if character == char::REPLACEMENT_CHARACTER {
            if !inside {
                runs += 1;
                inside = true;
            }
        } else {
            inside = false;
        }
    }
    runs
}

/// The pages a call selected, resolved against the document.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    /// Every page.
    All,
    /// These 1-indexed pages, in the order the call asked for them, each
    /// once, and every one of them in the document.
    Pages(Vec<u32>),
}

impl Selection {
    /// The pages to walk, in order.
    fn pages(&self, page_count: u32) -> Vec<u32> {
        match self {
            Self::All => (1..=page_count).collect(),
            Self::Pages(pages) => pages.clone(),
        }
    }

    /// The pages as a library page filter: `None` for every page. An empty
    /// filter means "all pages" on the wire but "no pages" to the library,
    /// so every page is spelled as no filter at all.
    fn filter(&self) -> Option<HashSet<u32>> {
        self.listed().map(|pages| pages.iter().copied().collect())
    }

    /// The selected pages, or `None` for every page.
    fn listed(&self) -> Option<&[u32]> {
        match self {
            Self::All => None,
            Self::Pages(pages) => Some(pages),
        }
    }
}

/// Refuse what is wrong with a page selection whatever the document is.
///
/// Page 0 cannot be refused by the library — its 1-indexed filter would
/// treat it as out of range and its 0-indexed extractor would treat it as
/// the first page — so it is refused here, where it is still a caller
/// mistake rather than a silent wrong answer. A list and a span at once
/// would leave which one counts to a guess, so they are refused too.
fn check_selection(options: &pb::PdfOptions) -> Result<(), Status> {
    let zero = || Status::invalid_argument("pages are 1-indexed; page 0 does not exist");
    if options.pages.contains(&0) || options.first_page == Some(0) || options.last_page == Some(0) {
        return Err(zero());
    }
    let spanned = options.first_page.is_some() || options.last_page.is_some();
    if spanned && !options.pages.is_empty() {
        return Err(Status::invalid_argument(
            "select pages with either `pages` or `first_page`/`last_page`, not both",
        ));
    }
    if let (Some(first), Some(last)) = (options.first_page, options.last_page)
        && first > last
    {
        return Err(Status::invalid_argument(format!(
            "the page span ends on page {last}, before it starts on page {first}"
        )));
    }
    Ok(())
}

/// Resolve a call's page selection against the document's page count.
///
/// A listed page is selected once, in the place it was first listed, and a
/// listed page past the end is left out with a
/// `PARSE_WARNING_CODE_PAGES_OUT_OF_RANGE` warning: the caller asked about
/// something that does not exist, and inventing a page for it would be a
/// worse answer than saying so. A span is clamped to the document, which is
/// what a span means. A selection with no page in the document at all is
/// `INVALID_ARGUMENT`, because an empty success would read as a document
/// with nothing in it.
fn select(
    options: &pb::PdfOptions,
    page_count: u32,
    warnings: &mut Vec<pb::ParseWarning>,
) -> Result<Selection, Status> {
    let none_exist = || {
        Status::invalid_argument(format!(
            "none of the selected pages exist; the document has {page_count} page(s)"
        ))
    };
    if options.first_page.is_some() || options.last_page.is_some() {
        let first = options.first_page.unwrap_or(1);
        let last = options.last_page.unwrap_or(page_count).min(page_count);
        if first > last {
            return Err(none_exist());
        }
        return Ok(Selection::Pages((first..=last).collect()));
    }
    if options.pages.is_empty() {
        return Ok(Selection::All);
    }
    let mut seen = vec![false; page_count as usize + 1];
    let mut pages = Vec::new();
    let mut out_of_range = 0usize;
    let mut first_out_of_range = None;
    for &page in &options.pages {
        match seen.get_mut(page as usize) {
            Some(seen) if !*seen => {
                *seen = true;
                pages.push(page);
            }
            Some(_) => {}
            None => {
                out_of_range += 1;
                first_out_of_range.get_or_insert(page);
            }
        }
    }
    if pages.is_empty() {
        return Err(none_exist());
    }
    if let Some(first) = first_out_of_range {
        warnings.push(pb::ParseWarning {
            code: pb::ParseWarningCode::PagesOutOfRange.into(),
            message: format!(
                "{out_of_range} listed page(s), the first of them page {first}, are past the end \
                 of the {page_count}-page document and were left out"
            ),
        });
    }
    Ok(Selection::Pages(pages))
}

/// The library options for the analysis pass, honouring the call's page
/// selection and password.
fn analyze_options(options: &pb::PdfOptions, selection: &Selection) -> PdfOptions {
    let mut analyze = PdfOptions::new().mode(ProcessMode::Analyze);
    if let Selection::Pages(pages) = selection {
        analyze = analyze.pages(pages.iter().copied());
    }
    if !options.password.is_empty() {
        analyze = analyze.password(options.password.clone());
    }
    analyze
}

/// Per-page OCR verdicts, keyed by 1-indexed page.
///
/// Two sources, merged the way the library merges them: what sampling
/// detection concluded about the document, and what reading its text layer
/// concluded about each page. A page in the map is a page judged unusable;
/// its first reason is the one a per-page event can hold, and the whole
/// list travels on the trailer.
fn page_verdicts(
    detection: &[pdf_inspector::PageOcrReasons],
    quality: &pdf_inspector::TextQualityReport,
) -> BTreeMap<u32, Vec<pb::OcrReason>> {
    let mut verdicts: BTreeMap<u32, Vec<pb::OcrReason>> = BTreeMap::new();
    let mut add = |page: u32, reasons: &[String]| {
        let entry = verdicts.entry(page).or_default();
        for reason in reasons {
            let reason = ocr_reason(reason);
            if !entry.contains(&reason) {
                entry.push(reason);
            }
        }
    };
    for page in detection {
        add(page.page, &page.reasons);
    }
    for (page, reasons) in &quality.reasons_by_page {
        add(*page, reasons);
    }
    // A page the quality pass listed without giving a reason for is still a
    // page it judged unusable.
    for page in &quality.pages_needing_ocr {
        verdicts.entry(*page).or_default();
    }
    verdicts
}

/// Add one reason to a page's verdict, once.
fn add_verdict(verdicts: &mut BTreeMap<u32, Vec<pb::OcrReason>>, page: u32, reason: pb::OcrReason) {
    let reasons = verdicts.entry(page).or_default();
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

/// Why a page that drew no text at all needs OCR, or `None` when it drew
/// text or drew nothing.
///
/// Text is a run of glyphs or a form field's value with something other
/// than whitespace in it. A page without any that placed an image is a scan
/// or a picture of words, which is `SCANNED`; one that placed only a Form
/// XObject drew its content as paths the parser cannot read as characters,
/// which is `NO_TEXT`. A page that drew none of these is blank, and a blank
/// page stays a blank page.
fn untexted_page_reason(
    items: &[pdf_inspector::TextItem],
    forms: Option<&Vec<pdf_inspector::PdfForm>>,
) -> Option<pb::OcrReason> {
    use pdf_inspector::types::ItemType;
    let has_text = items.iter().any(|item| {
        matches!(item.item_type, ItemType::Text | ItemType::FormField)
            && !item.text.trim().is_empty()
    });
    if has_text {
        None
    } else if items
        .iter()
        .any(|item| matches!(item.item_type, ItemType::Image))
    {
        Some(pb::OcrReason::Scanned)
    } else if forms.is_some_and(|forms| !forms.is_empty()) {
        Some(pb::OcrReason::NoText)
    } else {
        None
    }
}

/// How far a page's letters sit from where a natural language puts them, or
/// `None` when the page carried too few of them for the question to have an
/// answer.
///
/// The library reports the correlation, which is 1.0 for text whose letters
/// fall where they should. The wire reports its distance from that, because
/// `garble_score` is defined with 0.0 as clean. Nothing else is done to it:
/// the number is the library's own, turned the right way up.
fn garble_score(quality: &pdf_inspector::TextQualityReport, page_no: u32) -> Option<f64> {
    let score = quality.letter_frequency.get(&page_no)?;
    score
        .is_measurable()
        .then(|| (1.0 - score.english_cosine).clamp(0.0, 1.0))
}

/// Map the library's per-page OCR reasons onto the wire message.
fn ocr_reasons_proto(pages: &[pdf_inspector::PageOcrReasons]) -> Vec<pb::PageOcrReasons> {
    pages
        .iter()
        .map(|page| pb::PageOcrReasons {
            page: page.page,
            reasons: page
                .reasons
                .iter()
                .map(|reason| ocr_reason(reason).into())
                .collect(),
        })
        .collect()
}

/// Call a fallible parser entry point under the call's guard, with a panic
/// guard.
///
/// lopdf can panic on malformed input, and an unwinding panic on the blocking
/// thread would surface as a truncated stream; here it becomes an honest
/// `INTERNAL` instead. `PdfError` maps by variant per the fleet contract.
///
/// An interrupt outranks whatever the call returned. A stream the guard
/// refused reads as an undecodable one to code with no way to report it,
/// so a pass that hit a limit can return a quietly incomplete answer, and
/// the guard's own record is what says it is one.
fn guarded<T>(
    guard: &ParseGuard,
    call: impl FnOnce() -> Result<T, pdf_inspector::PdfError>,
) -> Result<T, Abort> {
    let (outcome, interrupt) =
        guard.run(|| std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)));
    if let Some(why) = interrupt {
        return Err(interrupted(guard, why));
    }
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(pdf_inspector::PdfError::Interrupted(why))) => Err(interrupted(guard, why)),
        Ok(Err(error)) => Err(map_error(error).into()),
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(Status::internal(format!(
                "the parser panicked, which is a bug in grpc-pdf-inspector: {detail}"
            ))
            .into())
        }
    }
}

/// Stop if the call's deadline has passed or its caller has gone.
fn check(guard: &ParseGuard) -> Result<(), Abort> {
    match guard.interrupted() {
        Some(why) => Err(interrupted(guard, why)),
        None => Ok(()),
    }
}

/// How a call the guard stopped ends.
///
/// A caller that cancelled is not told anything: it is gone. A passed
/// deadline is `DEADLINE_EXCEEDED`, and a document that inflates past its
/// limits is `RESOURCE_EXHAUSTED`, the code an oversize upload gets,
/// because it is the same complaint about a different measure of size.
fn interrupted(guard: &ParseGuard, why: Interrupt) -> Abort {
    match why {
        Interrupt::Cancelled => Abort::Gone,
        Interrupt::Deadline => Status::deadline_exceeded(
            "the parse ran past its time budget; raise GRPC_PDF_MAX_PARSE_SECONDS if the \
             document genuinely needs longer",
        )
        .into(),
        Interrupt::DecompressionLimit => Status::resource_exhausted(format!(
            "the document inflates past its decompression limits: a stream past {} bytes, or \
             one read past {} bytes in all; raise GRPC_PDF_MAX_STREAM_BYTES or \
             GRPC_PDF_MAX_DECOMPRESSED_BYTES if the document is genuinely this large",
            guard.max_stream_bytes, guard.max_run_bytes
        ))
        .into(),
    }
}

/// Map a library error onto the fleet's status codes.
///
/// Everything the caller can fix by sending a different file — not a PDF,
/// truncated, malformed, encrypted — is `INVALID_ARGUMENT`. `Io` cannot
/// happen on the diskless path, so it lands in `INTERNAL` as the bug it
/// would be.
fn map_error(error: pdf_inspector::PdfError) -> Status {
    use pdf_inspector::PdfError;
    match error {
        PdfError::NotAPdf(detail) => Status::invalid_argument(format!("not a PDF: {detail}")),
        PdfError::Parse(detail) => Status::invalid_argument(format!("malformed PDF: {detail}")),
        PdfError::InvalidStructure => {
            Status::invalid_argument("malformed PDF: invalid structure".to_owned())
        }
        PdfError::Encrypted => Status::invalid_argument(
            "the PDF is encrypted; supply its password in `options.password`".to_owned(),
        ),
        PdfError::Io(error) => Status::internal(format!("unexpected I/O error: {error}")),
        // `guarded` turns an interrupt into its own abort before it gets
        // here; this is the code each one would carry.
        PdfError::Interrupted(Interrupt::Deadline) => {
            Status::deadline_exceeded("the parse ran past its time budget")
        }
        PdfError::Interrupted(Interrupt::DecompressionLimit) => {
            Status::resource_exhausted("the document inflates past its decompression limits")
        }
        PdfError::Interrupted(Interrupt::Cancelled) => Status::cancelled("the call was cancelled"),
    }
}

/// Map the library's PDF type onto the wire enum.
fn pdf_type(pdf_type: PdfType) -> pb::PdfType {
    match pdf_type {
        PdfType::TextBased => pb::PdfType::TextBased,
        PdfType::Scanned => pb::PdfType::Scanned,
        PdfType::ImageBased => pb::PdfType::ImageBased,
        PdfType::Mixed => pb::PdfType::Mixed,
    }
}

/// Map one library OCR reason string onto the wire enum.
///
/// The library's reasons are strings so it can grow new ones; an unknown
/// reason maps to `UNSPECIFIED` rather than failing the call, and a client
/// that needs the new value upgrades its schema.
fn ocr_reason(reason: &str) -> pb::OcrReason {
    match reason {
        pdf_inspector::OCR_REASON_NO_TEXT => pb::OcrReason::NoText,
        pdf_inspector::OCR_REASON_SCANNED => pb::OcrReason::Scanned,
        pdf_inspector::OCR_REASON_SUSPECTED_GARBLED_TEXT => pb::OcrReason::SuspectedGarbled,
        pdf_inspector::OCR_REASON_VECTOR_TEXT => pb::OcrReason::VectorText,
        _ => pb::OcrReason::Unspecified,
    }
}

/// Map the library's layout analysis onto the wire message.
fn layout_proto(complexity: &pdf_inspector::LayoutComplexity) -> pb::LayoutComplexity {
    pb::LayoutComplexity {
        is_complex: complexity.is_complex,
        pages_with_tables: complexity.pages_with_tables.clone(),
        pages_with_columns: complexity.pages_with_columns.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One run of a page, with the text a rendering may or may not carry.
    fn run(text: &str) -> pb::TextSpan {
        pb::TextSpan {
            text: text.to_owned(),
            kind: pb::SpanKind::Text.into(),
            ..pb::TextSpan::default()
        }
    }

    /// The text of the runs a rendering left out.
    fn missing(runs: &[&str], markdown: &str) -> Vec<String> {
        absent_from(runs.iter().copied().map(run).collect(), markdown)
            .into_iter()
            .map(|run| run.text)
            .collect()
    }

    #[test]
    fn a_run_the_renderer_only_moved_is_not_missing() {
        // The rendering carries both columns, second one first. Under a
        // forward scan every run of the first column reads as dropped,
        // which is how a paper's body ended up in its furniture.
        let dropped = missing(
            &["left one", "right one", "left two", "right two"],
            "right one right two\n\nleft one left two",
        );
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn a_run_that_is_nowhere_in_the_rendering_is_missing() {
        assert_eq!(
            missing(&["kept prose", "left behind entirely"], "kept prose"),
            ["left behind entirely"]
        );
    }

    #[test]
    fn markdown_punctuation_does_not_make_a_run_missing() {
        assert!(missing(&["A Heading"], "## **A Heading**").is_empty());
    }

    #[test]
    fn a_run_too_short_to_look_for_is_never_reported() {
        // Two letters occur somewhere in any page of prose, so their
        // absence cannot be established either way.
        assert!(missing(&["7", "of"], "a page of prose about nothing").is_empty());
        assert!(missing(&["7", "of"], "").is_empty());
    }

    #[test]
    fn nothing_is_reported_for_a_page_with_no_runs_to_report_on() {
        assert!(missing(&[], "some markdown").is_empty());
    }

    /// One extracted item of the given kind on page 1.
    fn item(text: &str, item_type: pdf_inspector::types::ItemType) -> pdf_inspector::TextItem {
        pdf_inspector::TextItem {
            text: text.to_owned(),
            x: 72.0,
            y: 700.0,
            width: 100.0,
            height: 10.0,
            font: String::new(),
            font_tag: String::new(),
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type,
            mcid: None,
        }
    }

    /// One Form XObject placement on page 1.
    fn form() -> pdf_inspector::PdfForm {
        pdf_inspector::PdfForm {
            name: "Fx1".to_owned(),
            x: 72.0,
            y: 300.0,
            width: 400.0,
            height: 200.0,
            page: 1,
        }
    }

    #[test]
    fn a_page_that_drew_only_a_picture_is_a_scan() {
        use pdf_inspector::types::ItemType;
        let image = item("[Image: Im1]", ItemType::Image);
        assert_eq!(
            untexted_page_reason(std::slice::from_ref(&image), None),
            Some(pb::OcrReason::Scanned)
        );
        // Show operators that showed nothing are not text either.
        assert_eq!(
            untexted_page_reason(&[image, item("  ", ItemType::Text)], None),
            Some(pb::OcrReason::Scanned)
        );
    }

    #[test]
    fn a_page_that_drew_only_a_form_has_no_text_to_read() {
        assert_eq!(
            untexted_page_reason(&[], Some(&vec![form()])),
            Some(pb::OcrReason::NoText)
        );
    }

    #[test]
    fn a_page_with_any_text_or_with_nothing_at_all_is_not_flagged_here() {
        use pdf_inspector::types::ItemType;
        let image = item("[Image: Im1]", ItemType::Image);
        assert_eq!(
            untexted_page_reason(&[image.clone(), item("Figure 1", ItemType::Text)], None),
            None
        );
        assert_eq!(
            untexted_page_reason(&[image, item("Name: Ada", ItemType::FormField)], None),
            None
        );
        // A link annotation's target is not words on the page.
        assert_eq!(
            untexted_page_reason(
                &[item(
                    "https://example.org",
                    ItemType::Link("https://example.org".to_owned())
                )],
                None
            ),
            None,
            "a blank page with a link stays a blank page"
        );
        assert_eq!(untexted_page_reason(&[], None), None, "a blank page");
    }
}
