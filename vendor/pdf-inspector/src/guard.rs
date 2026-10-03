//! Resource limits for one parse: how far its streams may inflate, and when
//! it has to stop.
//!
//! The crate reads files other people made. A Flate stream inflates about a
//! thousand to one, so a five-megabyte upload can hold content, font or
//! Form XObject streams that decode to tens of gigabytes, and a page tree
//! that is merely large can keep a reader busy long after its caller has
//! given up. A caller that wants bounds installs a [`ParseGuard`] around a
//! call with [`ParseGuard::run`]. Every stream the crate decodes on that
//! thread is then decoded against the guard's per-stream ceiling and the
//! remainder of its per-run budget, every document it loads is loaded with
//! the ceiling as lopdf's own `max_decompressed_size`, and every page loop
//! checks the guard's deadline and cancellation flag between pages.
//!
//! The guard is ambient rather than a parameter on purpose. The decoders
//! sit at the bottom of call chains that run from every public entry point
//! through font, CMap and Form XObject code, and threading a budget through
//! all of them would change dozens of signatures for a value only the
//! outermost caller sets. The crate does its work on the calling thread
//! (lopdf's parallel object parsing at load time decodes nothing of the
//! crate's and is bounded by its own load option), so a guard installed for
//! the duration of one call is seen by everything that call does and by
//! nothing else.
//!
//! Without a guard installed, streams still decode against
//! [`DEFAULT_MAX_STREAM_BYTES`], so no entry point inflates without bound.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use lopdf::{DecompressError, Document, Encoding, Object, ObjectId, Stream};

use crate::PdfError;

/// The ceiling on one stream's decoded size when no guard says otherwise:
/// far above what a real file's content, font or CMap streams need, and
/// far below what a decompression bomb wants.
pub const DEFAULT_MAX_STREAM_BYTES: usize = 256 * 1024 * 1024;

/// Why a guarded run stopped before it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// A stream would have decoded past the per-stream ceiling, or the run
    /// past its decompression budget.
    DecompressionLimit,
    /// The deadline passed.
    Deadline,
    /// The caller cancelled.
    Cancelled,
}

impl std::fmt::Display for Interrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Interrupt::DecompressionLimit => "a stream exceeded the decompression limit",
            Interrupt::Deadline => "the deadline passed",
            Interrupt::Cancelled => "the caller cancelled",
        })
    }
}

/// The limits one call runs under.
#[derive(Debug, Clone)]
pub struct ParseGuard {
    /// The most any one stream may decode to. A page's content streams are
    /// one stream split in parts, so they share this ceiling.
    pub max_stream_bytes: usize,
    /// The most all the streams one [`run`](Self::run) decodes may add up
    /// to, which is what one read of a document may inflate in total.
    pub max_run_bytes: u64,
    /// When the work has to stop, if ever.
    pub deadline: Option<Instant>,
    /// Set by the caller when nobody wants the result any more.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl ParseGuard {
    /// A guard with these decompression bounds, no deadline and no
    /// cancellation flag.
    pub fn new(max_stream_bytes: usize, max_run_bytes: u64) -> Self {
        Self {
            max_stream_bytes,
            max_run_bytes,
            deadline: None,
            cancel: None,
        }
    }

    /// Stop at `deadline`.
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Stop when `cancel` is set.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Whether the caller has cancelled or the deadline has passed. The
    /// decompression budget belongs to a run and is not consulted here.
    pub fn interrupted(&self) -> Option<Interrupt> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            Some(Interrupt::Cancelled)
        } else if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Some(Interrupt::Deadline)
        } else {
            None
        }
    }

    /// Run `work` on this thread under this guard, with a fresh
    /// decompression budget, and say what stopped it, if anything did.
    ///
    /// The interrupt is the first one the work ran into, at a stream it was
    /// refused or at a page boundary. When there is one, whatever `work`
    /// returned is incomplete, whether it returned an error or not: a
    /// refused stream reads as an undecodable one to code that has no way
    /// to report it.
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> (T, Option<Interrupt>) {
        let previous = ACTIVE.with(|active| {
            active.replace(Some(Active {
                guard: self.clone(),
                decoded: 0,
                interrupt: None,
            }))
        });
        let restore = Restore(Some(previous));
        let value = work();
        let interrupt = ACTIVE.with(|active| {
            active
                .borrow()
                .as_ref()
                .and_then(|active| active.interrupt)
        });
        drop(restore);
        (value, interrupt)
    }
}

/// The guard in force on this thread, and what its run has used.
struct Active {
    guard: ParseGuard,
    /// Bytes decoded so far in this run.
    decoded: u64,
    /// The first interrupt the run ran into.
    interrupt: Option<Interrupt>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Puts the previous guard back when a run ends, panic or not.
struct Restore(Option<Option<Active>>);

impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            ACTIVE.with(|active| *active.borrow_mut() = previous);
        }
    }
}

/// The most the next stream may decode to: the per-stream ceiling, capped
/// by what is left of the run's budget and by `cap`. Zero once the run has
/// been interrupted, so nothing more is decoded.
fn allowance(cap: usize) -> usize {
    ACTIVE.with(|active| match active.borrow().as_ref() {
        None => DEFAULT_MAX_STREAM_BYTES.min(cap),
        Some(active) if active.interrupt.is_some() => 0,
        Some(active) => {
            let left = active.guard.max_run_bytes.saturating_sub(active.decoded);
            active
                .guard
                .max_stream_bytes
                .min(usize::try_from(left).unwrap_or(usize::MAX))
                .min(cap)
        }
    })
}

/// The per-stream ceiling in force on this thread.
pub(crate) fn max_stream_bytes() -> usize {
    ACTIVE.with(|active| {
        active
            .borrow()
            .as_ref()
            .map_or(DEFAULT_MAX_STREAM_BYTES, |active| active.guard.max_stream_bytes)
    })
}

/// Record an interrupt, keeping the first.
fn interrupt(why: Interrupt) {
    ACTIVE.with(|active| {
        if let Some(active) = active.borrow_mut().as_mut() {
            active.interrupt.get_or_insert(why);
        }
    });
}

/// Count `bytes` decoded against the run's budget.
fn charge(bytes: usize) {
    ACTIVE.with(|active| {
        if let Some(active) = active.borrow_mut().as_mut() {
            active.decoded = active.decoded.saturating_add(bytes as u64);
        }
    });
}

fn is_limit_error(error: &lopdf::Error) -> bool {
    matches!(
        error,
        lopdf::Error::Decompress(DecompressError::MemoryLimitExceeded { .. })
    )
}

/// Decode `stream` within `cap` bytes and the guard's allowance.
fn inflate_within(stream: &Stream, cap: usize) -> lopdf::Result<Vec<u8>> {
    match stream.decompressed_content_with_limit(allowance(cap)) {
        Ok(data) => {
            charge(data.len());
            Ok(data)
        }
        Err(error) => {
            if is_limit_error(&error) {
                interrupt(Interrupt::DecompressionLimit);
            }
            Err(error)
        }
    }
}

/// Decode a stream's content within the guard's limits.
///
/// The bounded counterpart of [`Stream::decompressed_content`]. A stream
/// that would decode past the allowance is refused with lopdf's
/// `MemoryLimitExceeded` and interrupts the run.
pub(crate) fn inflate(stream: &Stream) -> lopdf::Result<Vec<u8>> {
    inflate_within(stream, usize::MAX)
}

/// [`inflate`], falling back to the raw bytes for a stream that does not
/// decode for any other reason, which is what every caller that tolerates
/// a bad filter did before. A refused stream yields nothing: its raw bytes
/// are a compressed bomb, not content.
pub(crate) fn inflate_or_raw(stream: &Stream) -> Vec<u8> {
    match inflate(stream) {
        Ok(data) => data,
        Err(error) if is_limit_error(&error) => Vec::new(),
        Err(_) => stream.content.clone(),
    }
}

/// A page's content streams, decoded and joined, within the guard's
/// limits.
///
/// The bounded counterpart of [`Document::get_page_content`]: the streams
/// share one per-stream ceiling, since they are one content stream split in
/// parts, and a stream that does not decode for another reason contributes
/// its raw bytes, as it did there. A refused page yields nothing.
pub(crate) fn page_content(doc: &Document, page_id: ObjectId) -> Vec<u8> {
    let ceiling = max_stream_bytes();
    let mut content = Vec::new();
    for object_id in doc.get_page_contents(page_id) {
        let Ok(stream) = doc.get_object(object_id).and_then(Object::as_stream) else {
            continue;
        };
        let left = ceiling.saturating_sub(content.len());
        match inflate_within(stream, left) {
            Ok(data) => content.extend_from_slice(&data),
            Err(error) if is_limit_error(&error) => return Vec::new(),
            Err(_) => {
                if stream.content.len() > left {
                    interrupt(Interrupt::DecompressionLimit);
                    return Vec::new();
                }
                content.extend_from_slice(&stream.content);
            }
        }
        content.push(b'\n');
    }
    content
}

/// A font dictionary's encoding, decoding any CMap stream it names within
/// the guard's per-stream ceiling.
///
/// The bounded counterpart of `Dictionary::get_font_encoding`. lopdf does
/// not report how much it decoded, so the stream is held to the ceiling but
/// not charged to the run's budget.
pub(crate) fn font_encoding<'a>(
    font: &'a lopdf::Dictionary,
    doc: &'a Document,
) -> lopdf::Result<Encoding<'a>> {
    let result = font.get_font_encoding_with_limit(doc, allowance(usize::MAX));
    if let Err(error) = &result {
        if is_limit_error(error) {
            interrupt(Interrupt::DecompressionLimit);
        }
    }
    result
}

/// lopdf load options carrying the per-stream ceiling as
/// `max_decompressed_size`, which bounds the object and cross-reference
/// streams lopdf decodes while it loads.
pub(crate) fn load_options(password: Option<&str>) -> lopdf::LoadOptions {
    lopdf::LoadOptions {
        password: password.map(str::to_owned),
        max_decompressed_size: Some(max_stream_bytes()),
        ..lopdf::LoadOptions::default()
    }
}

/// Whether the run in force on this thread has been interrupted, checking
/// the deadline and the cancellation flag now.
pub(crate) fn stopped() -> bool {
    current().is_some()
}

/// `Err` once the run in force on this thread has been interrupted: the
/// page-boundary check every page loop makes.
pub(crate) fn checkpoint() -> Result<(), PdfError> {
    match current() {
        Some(why) => Err(PdfError::Interrupted(why)),
        None => Ok(()),
    }
}

/// The run's interrupt, recording the guard's own if there is a new one.
fn current() -> Option<Interrupt> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let active = active.as_mut()?;
        if active.interrupt.is_none() {
            active.interrupt = active.guard.interrupted();
        }
        active.interrupt
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// A Flate stream of `size` zero bytes.
    fn zeros(size: usize) -> Stream {
        let mut stream = Stream::new(dictionary! {}, vec![0; size]);
        stream.compress().expect("compress");
        stream
    }

    #[test]
    fn without_a_guard_streams_still_have_a_ceiling() {
        assert_eq!(allowance(usize::MAX), DEFAULT_MAX_STREAM_BYTES);
        assert_eq!(inflate(&zeros(1000)).expect("small").len(), 1000);
    }

    #[test]
    fn a_stream_past_the_ceiling_is_refused_and_interrupts_the_run() {
        let guard = ParseGuard::new(4096, u64::MAX);
        let ((first, second), why) = guard.run(|| {
            let first = inflate(&zeros(10_000)).is_err();
            // Nothing more is decoded once the run is interrupted.
            let second = inflate(&zeros(10)).is_err();
            (first, second)
        });
        assert!(first && second);
        assert_eq!(why, Some(Interrupt::DecompressionLimit));
        assert!(inflate_or_raw(&zeros(10)).len() == 10, "the guard is gone");
    }

    #[test]
    fn many_streams_under_the_ceiling_still_spend_the_run_budget() {
        let guard = ParseGuard::new(4096, 10_000);
        let (decoded, why) = guard.run(|| {
            (0..5)
                .map(|_| inflate_or_raw(&zeros(3000)).len())
                .collect::<Vec<_>>()
        });
        assert_eq!(decoded, [3000, 3000, 3000, 0, 0]);
        assert_eq!(why, Some(Interrupt::DecompressionLimit));
    }

    #[test]
    fn a_passed_deadline_or_a_cancellation_stops_the_run_at_a_checkpoint() {
        let late = ParseGuard::new(4096, u64::MAX).with_deadline(Instant::now());
        let (result, why) = late.run(checkpoint);
        assert!(matches!(
            result,
            Err(PdfError::Interrupted(Interrupt::Deadline))
        ));
        assert_eq!(why, Some(Interrupt::Deadline));

        let flag = Arc::new(AtomicBool::new(false));
        let guard = ParseGuard::new(4096, u64::MAX).with_cancel(Arc::clone(&flag));
        let (before, after) = guard
            .run(|| {
                let before = stopped();
                flag.store(true, Ordering::Relaxed);
                (before, stopped())
            })
            .0;
        assert!(!before && after);
    }
}
