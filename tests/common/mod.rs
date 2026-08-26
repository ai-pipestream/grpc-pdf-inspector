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

/// Build a one-page PDF whose middle line is the anchor text of a real
/// `/Link` annotation pointing at `uri`.
///
/// The anchor text is deliberately *not* a URL. That is the case the old
/// pipeline could not see at all: it auto-linked bare URLs it found in the
/// visible text with a regular expression and never read the annotation
/// layer, so a link over the word "here" was invisible and a URL nobody
/// linked became a link.
#[must_use]
pub fn link_pdf(anchor: &str, uri: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    // Three lines, well apart, so the extractor keeps them as separate
    // items and the annotation covers exactly the middle one.
    let content = format!(
        "BT /F1 12 Tf 50 700 Td (Some prose before the link) Tj ET\n\
         BT /F1 12 Tf 50 650 Td ({anchor}) Tj ET\n\
         BT /F1 12 Tf 50 600 Td (Some prose after the link) Tj ET"
    );
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

    let annotation_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![45.into(), 640.into(), 300.into(), 670.into()],
        "Border" => vec![0.into(), 0.into(), 0.into()],
        "A" => dictionary! {
            "Type" => "Action",
            "S" => "URI",
            "URI" => Object::string_literal(uri),
        },
    });

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
        "Contents" => content_id,
        "Annots" => vec![Object::Reference(annotation_id)],
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Build a three-page PDF that says as much about itself as a real one
/// does: a full information dictionary, an XMP packet, a catalog language,
/// a tagged flag, an outline, an embedded file, a named destination, page
/// labels, a rotated page, a cropped and rescaled page, and an internal
/// cross-reference link.
///
/// Every one of these was unread before this wave. The fixture exists so
/// that "unread" is a test failure rather than a documentation claim.
#[must_use]
pub fn metadata_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut page_ids = Vec::new();
    for page in 1..=3u32 {
        let content = format!(
            "BT /F1 12 Tf 50 700 Td (Body of page {page}) Tj ET\n\
             BT /F1 12 Tf 50 650 Td (see the appendix) Tj ET"
        );
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        };
        // Page 2 is turned a quarter turn, which is the case that makes
        // every unqualified coordinate on the page ambiguous.
        if page == 2 {
            page_dict.set("Rotate", 90);
        }
        // Page 3 is cropped inside its sheet and drawn at twice the
        // default scale: the two facts without which a box on it means
        // nothing.
        if page == 3 {
            page_dict.set(
                "CropBox",
                vec![36.into(), 36.into(), 576.into(), 756.into()],
            );
            page_dict.set("UserUnit", 2);
        }
        page_ids.push(doc.add_object(page_dict));
    }

    // An internal cross-reference on page 1, pointing at page 3. The
    // extractor reads /A /URI and nothing else, so this is invisible to it.
    let internal_link = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![45.into(), 640.into(), 300.into(), 670.into()],
        "Dest" => vec![Object::Reference(page_ids[2]), "Fit".into()],
    });
    if let Ok(page) = doc.get_dictionary_mut(page_ids[0]) {
        page.set("Annots", vec![Object::Reference(internal_link)]);
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => 3,
        }),
    );

    // The outline: two chapters, the second nesting one section.
    let outlines_id = doc.new_object_id();
    let section_id = doc.new_object_id();
    let chapter_two_id = doc.new_object_id();
    let chapter_one_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Chapter One"),
        "Parent" => outlines_id,
        "Next" => chapter_two_id,
        "Dest" => vec![Object::Reference(page_ids[0]), "Fit".into()],
    });
    doc.objects.insert(
        chapter_two_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal("Chapter Two"),
            "Parent" => outlines_id,
            "Prev" => chapter_one_id,
            "First" => section_id,
            "Last" => section_id,
            "Dest" => vec![Object::Reference(page_ids[1]), "Fit".into()],
        }),
    );
    doc.objects.insert(
        section_id,
        Object::Dictionary(dictionary! {
            "Title" => Object::string_literal("A Section"),
            "Parent" => chapter_two_id,
            "Dest" => vec![Object::Reference(page_ids[2]), "Fit".into()],
        }),
    );
    doc.objects.insert(
        outlines_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => chapter_one_id,
            "Last" => chapter_two_id,
            "Count" => 3,
        }),
    );

    // One embedded file, declared with its media type and size.
    let payload = b"first,second\n1,2\n".to_vec();
    let mut embedded = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Subtype" => "text/csv",
            "Params" => dictionary! { "Size" => payload.len() as i64 },
        },
        payload.clone(),
    );
    embedded.set_plain_content(payload);
    let embedded_id = doc.add_object(embedded);
    let filespec_id = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal("data.csv"),
        "UF" => Object::string_literal("data.csv"),
        "Desc" => Object::string_literal("The numbers behind the table"),
        "EF" => dictionary! { "F" => embedded_id },
    });

    // A named destination other parts of the file can point at.
    let named_dest_id = doc.add_object(Object::Array(vec![
        Object::Reference(page_ids[2]),
        "Fit".into(),
    ]));

    let names_id = doc.add_object(dictionary! {
        "EmbeddedFiles" => dictionary! {
            "Names" => vec![Object::string_literal("data.csv"), Object::Reference(filespec_id)],
        },
        "Dests" => dictionary! {
            "Names" => vec![Object::string_literal("appendix"), Object::Reference(named_dest_id)],
        },
    });

    // Roman front matter, then arabic numbering restarting at 1.
    let page_labels_id = doc.add_object(dictionary! {
        "Nums" => vec![
            0.into(),
            Object::Dictionary(dictionary! { "S" => "r" }),
            1.into(),
            Object::Dictionary(dictionary! { "S" => "D", "St" => 1 }),
        ],
    });

    let xmp = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"/><?xpacket end="r"?>"#.to_vec();
    let mut xmp_stream = Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        xmp.clone(),
    );
    xmp_stream.set_plain_content(xmp);
    let xmp_id = doc.add_object(xmp_stream);

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Lang" => Object::string_literal("en-GB"),
        "MarkInfo" => dictionary! { "Marked" => true },
        "Metadata" => xmp_id,
        "Outlines" => outlines_id,
        "Names" => names_id,
        "PageLabels" => page_labels_id,
    });

    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("The Analytical Engine"),
        "Author" => Object::string_literal("Ada Lovelace; Charles Babbage"),
        "Subject" => Object::string_literal("Mechanical computation"),
        "Keywords" => Object::string_literal("engine, difference, notes"),
        "Creator" => Object::string_literal("An Authoring Application"),
        "Producer" => Object::string_literal("A PDF Writer 2.0"),
        "CreationDate" => Object::string_literal("D:20240115103000Z"),
        "ModDate" => Object::string_literal("D:20240220181500+02'00'"),
        "Trapped" => "False",
    });

    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    doc.trailer.set(
        "ID",
        vec![
            Object::String(
                vec![0xde, 0xad, 0xbe, 0xef],
                lopdf::StringFormat::Hexadecimal,
            ),
            Object::String(
                vec![0xde, 0xad, 0xbe, 0xef],
                lopdf::StringFormat::Hexadecimal,
            ),
        ],
    );

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize fixture");
    bytes
}

/// Build a one-page tagged PDF whose first line is declared an `H3` and
/// whose body is declared a `P`, both drawn at the same size.
///
/// Same size is the point. Heading depth in the markdown pipeline is
/// inferred by comparing type sizes, so it cannot reach three here; the
/// document's own structure tree can, and says so. A level-3 section header
/// in the fold therefore came from the tagging and from nowhere else.
#[must_use]
pub fn tagged_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let content = b"/P <</MCID 0>> BDC BT /F1 11 Tf 50 700 Td (An Authored Heading) Tj ET EMC\n\
                    /P <</MCID 1>> BDC BT /F1 11 Tf 50 670 Td \
                    (Ordinary body prose follows it, running on at some length so that) Tj \
                    0 -14 Td (nothing about its shape on the page suggests a heading to a) Tj \
                    0 -14 Td (pipeline that has only type sizes to go on. ) Tj ET EMC"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let struct_root_id = doc.new_object_id();
    let heading_id = doc.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "H3",
        "P" => struct_root_id,
        "Pg" => page_id,
        "K" => 0,
    });
    let paragraph_id = doc.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "P",
        "P" => struct_root_id,
        "Pg" => page_id,
        "K" => 1,
    });
    doc.objects.insert(
        struct_root_id,
        Object::Dictionary(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => vec![Object::Reference(heading_id), Object::Reference(paragraph_id)],
        }),
    );

    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
            "StructParents" => 0,
        }),
    );
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "MarkInfo" => dictionary! { "Marked" => true },
        "StructTreeRoot" => struct_root_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize fixture");
    bytes
}

/// Build a one-page PDF whose content is a grid of short cells in three
/// columns and four rows, which is what a borderless data table is.
///
/// No rules are drawn: the columns are made of aligned text and nothing
/// else, which is the case markdown can only render as pipe characters and
/// the detector can render as a grid with coordinates.
#[must_use]
pub fn table_pdf() -> Vec<u8> {
    const COLUMNS: [i32; 3] = [72, 240, 400];
    const ROWS: [(&str, &str, &str); 4] = [
        ("Year", "Engine", "Cards"),
        ("1837", "Analytical", "Punched"),
        ("1843", "Notes", "Woven"),
        ("1854", "Difference", "None"),
    ];

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut content = String::new();
    for (index, (first, second, third)) in ROWS.iter().enumerate() {
        let y = 700 - 24 * i32::try_from(index).expect("four rows fit in an i32");
        for (column, text) in COLUMNS.iter().zip([first, second, third]) {
            content.push_str(&format!("BT /F1 11 Tf {column} {y} Td ({text}) Tj ET\n"));
        }
    }

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
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Build a one-page PDF whose table is drawn with real rules and whose
/// cells hold prose rather than short aligned tokens.
///
/// The rules are the structure: horizontal lines between the rows and
/// vertical lines between the columns, drawn as path operators. Nothing
/// about the text says "table" on its own, which is the point. The
/// alignment-based detector reads a page like this as paragraphs, and only
/// the line-driven detector sees the grid the document actually drew.
#[must_use]
pub fn ruled_table_pdf() -> Vec<u8> {
    const LEFT: i32 = 72;
    const MIDDLE: i32 = 220;
    const RIGHT: i32 = 540;
    const ROWS: [(&str, &str); 4] = [
        (
            "Analytical Engine",
            "A general purpose machine described in 1837 and never built.",
        ),
        (
            "Difference Engine",
            "A special purpose calculator for polynomial tables.",
        ),
        (
            "Jacquard loom",
            "The punched card mechanism both engines borrowed from weaving.",
        ),
        (
            "Notes upon it",
            "The 1843 translation whose appendix carries the first program.",
        ),
    ];

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut content = String::new();
    // Prose above the table, so the document classifies as text-based and
    // the table is not the only thing on the page.
    for line in 0..4 {
        let y = 760 - 14 * line;
        content.push_str(&format!(
            "BT /F1 11 Tf 72 {y} Td (Introductory line {line} of ordinary running prose, set to fill the measure.) Tj ET\n"
        ));
    }

    // The rules: one horizontal per row edge, one vertical per column edge.
    let row_edges: Vec<i32> = (0..=4).map(|row| 700 - 30 * row).collect();
    for y in &row_edges {
        content.push_str(&format!("{LEFT} {y} m {RIGHT} {y} l S\n"));
    }
    let (top, bottom) = (row_edges[0], row_edges[row_edges.len() - 1]);
    for x in [LEFT, MIDDLE, RIGHT] {
        content.push_str(&format!("{x} {bottom} m {x} {top} l S\n"));
    }

    // The cells, each sitting inside its ruled box.
    for (row, (label, description)) in ROWS.iter().enumerate() {
        let y = row_edges[row] - 20;
        content.push_str(&format!(
            "BT /F1 10 Tf {} {y} Td ({label}) Tj ET\n",
            LEFT + 6
        ));
        content.push_str(&format!(
            "BT /F1 10 Tf {} {y} Td ({description}) Tj ET\n",
            MIDDLE + 6
        ));
    }

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
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Build a PDF of `pages` pages, each carrying a running head at the top,
/// a folio number at the bottom, and a paragraph of body text between them.
///
/// The head repeats verbatim on every page and the folio counts, which is
/// what the header, footer and page-number strippers look for. They strip
/// by default and used to strip silently.
#[must_use]
pub fn furniture_pdf(pages: u32, head: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let mut content = String::new();
        content.push_str(&format!("BT /F1 9 Tf 72 760 Td ({head}) Tj ET\n"));
        for line in 0..8 {
            let y = 700 - 16 * line;
            content.push_str(&format!(
                "BT /F1 11 Tf 72 {y} Td (Body line {line} of page {page}, long enough to read as prose.) Tj ET\n"
            ));
        }
        content.push_str(&format!("BT /F1 9 Tf 300 40 Td ({page}) Tj ET"));

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

/// Build a one-page PDF whose middle line is set in a bold face and whose
/// surrounding lines are not.
///
/// The three lines join into one paragraph, so the bold is a property of
/// part of a block rather than of the block. Markdown can only spell that
/// as `**` characters inside the text; a run-level span can say which
/// characters it covers.
#[must_use]
pub fn styled_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let regular_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let bold_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });

    let content = b"BT /F1 11 Tf 72 700 Td (Ordinary prose leading into the emphasis,) Tj ET\n\
                    BT /F2 11 Tf 72 686 Td (a phrase set in a bold face,) Tj ET\n\
                    BT /F1 11 Tf 72 672 Td (and ordinary prose after it again.) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => regular_id, "F2" => bold_id },
        },
        "Contents" => content_id,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Build a one-page PDF carrying an article of text, an image below it,
/// and a `/Link` annotation covering the image's region.
///
/// The text is what keeps the document classifying as text-based, so
/// extraction runs at all. The image is what the markdown renderer
/// discards by default, and the annotation over it is a link with no
/// anchor text — the case that has nowhere to go except onto the picture.
#[must_use]
pub fn illustrated_pdf(uri: &str) -> Vec<u8> {
    illustrated(FigureLink::External(uri))
}

/// The same page, but the annotation over the figure jumps to a second
/// page of the same document instead of out of it.
///
/// A figure that leads into the document rather than out of it is the case
/// with nowhere to go until `PictureItem` grew a target: a hyperlink cannot
/// hold it and no inline span can either, because the rectangle covers no
/// words.
#[must_use]
pub fn illustrated_pdf_linking_inward() -> Vec<u8> {
    illustrated(FigureLink::Internal)
}

/// Where the annotation over the figure leads.
enum FigureLink<'a> {
    /// Out of the document, to a URI.
    External(&'a str),
    /// Into the document, to its second page.
    Internal,
}

/// Build the illustrated fixture with the given annotation action.
fn illustrated(link: FigureLink<'_>) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

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

    let mut content = String::from("q 120 0 0 80 72 120 cm /Im1 Do Q\n");
    for line in 0..24 {
        let y = 700 - 16 * line;
        content.push_str(&format!(
            "BT /F1 11 Tf 72 {y} Td (Body line {line} of the article, long enough that the page reads as prose rather than as a picture.) Tj ET\n"
        ));
    }
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

    // An internal destination needs somewhere to land, so that arm gets a
    // second page. The external arm keeps the one-page document it had.
    let page_id = doc.new_object_id();
    let mut kids = vec![Object::Reference(page_id)];
    let mut annotation = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![72.into(), 120.into(), 192.into(), 200.into()],
    };
    match link {
        FigureLink::External(uri) => annotation.set(
            "A",
            dictionary! {
                "Type" => "Action",
                "S" => "URI",
                "URI" => Object::string_literal(uri),
            },
        ),
        FigureLink::Internal => {
            let second_content = doc.add_object(Stream::new(
                dictionary! {},
                b"BT /F1 11 Tf 72 700 Td (The page the figure leads to.) Tj ET".to_vec(),
            ));
            let second_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                },
                "Contents" => second_content,
            });
            kids.push(Object::Reference(second_id));
            annotation.set("Dest", vec![Object::Reference(second_id), "Fit".into()]);
        }
    }
    let annotation_id = doc.add_object(annotation);

    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
                "XObject" => dictionary! { "Im1" => image_id },
            },
            "Contents" => content_id,
            "Annots" => vec![Object::Reference(annotation_id)],
        }),
    );
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
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

/// Build a one-page PDF carrying a paragraph of visible prose and a
/// watermark drawn with text rendering mode 3.
///
/// `3 Tr` selects the neither-fill-nor-stroke rendering mode: the show
/// operator runs, the text matrix advances, and no glyph is painted. It is
/// how a scan's OCR layer hides behind its raster and how a watermark is
/// carried without being printed. A reader sees only the paragraph, and
/// every extraction this service ever did saw only the paragraph too.
#[must_use]
pub fn invisible_text_pdf(watermark: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut content = String::new();
    for line in 0..6 {
        let y = 700 - 16 * line;
        content.push_str(&format!(
            "BT /F1 11 Tf 72 {y} Td (Visible line {line} of the article a reader actually sees.) Tj ET\n"
        ));
    }
    // The hidden run: its own text block, its own rendering mode, far
    // enough from the prose that nothing joins it to a visible line.
    content.push_str(&format!(
        "BT 3 Tr /F1 30 Tf 140 300 Td ({watermark}) Tj ET\n"
    ));

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
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
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

/// Prose long enough for letter statistics to mean anything, one line per
/// entry.
const PROSE: [&str; 8] = [
    "The analytical engine weaves algebraic patterns just as the loom weaves",
    "flowers and leaves, and the distinctive characteristic of the machine is",
    "the introduction of the principle which Jacquard devised for regulating,",
    "by means of punched cards, the most complicated patterns in the fabrication",
    "of brocaded stuffs. In enabling mechanism to combine together general",
    "symbols in successions of unlimited variety and extent, a uniting link is",
    "established between the operations of matter and the abstract mental",
    "processes of the most abstract branch of mathematical science.",
];

/// Substitute every letter through a fixed permutation of the alphabet,
/// alternating case as it goes.
///
/// This is what a broken ToUnicode CMap does to a text layer: every
/// character is mapped through a constant offset, so the output is
/// printable ASCII with word-like token lengths and no replacement
/// character anywhere. What gives it away is the letter statistics. The
/// histogram is a permutation of a natural one, so its shape is unchanged
/// and its positions are wrong; the vowels starve, because the letters that
/// land on vowels are rarer than vowels are; and words flip case in the
/// middle, because a shifted alphabet straddles the ASCII case boundary.
#[must_use]
pub fn ciphered(source: &str) -> String {
    source
        .chars()
        .map(|character| {
            if !character.is_ascii_alphabetic() {
                return character;
            }
            let letter = (character.to_ascii_lowercase() as u8 - b'a' + 13) % 26;
            if letter.is_multiple_of(2) {
                (b'A' + letter) as char
            } else {
                (b'a' + letter) as char
            }
        })
        .collect()
}

/// Build a two-page PDF whose first page is ordinary prose and whose second
/// page is the same prose put through [`ciphered`].
///
/// Both pages carry the same number of letters, so the only difference
/// between them is where those letters fall.
#[must_use]
pub fn garbled_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for garble in [false, true] {
        let mut content = String::new();
        for (index, line) in PROSE.iter().enumerate() {
            let y = 700 - 16 * i32::try_from(index).expect("eight lines fit in an i32");
            let text = if garble {
                ciphered(line)
            } else {
                (*line).to_owned()
            };
            content.push_str(&format!("BT /F1 11 Tf 72 {y} Td ({text}) Tj ET\n"));
        }

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
            "Count" => 2,
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
            pb::parse_pdf_response::Event::Spans(_) => "spans",
            pb::parse_pdf_response::Event::Metadata(_) => "metadata",
            pb::parse_pdf_response::Event::Structure(_) => "structure",
            pb::parse_pdf_response::Event::Tables(_) => "tables",
        })
        .collect()
}

/// Every `spans` event, in the order received. Only when
/// `options.emit_spans` was set.
#[must_use]
pub fn spans(events: &[pb::parse_pdf_response::Event]) -> Vec<&pb::PageSpans> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Spans(spans) => Some(spans),
            _ => None,
        })
        .collect()
}

/// Every `tables` event, in the order received. Only when
/// `options.emit_tables` was set, and only for pages that have one.
#[must_use]
pub fn tables(events: &[pb::parse_pdf_response::Event]) -> Vec<&pb::PageTables> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Tables(tables) => Some(tables),
            _ => None,
        })
        .collect()
}

/// Every `structure` event, in the order received. Only when
/// `options.emit_structure` was set.
#[must_use]
pub fn structure(events: &[pb::parse_pdf_response::Event]) -> Vec<&pb::PageStructure> {
    events
        .iter()
        .filter_map(|event| match event {
            pb::parse_pdf_response::Event::Structure(structure) => Some(structure),
            _ => None,
        })
        .collect()
}

/// The one `metadata` event, which the caller must have asked for.
#[must_use]
pub fn metadata(events: &[pb::parse_pdf_response::Event]) -> &pb::PdfMetadata {
    events
        .iter()
        .find_map(|event| match event {
            pb::parse_pdf_response::Event::Metadata(metadata) => Some(metadata),
            _ => None,
        })
        .expect("a metadata event")
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
