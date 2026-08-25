// SPDX-License-Identifier: Apache-2.0

//! What the file says about itself.
//!
//! The parser this service wraps reads exactly one metadata field — the
//! title — out of one dictionary, and touches none of the rest. Author,
//! subject, keywords, the two applications that made the file, both dates,
//! the XMP packet, the header version, the catalog language, the tagged
//! flag, the encryption posture, the outline, embedded attachments, the
//! per-page boxes and rotation, page labels, named destinations and the
//! destinations of internal links are all one dictionary lookup away and
//! none of them were ever looked up.
//!
//! They are looked up here, by parsing the buffer a second time. The parser
//! crate does not re-export the PDF library it reads with and its own
//! loaders are private, so there is no way to borrow the document it
//! already has; a second read is the price, which is why the caller only
//! pays it when `emit_metadata` or `emit_document` asks for it.
//!
//! Everything below is defensive to the point of dullness. A metadata
//! dictionary is the least validated part of a PDF — producers write
//! whatever they like there, and a hostile file writes something worse — so
//! every lookup is fallible, every tree walk is depth- and budget-capped,
//! and a value that does not parse is reported as the string it was rather
//! than dropped or guessed at.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId, Permissions};
use prost_types::Timestamp;

use crate::proto::v1 as pb;

/// How deep a name tree, number tree or outline is followed before the
/// walk gives up. Real trees are a handful of levels; a crafted one is a
/// stack overflow.
const MAX_DEPTH: usize = 32;

/// Ceiling on how far any single stream may decompress while the document
/// loads. Comfortably above what a real file's object streams need and far
/// below what a decompression bomb wants.
const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

/// How many nodes one tree walk visits. A cycle is caught by the visited
/// set, but a wide fan-out of distinct nodes is not a cycle and still has
/// to end.
const MAX_NODES: usize = 100_000;

/// Read everything the document says about itself.
///
/// Returns `None` when the buffer cannot be loaded at all — which is not a
/// parse failure, because the text extraction runs off its own reader and
/// is unaffected. The caller reports the gap as a warning.
#[must_use]
pub fn read(bytes: &[u8], password: Option<&str>) -> Option<pb::PdfMetadata> {
    let options = lopdf::LoadOptions {
        password: password.map(ToOwned::to_owned),
        // Object and cross-reference streams are decompressed eagerly while
        // the document loads, so an unbounded limit here is a decompression
        // bomb waiting for a hostile upload — and hostile uploads are the
        // normal case for this service.
        max_decompressed_size: Some(MAX_DECOMPRESSED_BYTES),
        ..lopdf::LoadOptions::default()
    };
    let document = Document::load_mem_with_options(bytes, options).ok()?;
    Some(Reader::new(&document).read())
}

/// One loaded document, plus the page lookup every destination needs.
struct Reader<'a> {
    /// The document being read.
    doc: &'a Document,
    /// 1-indexed page numbers by page object, for resolving destinations.
    page_numbers: HashMap<ObjectId, u32>,
    /// Page objects in page order, 1-indexed.
    page_ids: Vec<(u32, ObjectId)>,
}

impl<'a> Reader<'a> {
    /// Index a document's pages so destinations can be resolved.
    fn new(doc: &'a Document) -> Self {
        let pages = doc.get_pages();
        Self {
            doc,
            page_numbers: pages.iter().map(|(number, id)| (*id, *number)).collect(),
            page_ids: pages.into_iter().collect(),
        }
    }

    /// The whole metadata message.
    fn read(&self) -> pb::PdfMetadata {
        let destinations = self.named_destinations();
        pb::PdfMetadata {
            info: Some(self.info()),
            xmp_packet: self.xmp_packet(),
            pdf_version: self.doc.version.clone(),
            language: self
                .catalog_text(b"Lang")
                // A catalog language is a BCP 47 tag, not a text string,
                // but producers write it both ways.
                .unwrap_or_default(),
            tagged: self.tagged(),
            file_id: self.file_id(),
            encryption: Some(self.encryption()),
            outline: self.outline(&destinations),
            embedded_files: self.embedded_files(),
            pages: self.page_geometry(),
            links: self.links(&destinations),
            destinations: destinations
                .iter()
                .map(|(name, page_no)| pb::NamedDestination {
                    name: name.clone(),
                    page_no: *page_no,
                })
                .collect(),
        }
    }

    // --- The information dictionary ---------------------------------------

    /// The `/Info` dictionary, field by field.
    fn info(&self) -> pb::DocumentInfo {
        let Some(info) = self
            .doc
            .trailer
            .get_deref(b"Info", self.doc)
            .ok()
            .and_then(|object| object.as_dict().ok())
        else {
            return pb::DocumentInfo::default();
        };
        let created_raw = text_of(info, b"CreationDate").unwrap_or_default();
        let modified_raw = text_of(info, b"ModDate").unwrap_or_default();
        let trapped = info
            .get(b"Trapped")
            .ok()
            .and_then(|object| object.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        pb::DocumentInfo {
            title: text_of(info, b"Title").unwrap_or_default(),
            authors: split_list(text_of(info, b"Author").as_deref()),
            subject: text_of(info, b"Subject").unwrap_or_default(),
            keywords: split_list(text_of(info, b"Keywords").as_deref()),
            creator_tool: text_of(info, b"Creator").unwrap_or_default(),
            producer: text_of(info, b"Producer").unwrap_or_default(),
            created: timestamp(&created_raw),
            modified: timestamp(&modified_raw),
            created_raw,
            modified_raw,
            trapped: trapped.clone(),
            trapped_state: trapped_state(&trapped).into(),
        }
    }

    /// The XMP packet from `/Root /Metadata`, decompressed but otherwise
    /// untouched.
    fn xmp_packet(&self) -> Vec<u8> {
        self.doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"Metadata", self.doc).ok())
            .and_then(|object| object.as_stream().ok())
            .map(|stream| {
                stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone())
            })
            .unwrap_or_default()
    }

    /// Whether the document declares itself tagged.
    fn tagged(&self) -> bool {
        self.doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"MarkInfo", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
            .and_then(|mark_info| mark_info.get(b"Marked").ok())
            .and_then(|object| object.as_bool().ok())
            .unwrap_or(false)
    }

    /// The first half of the trailer's `/ID`, as lower-case hex.
    fn file_id(&self) -> String {
        self.doc
            .trailer
            .get(b"ID")
            .ok()
            .and_then(|object| object.as_array().ok())
            .and_then(|parts| parts.first())
            .and_then(|part| part.as_str().ok())
            .map(|bytes| bytes.iter().map(|byte| format!("{byte:02x}")).collect())
            .unwrap_or_default()
    }

    /// A text string from the catalog.
    fn catalog_text(&self, key: &[u8]) -> Option<String> {
        self.doc
            .catalog()
            .ok()
            .and_then(|catalog| text_of(catalog, key))
    }

    // --- Encryption --------------------------------------------------------

    /// How the document is protected.
    ///
    /// The `/Encrypt` dictionary stays in the trailer after the loader has
    /// decrypted with the empty password, so this describes the file's
    /// declared posture whether or not that posture stopped anything.
    fn encryption(&self) -> pb::EncryptionInfo {
        let Ok(encrypt) = self.doc.get_encrypted() else {
            // No `/Encrypt` dictionary is not "permits nothing"; it is a
            // file with no permission bits at all, which permits
            // everything. Reporting the zero value here would read as a
            // locked-down document.
            return pb::EncryptionInfo {
                encrypted: false,
                opened_with_empty_password: true,
                allows_extraction: true,
                allows_printing: true,
                ..pb::EncryptionInfo::default()
            };
        };
        let permissions = encrypt
            .get(b"P")
            .ok()
            .and_then(|object| object.as_i64().ok())
            // /P is a signed 32-bit value with the reserved high bits set,
            // so it arrives negative on almost every real file.
            .map_or(Permissions::all(), |bits| {
                Permissions::from_bits_truncate(u64::from(bits as u32))
            });
        pb::EncryptionInfo {
            encrypted: true,
            filter: encrypt
                .get(b"Filter")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .unwrap_or_default(),
            version: number(encrypt, b"V").unwrap_or(0),
            revision: number(encrypt, b"R").unwrap_or(0),
            // A file that omits /Length is using the original 40-bit key.
            key_bits: number(encrypt, b"Length").unwrap_or(40),
            opened_with_empty_password: !self.doc.is_encrypted(),
            allows_extraction: permissions.contains(Permissions::COPYABLE),
            allows_printing: permissions.contains(Permissions::PRINTABLE),
        }
    }

    // --- Destinations ------------------------------------------------------

    /// Every named destination the document declares, resolved to a page.
    ///
    /// Both spellings are read: the PDF 1.1 `/Root /Dests` dictionary and
    /// the PDF 1.2 `/Root /Names /Dests` name tree.
    fn named_destinations(&self) -> Vec<(String, u32)> {
        let mut found = Vec::new();
        if let Some(dests) = self
            .doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"Dests", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
        {
            for (name, value) in dests {
                found.push((
                    String::from_utf8_lossy(name).into_owned(),
                    self.destination_page(value),
                ));
            }
        }
        let tree = self
            .doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"Names", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
            .and_then(|names| names.get_deref(b"Dests", self.doc).ok());
        if let Some(tree) = tree {
            let mut entries = Vec::new();
            self.walk_tree(tree, b"Names", &mut entries, &mut 0, 0);
            for (key, value) in entries {
                found.push((key, self.destination_page(&value)));
            }
        }
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found.dedup_by(|left, right| left.0 == right.0);
        found
    }

    /// The 1-indexed page a destination leads to, 0 when it does not
    /// resolve.
    ///
    /// A destination is an array whose first element names the page, or a
    /// dictionary wrapping one under `/D`, or a reference to either.
    fn destination_page(&self, destination: &Object) -> u32 {
        let Ok((_, destination)) = self.doc.dereference(destination) else {
            return 0;
        };
        if let Ok(dict) = destination.as_dict() {
            return dict
                .get(b"D")
                .map_or(0, |inner| self.destination_page_from_array(inner));
        }
        self.destination_page_from_array(destination)
    }

    /// The page named by a destination array.
    ///
    /// The first element is either a reference to the page object or, in a
    /// remote destination, its 0-indexed number.
    fn destination_page_from_array(&self, destination: &Object) -> u32 {
        let Ok((_, destination)) = self.doc.dereference(destination) else {
            return 0;
        };
        let Ok(target) = destination.as_array().map(|array| array.first()) else {
            return 0;
        };
        let Some(target) = target else {
            return 0;
        };
        if let Ok(page_id) = target.as_reference() {
            return self.page_numbers.get(&page_id).copied().unwrap_or(0);
        }
        target
            .as_i64()
            .ok()
            .and_then(|index| u32::try_from(index).ok())
            .and_then(|index| index.checked_add(1))
            .unwrap_or(0)
    }

    // --- Outline -----------------------------------------------------------

    /// The document's outline, depth-first, each entry keeping its depth.
    fn outline(&self, destinations: &[(String, u32)]) -> Vec<pb::OutlineEntry> {
        let mut entries = Vec::new();
        let first = self
            .doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"Outlines", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
            .and_then(|outlines| outlines.get(b"First").ok())
            .and_then(|object| object.as_reference().ok());
        let mut visited = Vec::new();
        self.walk_outline(first, 1, destinations, &mut entries, &mut visited);
        entries
    }

    /// Follow one `/Next` chain, recursing into each entry's `/First`.
    fn walk_outline(
        &self,
        mut node: Option<ObjectId>,
        level: u32,
        destinations: &[(String, u32)],
        entries: &mut Vec<pb::OutlineEntry>,
        visited: &mut Vec<ObjectId>,
    ) {
        if usize::try_from(level).unwrap_or(usize::MAX) > MAX_DEPTH {
            return;
        }
        while let Some(id) = node {
            if visited.contains(&id) || visited.len() >= MAX_NODES {
                return;
            }
            visited.push(id);
            let Ok(item) = self.doc.get_dictionary(id) else {
                return;
            };
            let (page_no, dest_name, uri) = self.action_target(item, destinations);
            entries.push(pb::OutlineEntry {
                title: text_of(item, b"Title").unwrap_or_default(),
                level,
                page_no,
                uri,
                dest_name,
            });
            let child = item
                .get(b"First")
                .ok()
                .and_then(|object| object.as_reference().ok());
            self.walk_outline(child, level + 1, destinations, entries, visited);
            node = item
                .get(b"Next")
                .ok()
                .and_then(|object| object.as_reference().ok());
        }
    }

    /// Where an outline entry or a link annotation leads: a page, a named
    /// destination, or a URI.
    fn action_target(
        &self,
        item: &Dictionary,
        destinations: &[(String, u32)],
    ) -> (u32, String, String) {
        // A direct /Dest wins; otherwise the /A action is read, which is
        // either a GoTo carrying a destination or a URI.
        let mut destination = item.get(b"Dest").ok();
        let mut uri = String::new();
        if let Some(action) = item
            .get_deref(b"A", self.doc)
            .ok()
            .and_then(|object| object.as_dict().ok())
        {
            match action.get(b"S").ok().and_then(|s| s.as_name().ok()) {
                Some(b"URI") => {
                    uri = action
                        .get(b"URI")
                        .ok()
                        .and_then(|object| object.as_str().ok())
                        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                        .unwrap_or_default();
                }
                Some(b"GoTo") if destination.is_none() => destination = action.get(b"D").ok(),
                _ => {}
            }
        }
        let Some(destination) = destination else {
            return (0, String::new(), uri);
        };
        // A destination given by name is looked up rather than followed:
        // that is the whole point of a named destination, and it is also
        // what makes the name worth reporting.
        if let Some(name) = destination_name(destination) {
            let page_no = destinations
                .iter()
                .find(|(known, _)| *known == name)
                .map_or(0, |(_, page_no)| *page_no);
            return (page_no, name, uri);
        }
        (self.destination_page(destination), String::new(), uri)
    }

    // --- Attachments -------------------------------------------------------

    /// The document's embedded file attachments.
    fn embedded_files(&self) -> Vec<pb::EmbeddedFile> {
        let tree = self
            .doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"Names", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
            .and_then(|names| names.get_deref(b"EmbeddedFiles", self.doc).ok());
        let Some(tree) = tree else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        self.walk_tree(tree, b"Names", &mut entries, &mut 0, 0);
        entries
            .into_iter()
            .filter_map(|(name, spec)| self.embedded_file(&name, &spec))
            .collect()
    }

    /// One `/Filespec` as an attachment.
    fn embedded_file(&self, key: &str, spec: &Object) -> Option<pb::EmbeddedFile> {
        let (_, spec) = self.doc.dereference(spec).ok()?;
        let spec = spec.as_dict().ok()?;
        // /UF is the Unicode file name and /F the legacy one; the tree key
        // is the last resort, because a name tree key is an identifier
        // rather than a file name.
        let name = text_of(spec, b"UF")
            .or_else(|| text_of(spec, b"F"))
            .unwrap_or_else(|| key.to_owned());
        let stream = spec
            .get_deref(b"EF", self.doc)
            .ok()
            .and_then(|object| object.as_dict().ok())
            .and_then(|embedded| embedded.get_deref(b"F", self.doc).ok())
            .and_then(|object| object.as_stream().ok());
        let media_type = stream
            .and_then(|stream| stream.dict.get(b"Subtype").ok())
            .and_then(|object| object.as_name().ok())
            // A name's `#xx` escapes are already decoded by the reader, so
            // a media type written as `/text#2Fcsv` arrives as `text/csv`.
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        let size_bytes = stream
            .and_then(|stream| stream.dict.get_deref(b"Params", self.doc).ok())
            .and_then(|object| object.as_dict().ok())
            .and_then(|params| params.get(b"Size").ok())
            .and_then(|object| object.as_i64().ok())
            .and_then(|size| u64::try_from(size).ok())
            // A file that declares no size still has one.
            .or_else(|| stream.map(|stream| stream.content.len() as u64))
            .unwrap_or(0);
        Some(pb::EmbeddedFile {
            name,
            media_type,
            size_bytes,
            description: text_of(spec, b"Desc").unwrap_or_default(),
        })
    }

    // --- Pages -------------------------------------------------------------

    /// Every page's boxes, rotation, scale and printed number.
    fn page_geometry(&self) -> Vec<pb::PageGeometry> {
        let labels = self.page_labels();
        self.page_ids
            .iter()
            .map(|(page_no, page_id)| {
                let media_box = self.inherited_rect(*page_id, b"MediaBox");
                let crop_box = self.inherited_rect(*page_id, b"CropBox").or(media_box);
                pb::PageGeometry {
                    page_no: *page_no,
                    media_box,
                    crop_box,
                    // /Rotate is inheritable and is specified as a multiple
                    // of 90, which producers do not always honour; negative
                    // and over-turned values are folded into [0, 360).
                    rotation: self
                        .inherited(*page_id, b"Rotate")
                        .and_then(|object| object.as_i64().ok())
                        .map_or(0, |degrees| degrees.rem_euclid(360) as u32),
                    user_unit: self
                        .inherited(*page_id, b"UserUnit")
                        .and_then(as_f64)
                        .unwrap_or(1.0),
                    label: labels.get(page_no).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// An inheritable page attribute, looked up from the page and then up
    /// the page tree.
    fn inherited(&self, page_id: ObjectId, key: &[u8]) -> Option<&Object> {
        let mut node = self.doc.get_dictionary(page_id).ok()?;
        for _ in 0..MAX_DEPTH {
            if let Ok(value) = node.get_deref(key, self.doc) {
                return Some(value);
            }
            let parent = node.get(b"Parent").ok()?.as_reference().ok()?;
            node = self.doc.get_dictionary(parent).ok()?;
        }
        None
    }

    /// An inheritable page attribute that is a rectangle.
    fn inherited_rect(&self, page_id: ObjectId, key: &[u8]) -> Option<pb::Rect> {
        rect(self.inherited(page_id, key)?)
    }

    /// The document's page labels, by 1-indexed page.
    ///
    /// `/PageLabels` is a number tree of ranges: each entry names the page
    /// its numbering starts at and how to render it. A document without one
    /// yields an empty map, which means every page's index is its number.
    fn page_labels(&self) -> HashMap<u32, String> {
        let tree = self
            .doc
            .catalog()
            .ok()
            .and_then(|catalog| catalog.get_deref(b"PageLabels", self.doc).ok());
        let Some(tree) = tree else {
            return HashMap::new();
        };
        let mut entries = Vec::new();
        self.walk_tree(tree, b"Nums", &mut entries, &mut 0, 0);

        // Ranges run until the next one starts, so they are read in order.
        let mut ranges: Vec<(u32, Dictionary)> = entries
            .into_iter()
            .filter_map(|(key, value)| {
                let start = key.parse::<u32>().ok()?;
                let (_, value) = self.doc.dereference(&value).ok()?;
                Some((start, value.as_dict().ok()?.clone()))
            })
            .collect();
        ranges.sort_by_key(|(start, _)| *start);

        let mut labels = HashMap::new();
        for (index, (start, spec)) in ranges.iter().enumerate() {
            let end = ranges
                .get(index + 1)
                .map_or(self.page_ids.len() as u32, |(next, _)| *next);
            let prefix = text_of(spec, b"P").unwrap_or_default();
            let style = spec
                .get(b"S")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(<[u8]>::to_vec);
            let first = number(spec, b"St").unwrap_or(1);
            for offset in 0..end.saturating_sub(*start) {
                // The tree is keyed by 0-indexed page; the wire is
                // 1-indexed.
                let page_no = start + offset + 1;
                let number = first.saturating_add(offset);
                labels.insert(page_no, format!("{prefix}{}", numbering(&style, number)));
            }
        }
        labels
    }

    // --- Link annotations --------------------------------------------------

    /// Every `/Link` annotation in the document, with its target resolved.
    fn links(&self, destinations: &[(String, u32)]) -> Vec<pb::LinkTarget> {
        let mut links = Vec::new();
        for (page_no, page_id) in &self.page_ids {
            let Some(annotations) = self
                .doc
                .get_dictionary(*page_id)
                .ok()
                .and_then(|page| page.get_deref(b"Annots", self.doc).ok())
                .and_then(|object| object.as_array().ok())
            else {
                continue;
            };
            for annotation in annotations {
                let Some(annotation) = self
                    .doc
                    .dereference(annotation)
                    .ok()
                    .and_then(|(_, object)| object.as_dict().ok())
                else {
                    continue;
                };
                if annotation
                    .get(b"Subtype")
                    .ok()
                    .and_then(|s| s.as_name().ok())
                    != Some(b"Link")
                {
                    continue;
                }
                let (dest_page_no, dest_name, uri) = self.action_target(annotation, destinations);
                links.push(pb::LinkTarget {
                    page_no: *page_no,
                    rect: annotation.get(b"Rect").ok().and_then(rect),
                    uri,
                    dest_page_no,
                    dest_name,
                });
            }
        }
        links
    }

    // --- Trees -------------------------------------------------------------

    /// Walk a name or number tree, collecting its leaves in order.
    ///
    /// The two differ only in their leaf key: `/Names` pairs a string key
    /// with a value, `/Nums` an integer key. Both are collected as strings,
    /// because the caller knows which it asked for.
    fn walk_tree(
        &self,
        node: &Object,
        leaf_key: &[u8],
        entries: &mut Vec<(String, Object)>,
        visited: &mut usize,
        depth: usize,
    ) {
        if depth > MAX_DEPTH || *visited >= MAX_NODES {
            return;
        }
        *visited += 1;
        let Ok((_, node)) = self.doc.dereference(node) else {
            return;
        };
        let Ok(node) = node.as_dict() else {
            return;
        };
        if let Ok(kids) = node
            .get_deref(b"Kids", self.doc)
            .and_then(lopdf::Object::as_array)
        {
            for kid in kids {
                self.walk_tree(kid, leaf_key, entries, visited, depth + 1);
            }
        }
        if let Ok(leaves) = node
            .get_deref(leaf_key, self.doc)
            .and_then(lopdf::Object::as_array)
        {
            for pair in leaves.chunks(2) {
                let [key, value] = pair else {
                    continue;
                };
                let key = match key {
                    Object::Integer(number) => number.to_string(),
                    other => other
                        .as_str()
                        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                        .unwrap_or_default(),
                };
                entries.push((key, value.clone()));
            }
        }
    }
}

/// Map a `/Trapped` name onto the wire enum.
///
/// The format defines exactly three names. Anything else — and there is
/// something else in the wild, because producers write what they like into
/// the information dictionary — is UNSPECIFIED here and stays verbatim in
/// `DocumentInfo.trapped`, so a name this build has never seen is reported
/// rather than reinterpreted.
fn trapped_state(name: &str) -> pb::Trapped {
    match name {
        "True" => pb::Trapped::True,
        "False" => pb::Trapped::False,
        "Unknown" => pb::Trapped::Unknown,
        _ => pb::Trapped::Unspecified,
    }
}

/// The name a destination is given by, when it is given by name.
fn destination_name(destination: &Object) -> Option<String> {
    match destination {
        Object::Name(name) => Some(String::from_utf8_lossy(name).into_owned()),
        Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

/// A text string entry of a dictionary, decoded from whichever of the
/// three PDF text encodings it was written in.
fn text_of(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let value = dict.get(key).ok()?;
    let text = lopdf::decode_text_string(value)
        .ok()
        .or_else(|| Some(String::from_utf8_lossy(value.as_str().ok()?).into_owned()))?;
    (!text.is_empty()).then_some(text)
}

/// A non-negative integer entry of a dictionary.
fn number(dict: &Dictionary, key: &[u8]) -> Option<u32> {
    u32::try_from(dict.get(key).ok()?.as_i64().ok()?).ok()
}

/// A number that may be written as an integer or a real.
fn as_f64(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

/// A four-number array as a rectangle.
///
/// PDF writes a rectangle as two opposite corners in either order, so the
/// corners are sorted into an origin and two non-negative extents.
fn rect(object: &Object) -> Option<pb::Rect> {
    let array = object.as_array().ok()?;
    let [x1, y1, x2, y2] = array.get(..4)? else {
        return None;
    };
    let (x1, y1) = (as_f64(x1)?, as_f64(y1)?);
    let (x2, y2) = (as_f64(x2)?, as_f64(y2)?);
    Some(pb::Rect {
        x: x1.min(x2),
        y: y1.min(y2),
        width: (x2 - x1).abs(),
        height: (y2 - y1).abs(),
    })
}

/// Split one metadata string that lists several values.
///
/// `/Author` and `/Keywords` are single strings by specification and lists
/// by practice. Semicolons and commas are the separators producers actually
/// use; a value containing neither yields one entry, which is the common
/// case and the one that must not be damaged.
fn split_list(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split([';', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Render a page number in one of the five numbering styles.
///
/// An unknown or absent style means the range is a prefix only, which is
/// how a document labels a run of pages "Cover" with no number at all.
fn numbering(style: &Option<Vec<u8>>, number: u32) -> String {
    match style.as_deref() {
        Some(b"D") => number.to_string(),
        Some(b"R") => roman(number).to_uppercase(),
        Some(b"r") => roman(number),
        Some(b"A") => letters(number).to_uppercase(),
        Some(b"a") => letters(number),
        _ => String::new(),
    }
}

/// Lower-case roman numerals. 0 has no numeral and yields nothing.
fn roman(mut number: u32) -> String {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while number >= value {
            out.push_str(numeral);
            number -= value;
        }
    }
    out
}

/// The alphabetic style: a, b, ... z, aa, bb, ... — a repeated letter
/// rather than a base-26 count, as the specification requires.
fn letters(number: u32) -> String {
    if number == 0 {
        return String::new();
    }
    let index = (number - 1) % 26;
    let repeats = (number - 1) / 26 + 1;
    let letter = char::from(b'a' + index as u8);
    std::iter::repeat_n(letter, repeats as usize).collect()
}

/// A PDF date string as an instant.
///
/// The form is `D:YYYYMMDDHHmmSSOHH'mm'`, everything after the year
/// optional, and `O` one of `+`, `-` or `Z`. Anything that does not parse
/// returns `None` and leaves the raw string to speak for itself.
fn timestamp(raw: &str) -> Option<Timestamp> {
    let digits: Vec<char> = raw
        .trim()
        .strip_prefix("D:")
        .unwrap_or(raw.trim())
        .chars()
        .collect();
    let field = |at: usize, len: usize, default: i64| -> Option<i64> {
        if digits.len() < at + len {
            return Some(default);
        }
        let text: String = digits[at..at + len].iter().collect();
        text.parse().ok()
    };
    let year = field(0, 4, -1)?;
    if year < 0 {
        return None;
    }
    let month = field(4, 2, 1)?;
    let day = field(6, 2, 1)?;
    let hour = field(8, 2, 0)?;
    let minute = field(10, 2, 0)?;
    let second = field(12, 2, 0)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }

    // The trailing offset, when the file states one.
    let mut offset_seconds = 0;
    if let Some(sign) = digits.get(14) {
        let sign = match sign {
            '+' => 1,
            '-' => -1,
            _ => 0,
        };
        if sign != 0 {
            let offset_hours: String = digits.iter().skip(15).take(2).collect();
            let offset_minutes: String = digits
                .iter()
                .skip(18)
                .take(2)
                .filter(|character| character.is_ascii_digit())
                .collect();
            let hours: i64 = offset_hours.parse().unwrap_or(0);
            let minutes: i64 = offset_minutes.parse().unwrap_or(0);
            offset_seconds = sign * (hours * 3600 + minutes * 60);
        }
    }

    let days = days_from_civil(year, month, day);
    Some(Timestamp {
        seconds: days * 86_400 + hour * 3600 + minute * 60 + second.min(60) - offset_seconds,
        nanos: 0,
    })
}

/// Days between 1970-01-01 and the given civil date, which may be before
/// it.
///
/// Howard Hinnant's `days_from_civil`, which is exact for the whole proleptic
/// Gregorian calendar and needs no lookup tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_pdf_date_parses_to_its_instant() {
        // 2001-09-09T01:46:40Z is 1_000_000_000 seconds after the epoch.
        let parsed = timestamp("D:20010909014640Z").expect("a well-formed date");
        assert_eq!(parsed.seconds, 1_000_000_000);
    }

    #[test]
    fn a_pdf_date_offset_is_applied() {
        let utc = timestamp("D:20240101120000Z").expect("utc");
        let ahead = timestamp("D:20240101120000+02'00'").expect("two hours ahead");
        assert_eq!(utc.seconds - ahead.seconds, 7200);
    }

    #[test]
    fn a_pdf_date_may_stop_after_any_field() {
        let year_only = timestamp("D:1998").expect("a year is a date");
        let explicit = timestamp("D:19980101000000").expect("the same date, spelled out");
        assert_eq!(year_only.seconds, explicit.seconds);
    }

    #[test]
    fn a_date_that_is_not_one_parses_to_nothing() {
        assert!(timestamp("").is_none());
        assert!(timestamp("today").is_none());
        assert!(timestamp("D:20241301").is_none(), "there is no month 13");
    }

    #[test]
    fn the_three_trapping_names_are_typed_and_anything_else_is_not() {
        assert_eq!(trapped_state("True"), pb::Trapped::True);
        assert_eq!(trapped_state("False"), pb::Trapped::False);
        assert_eq!(
            trapped_state("Unknown"),
            pb::Trapped::Unknown,
            "declaring ignorance is a declaration"
        );
        assert_eq!(trapped_state(""), pb::Trapped::Unspecified);
        assert_eq!(
            trapped_state("Partial"),
            pb::Trapped::Unspecified,
            "a name the format does not define is not reinterpreted"
        );
    }

    #[test]
    fn one_author_string_is_not_damaged_by_the_list_split() {
        assert_eq!(split_list(Some("Ada Lovelace")), ["Ada Lovelace"]);
        assert_eq!(
            split_list(Some("Ada Lovelace; Charles Babbage")),
            ["Ada Lovelace", "Charles Babbage"]
        );
        assert!(split_list(None).is_empty());
        assert!(split_list(Some("  ")).is_empty());
    }

    #[test]
    fn page_label_styles_render_as_the_specification_says() {
        assert_eq!(numbering(&Some(b"D".to_vec()), 12), "12");
        assert_eq!(numbering(&Some(b"r".to_vec()), 4), "iv");
        assert_eq!(numbering(&Some(b"R".to_vec()), 1990), "MCMXC");
        assert_eq!(numbering(&Some(b"a".to_vec()), 27), "aa");
        assert_eq!(numbering(&Some(b"A".to_vec()), 2), "B");
        assert_eq!(
            numbering(&None, 3),
            "",
            "a range with no style is a prefix only"
        );
    }

    #[test]
    fn a_rectangle_is_normalized_whichever_corners_it_names() {
        let backwards = Object::Array(vec![
            Object::Integer(300),
            Object::Integer(700),
            Object::Integer(100),
            Object::Real(600.5),
        ]);
        let normalized = rect(&backwards).expect("four numbers are a rectangle");
        assert!((normalized.x - 100.0).abs() < f64::EPSILON);
        assert!((normalized.y - 600.5).abs() < f64::EPSILON);
        assert!((normalized.width - 200.0).abs() < f64::EPSILON);
        assert!((normalized.height - 99.5).abs() < f64::EPSILON);
        assert!(rect(&Object::Array(vec![Object::Integer(1)])).is_none());
    }
}
