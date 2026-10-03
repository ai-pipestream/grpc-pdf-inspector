// SPDX-License-Identifier: Apache-2.0

//! The tonic service: upload handling, concurrency bound, and the supervisor
//! that turns a panicking parse into an `INTERNAL` status instead of a stream
//! that just stops.
//!
//! The parse itself runs on [`tokio::task::spawn_blocking`], because
//! extraction is CPU-bound (and parallelizes with rayon internally) and would
//! otherwise occupy async workers for the length of a document.
//! [`crate::parse::Sink`] carries backpressure across that boundary: the
//! outbound channel is bounded, and the blocking thread waits on it, so a
//! slow reader slows the parser rather than growing a queue behind it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::limits::Limits;
use crate::metrics::Metrics;
use crate::parse::{self, Outcome, Sink};
use crate::proto::v1 as pb;

/// Events buffered on the outbound channel before the parser has to wait.
///
/// Small on purpose. The channel is a smoothing buffer, not a place to store
/// the document: a page of markdown can be kilobytes to megabytes, so a deep
/// queue would quietly reintroduce the whole-document buffering this service
/// exists to avoid.
const OUTBOUND_BUFFER: usize = 4;

/// How long the parser waits on a full outbound channel before giving up.
///
/// A client that has read nothing for this long has abandoned the call, and
/// waiting without a bound would pin a blocking-pool thread until the process
/// restarted.
const CONSUMER_STALL: Duration = Duration::from_secs(30);

/// How long an admitted call may go without sending any upload bytes.
///
/// The call holds a parse slot while it uploads, so a client that opens a
/// stream and then sends nothing would hold the slot for its whole upload
/// budget. One that has sent nothing for this long has abandoned the call.
/// Only bytes count: a frame with an empty chunk, or with no frame at all,
/// is not progress.
const UPLOAD_STALL: Duration = Duration::from_secs(30);

/// The `ai.pipestream.pdf.v1.PdfParseService` implementation.
pub struct PdfGrpc {
    /// The ceilings this server enforces.
    limits: Limits,
    /// Process counters.
    metrics: Arc<Metrics>,
    /// Bounds how many calls may parse at once, capping CPU and heap rather
    /// than shedding load: a call past the bound waits for a permit.
    parse_slots: Arc<tokio::sync::Semaphore>,
    /// How long an upload may go without a byte; [`UPLOAD_STALL`] unless a
    /// test shortened it.
    upload_stall: Duration,
}

impl PdfGrpc {
    /// Build a service with the given limits and a fresh set of counters.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self::with_metrics(limits, Metrics::new())
    }

    /// Build a service sharing an existing set of counters.
    ///
    /// Tests use this to watch a parse from outside: the counters are the only
    /// honest way to ask "how far has the server actually got", which is what
    /// distinguishes a live stream from a batch that was buffered and then
    /// handed over all at once.
    #[must_use]
    pub fn with_metrics(limits: Limits, metrics: Arc<Metrics>) -> Self {
        Self {
            limits,
            metrics,
            parse_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_parses)),
            upload_stall: UPLOAD_STALL,
        }
    }

    /// Give up on an upload after `stall` without a byte, instead of
    /// [`UPLOAD_STALL`].
    ///
    /// Not a deployment setting: it exists so a test can watch a stall end
    /// without waiting half a minute for it.
    #[must_use]
    pub fn with_upload_stall(mut self, stall: Duration) -> Self {
        self.upload_stall = stall;
        self
    }

    /// The counters this service reports into.
    #[must_use]
    pub fn metrics(&self) -> Arc<Metrics> {
        Arc::clone(&self.metrics)
    }

    /// Wrap this service in its generated tonic server.
    ///
    /// tonic's decoding limit is set to twice the chunk cap, deliberately
    /// above it rather than equal to it. The two mean different things: the
    /// cap is advice, refused with an `INVALID_ARGUMENT` naming the number and
    /// saying to split the upload, while tonic's is a hard backstop against a
    /// hostile length prefix. Equal limits would make the backstop fire first
    /// for every ordinary overshoot, and the caller would get `OutOfRange` and
    /// a sentence about decoded message lengths instead of the one telling
    /// them what to do.
    #[must_use]
    pub fn into_service(self) -> pb::pdf_parse_service_server::PdfParseServiceServer<Self> {
        let backstop =
            usize::try_from(self.limits.max_chunk_bytes.saturating_mul(2)).unwrap_or(usize::MAX);
        pb::pdf_parse_service_server::PdfParseServiceServer::new(self)
            .max_decoding_message_size(backstop)
    }
}

#[tonic::async_trait]
impl pb::pdf_parse_service_server::PdfParseService for PdfGrpc {
    type ParsePdfStream = ReceiverStream<Result<pb::ParsePdfResponse, Status>>;

    async fn parse_pdf(
        &self,
        request: Request<Streaming<pb::ParsePdfRequest>>,
    ) -> Result<Response<Self::ParsePdfStream>, Status> {
        let mut inbound = request.into_inner();

        // Options first, so every way the request can be rejected outright is
        // resolved before the response stream opens. Once it is open, only the
        // parse can end it badly.
        let options = match inbound.message().await? {
            Some(pb::ParsePdfRequest {
                frame: Some(pb::parse_pdf_request::Frame::Options(options)),
            }) => options,
            Some(_) => {
                return Err(Status::invalid_argument(
                    "the first frame must carry `options`",
                ));
            }
            None => {
                return Err(Status::invalid_argument(
                    "the request stream closed before sending `options`",
                ));
            }
        };

        // Admission comes before the upload is buffered, so the memory
        // ceiling (max_concurrent_parses uploads of max_document_bytes each)
        // counts every buffered upload rather than only the ones being
        // parsed. A call waiting for a slot holds nothing but what the
        // transport's flow-control window lets its client send ahead.
        let permit = Arc::clone(&self.parse_slots)
            .acquire_owned()
            .await
            .map_err(|_| Status::unavailable("the server is shutting down"))?;
        // The call's time budget starts with its slot, and the upload
        // spends it too: the slot is what the budget protects. The upload
        // has a shorter budget of its own inside it, so a client trickling
        // bytes cannot keep the slot for the whole parse budget.
        let admitted = Instant::now();
        let deadline = admitted + self.limits.max_parse_time;
        let upload_deadline = deadline.min(admitted + self.limits.max_upload_time);

        let bytes = self
            .receive(&mut inbound, deadline, upload_deadline)
            .await?;

        // The call's limits: how far the document may inflate, how long the
        // call may hold its slot, and a flag the supervisor below sets when
        // nobody is listening any more. The parser checks the last two
        // between pages, so a pathological document cannot hold a slot past
        // its budget or past its caller.
        let cancel = Arc::new(AtomicBool::new(false));
        let guard = self
            .limits
            .parse_guard()
            .with_deadline(deadline)
            .with_cancel(Arc::clone(&cancel));

        self.metrics.parse_started();
        let metrics = Arc::clone(&self.metrics);
        let (tx, rx) = mpsc::channel(OUTBOUND_BUFFER);
        let supervisor = tx.clone();

        let mut handle = tokio::task::spawn_blocking(move || {
            let sink = Sink::new(tx, CONSUMER_STALL);
            parse::run_guarded(&bytes, &options, &metrics, &sink, &guard)
        });

        let metrics = Arc::clone(&self.metrics);
        tokio::spawn(async move {
            // A caller that hangs up mid-parse is heard here, the moment the
            // response stream is dropped, rather than at the parser's next
            // send: a document's extraction pass can run a long time between
            // two events, and the slot it holds is somebody else's.
            let joined = tokio::select! {
                joined = &mut handle => joined,
                () = supervisor.closed() => {
                    cancel.store(true, Ordering::Relaxed);
                    handle.await
                }
            };
            // A panic drops the parser's sender, and without this the stream
            // would end *successfully* with whatever had been delivered — a
            // truncated document indistinguishable from a short one. The
            // supervisor's own sender is what makes the difference reportable.
            let status = match joined {
                Ok(Outcome::Complete) => {
                    metrics.parse_succeeded();
                    None
                }
                Ok(Outcome::Abandoned) => {
                    metrics.parse_failed();
                    None
                }
                Ok(Outcome::Failed(status)) => {
                    metrics.parse_failed();
                    Some(*status)
                }
                Err(join) => {
                    metrics.parse_failed();
                    Some(Status::internal(panic_detail(join)))
                }
            };
            if let Some(status) = status {
                let _ = supervisor.send(Err(status)).await;
            }
            drop(permit);
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn get_service_info(
        &self,
        _request: Request<pb::GetServiceInfoRequest>,
    ) -> Result<Response<pb::GetServiceInfoResponse>, Status> {
        Ok(Response::new(pb::GetServiceInfoResponse {
            name: "grpc-pdf-inspector".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            limits: Some(self.limits.to_proto()),
            features: vec![
                "diskless".to_owned(),
                "classification".to_owned(),
                "page-stream".to_owned(),
                "markdown".to_owned(),
                "document-projection".to_owned(),
                "health".to_owned(),
                "reflection".to_owned(),
            ],
            ui: Some(pb::UiInfo {
                title: "PDF Inspector".to_owned(),
                path: "/ui/pdf".to_owned(),
                description: "Classifies PDFs in memory and streams per-page markdown".to_owned(),
            }),
        }))
    }
}

impl PdfGrpc {
    /// Drain the request stream into one buffer, enforcing the upload cap.
    ///
    /// The buffer is unavoidable: a PDF's cross-reference table lives at the
    /// end of the file, so no page is readable until the last byte has
    /// arrived. The cap is checked as bytes land rather than at the end, so a
    /// hostile upload is cut off at the limit instead of after it.
    ///
    /// The call already holds its parse slot, so the upload is bounded in
    /// time as well: it ends at `upload_deadline`, which is never later than
    /// the call's `deadline`, and after the stall window without a byte.
    async fn receive(
        &self,
        inbound: &mut Streaming<pb::ParsePdfRequest>,
        deadline: Instant,
        upload_deadline: Instant,
    ) -> Result<Vec<u8>, Status> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut progressed = Instant::now();
        loop {
            let wait_until = upload_deadline.min(progressed + self.upload_stall);
            let next = tokio::time::timeout_at(wait_until.into(), inbound.message()).await;
            let Ok(next) = next else {
                let now = Instant::now();
                return Err(Status::deadline_exceeded(if now >= deadline {
                    "the call ran past its time budget while its upload was still arriving; \
                     raise GRPC_PDF_MAX_PARSE_SECONDS if uploads genuinely take this long"
                        .to_owned()
                } else if now >= upload_deadline {
                    format!(
                        "the upload was still arriving after its {} second budget; raise \
                         GRPC_PDF_MAX_UPLOAD_SECONDS if uploads genuinely take this long",
                        self.limits.max_upload_time.as_secs()
                    )
                } else {
                    format!(
                        "no upload bytes arrived for {:?}, so the call was abandoned",
                        self.upload_stall
                    )
                }));
            };
            let Some(request) = next? else {
                break;
            };
            let chunk = match request.frame {
                Some(pb::parse_pdf_request::Frame::Chunk(chunk)) => chunk,
                Some(pb::parse_pdf_request::Frame::Options(_)) => {
                    return Err(Status::invalid_argument(
                        "`options` may only be sent once, as the first frame",
                    ));
                }
                // An empty frame carries nothing and means nothing; skip it
                // rather than treat it as the end of the upload.
                None => continue,
            };
            // Nor does an empty chunk, and neither counts as progress: only
            // bytes put off the stall.
            if chunk.is_empty() {
                continue;
            }
            if chunk.len() as u64 > self.limits.max_chunk_bytes {
                return Err(Status::invalid_argument(format!(
                    "chunk of {} bytes exceeds the {} byte frame limit; split the upload into \
                     more, smaller chunks",
                    chunk.len(),
                    self.limits.max_chunk_bytes
                )));
            }
            if bytes.len() as u64 + chunk.len() as u64 > self.limits.max_document_bytes {
                self.metrics.uploaded(bytes.len() as u64);
                return Err(Status::resource_exhausted(format!(
                    "the upload passed its {} byte limit; raise GRPC_PDF_MAX_BYTES if the \
                     document is genuinely this large",
                    self.limits.max_document_bytes
                )));
            }
            bytes.extend_from_slice(&chunk);
            progressed = Instant::now();
        }
        self.metrics.uploaded(bytes.len() as u64);
        if bytes.is_empty() {
            return Err(Status::invalid_argument("the upload was empty"));
        }
        Ok(bytes)
    }
}

/// Render a `JoinError` into something worth putting on the wire.
fn panic_detail(join: tokio::task::JoinError) -> String {
    if !join.is_panic() {
        return "the parse task was cancelled".to_owned();
    }
    let payload = join.into_panic();
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned());
    format!("the parser panicked, which is a bug in grpc-pdf-inspector: {detail}")
}
