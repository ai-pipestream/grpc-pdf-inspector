// SPDX-License-Identifier: Apache-2.0

//! When the second reader cannot open what the first one did, and what it
//! says about the files it does open.
//!
//! The metadata reader and the text extractor are two loads of the same
//! bytes with two versions of the PDF library, and they do not always
//! agree. Where they disagree the text arrives and the file's own facts do
//! not. When only `emit_document` asked for those facts, the document went
//! out without its source metadata, outline, attachments, page geometry and
//! link targets, and nothing said so.

mod common;

use grpc_pdf_inspector::proto::v1 as pb;

const MIB: usize = 1024 * 1024;

/// A server whose streams may inflate to one mebibyte.
async fn small_stream_limit() -> common::Harness {
    common::start_with(grpc_pdf_inspector::Limits {
        max_stream_bytes: MIB as u64,
        max_decompressed_bytes: 4 * MIB as u64,
        ..Default::default()
    })
    .await
}

/// The concatenated markdown of every page event.
fn markdown(events: &[pb::parse_pdf_response::Event]) -> String {
    common::pages(events)
        .iter()
        .map(|page| page.markdown.as_str())
        .collect()
}

/// The status warnings with the given code.
fn warnings_with(
    events: &[pb::parse_pdf_response::Event],
    code: pb::ParseWarningCode,
) -> Vec<&pb::ParseWarning> {
    common::status(events)
        .warnings
        .iter()
        .filter(|warning| warning.code == code as i32)
        .collect()
}

#[tokio::test]
async fn a_document_that_lost_its_metadata_says_so_when_only_the_fold_asked() {
    let pdf = common::oversized_object_stream_pdf("Lost Title", "LOSTMARK", 8 * MIB);
    let events = small_stream_limit()
        .await
        .parse(
            &pdf,
            pb::PdfOptions {
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the text extractor opens the file");

    assert!(
        markdown(&events).contains("LOSTMARK"),
        "the control: the text arrived, got {:?}",
        markdown(&events)
    );
    let documents = common::documents(&events);
    let document = documents.first().expect("a document event");
    assert!(
        document.source_meta.is_none(),
        "the control: the metadata reader could not open this file"
    );

    let unavailable = warnings_with(&events, pb::ParseWarningCode::MetadataUnavailable);
    assert_eq!(
        unavailable.len(),
        1,
        "the document went out without its own metadata and nothing said so: {:?}",
        common::status(&events).warnings
    );
    assert!(
        unavailable[0].message.contains("source metadata"),
        "{:?}",
        unavailable[0].message
    );
}

#[tokio::test]
async fn the_metadata_event_and_the_fold_report_the_same_loss_once() {
    let pdf = common::oversized_object_stream_pdf("Lost Title", "LOSTMARK", 8 * MIB);
    let events = small_stream_limit()
        .await
        .parse(
            &pdf,
            pb::PdfOptions {
                emit_metadata: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the text extractor opens the file");
    assert!(!common::shape(&events).contains(&"metadata"));
    assert_eq!(
        warnings_with(&events, pb::ParseWarningCode::MetadataUnavailable).len(),
        1,
        "{:?}",
        common::status(&events).warnings
    );
}

#[tokio::test]
async fn an_encrypted_file_that_opened_is_still_reported_encrypted() {
    // The everyday "protected" file: an owner password guards the
    // permission bits and the user password is empty. The caller sends a
    // password the file does not accept; both readers fall back to the
    // empty one.
    let pdf = common::owner_protected_pdf("Protected Title", "PROTECTEDMARK");
    let events = common::start()
        .await
        .parse(
            &pdf,
            pb::PdfOptions {
                password: "not-the-password".to_owned(),
                emit_metadata: true,
                emit_document: true,
                ..Default::default()
            },
        )
        .await
        .expect("the file opens with its empty user password");
    assert!(markdown(&events).contains("PROTECTEDMARK"));

    let metadata = common::metadata(&events);
    assert_eq!(
        metadata.info.as_ref().expect("an info").title,
        "Protected Title"
    );
    let encryption = metadata.encryption.as_ref().expect("an encryption block");
    assert!(
        encryption.encrypted,
        "decrypting the file must not erase the fact that it was encrypted: {encryption:?}"
    );
    assert_eq!(encryption.filter, "Standard");
    assert_eq!((encryption.version, encryption.revision), (2, 3));
    assert_eq!(encryption.key_bits, 128);
    assert!(
        encryption.opened_with_empty_password,
        "the password that opened it was the empty one, not the caller's"
    );

    let documents = common::documents(&events);
    let protection = documents
        .first()
        .and_then(|document| document.source_meta.as_ref())
        .and_then(|meta| meta.protection.as_ref())
        .expect("the fold states the protection too");
    assert!(protection.encrypted);
    assert!(protection.opened_without_password);

    assert!(
        warnings_with(&events, pb::ParseWarningCode::MetadataUnavailable).is_empty(),
        "nothing went missing, so nothing is reported missing"
    );
}
