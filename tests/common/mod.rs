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
    ruled_table_page(false)
}

/// The ruled table's page with a borderless table of short aligned cells
/// below it: the [`table_pdf`] grid, set lower on the page.
///
/// The two tables come out of different detectors. The rules make the line
/// detector report the ruled grid, and only it, because a detector that
/// finds a data table ends the search; the markdown renderer's own table
/// finder misses the ruled table and prints the borderless one as pipe
/// characters. Nothing pairs the pipe block with the ruled grid but their
/// order on the page.
#[must_use]
pub fn ruled_and_borderless_tables_pdf() -> Vec<u8> {
    ruled_table_page(true)
}

/// The ruled table's page, with or without the borderless table under it.
fn ruled_table_page(with_borderless: bool) -> Vec<u8> {
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

    if with_borderless {
        const COLUMNS: [i32; 3] = [72, 240, 400];
        const CELLS: [(&str, &str, &str); 4] = [
            ("Year", "Engine", "Cards"),
            ("1837", "Analytical", "Punched"),
            ("1843", "Notes", "Woven"),
            ("1854", "Difference", "None"),
        ];
        for (index, (first, second, third)) in CELLS.iter().enumerate() {
            let y = 420 - 24 * i32::try_from(index).expect("four rows fit in an i32");
            for (column, text) in COLUMNS.iter().zip([first, second, third]) {
                content.push_str(&format!("BT /F1 11 Tf {column} {y} Td ({text}) Tj ET\n"));
            }
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

/// Build a `pages`-page PDF laid out in two columns, with a running head
/// at the top of every page and a line number in the left margin of every
/// row.
///
/// This is the shape a conference paper has, and the shape that starved the
/// body. The rows are drawn left cell then right cell, so extraction order
/// interleaves the columns and the markdown renderer, which reads a page
/// column by column, cannot emit every run in the order it was drawn.
/// Whatever it leaves out is content, because the columns are prose, while
/// the running head and the margin numbers are the only chrome on the page.
///
/// The head repeats verbatim and the margin numbers repeat as a set, which
/// is what cross-page repetition evidence is made of; the columns say
/// something different on every row of every page, which is what it is not.
#[must_use]
pub fn two_column_pdf(pages: u32, head: &str) -> Vec<u8> {
    const ROWS: u32 = 18;

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
        content.push_str(&format!("BT /F1 8 Tf 72 770 Td ({head}) Tj ET\n"));
        // A section heading, set larger than the columns so the renderer
        // reads it as one, and far enough below the running head that the
        // head stands alone at the page edge.
        content.push_str(&format!(
            "BT /F1 14 Tf 72 730 Td (Section {page}. Findings) Tj ET\n"
        ));
        for row in 1..=ROWS {
            let y = 700 - 16 * (row - 1);
            // The margin number, outside the text block and set smaller:
            // the shape a line-numbered manuscript has.
            content.push_str(&format!("BT /F1 7 Tf 40 {y} Td ({row}) Tj ET\n"));
            content.push_str(&format!(
                "BT /F1 9 Tf 72 {y} Td (Method note {row} of page {page} on the setup.) Tj ET\n"
            ));
            content.push_str(&format!(
                "BT /F1 9 Tf 320 {y} Td (Result note {row} of page {page} on the yield.) Tj ET\n"
            ));
        }
        content.push_str(&format!("BT /F1 8 Tf 300 40 Td ({page}) Tj ET"));

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

/// Build a `pages`-page paper in the review format: a running head with a
/// rule under it, a line number in the left margin of every row, dense body
/// prose, and a folio at the foot.
///
/// This is the shape of the document that found the last two faults. The
/// head is *underlined*, which matters: the renderer spells an underlined
/// run `<u>text</u>`, and those tags carry letters, so a chrome report
/// matched against the rendered block on letters alone missed it and the
/// head was filed as furniture and folded into the body as well. The head
/// also sits one ordinary line above the body rather than off on its own,
/// so no white space isolates it and only its type size sets it apart.
///
/// The margin numbers count on across the pages, so no two are the same
/// text and only their shape and their position repeat.
#[must_use]
pub fn review_paper_pdf(pages: u32, head: &str) -> Vec<u8> {
    const ROWS: i32 = 40;

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    // The margin numbers are set in a bold face, as the review templates
    // set them. It is what the renderer prints `**001**` for when the
    // number fuses into the line beside it.
    let number_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let mut content = String::new();
        content.push_str(&format!("BT /F1 9 Tf 72 716 Td ({head}) Tj ET\n"));
        // The rule under the head: what makes the extractor call it
        // underlined and the renderer print tags around it.
        content.push_str("0.6 w 72 713 m 400 713 l S\n");
        for row in 0..ROWS {
            let y = 690 - 16 * row;
            let number = (i32::try_from(page).expect("a page number fits") - 1) * ROWS + row;
            // Drawn on the row's own baseline, which is the fusion
            // trigger: the renderer assembles a line from the runs that
            // share it, so a number left in its input joins the sentence.
            content.push_str(&format!("BT /F2 7 Tf 45 {y} Td ({number:03}) Tj ET\n"));
            content.push_str(&format!(
                "BT /F1 10 Tf 72 {y} Td (Body row {row} of page {page} carrying ordinary prose about the method.) Tj ET\n"
            ));
        }
        content.push_str(&format!("BT /F1 9 Tf 300 60 Td ({page}) Tj ET"));

        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id, "F2" => number_id },
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

/// Build a one-page PDF whose figure is drawn as vector art through a
/// Form XObject, placed between two paragraphs, with a caption under it.
///
/// A figure included from another PDF, which is how a paper's plots are
/// set, is a form: paths and a few labels, no image XObject anywhere. The
/// walker used to report nothing for it, so the paper's figures had no
/// picture items. The form's `/BBox` is 200 by 100 and the page draws it
/// scaled by two at (72, 400), so its placed box is 400 by 200 with its
/// top at 600.
#[must_use]
pub fn vector_figure_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    // The figure: a frame, a curve, and a label set in its own font.
    let figure_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
        b"0.5 w 5 5 190 90 re S 10 20 m 60 80 120 10 190 70 c S \
          BT /F1 6 Tf 20 8 Td (yield) Tj ET"
            .to_vec(),
    ));
    let content = "BT /F1 11 Tf 72 700 Td (Prose above the figure describes the method in some detail.) Tj ET\n\
                   BT /F1 11 Tf 72 686 Td (It continues for a second line before the figure.) Tj ET\n\
                   q 2 0 0 2 72 400 cm /Fx1 Do Q\n\
                   BT /F1 9 Tf 72 385 Td (Figure 1: A vector drawing of the yield.) Tj ET\n\
                   BT /F1 11 Tf 72 340 Td (Prose below the figure discusses what it shows.) Tj ET\n\
                   BT /F1 11 Tf 72 326 Td (It also runs to a second line.) Tj ET";
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Fx1" => figure_id },
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

/// Build a one-page PDF whose whole content is drawn through one Form
/// XObject, the shape print-to-PDF producers emit: the page stream is a
/// single `Do`, and everything a reader sees is inside the form.
#[must_use]
pub fn wrapped_page_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let wrapper_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
        b"BT /F1 12 Tf 72 700 Td (A page drawn entirely through one form.) Tj ET \
          BT /F1 12 Tf 72 686 Td (Nothing here is a figure.) Tj ET"
            .to_vec(),
    ));
    let content_id = doc.add_object(Stream::new(dictionary! {}, b"q /X1 Do Q".to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {
            "XObject" => dictionary! { "X1" => wrapper_id },
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

/// Build a one-page landscape PDF the way Acrobat Distiller writes one: a
/// portrait 612 x 792 sheet with `/Rotate 90`, and every line of [`PROSE`]
/// drawn with a text matrix turned a quarter, so the page reads as
/// horizontal text on a 792 x 612 page.
///
/// Line `n` starts at user space `(60 + 16n, 54)`, which the page shows 54
/// points from its left edge and `60 + 16n` points from its top.
///
/// The page also invokes one 50 x 30 Form XObject at user space (100, 200),
/// a vector figure the way Distiller places one: the page shows it 200
/// points from its left edge, 30 wide and 50 tall, with its bottom edge
/// 612 - 150 = 462 points from the bottom.
#[must_use]
pub fn landscape_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let mut content = String::new();
    for (index, line) in PROSE.iter().enumerate() {
        let x = 60 + 16 * i32::try_from(index).expect("eight lines fit in an i32");
        content.push_str(&format!(
            "BT /F1 1 Tf 0 11 -11 0 {x} 54 Tm ({line}) Tj ET\n"
        ));
    }
    let figure_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 50.into(), 30.into()],
        },
        b"0.5 w 2 2 46 26 re S".to_vec(),
    ));
    content.push_str("q 1 0 0 1 100 200 cm /Fx1 Do Q\n");
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Rotate" => 90,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Fx1" => figure_id },
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

/// A one-page document drawing `content` on a letter sheet, with the font
/// `F1` (Helvetica) available, an optional crop box and an optional
/// `/Rotate`: the fixture for everything about where a box lands on the
/// page a reader sees.
#[must_use]
pub fn framed_pdf(content: &str, crop_box: Option<[i64; 4]>, rotate: Option<i64>) -> Vec<u8> {
    framed_pdf_with(FramedPage {
        content: content.to_owned(),
        crop_box,
        rotate: rotate.map(Object::Integer),
        ..FramedPage::default()
    })
}

/// What [`framed_pdf_with`] builds.
#[derive(Debug, Clone)]
pub struct FramedPage {
    /// The page's content stream.
    pub content: String,
    /// The media box, letter when not said.
    pub media_box: [i64; 4],
    /// The crop box, when the page declares one.
    pub crop_box: Option<[i64; 4]>,
    /// `/Rotate`, as whatever object the fixture wants to write.
    pub rotate: Option<Object>,
}

impl Default for FramedPage {
    fn default() -> Self {
        Self {
            content: String::new(),
            media_box: [0, 0, 612, 792],
            crop_box: None,
            rotate: None,
        }
    }
}

/// A one-page document drawing `page.content` with the font `F1`
/// (Helvetica) available, under the boxes and rotation the page says.
#[must_use]
pub fn framed_pdf_with(page: FramedPage) -> Vec<u8> {
    let FramedPage {
        content,
        media_box,
        crop_box,
        rotate,
    } = page;
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => media_box.iter().map(|v| Object::Integer(*v)).collect::<Vec<_>>(),
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
        "Contents" => content_id,
    };
    if let Some(crop) = crop_box {
        page.set(
            "CropBox",
            crop.iter().map(|v| Object::Integer(*v)).collect::<Vec<_>>(),
        );
    }
    if let Some(rotate) = rotate {
        page.set("Rotate", rotate);
    }
    let page_id = doc.add_object(page);
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
    prose_then(ciphered)
}

/// Map every letter of `source` to the low-ASCII symbol its glyph code would
/// decode to under a font with no ToUnicode CMap and a custom encoding.
///
/// Type 3 fonts number their glyphs from 1 and subset fonts from a small
/// offset, so with no CMap the codes land on `!"#$%&` and the digits. What
/// comes out has almost no letters, which is why the letter statistics
/// behind [`ciphered`] cannot measure it.
#[must_use]
pub fn symbol_soup(source: &str) -> String {
    const GLYPHS: &[u8] = b"!\"#$%&'*+,-./0123456789:;<=>?";
    source
        .chars()
        .map(|character| {
            if !character.is_ascii_alphabetic() {
                return character;
            }
            let letter = usize::from(character.to_ascii_lowercase() as u8 - b'a');
            GLYPHS[letter % GLYPHS.len()] as char
        })
        .collect()
}

/// Build a two-page PDF whose first page is ordinary prose and whose second
/// page is the same prose put through [`symbol_soup`].
#[must_use]
pub fn symbol_soup_pdf() -> Vec<u8> {
    prose_then(symbol_soup)
}

/// Two pages of [`PROSE`], the second put through `second`.
fn prose_then(second: fn(&str) -> String) -> Vec<u8> {
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
                second(line)
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

/// A scanner's page image: one flat gray, letter-sized at 200 dpi.
///
/// The pixel count is the point. The detector calls an image a page scan
/// when it is large enough to cover the page at a scanning resolution, and
/// an 8x8 square stretched over the page is not; this one is, and its
/// pixels compress to almost nothing.
fn scan_image(doc: &mut Document) -> lopdf::ObjectId {
    const WIDTH: usize = 1700;
    const HEIGHT: usize = 2200;
    let mut image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => WIDTH as i64,
            "Height" => HEIGHT as i64,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0xee; WIDTH * HEIGHT],
    );
    image.compress().expect("compress the page image");
    doc.add_object(image)
}

/// The words a scan's OCR layer transcribes, drawn one show operator per
/// word the way OCRmyPDF and Tesseract lay a layer down, in text
/// rendering mode 3 behind the page image.
fn ocr_layer(page: u32) -> String {
    ocr_layer_reading(page, |line| line.to_owned())
}

/// An OCR layer whose engine read each line of [`PROSE`] as `reading` of it.
fn ocr_layer_reading(page: u32, reading: impl Fn(&str) -> String) -> String {
    let mut content = String::from("BT\n3 Tr\n/F1 10 Tf\n");
    for (row, line) in PROSE.iter().enumerate() {
        let y = 700 - 14 * i32::try_from(row).expect("eight lines fit in an i32");
        let mut x = 72;
        for word in reading(line).split_whitespace() {
            let word = word.replace(['(', ')'], "");
            content.push_str(&format!("1 0 0 1 {x} {y} Tm ({word}) Tj\n"));
            x += 6 * i32::try_from(word.len() + 1).expect("a word fits in an i32");
        }
    }
    content.push_str(&format!("1 0 0 1 72 100 Tm (Scanned page {page}) Tj\nET\n"));
    content
}

/// Build a searchable scan: `pages` pages, each a page-sized raster with an
/// OCR layer drawn invisibly behind it, which is what OCRmyPDF, ABBYY and
/// Acrobat's "make searchable" all produce.
///
/// No glyph on any page is visible. Every word a reader sees is pixels, and
/// the text layer is the OCR engine's transcription of them, so the pages
/// need OCR whatever the layer says. Each page draws well over fifty show
/// operators, which is how many it took for the detector to stop calling
/// such a page a scan and start calling it text.
#[must_use]
pub fn searchable_scan_pdf(pages: u32) -> Vec<u8> {
    searchable_scan_with_layer(pages, ocr_layer)
}

/// The words of [`ocr_layer`], laid down by a producer that sets text
/// rendering mode 3 once, before any text object, and then draws each line
/// in a text object of its own.
///
/// The mode is graphics state, so `BT` does not reset it and every one of
/// these lines is as invisible as in [`searchable_scan_pdf`]; only a reader
/// that resets the mode at `BT` sees them.
fn ocr_layer_with_mode_set_before_bt(page: u32) -> String {
    let mut content = String::from("3 Tr\n");
    for (row, line) in PROSE.iter().enumerate() {
        let y = 700 - 14 * i32::try_from(row).expect("eight lines fit in an i32");
        content.push_str("BT\n/F1 10 Tf\n");
        let mut x = 72;
        for word in line.split_whitespace() {
            let word = word.replace(['(', ')'], "");
            content.push_str(&format!("1 0 0 1 {x} {y} Tm ({word}) Tj\n"));
            x += 6 * i32::try_from(word.len() + 1).expect("a word fits in an i32");
        }
        content.push_str("ET\n");
    }
    content.push_str(&format!(
        "BT\n/F1 10 Tf\n1 0 0 1 72 100 Tm (Scanned page {page}) Tj\nET\n"
    ));
    content
}

/// Build a searchable scan like [`searchable_scan_pdf`] whose producer sets
/// the invisible rendering mode before its text objects rather than inside
/// them.
#[must_use]
pub fn searchable_scan_with_mode_before_bt_pdf(pages: u32) -> Vec<u8> {
    searchable_scan_with_layer(pages, ocr_layer_with_mode_set_before_bt)
}

/// A searchable scan whose pages each draw a page-sized raster and then
/// the OCR layer `layer` writes for the page number.
fn searchable_scan_with_layer(pages: u32, layer: fn(u32) -> String) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let image_id = scan_image(&mut doc);
        let mut content = String::from("q 612 0 0 792 0 0 cm /Im1 Do Q\n");
        content.push_str(&layer(page));
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
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

/// Build a mostly born-digital document with scanned pages inside it:
/// `pages` pages of real text, except the pages in `scanned`, which are a
/// page-sized raster and nothing else.
///
/// An annual report with a scanned appendix, a contract with a signed page
/// scanned back in. Detection samples eight pages, so a long enough
/// document can keep its scans out of the sample entirely.
#[must_use]
pub fn mixed_text_and_scan_pdf(pages: u32, scanned: &[u32]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let (content, resources) = if scanned.contains(&page) {
            let image_id = scan_image(&mut doc);
            (
                String::from("q 612 0 0 792 0 0 cm /Im1 Do Q"),
                dictionary! { "XObject" => dictionary! { "Im1" => image_id } },
            )
        } else {
            let mut content = format!("BT /F1 12 Tf 72 740 Td (Born digital page {page}) Tj\n");
            for line in PROSE {
                content.push_str(&format!("0 -16 Td ({line}) Tj\n"));
            }
            content.push_str("ET");
            (
                content,
                dictionary! { "Font" => dictionary! { "F1" => font_id } },
            )
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources,
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

/// Build a three-page born-digital document whose middle page is a scan
/// with an OCR layer that misread every letter: [`ciphered`] prose, drawn
/// invisibly behind the page image.
///
/// The layer is real text by every measure the OCR-layer fallback applies,
/// so it becomes the page's text, and its letter statistics are those of a
/// garbled text layer. It is still not a broken font encoding, and the page
/// needs OCR whatever it says.
#[must_use]
pub fn text_with_misread_scan_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=3u32 {
        let (content, resources) = if page == 2 {
            let image_id = scan_image(&mut doc);
            let mut content = String::from("q 612 0 0 792 0 0 cm /Im1 Do Q\n");
            content.push_str(&ocr_layer_reading(page, ciphered));
            (
                content,
                dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                    "XObject" => dictionary! { "Im1" => image_id },
                },
            )
        } else {
            let mut content = format!("BT /F1 12 Tf 72 740 Td (Born digital page {page}) Tj\n");
            for line in PROSE {
                content.push_str(&format!("0 -16 Td ({line}) Tj\n"));
            }
            content.push_str("ET");
            (
                content,
                dictionary! { "Font" => dictionary! { "F1" => font_id } },
            )
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources,
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => 3,
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

/// Build a three-page document whose middle page is a photograph with
/// empty text objects over it, between two pages of prose.
///
/// Some capture software writes a text object per region whether it read
/// anything there or not, so the page carries show operators that show
/// nothing. They are enough to keep the page off every list sampling
/// detection keeps, and the page still has no word on it that is not
/// pixels.
#[must_use]
pub fn photo_with_empty_text_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let mut photo = Stream::new(
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
    photo.set_plain_content(vec![0x80; 64]);
    let photo_id = doc.add_object(photo);

    let mut kids = Vec::new();
    for page in 1..=3u32 {
        let (content, resources) = if page == 2 {
            (
                String::from(
                    "q 468 0 0 600 72 96 cm /Im1 Do Q\n\
                     BT /F1 10 Tf 72 80 Td () Tj () Tj () Tj ET",
                ),
                dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                    "XObject" => dictionary! { "Im1" => photo_id },
                },
            )
        } else {
            let mut content = format!("BT /F1 12 Tf 72 740 Td (Prose page {page}) Tj\n");
            for line in PROSE {
                content.push_str(&format!("0 -16 Td ({line}) Tj\n"));
            }
            content.push_str("ET");
            (
                content,
                dictionary! { "Font" => dictionary! { "F1" => font_id } },
            )
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources,
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => 3,
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

/// Build a `pages`-page document whose every content stream inflates to
/// `inflated_bytes`: a line of real text and then that much white space,
/// Flate-compressed.
///
/// White space is legal content, so a reader that decodes the stream finds
/// one ordinary line in it. What it costs to get there is the point: Flate
/// packs a run of spaces about a thousand to one, which is how a few
/// kilobytes of upload ask for megabytes or gigabytes of memory.
#[must_use]
pub fn inflating_pdf(pages: u32, inflated_bytes: usize) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let mut kids = Vec::new();
    for page in 1..=pages {
        let mut content =
            format!("BT /F1 12 Tf 72 700 Td (A line of text on page {page}) Tj ET\n").into_bytes();
        content.resize(content.len() + inflated_bytes, b' ');
        let mut stream = Stream::new(dictionary! {}, content);
        stream.compress().expect("compress the content stream");
        let content_id = doc.add_object(stream);
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
    start_with_service(|metrics| PdfGrpc::with_metrics(limits, metrics)).await
}

/// Start a server on an ephemeral localhost port, built by `build` around
/// the counters the harness will report.
pub async fn start_with_service(build: impl FnOnce(Arc<Metrics>) -> PdfGrpc) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local address");

    let metrics = Metrics::new();
    let service = build(Arc::clone(&metrics)).into_service();
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
            pb::parse_pdf_response::Event::PageDocument(_) => "page_document",
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

// --- Reading a folded Document --------------------------------------------

use std::collections::HashSet;

use grpc_pdf_inspector::proto::ai::pipestream::document::v1 as doc;

/// Self ref of the group every body item hangs under.
pub const BODY: &str = "#/body";

/// Self ref of the group every item that is not body hangs under.
pub const FURNITURE: &str = "#/furniture";

/// One item of a folded fragment, whichever arena it lives in: what it is
/// called, what it hangs off, which layer it declares, and what it lists as
/// its own children.
pub struct Placed {
    /// The item's own ref.
    pub self_ref: String,
    /// The ref it names as its parent.
    pub parent: String,
    /// The content layer it declares.
    pub layer: i32,
    /// The refs it lists as its children.
    pub children: Vec<String>,
    /// Its text, when it has any, for a readable assertion message.
    pub text: String,
}

/// Every item of a folded fragment, from every arena.
///
/// The two root groups are not included: they are where the walk starts,
/// not things placed in it.
#[must_use]
pub fn placed(document: &doc::Document) -> Vec<Placed> {
    let mut items = Vec::new();
    for item in &document.texts {
        let (self_ref, parent, layer, children, text) = match item.item.as_ref().expect("a variant")
        {
            doc::base_text_item::Item::Code(code) => (
                code.self_ref.clone(),
                code.parent.clone(),
                code.content_layer,
                code.children.clone(),
                code.text.clone(),
            ),
            other => {
                let base = match other {
                    doc::base_text_item::Item::Text(text) => text.base.as_ref(),
                    doc::base_text_item::Item::SectionHeader(header) => header.base.as_ref(),
                    doc::base_text_item::Item::ListItem(list_item) => list_item.base.as_ref(),
                    other => panic!("this fold makes no {other:?}"),
                }
                .expect("a base");
                (
                    base.self_ref.clone(),
                    base.parent.clone(),
                    base.content_layer,
                    base.children.clone(),
                    base.text.clone(),
                )
            }
        };
        items.push(Placed {
            self_ref,
            parent: parent.expect("a parent").r#ref,
            layer,
            children: children.into_iter().map(|child| child.r#ref).collect(),
            text,
        });
    }
    for group in &document.groups {
        items.push(Placed {
            self_ref: group.self_ref.clone(),
            parent: group.parent.as_ref().expect("a parent").r#ref.clone(),
            layer: group.content_layer,
            children: group
                .children
                .iter()
                .map(|child| child.r#ref.clone())
                .collect(),
            text: group.name.clone().unwrap_or_default(),
        });
    }
    for table in &document.tables {
        items.push(Placed {
            self_ref: table.self_ref.clone(),
            parent: table.parent.as_ref().expect("a parent").r#ref.clone(),
            layer: table.content_layer,
            children: table
                .children
                .iter()
                .map(|child| child.r#ref.clone())
                .collect(),
            text: String::from("<table>"),
        });
    }
    for picture in &document.pictures {
        items.push(Placed {
            self_ref: picture.self_ref.clone(),
            parent: picture.parent.as_ref().expect("a parent").r#ref.clone(),
            layer: picture.content_layer,
            children: picture
                .children
                .iter()
                .map(|child| child.r#ref.clone())
                .collect(),
            text: String::from("<picture>"),
        });
    }
    items
}

/// Every ref reachable from `#/body`, following children through groups and
/// through anything else that lists them.
#[must_use]
pub fn reachable_from_body(document: &doc::Document) -> HashSet<String> {
    let items = placed(document);
    let mut found = HashSet::new();
    let mut queue: Vec<String> = document
        .body
        .as_ref()
        .expect("a body")
        .children
        .iter()
        .map(|child| child.r#ref.clone())
        .collect();
    while let Some(self_ref) = queue.pop() {
        if !found.insert(self_ref.clone()) {
            continue;
        }
        if let Some(item) = items.iter().find(|item| item.self_ref == self_ref) {
            queue.extend(item.children.iter().cloned());
        }
    }
    found
}

/// The root group an item hangs under, following its parents up.
///
/// `None` when the chain does not end at a root, which is a fragment no
/// consumer can walk.
#[must_use]
pub fn root_of(document: &doc::Document, self_ref: &str) -> Option<String> {
    let items = placed(document);
    let mut at = self_ref.to_owned();
    // A fragment is a tree; the bound is what stops a cycle from hanging
    // the test instead of failing it.
    for _ in 0..=items.len() {
        let item = items.iter().find(|item| item.self_ref == at)?;
        if item.parent == BODY || item.parent == FURNITURE {
            return Some(item.parent.clone());
        }
        at = item.parent.clone();
    }
    None
}

/// The invariant every consumer of this plane relies on: an item's layer
/// and the group it hangs under say the same thing, and the body walk
/// reaches exactly the body.
///
/// An item in the body layer that hangs off `#/furniture` is body content
/// no body walk can reach, which is the shape of a fold that decides the
/// layer in one place and the parent in another.
pub fn assert_layers_and_parents_agree(document: &doc::Document) {
    let body_layer = doc::ContentLayer::Body as i32;
    let reached = reachable_from_body(document);
    for item in placed(document) {
        let root = root_of(document, &item.self_ref)
            .unwrap_or_else(|| panic!("{} hangs off nothing: {:?}", item.self_ref, item.text));
        let expected = if item.layer == body_layer {
            BODY
        } else {
            FURNITURE
        };
        assert_eq!(
            root, expected,
            "{} declares layer {} and hangs under {root}: {:?}",
            item.self_ref, item.layer, item.text
        );
        assert_eq!(
            reached.contains(&item.self_ref),
            item.layer == body_layer,
            "{} declares layer {} and the body walk {} it: {:?}",
            item.self_ref,
            item.layer,
            if reached.contains(&item.self_ref) {
                "reaches"
            } else {
                "does not reach"
            },
            item.text
        );
    }
}
