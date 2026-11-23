// SPDX-License-Identifier: Apache-2.0

//! A page whose font names its glyphs by glyph id needs OCR however its
//! text reads.
//!
//! A subsetter that renames glyphs `/gid00036` in a font's `/Differences`
//! has thrown away what each code means; only a ToUnicode CMap can say
//! which character the shape is. The walk decodes such codes through the
//! base encoding and knows it is guessing: it reports the page as
//! gid-encoded, and the library's own pipeline sends those pages to OCR.
//! FULL mode threw the report away. On `d8891a1` the page below went out
//! with `needs_ocr` false, `encoding_issues` false and no verdict anywhere,
//! because the text it decodes to reads as perfect English and the letter
//! statistics have nothing to convict.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

async fn full(pdf: &[u8]) -> Vec<pb::parse_pdf_response::Event> {
    common::start()
        .await
        .parse(
            pdf,
            pb::PdfOptions {
                mode: pb::ProcessMode::Full.into(),
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse")
}

#[tokio::test]
async fn a_page_set_in_glyph_ids_without_a_cmap_needs_ocr() {
    for in_form in [false, true] {
        glyph_ids_without_a_cmap(in_form).await;
    }
}

async fn glyph_ids_without_a_cmap(in_form: bool) {
    let events = full(&common::gid_named_glyphs_pdf(false, in_form)).await;
    let pages = common::pages(&events);
    assert_eq!(pages.len(), 2);

    let guessed = pages[0];
    // The words still go out: a caller without OCR gets the best reading
    // there is. The flags say it is a reading nobody can vouch for.
    assert!(guessed.markdown.contains("The committee met on Tuesday"));
    assert!(guessed.needs_ocr);
    assert_eq!(guessed.ocr_reason(), pb::OcrReason::SuspectedGarbled);
    assert!(guessed.encoding_issues);

    // Page two lists the same font in its resources and never shows text
    // in it.
    let plain = pages[1];
    assert!(!plain.needs_ocr, "{plain:?}");
    assert!(!plain.encoding_issues);

    let status = common::status(&events);
    assert!(status.has_encoding_issues);
    assert_eq!(
        status
            .extraction_ocr_reasons
            .iter()
            .map(|page| (page.page, page.reasons().collect::<Vec<_>>()))
            .collect::<Vec<_>>(),
        [(1, vec![pb::OcrReason::SuspectedGarbled])]
    );
}

#[tokio::test]
async fn the_same_page_with_a_cmap_for_its_codes_is_text() {
    let events = full(&common::gid_named_glyphs_pdf(true, false)).await;
    for page in common::pages(&events) {
        assert!(!page.needs_ocr, "{page:?}");
        assert!(!page.encoding_issues);
    }
    let status = common::status(&events);
    assert!(!status.has_encoding_issues);
    assert!(status.extraction_ocr_reasons.is_empty(), "{status:?}");
}
