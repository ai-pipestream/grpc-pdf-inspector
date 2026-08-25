// SPDX-License-Identifier: Apache-2.0

//! What the file says about itself, and where it lands.
//!
//! Before this wave the answer was one field: the title. Everything here —
//! the rest of the information dictionary, the XMP packet, the header
//! version, the catalog language, the tagged flag, the encryption posture,
//! the outline, embedded attachments, per-page boxes and rotation, page
//! labels, named destinations and the destinations of internal links — was
//! one dictionary lookup away and never looked up.

mod common;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;
use grpc_pdf_inspector::proto::v1 as pb;

/// Parse the metadata fixture with the given options.
async fn parse(options: pb::PdfOptions) -> Vec<pb::parse_pdf_response::Event> {
    let harness = common::start().await;
    harness
        .parse(&common::metadata_pdf(), options)
        .await
        .expect("the fixture should parse")
}

/// Metadata only, with no extraction to slow the test down.
fn metadata_only() -> pb::PdfOptions {
    pb::PdfOptions {
        mode: pb::ProcessMode::DetectOnly.into(),
        emit_metadata: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn metadata_stays_off_the_wire_unless_it_is_asked_for() {
    let events = parse(pb::PdfOptions {
        mode: pb::ProcessMode::DetectOnly.into(),
        ..Default::default()
    })
    .await;
    assert_eq!(common::shape(&events), ["info", "status"]);
}

#[tokio::test]
async fn metadata_arrives_once_immediately_after_info() {
    let events = parse(metadata_only()).await;
    assert_eq!(common::shape(&events), ["info", "metadata", "status"]);
}

#[tokio::test]
async fn the_whole_information_dictionary_is_read_not_only_the_title() {
    let events = parse(metadata_only()).await;
    let info = common::metadata(&events).info.as_ref().expect("an info");

    assert_eq!(info.title, "The Analytical Engine");
    assert_eq!(info.authors, ["Ada Lovelace", "Charles Babbage"]);
    assert_eq!(info.subject, "Mechanical computation");
    assert_eq!(info.keywords, ["engine", "difference", "notes"]);
    assert_eq!(info.creator_tool, "An Authoring Application");
    assert_eq!(info.producer, "A PDF Writer 2.0");
    assert_eq!(info.trapped, "False", "the name the file spells");
    assert_eq!(
        info.trapped_state,
        pb::Trapped::False as i32,
        "and the same declaration, typed"
    );

    // The dates are instants, with the file's own spelling kept beside
    // them so nothing is lost to the parse.
    assert_eq!(info.created_raw, "D:20240115103000Z");
    let created = info.created.expect("a parsed creation date");
    assert_eq!(created.seconds, 1_705_314_600);
    let modified = info.modified.expect("a parsed modification date");
    assert!(
        modified.seconds > created.seconds,
        "the file was modified after it was created"
    );
}

#[tokio::test]
async fn the_documents_own_declarations_are_read() {
    let events = parse(metadata_only()).await;
    let metadata = common::metadata(&events);

    assert_eq!(metadata.pdf_version, "1.7");
    assert_eq!(metadata.language, "en-GB");
    assert!(metadata.tagged, "the fixture declares itself tagged");
    assert_eq!(metadata.file_id, "deadbeef");
    assert!(
        metadata.xmp_packet.starts_with(b"<?xpacket"),
        "the XMP packet arrives verbatim, not reinterpreted"
    );

    let encryption = metadata.encryption.as_ref().expect("an encryption block");
    assert!(!encryption.encrypted, "the fixture is not encrypted");
    assert!(
        encryption.allows_extraction && encryption.allows_printing,
        "no permission bits is no restriction, not every restriction"
    );
    assert!(encryption.opened_with_empty_password);
}

#[tokio::test]
async fn the_outline_keeps_its_depth_and_its_destinations() {
    let events = parse(metadata_only()).await;
    let outline = &common::metadata(&events).outline;

    let shape: Vec<(&str, u32, u32)> = outline
        .iter()
        .map(|entry| (entry.title.as_str(), entry.level, entry.page_no))
        .collect();
    assert_eq!(
        shape,
        [
            ("Chapter One", 1, 1),
            ("Chapter Two", 1, 2),
            ("A Section", 2, 3),
        ],
        "depth-first, with each entry's own depth and page"
    );
}

#[tokio::test]
async fn an_embedded_file_is_reported_with_its_type_and_size() {
    let events = parse(metadata_only()).await;
    let files = &common::metadata(&events).embedded_files;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "data.csv");
    assert_eq!(files[0].media_type, "text/csv");
    assert_eq!(files[0].size_bytes, 17);
    assert_eq!(files[0].description, "The numbers behind the table");
}

#[tokio::test]
async fn every_page_reports_its_box_rotation_and_printed_number() {
    let events = parse(metadata_only()).await;
    let pages = &common::metadata(&events).pages;
    assert_eq!(pages.len(), 3);

    for page in pages {
        let media = page.media_box.as_ref().expect("a media box");
        assert!((media.width - 612.0).abs() < f64::EPSILON, "{media:?}");
        assert!((media.height - 792.0).abs() < f64::EPSILON, "{media:?}");
        assert!(
            page.crop_box.is_some(),
            "a page with no crop box crops to its media box"
        );
    }

    // Page 3 is cropped inside its sheet and drawn at twice the scale.
    let cropped = pages[2].crop_box.as_ref().expect("a crop box");
    assert!((cropped.width - 540.0).abs() < f64::EPSILON, "{cropped:?}");
    assert!((cropped.height - 720.0).abs() < f64::EPSILON, "{cropped:?}");
    assert!((pages[2].user_unit - 2.0).abs() < f64::EPSILON);
    assert!(
        (pages[0].user_unit - 1.0).abs() < f64::EPSILON,
        "a page that says nothing is at the default scale"
    );

    let rotations: Vec<u32> = pages.iter().map(|page| page.rotation).collect();
    assert_eq!(rotations, [0, 90, 0], "the turned page says so");

    let labels: Vec<&str> = pages.iter().map(|page| page.label.as_str()).collect();
    assert_eq!(
        labels,
        ["i", "1", "2"],
        "roman front matter, then arabic restarting at 1"
    );
}

#[tokio::test]
async fn named_destinations_and_internal_links_resolve_to_pages() {
    let events = parse(metadata_only()).await;
    let metadata = common::metadata(&events);

    assert_eq!(metadata.destinations.len(), 1);
    assert_eq!(metadata.destinations[0].name, "appendix");
    assert_eq!(metadata.destinations[0].page_no, 3);

    assert_eq!(metadata.links.len(), 1);
    let link = &metadata.links[0];
    assert_eq!(link.page_no, 1, "the annotation is drawn on page 1");
    assert_eq!(link.dest_page_no, 3, "and it leads to page 3");
    assert!(link.uri.is_empty(), "an internal jump is not a URL");
    assert!(link.rect.is_some());
}

#[tokio::test]
async fn a_document_that_declares_no_trapping_is_not_reported_as_untrapped() {
    let harness = common::start().await;
    let events = harness
        .parse(
            &common::text_pdf(1, 20, "untrapped-marker"),
            pb::PdfOptions {
                mode: pb::ProcessMode::DetectOnly.into(),
                emit_metadata: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the document should parse");

    let info = common::metadata(&events).info.as_ref().expect("an info");
    assert!(info.trapped.is_empty());
    assert_eq!(info.trapped_state, pb::Trapped::Unspecified as i32);

    let document = common::documents(&events)[0];
    assert!(
        document
            .source_meta
            .as_ref()
            .is_none_or(|meta| meta.trapped.is_none()),
        "saying nothing is not the same as saying no"
    );
}

#[tokio::test]
async fn the_fold_writes_the_metadata_the_schema_has_homes_for() {
    let events = parse(pb::PdfOptions {
        emit_document: true,
        ..Default::default()
    })
    .await;
    let document = common::documents(&events)[0];

    let meta = document.source_meta.as_ref().expect("source metadata");
    assert_eq!(meta.title.as_deref(), Some("The Analytical Engine"));
    assert_eq!(meta.authors, ["Ada Lovelace", "Charles Babbage"]);
    assert_eq!(meta.language.as_deref(), Some("en-GB"));
    assert_eq!(meta.generator.as_deref(), Some("A PDF Writer 2.0"));
    assert_eq!(
        meta.authoring_tool.as_deref(),
        Some("An Authoring Application"),
        "the application it was written in is not the one that produced it"
    );
    assert_eq!(meta.subject.as_deref(), Some("Mechanical computation"));
    assert_eq!(meta.keywords, ["engine", "difference", "notes"]);
    assert!(meta.created.is_some(), "the instant is typed");
    assert_eq!(meta.created_raw.as_deref(), Some("D:20240115103000Z"));

    // What the file says about its own format and posture.
    assert_eq!(meta.format_version.as_deref(), Some("1.7"));
    assert_eq!(meta.structured, Some(true), "the fixture is tagged");
    assert_eq!(
        meta.raw_metadata.as_deref().map(|packet| &packet[..9]),
        Some(&b"<?xpacket"[..]),
        "the XMP packet travels verbatim"
    );
    assert_eq!(
        meta.trapped,
        Some(doc::Trapped::False as i32),
        "the file declares that trapping was not applied"
    );
    let protection = meta.protection.as_ref().expect("a protection block");
    assert!(!protection.encrypted);
    assert!(protection.opened_without_password);
    assert!(protection.allows_extraction && protection.allows_printing);

    // The identity the file carries inside itself, which is the only
    // identity a byte stream has.
    assert_eq!(
        document
            .origin
            .as_ref()
            .expect("an origin")
            .source_id
            .as_deref(),
        Some("deadbeef")
    );

    // The authored outline, which is better evidence of structure than a
    // heading level inferred from type size.
    let outline: Vec<(&str, i32, Option<i32>)> = document
        .outline
        .iter()
        .map(|entry| (entry.title.as_str(), entry.level, entry.page_no))
        .collect();
    assert_eq!(
        outline,
        [
            ("Chapter One", 1, Some(1)),
            ("Chapter Two", 1, Some(2)),
            ("A Section", 2, Some(3)),
        ]
    );
    assert_eq!(
        document.outline[0]
            .target
            .as_ref()
            .expect("a resolved target")
            .r#ref,
        "#/pages/1"
    );

    // The file the PDF carries inside itself.
    assert_eq!(document.attachments.len(), 1);
    let attachment = &document.attachments[0];
    assert_eq!(attachment.name, "data.csv");
    assert_eq!(attachment.media_type, "text/csv");
    assert_eq!(attachment.size_bytes, 17);
    assert!(
        attachment.id.starts_with("pdf-embedded-file:"),
        "the pointer says where the payload lives: {}",
        attachment.id
    );

    // The positions its own cross-references point at.
    assert_eq!(document.anchors.len(), 1);
    assert_eq!(document.anchors[0].name, "appendix");
    assert_eq!(
        document.anchors[0].target.as_ref().expect("a target").r#ref,
        "#/pages/3"
    );

    // Page geometry and the rotation that qualifies every box on that page.
    let second = document.pages.get(&2).expect("page 2");
    let size = second.size.as_ref().expect("a measured page");
    assert!((size.width - 612.0).abs() < f64::EPSILON);
    assert!(
        second.media_size.is_none(),
        "the sheet and the visible box are the same here, so only one is said"
    );
    assert!(
        second.user_unit.is_none(),
        "a scale of 1 is the default and says nothing"
    );

    // The cropped, rescaled page says both.
    let third = document.pages.get(&3).expect("page 3");
    let visible = third.size.as_ref().expect("the visible box");
    assert!((visible.width - 540.0).abs() < f64::EPSILON, "{visible:?}");
    let sheet = third.media_size.as_ref().expect("the sheet it sits on");
    assert!((sheet.width - 612.0).abs() < f64::EPSILON, "{sheet:?}");
    assert_eq!(third.user_unit, Some(2.0));

    // The pages' own printed numbers: roman front matter, then arabic.
    let labels: Vec<Option<&str>> = (1..=3)
        .map(|page_no| document.pages[&page_no].page_label.as_deref())
        .collect();
    assert_eq!(labels, [Some("i"), Some("1"), Some("2")]);
    let quality = second.quality.as_ref().expect("the turned page's quality");
    assert_eq!(quality.rotation_degrees, Some(90.0));
    assert!(
        document
            .pages
            .get(&1)
            .expect("page 1")
            .quality
            .as_ref()
            .is_none_or(|quality| quality.rotation_degrees.is_none_or(|d| d == 0.0)),
        "an unturned page claims no rotation"
    );
}

#[tokio::test]
async fn an_internal_link_anchors_its_text_to_the_page_it_leads_to() {
    let events = parse(pb::PdfOptions {
        emit_document: true,
        ..Default::default()
    })
    .await;
    let document = common::documents(&events)[0];

    let anchored = document
        .texts
        .iter()
        .filter_map(|item| match item.item.as_ref() {
            Some(doc::base_text_item::Item::Text(text)) => text.base.as_ref(),
            Some(doc::base_text_item::Item::SectionHeader(header)) => header.base.as_ref(),
            _ => None,
        })
        .find(|base| !base.spans.is_empty())
        .expect("the cross-reference's anchor text carries a run");

    let span = &anchored.spans[0];
    assert!(
        span.hyperlink.is_none(),
        "a jump inside the document is not a URL"
    );
    assert_eq!(
        span.target.as_ref().expect("a target").r#ref,
        "#/pages/3",
        "the run points at the page the annotation leads to"
    );
}
