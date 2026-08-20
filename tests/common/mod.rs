// SPDX-License-Identifier: Apache-2.0

//! In-memory PDF fixtures, authored by the tests, plus the server harness.
//!
//! Nothing binary is committed. Every document in this suite is built here
//! with lopdf — the same PDF library the parser reads with — which means a
//! fixture is a *program* rather than a blob: "the same document with twelve
//! pages" is one call, and so is "the same page with a raster where the text
//! should be". A committed `.pdf` cannot express either without a second
//! committed `.pdf`.

#![allow(dead_code)] // Each test binary uses a different part of this module.

use lopdf::{Document, Object, Stream, dictionary};

/// Build a text-based PDF of `pages` pages.
///
/// Each page carries one line of real text drawn with the base-14 Helvetica
/// font, padded with `body_words` words so classification and extraction have
/// something to chew on. `marker` is included in the first line of every
/// page, so a test can assert the markdown that came back is this document's.
#[must_use]
pub fn text_pdf(pages: u32, body_words: usize, marker: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let body = "lorem ipsum dolor sit amet ".repeat(body_words / 5 + 1);
        // One Tj per line; long bodies are split so no line runs off the page.
        let mut content = String::from("BT /F1 12 Tf 50 750 Td\n");
        content.push_str(&format!("({marker} page {page}) Tj\n"));
        let mut rest = body.as_str();
        while !rest.is_empty() {
            let take = rest.len().min(80);
            let (line, tail) = rest.split_at(take);
            content.push_str(&format!("0 -14 Td ({}) Tj\n", line.replace(['(', ')'], "")));
            rest = tail;
        }
        content.push_str("ET");

        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => pages,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize fixture");
    bytes
}

/// Build a scanned-style PDF: `pages` pages, each a full-page raster with no
/// text operators at all.
///
/// The image is a tiny 8x8 gray square drawn scaled to the full page box,
/// which is exactly what a scan is as far as classification is concerned:
/// one image covering the page and no text layer.
#[must_use]
pub fn image_pdf(pages: u32) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let mut kids = Vec::new();
    for _ in 0..pages {
        let mut image = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 8,
                "Height" => 8,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0x80; 64],
        );
        image.set_plain_content(vec![0x80; 64]);
        let image_id = doc.add_object(image);

        // Draw the image over the whole page: scale the unit square to the
        // MediaBox, then paint the XObject.
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            b"q 612 0 0 792 0 0 cm /Im1 Do Q".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im1" => image_id },
            },
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => pages,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize fixture");
    bytes
}

/// Bytes that are not a PDF at all.
#[must_use]
pub fn garbage() -> Vec<u8> {
    b"this is not a PDF, it is not even trying to be one".to_vec()
}

// --- Server harness -------------------------------------------------------

use std::sync::Arc;

use grpc_pdf_inspector::limits::Limits;
use grpc_pdf_inspector::metrics::Metrics;
use grpc_pdf_inspector::proto::v1 as pb;
use grpc_pdf_inspector::proto::v1::pdf_parse_service_client::PdfParseServiceClient;
use grpc_pdf_inspector::service::PdfGrpc;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Endpoint, Server};

/// A running server plus a client connected to it.
pub struct Harness {
    /// Connected client for the server below.
    pub client: PdfParseServiceClient<Channel>,
    /// The counters the server reports into, so a test can ask how far the
    /// parse has actually got rather than inferring it from timing.
    pub metrics: Arc<Metrics>,
}

/// Start a server on an ephemeral localhost port with the default limits.
pub async fn start() -> Harness {
    start_with(Limits::default()).await
}

/// Start a server on an ephemeral localhost port with the given limits.
pub async fn start_with(limits: Limits) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local address");

    let metrics = Metrics::new();
    let service = PdfGrpc::with_metrics(limits, Arc::clone(&metrics)).into_service();
    tokio::spawn(async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("server failed");
    });

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .expect("endpoint")
        .connect()
        .await
        .expect("connect to server");
    Harness {
        client: PdfParseServiceClient::new(channel),
        metrics,
    }
}

impl Harness {
    /// Upload `pdf` in one frame and collect every event.
    pub async fn parse(
        &self,
        pdf: &[u8],
        options: pb::PdfOptions,
    ) -> Result<Vec<pb::parse_pdf_response::Event>, tonic::Status> {
        self.parse_chunked(pdf, options, usize::MAX).await
    }

    /// Upload `pdf` in `chunk_size` slices and collect every event.
    pub async fn parse_chunked(
        &self,
        pdf: &[u8],
        options: pb::PdfOptions,
        chunk_size: usize,
    ) -> Result<Vec<pb::parse_pdf_response::Event>, tonic::Status> {
        let mut client = self.client.clone();
        let mut frames = vec![pb::ParsePdfRequest {
            frame: Some(pb::parse_pdf_request::Frame::Options(options)),
        }];
        frames.extend(pdf.chunks(chunk_size.min(pdf.len().max(1))).map(|chunk| {
            pb::ParsePdfRequest {
                frame: Some(pb::parse_pdf_request::Frame::Chunk(chunk.to_vec())),
            }
        }));

        let mut stream = client
            .parse_pdf(tokio_stream::iter(frames))
            .await?
            .into_inner();
        let mut events = Vec::new();
        while let Some(response) = stream.message().await? {
            events.push(response.event.expect("every response carries an event"));
        }
        Ok(events)
    }

    /// Parse with the default options, expecting success.
    pub async fn parse_ok(&self, pdf: &[u8]) -> Vec<pb::parse_pdf_response::Event> {
        self.parse(pdf, pb::PdfOptions::default())
            .await
            .expect("the document should parse")
    }

    /// Parse with the default options, expecting a failure status.
    pub async fn parse_err(&self, pdf: &[u8]) -> tonic::Status {
        self.parse(pdf, pb::PdfOptions::default())
            .await
            .expect_err("the document should be refused")
    }
}

/// The `info` event, which must be first.
#[must_use]
pub fn info(events: &[pb::parse_pdf_response::Event]) -> &pb::PdfInfo {
    match events.first() {
        Some(pb::parse_pdf_response::Event::Info(info)) => info,
        other => panic!("the first event must be `info`, got {other:?}"),
    }
}

/// The `status` trailer, which must be last.
#[must_use]
pub fn status(events: &[pb::parse_pdf_response::Event]) -> &pb::ParseStatus {
    match events.last() {
        Some(pb::parse_pdf_response::Event::Status(status)) => status,
        other => panic!("the last event must be `status`, got {other:?}"),
    }
}

/// Every `page` event, in the order received.
#[must_use]
pub fn pages(events: &[pb::parse_pdf_response::Event]) -> Vec<&pb::PageMarkdown> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Page(page) => Some(page),
            _ => None,
        })
        .collect()
}

/// A one-word name per event, for order assertions that read like the
/// contract: `["info", "page", "page", "status"]`.
#[must_use]
pub fn shape(events: &[pb::parse_pdf_response::Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            pb::parse_pdf_response::Event::Info(_) => "info",
            pb::parse_pdf_response::Event::Page(_) => "page",
            pb::parse_pdf_response::Event::Status(_) => "status",
            pb::parse_pdf_response::Event::Document(_) => "document",
        })
        .collect()
}

/// Every `document` event, in the order received. At most one, and only
/// when `options.emit_document` was set.
#[must_use]
pub fn documents(
    events: &[pb::parse_pdf_response::Event],
) -> Vec<&grpc_pdf_inspector::proto::ai::pipestream::document::v1::Document> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Document(document) => Some(document),
            _ => None,
        })
        .collect()
}
