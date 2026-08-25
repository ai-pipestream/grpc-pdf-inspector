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
//! before a single page has been extracted. In FULL mode each `page` goes
//! out as that page's markdown comes back. `tests/streaming.rs` holds the
//! test that fails if someone turns this back into a batch.
//!
//! # Three library passes in FULL, on purpose
//!
//! Detection (`process_pdf_mem_with_options` in detect-only mode) runs
//! first and alone, because `info` is the product's front door and it has
//! to go out before anything expensive starts. It is cheap: it reads
//! content streams, not glyphs.
//!
//! FULL then runs two more. The analysis pass
//! (`process_pdf_mem_with_options` in analyze mode) is what computes layout
//! complexity and the per-page text-quality verdicts, and the library
//! exposes no way to compute either from items a caller already holds — the
//! column detector and the quality module are both crate-private. The
//! extraction pass is `extract_text_with_positions_mem_pages`, whose items
//! this module renders to markdown itself with
//! [`to_markdown_from_items_with_rects_and_page_count`].
//!
//! That last choice is the one worth defending, because the obvious call is
//! `extract_pages_markdown_mem` and this used to make it. It takes no
//! options — it hardcodes `MarkdownOptions::default()` — and it throws the
//! items away, and the items are where every coordinate, every font, every
//! marked-content id and the entire link layer live. Rendering from items
//! keeps all of that in hand and costs the analysis pass to keep the layout
//! answer honest. The trade is deliberate: the wire gained geometry and
//! links, and one more read of the file is what they cost.
//!
//! [`to_markdown_from_items_with_rects_and_page_count`]: pdf_inspector::to_markdown_from_items_with_rects_and_page_count
//!
//! # Page indexing
//!
//! The wire is 1-indexed everywhere, as PDF viewers are, and so are the
//! positioned items (`TextItem::page`) and `PdfOptions::pages`. Any
//! conversion happens here, at the library boundary, and nowhere else.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use pdf_inspector::{MarkdownOptions, PdfOptions, PdfType, ProcessMode};
use tokio::sync::mpsc;
use tonic::Status;

use crate::document_fold::DocumentFold;
use crate::metrics::Metrics;
use crate::proto::v1 as pb;
use crate::spans;

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

/// Classify `bytes` and stream the events the mode calls for into `sink`.
///
/// Synchronous on purpose: extraction is CPU-bound and parallelizes with
/// rayon internally, so the caller runs this on
/// [`tokio::task::spawn_blocking`] and [`Sink`] bridges back to the async
/// channel. Everything stays in memory; nothing is written anywhere.
#[must_use]
pub fn run(bytes: &[u8], options: &pb::PdfOptions, metrics: &Metrics, sink: &Sink) -> Outcome {
    match parse(bytes, options, metrics, sink) {
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
) -> Result<(), Abort> {
    let started = Instant::now();
    let mode = pb::ProcessMode::try_from(options.mode)
        .map_err(|_| Status::invalid_argument(format!("unknown process mode {}", options.mode)))?;
    let mode = match mode {
        pb::ProcessMode::Unspecified => pb::ProcessMode::Full,
        known => known,
    };

    // Page 0 cannot be rejected by the library — its 1-indexed filter would
    // treat it as out of range and its 0-indexed extractor would treat it as
    // the first page — so it is rejected here, where it is still a caller
    // mistake rather than a silent wrong answer.
    if options.pages.contains(&0) {
        return Err(Status::invalid_argument("pages are 1-indexed; page 0 does not exist").into());
    }

    // Pass one: classification. Cheap, and the reason this stream opens with
    // an answer instead of with work.
    let mut detect = PdfOptions::detect_only();
    if !options.password.is_empty() {
        detect = detect.password(options.password.clone());
    }
    let detected = guarded(|| pdf_inspector::process_pdf_mem_with_options(bytes, detect))?;

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
    }))?;

    let mut warnings = Vec::new();

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
        let metadata = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::metadata::read(bytes, password)
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

    // What happens after `info` depends on the mode and on what detection
    // found. Scanned and image-based documents have no text layer at all, so
    // there is nothing to analyze or extract at any mode.
    let text_bearing = matches!(detected.pdf_type, PdfType::TextBased | PdfType::Mixed);
    match mode {
        pb::ProcessMode::DetectOnly | pb::ProcessMode::Unspecified => {}
        _ if !text_bearing => {}
        pb::ProcessMode::Analyze => {
            let analyzed = guarded(|| {
                pdf_inspector::process_pdf_mem_with_options(bytes, analyze_options(options))
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
            if !options.pages.is_empty() {
                full = full.pages(options.pages.iter().copied());
            }
            full = full.password(options.password.clone());
            let processed = guarded(|| pdf_inspector::process_pdf_mem_with_options(bytes, full))?;
            if let Some(markdown) = processed.markdown.filter(|md| !md.is_empty()) {
                let markdown_bytes = markdown.len() as u64;
                events.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no: 0,
                    markdown,
                    // Whole-document extraction reports its OCR verdicts by
                    // page, and this event is not a page; the trailer's
                    // `extraction_ocr_reasons` carries them instead.
                    needs_ocr: false,
                    ocr_reason: pb::OcrReason::Unspecified.into(),
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
            // The analysis pass. Layout complexity and the per-page
            // text-quality verdicts are computed inside the library from
            // machinery it does not expose (the column detector and the
            // text-quality module are both crate-private), so they cannot be
            // recovered from the items the extraction pass returns. Asking
            // for them costs a read of the file and is the price of the
            // markdown pass below giving up the answers it used to come
            // with.
            let analyzed = guarded(|| {
                pdf_inspector::process_pdf_mem_with_options(bytes, analyze_options(options))
            })?;
            layout = Some(layout_proto(&analyzed.layout));
            has_encoding_issues = analyzed.has_encoding_issues;
            extraction_ocr_reasons = ocr_reasons_proto(&analyzed.ocr_reasons_by_page);
            let verdicts = page_verdicts(&analyzed);

            // The extraction pass. Positioned items, not markdown: the
            // markdown is rendered from them here so that the items — the
            // boxes, the fonts, the marked-content ids, the link
            // annotations — stay in hand instead of being rendered away
            // inside the library.
            let filter: Option<HashSet<u32>> =
                (!options.pages.is_empty()).then(|| options.pages.iter().copied().collect());
            let items = guarded(|| {
                pdf_inspector::extractor::extract_text_with_positions_mem_pages(
                    bytes,
                    filter.as_ref(),
                )
            })?;
            let mut by_page = spans::by_page(items);

            for page_no in requested_pages(options, detected.page_count) {
                let page_items = by_page.remove(&page_no).unwrap_or_default();

                // The runs go out before the rendering they produced, so a
                // consumer reading both never has to buffer one to
                // interpret the other.
                if events.wanted(options.emit_spans) {
                    let spans = spans::page_spans(page_no, &page_items);
                    events.route(
                        pb::parse_pdf_response::Event::Spans(spans),
                        options.emit_spans,
                    )?;
                }

                let markdown = pdf_inspector::to_markdown_from_items_with_rects_and_page_count(
                    page_items,
                    MarkdownOptions::default(),
                    &[],
                    detected.page_count,
                );
                let markdown = markdown.trim().to_owned();
                let markdown_bytes = markdown.len() as u64;
                let (needs_ocr, reason) = verdicts
                    .get(&page_no)
                    .copied()
                    .unwrap_or((false, pb::OcrReason::Unspecified));
                events.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no,
                    markdown,
                    needs_ocr,
                    ocr_reason: reason.into(),
                }))?;
                // Counted after the send: "emitted" means on the wire, and a
                // counter that runs ahead of a blocked send is how a batch
                // hides from the streaming tests.
                metrics.page_emitted(markdown_bytes);
                pages_extracted += 1;
            }
        }
    }

    // The fold has seen every content event now, so its Document goes out
    // here — after the last `page`, before the `status` trailer that closes
    // the stream.
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
    }))?;
    Ok(())
}

/// The 1-indexed pages a call asks for, in the order it asked for them.
///
/// An empty filter means every page. A page past the end of the document is
/// dropped rather than answered with an empty event: the caller asked about
/// something that does not exist, and inventing a page for it would be a
/// worse answer than saying nothing.
fn requested_pages(options: &pb::PdfOptions, page_count: u32) -> Vec<u32> {
    if options.pages.is_empty() {
        (1..=page_count).collect()
    } else {
        options
            .pages
            .iter()
            .copied()
            .filter(|page| *page <= page_count)
            .collect()
    }
}

/// The library options for the analysis pass, honouring the call's page
/// filter and password.
fn analyze_options(options: &pb::PdfOptions) -> PdfOptions {
    let mut analyze = PdfOptions::new().mode(ProcessMode::Analyze);
    // An empty filter means "all pages" on the wire but "no pages" to the
    // library, so it is only set when the caller named pages.
    if !options.pages.is_empty() {
        analyze = analyze.pages(options.pages.iter().copied());
    }
    if !options.password.is_empty() {
        analyze = analyze.password(options.password.clone());
    }
    analyze
}

/// Per-page OCR verdicts from an analysis result, keyed by 1-indexed page.
///
/// A page can carry several reasons; the first is the one a per-page event
/// can hold, and the whole list still travels on the trailer.
fn page_verdicts(
    analyzed: &pdf_inspector::PdfProcessResult,
) -> HashMap<u32, (bool, pb::OcrReason)> {
    // A page the analysis gave a reason for is a page it judged unusable,
    // whether or not it also made the OCR list, so both sources set the
    // verdict and only the reason distinguishes them.
    let mut verdicts: HashMap<u32, (bool, pb::OcrReason)> = analyzed
        .ocr_reasons_by_page
        .iter()
        .filter_map(|page| {
            page.reasons
                .first()
                .map(|reason| (page.page, (true, ocr_reason(reason))))
        })
        .collect();
    for page in &analyzed.pages_needing_ocr {
        verdicts
            .entry(*page)
            .or_insert((true, pb::OcrReason::Unspecified));
    }
    verdicts
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

/// Call a fallible parser entry point with a panic guard.
///
/// lopdf can panic on malformed input, and an unwinding panic on the blocking
/// thread would surface as a truncated stream; here it becomes an honest
/// `INTERNAL` instead. `PdfError` maps by variant per the fleet contract.
fn guarded<T>(call: impl FnOnce() -> Result<T, pdf_inspector::PdfError>) -> Result<T, Status> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(map_error(error)),
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(Status::internal(format!(
                "the parser panicked, which is a bug in grpc-pdf-inspector: {detail}"
            )))
        }
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
