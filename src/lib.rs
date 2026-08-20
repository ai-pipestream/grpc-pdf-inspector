// SPDX-License-Identifier: Apache-2.0

//! A gRPC server that classifies PDFs in memory and streams per-page
//! markdown back.
//!
//! Design rules:
//!
//! - **Classification is the product's front door.** A PDF is either cheap
//!   to read (real text layer) or expensive (scanned pages that need OCR),
//!   and `info` answers that in tens of milliseconds, before a single page
//!   is extracted. Callers route on it: fast local markdown here, heavy OCR
//!   elsewhere — this server never OCRs.
//! - **Nothing touches disk.** The upload lives in one `Vec<u8>` and every
//!   library call is the `*_mem` entry point; the container runs read-only.
//! - **The stream is the product.** `info` goes out the moment detection
//!   returns, each `page` goes out as that page's markdown is ready, and
//!   `status` is a trailer of counts, never the payload.
//! - **Hostile input is the normal case.** Every call into the parser is
//!   wrapped in [`std::panic::catch_unwind`] (lopdf can panic on malformed
//!   input) and runs on [`tokio::task::spawn_blocking`] (extraction is
//!   CPU-bound and parallelizes internally with rayon). A panic becomes an
//!   `INTERNAL` status, never a wedged stream.

pub mod limits;
pub mod metrics;
pub mod parse;
pub mod proto;
pub mod service;

pub use limits::Limits;
pub use metrics::Metrics;
pub use service::PdfGrpc;
