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
//! # Two library passes, on purpose
//!
//! Detection (`process_pdf_mem_with_options` in detect-only mode) and
//! per-page extraction (`extract_pages_markdown_mem`) are separate calls,
//! so the document is parsed twice. That is the price of emitting `info`
//! before extraction starts, and it is cheap: detection reads content
//! streams, not glyphs.
//!
//! # Page indexing
//!
//! The wire is 1-indexed everywhere, as PDF viewers are. The library is
//! not: `pages_needing_ocr` and `PdfOptions::pages` are 1-indexed but
//! `extract_pages_markdown_mem` takes and returns 0-indexed pages. All
//! conversion happens here, at the library boundary, and nowhere else.

use std::time::{Duration, Instant};

use pdf_inspector::{PdfOptions, PdfType, ProcessMode};
use tokio::sync::mpsc;
use tonic::Status;

use crate::metrics::Metrics;
use crate::proto::v1 as pb;

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

    sink.send(pb::parse_pdf_response::Event::Info(pb::PdfInfo {
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
    let mut pages_extracted = 0u32;
    let mut layout = None;
    let mut has_encoding_issues = false;

    // What happens after `info` depends on the mode and on what detection
    // found. Scanned and image-based documents have no text layer at all, so
    // there is nothing to analyze or extract at any mode.
    let text_bearing = matches!(detected.pdf_type, PdfType::TextBased | PdfType::Mixed);
    match mode {
        pb::ProcessMode::DetectOnly | pb::ProcessMode::Unspecified => {}
        _ if !text_bearing => {}
        pb::ProcessMode::Analyze => {
            let mut analyze = PdfOptions::new().mode(ProcessMode::Analyze);
            // An empty filter means "all pages" on the wire but "no pages"
            // to the library, so it is only set when the caller named pages.
            if !options.pages.is_empty() {
                analyze = analyze.pages(options.pages.iter().copied());
            }
            if !options.password.is_empty() {
                analyze = analyze.password(options.password.clone());
            }
            let analyzed = guarded(|| pdf_inspector::process_pdf_mem_with_options(bytes, analyze))?;
            layout = Some(layout_proto(&analyzed.layout));
            has_encoding_issues = analyzed.has_encoding_issues;
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
                sink.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no: 0,
                    markdown,
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
        }
        pb::ProcessMode::Full => {
            // 1-indexed wire pages to the extractor's 0-indexed selection.
            let selected: Vec<u32> = options.pages.iter().map(|page| page - 1).collect();
            let selection = (!selected.is_empty()).then_some(selected);
            let extracted =
                guarded(|| pdf_inspector::extract_pages_markdown_mem(bytes, selection.as_deref()))?;
            for page in extracted.pages {
                let markdown_bytes = page.markdown.len() as u64;
                // 0-indexed library page back to the 1-indexed wire page.
                sink.send(pb::parse_pdf_response::Event::Page(pb::PageMarkdown {
                    page_no: page.page + 1,
                    markdown: page.markdown,
                }))?;
                // Counted after the send: "emitted" means on the wire, and a
                // counter that runs ahead of a blocked send is how a batch
                // hides from the streaming tests.
                metrics.page_emitted(markdown_bytes);
                pages_extracted += 1;
            }
            layout = Some(pb::LayoutComplexity {
                is_complex: extracted.is_complex,
                pages_with_tables: extracted.pages_with_tables,
                pages_with_columns: extracted.pages_with_columns,
            });
            has_encoding_issues = extracted.ocr_reasons_by_page.iter().any(|page| {
                page.reasons
                    .iter()
                    .any(|reason| reason == pdf_inspector::OCR_REASON_SUSPECTED_GARBLED_TEXT)
            });
        }
    }

    sink.send(pb::parse_pdf_response::Event::Status(pb::ParseStatus {
        pages_extracted,
        warnings,
        layout,
        has_encoding_issues,
        processing_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }))?;
    Ok(())
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
