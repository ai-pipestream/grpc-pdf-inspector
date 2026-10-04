// SPDX-License-Identifier: Apache-2.0

//! Landscape pages in the frame a reader sees.
//!
//! A page whose `/Rotate` is 90 is displayed turned a quarter clockwise, and
//! producers that lay out a landscape page that way (Acrobat Distiller's
//! landscape output is the common case) draw their text at 90 degrees in
//! user space. The library notices that text, and so that its layout engine
//! reads it as horizontal it swaps the page's runs, rectangles and lines
//! into a landscape frame: x becomes the user-space y and y becomes the
//! negated user-space x. That keeps the reading order and drops the
//! translation, so every y on such a page is negative and every box lands
//! off the page.
//!
//! The displayed page is that frame moved onto the sheet. For a crop box
//! `[x0, y0, x1, y1]` turned a quarter clockwise, a user-space point `(X, Y)`
//! is shown at `(Y - y0, x1 - X)` on a `(y1 - y0) x (x1 - x0)` page, so the
//! library's `(Y, -X)` needs only `(-y0, +x1)` added. The order the library
//! read the page in is untouched, because a translation moves every run
//! together.
//!
//! Only what goes on the wire is moved. The library's markdown renderer,
//! its column detector, the table detectors and the chrome verdicts all
//! keep the frame they were built against, so the page reads exactly as it
//! did; the boxes on the runs, the grids and the runs reported beside the
//! markdown are moved as they are emitted. Form XObject placements come
//! back swapped into the same frame as the runs (the library turns them
//! with its rectangles), so they too need only the translation.
//!
//! Only `/Rotate 90` is moved. A page drawn at 90 degrees under any other
//! rotation is not a pure translation of the library's frame (under
//! `/Rotate 270` it is a half turn, which would also reverse the order the
//! library read it in), and a page whose text was not drawn turned keeps
//! user-space boxes; both are left as the library returned them.

use std::collections::{BTreeMap, BTreeSet};

use pdf_inspector::PdfForm;

use crate::proto::v1 as pb;

/// A page's crop box corners, in user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropBox {
    /// Left edge.
    pub x0: f32,
    /// Bottom edge.
    pub y0: f32,
    /// Right edge.
    pub x1: f32,
    /// Top edge.
    pub y1: f32,
}

impl CropBox {
    /// The crop box of a metadata page, falling back to the media box.
    #[must_use]
    pub fn of(page: &pb::PageGeometry) -> Option<Self> {
        let rect = page.crop_box.as_ref().or(page.media_box.as_ref())?;
        #[allow(clippy::cast_possible_truncation)]
        Some(Self {
            x0: rect.x as f32,
            y0: rect.y as f32,
            x1: (rect.x + rect.width) as f32,
            y1: (rect.y + rect.height) as f32,
        })
    }
}

/// The pages whose library frame can be moved onto the displayed page:
/// drawn at 90 degrees, shown under `/Rotate 90`, with a known crop box.
#[must_use]
pub fn movable_pages(
    rotated_pages: &BTreeSet<u32>,
    geometry: &[pb::PageGeometry],
) -> BTreeMap<u32, CropBox> {
    geometry
        .iter()
        .filter(|page| page.rotation == 90 && rotated_pages.contains(&page.page_no))
        .filter_map(|page| CropBox::of(page).map(|crop| (page.page_no, crop)))
        .collect()
}

/// Move the form placements on `pages` from the library's frame onto the
/// displayed page.
///
/// The library already swaps a turned page's forms with its rectangles
/// (`x = Y`, `y = -(X + width)`, width and height exchanged), so a form
/// arrives here in the same frame as the runs and takes the same
/// translation. Applying the whole user-space turn here a second time put
/// every form on a landscape page off the sheet.
pub fn place_forms(pages: &BTreeMap<u32, CropBox>, forms: &mut [PdfForm]) {
    for form in forms {
        if let Some(crop) = pages.get(&form.page) {
            form.x -= crop.y0;
            form.y += crop.x1;
        }
    }
}

/// Move one box from the library's frame onto the displayed page.
pub fn place_rect(crop: CropBox, rect: &mut pb::Rect) {
    rect.x -= f64::from(crop.y0);
    rect.y += f64::from(crop.x1);
}

/// Move a page's runs from the library's frame onto the displayed page.
pub fn place_spans(crop: CropBox, spans: &mut [pb::TextSpan]) {
    for rect in spans.iter_mut().filter_map(|span| span.bbox.as_mut()) {
        place_rect(crop, rect);
    }
}

/// Move a page's grids from the library's frame onto the displayed page:
/// their extents, their column edges (x) and their row edges (y).
pub fn place_tables(crop: CropBox, tables: &mut pb::PageTables) {
    for table in &mut tables.tables {
        if let Some(rect) = table.bbox.as_mut() {
            place_rect(crop, rect);
        }
        for column in &mut table.column_boundaries {
            *column -= f64::from(crop.y0);
        }
        for row in &mut table.row_boundaries {
            *row += f64::from(crop.x1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER: CropBox = CropBox {
        x0: 0.0,
        y0: 0.0,
        x1: 612.0,
        y1: 792.0,
    };

    fn geometry(page_no: u32, rotation: u32) -> pb::PageGeometry {
        pb::PageGeometry {
            page_no,
            crop_box: Some(pb::Rect {
                x: 0.0,
                y: 0.0,
                width: 612.0,
                height: 792.0,
            }),
            rotation,
            ..Default::default()
        }
    }

    #[test]
    fn only_turned_text_under_rotate_90_is_movable() {
        let rotated = BTreeSet::from([1, 2, 3]);
        let pages = movable_pages(
            &rotated,
            &[geometry(1, 90), geometry(2, 270), geometry(4, 90)],
        );
        assert_eq!(pages.keys().copied().collect::<Vec<_>>(), [1]);
        assert_eq!(pages[&1], LETTER);
    }

    #[test]
    fn a_run_in_the_library_frame_lands_where_the_page_shows_it() {
        // Distiller's first line on a landscape court opinion: the text
        // matrix puts it at user space (60.52, 54.04), and the page shows it
        // 54 points from the left and 60 points from the top of a 792 x 612
        // page, which is y = 612 - 60.52 from the bottom.
        let mut spans = vec![pb::TextSpan {
            text: "UNITED".to_owned(),
            bbox: Some(pb::Rect {
                x: 54.04,
                y: -60.52,
                width: 40.0,
                height: 12.0,
            }),
            ..Default::default()
        }];
        place_spans(LETTER, &mut spans);
        let rect = spans[0].bbox.as_ref().expect("a box");
        assert!((rect.x - 54.04).abs() < 1e-3);
        assert!((rect.y - 551.48).abs() < 1e-3);
    }

    #[test]
    fn a_grid_moves_with_its_edges() {
        let mut tables = pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: -300.0,
                    width: 200.0,
                    height: 100.0,
                }),
                column_boundaries: vec![100.0, 200.0],
                row_boundaries: vec![-220.0, -260.0],
                ..Default::default()
            }],
        };
        place_tables(LETTER, &mut tables);
        let table = &tables.tables[0];
        assert_eq!(table.bbox.as_ref().map(|rect| rect.y), Some(312.0));
        assert_eq!(table.column_boundaries, [100.0, 200.0]);
        assert_eq!(table.row_boundaries, [392.0, 352.0]);
    }

    #[test]
    fn a_form_in_the_library_frame_lands_where_the_page_shows_it() {
        // A 50 x 30 form drawn at user space (100, 200) on a letter sheet
        // shown under /Rotate 90. The library hands it over already turned
        // into its frame, as (200, -150) with the sides exchanged, exactly
        // as it hands over the runs; the page shows it 200 points from the
        // left with its bottom edge 612 - 150 = 462 points up.
        let mut forms = vec![PdfForm {
            name: "Fm0".to_owned(),
            x: 200.0,
            y: -150.0,
            width: 30.0,
            height: 50.0,
            page: 1,
        }];
        let pages = BTreeMap::from([(1, LETTER)]);
        place_forms(&pages, &mut forms);
        let form = &forms[0];
        assert_eq!(
            (form.x, form.y, form.width, form.height),
            (200.0, 462.0, 30.0, 50.0)
        );
    }

    #[test]
    fn forms_on_other_pages_are_left_alone() {
        let mut forms = vec![PdfForm {
            name: "Fm0".to_owned(),
            x: 72.0,
            y: 700.0,
            width: 10.0,
            height: 10.0,
            page: 2,
        }];
        place_forms(&BTreeMap::from([(1, LETTER)]), &mut forms);
        assert_eq!((forms[0].x, forms[0].y), (72.0, 700.0));
    }
}
