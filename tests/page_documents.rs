// SPDX-License-Identifier: Apache-2.0

//! The `emit_page_documents` contract: one `page_document` after each
//! `page`, carrying exactly the share of the fold that page made, so the
//! slices of one stream add up to the `document` event item for item.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// Every `page_document` event, in the order received.
fn page_documents(events: &[pb::parse_pdf_response::Event]) -> Vec<&pb::PageDocument> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::PageDocument(page) => Some(page),
            _ => None,
        })
        .collect()
}

/// The page a text item names first, 0 when it names none.
fn text_page(item: &doc::BaseTextItem) -> i32 {
    let prov = match item.item.as_ref().expect("a variant") {
        doc::base_text_item::Item::Code(code) => &code.prov,
        doc::base_text_item::Item::Text(text) => &text.base.as_ref().expect("a base").prov,
        doc::base_text_item::Item::SectionHeader(header) => {
            &header.base.as_ref().expect("a base").prov
        }
        doc::base_text_item::Item::ListItem(list) => &list.base.as_ref().expect("a base").prov,
        other => panic!("the fold does not make {other:?}"),
    };
    prov.first().map_or(0, |prov| prov.page_no)
}

/// Every option that puts more items into the fold, so the slices are
/// checked against furniture, invisible runs, dropped runs and tables too.
fn rich(emit_document: bool) -> pb::PdfOptions {
    pb::PdfOptions {
        emit_document,
        emit_page_documents: true,
        report_furniture: true,
        report_invisible: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn each_page_is_followed_by_its_page_document() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(3, 40, "slice-marker"),
            pb::PdfOptions {
                emit_page_documents: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    // Page documents alone build the fold without sending it whole.
    assert_eq!(
        common::shape(&events),
        [
            "info",
            "page",
            "page_document",
            "page",
            "page_document",
            "page",
            "page_document",
            "status"
        ]
    );
    let slices = page_documents(&events);
    for (index, slice) in slices.iter().enumerate() {
        let page_no = u32::try_from(index + 1).expect("a small page number");
        assert_eq!(slice.page_no, page_no, "slices follow their pages in order");
        let document = slice.document.as_ref().expect("a slice carries a document");
        assert!(!document.texts.is_empty(), "page {page_no} folded text");
        let page_no = i32::try_from(page_no).expect("a small page number");
        assert_eq!(
            document.pages.keys().copied().collect::<Vec<_>>(),
            [page_no],
            "a slice names its own page only"
        );
        let size = document.pages[&page_no]
            .size
            .as_ref()
            .expect("a measured page");
        assert!((size.width - 612.0).abs() < f64::EPSILON, "{size:?}");
    }
}

#[tokio::test]
async fn slices_add_up_to_the_whole_document() {
    let harness = common::start().await;
    let fixtures = [
        ("text", common::text_pdf(3, 40, "slice-marker")),
        ("furniture", common::furniture_pdf(3, "A Running Head")),
        ("two columns", common::two_column_pdf(2, "Two Columns")),
        ("review paper", common::review_paper_pdf(2, "Review Paper")),
        (
            "illustrated",
            common::illustrated_pdf("https://example.com/figure"),
        ),
        ("table", common::table_pdf()),
        ("ruled tables", common::ruled_and_borderless_tables_pdf()),
        ("invisible", common::invisible_text_pdf("HIDDEN WATERMARK")),
        ("landscape", common::landscape_pdf()),
        ("mixed", common::mixed_text_and_scan_pdf(3, &[2])),
    ];
    for (name, pdf) in fixtures {
        let events = harness
            .parse(&pdf, rich(true))
            .await
            .expect("the document should parse");
        let documents = common::documents(&events);
        assert_eq!(documents.len(), 1, "{name}: one whole document");
        let whole = documents[0];
        let slices = page_documents(&events);
        assert_eq!(
            slices.len(),
            common::pages(&events).len(),
            "{name}: one slice per page"
        );

        let mut texts = Vec::new();
        let mut tables = Vec::new();
        let mut pictures = Vec::new();
        let mut groups = Vec::new();
        let mut body = Vec::new();
        let mut furniture = Vec::new();
        for slice in &slices {
            let document = slice.document.as_ref().expect("a slice carries a document");
            let page_no = i32::try_from(slice.page_no).expect("a small page number");
            for item in &document.texts {
                assert_eq!(
                    text_page(item),
                    page_no,
                    "{name}: a text on its slice's page"
                );
            }
            for table in &document.tables {
                assert_eq!(
                    table.prov[0].page_no, page_no,
                    "{name}: a table on its page"
                );
            }
            for picture in &document.pictures {
                assert_eq!(
                    picture.prov[0].page_no, page_no,
                    "{name}: a picture on its page"
                );
            }
            assert_eq!(
                document.pages.get(&page_no),
                whole.pages.get(&page_no),
                "{name}: the slice's page is the document's page"
            );
            texts.extend(document.texts.iter().cloned());
            tables.extend(document.tables.iter().cloned());
            pictures.extend(document.pictures.iter().cloned());
            groups.extend(document.groups.iter().cloned());
            body.extend(
                document
                    .body
                    .as_ref()
                    .expect("a body")
                    .children
                    .iter()
                    .cloned(),
            );
            furniture.extend(
                document
                    .furniture
                    .as_ref()
                    .expect("a furniture group")
                    .children
                    .iter()
                    .cloned(),
            );
        }
        assert_eq!(texts, whole.texts, "{name}: texts add up");
        assert_eq!(tables, whole.tables, "{name}: tables add up");
        assert_eq!(pictures, whole.pictures, "{name}: pictures add up");
        assert_eq!(groups, whole.groups, "{name}: groups add up");
        assert_eq!(
            body,
            whole.body.as_ref().expect("a body").children,
            "{name}: the body's reading order adds up"
        );
        assert_eq!(
            furniture,
            whole
                .furniture
                .as_ref()
                .expect("a furniture group")
                .children,
            "{name}: the furniture adds up"
        );
    }
}

#[tokio::test]
async fn page_encoding_verdicts_account_for_the_document_flag() {
    let harness = common::start().await;
    let clean = harness
        .parse(&common::text_pdf(3, 40, "clean"), pb::PdfOptions::default())
        .await
        .expect("the document should parse");
    assert!(
        common::pages(&clean)
            .iter()
            .all(|page| !page.encoding_issues),
        "a clean text layer convicts no page"
    );
    for (name, pdf) in [
        ("garbled", common::garbled_pdf()),
        ("symbol soup", common::symbol_soup_pdf()),
    ] {
        let events = harness
            .parse(&pdf, pb::PdfOptions::default())
            .await
            .expect("the document should parse");
        assert!(
            common::status(&events).has_encoding_issues,
            "{name}: the fixture sets the document's flag"
        );
        assert!(
            common::pages(&events)
                .iter()
                .any(|page| page.encoding_issues || page.needs_ocr),
            "{name}: the document's flag is some page's verdict, known as that page arrives"
        );
    }
}
