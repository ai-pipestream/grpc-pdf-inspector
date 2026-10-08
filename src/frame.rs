// SPDX-License-Identifier: Apache-2.0

//! Boxes onto the page a reader sees.
//!
//! Every box on this wire is measured against the displayed page: the
//! crop box, turned by the page's `/Rotate`, with the origin at the
//! displayed page's bottom-left corner. That is the only frame in which a
//! box and the page size the fold reports beside it agree, because the
//! file itself measures neither of them that way:
//!
//! - User space starts at the media box's origin, and a crop box need not.
//!   A page cropped to `[36 0 432 396]` is shown 396 points wide with its
//!   left edge at user-space x = 36, so every box moves by the crop
//!   box's corner.
//! - `/Rotate 90` shows the sheet turned a quarter clockwise, `/Rotate
//!   270` a quarter anticlockwise, `/Rotate 180` upside down. For a crop
//!   box `[x0, y0, x1, y1]` a user-space point `(X, Y)` is shown at
//!   `(Y - y0, x1 - X)`, `(y1 - Y, X - x0)` and `(x1 - X, y1 - Y)`
//!   respectively, on a page whose sides are exchanged for the quarter
//!   turns.
//! - A page whose text is drawn at 90 degrees in user space (Distiller's
//!   landscape output is the common case) has its runs, rectangles, lines
//!   and form placements swapped by the library into a landscape frame of
//!   its own, so that its layout engine reads the text as horizontal: x
//!   becomes the user-space y and y the negated user-space x, with no
//!   translation. Such a page's runs carry their user-space hull beside
//!   the frame coordinates; its rectangles, grids and forms are turned
//!   back here (`X = -y`, `Y = x`) before they are placed.
//!
//! What is placed is what goes on the wire. The library's markdown
//! renderer, its column detector, the table detectors and the chrome
//! verdicts all keep the frame they were built against, so the page reads
//! exactly as it did; only the boxes move.
//!
//! A run drawn across the page's edge is clipped to the page: its box is
//! where a reader sees it, and what hangs off the sheet is seen by no one.
//! A run drawn wholly off the page never arrives here, because the library
//! drops it (`extractor::extract_all_pages_text`); a placement that still
//! lands entirely off the page collapses to a zero-area box at the edge.

use std::collections::{BTreeMap, BTreeSet};

use pdf_inspector::{PageBox, PdfForm, TextItem};

use crate::proto::v1 as pb;

/// One page's displayed frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageFrame {
    /// The crop box's corners in user space: left, bottom, right, top.
    crop: [f64; 4],
    /// The page's `/Rotate`, one of 0, 90, 180 and 270.
    rotation: u32,
    /// Whether the library swapped this page's geometry into its landscape
    /// frame.
    turned: bool,
}

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
        let corners = |rect: &pb::Rect| {
            #[allow(clippy::cast_possible_truncation)]
            [
                rect.x as f32,
                rect.y as f32,
                (rect.x + rect.width) as f32,
                (rect.y + rect.height) as f32,
            ]
        };
        Self::visible(
            page.media_box.as_ref().map(corners),
            page.crop_box.as_ref().map(corners),
        )
    }

    /// The visible box from a media box and a crop box, either of which
    /// may be missing.
    ///
    /// A crop box reaching past the media box is legal, and viewers show
    /// only the overlap, so the two are intersected when both are known.
    #[must_use]
    pub fn visible(media: Option<[f32; 4]>, crop: Option<[f32; 4]>) -> Option<Self> {
        let valid =
            |[x0, y0, x1, y1]: [f32; 4]| (x1 > x0 && y1 > y0).then_some(Self { x0, y0, x1, y1 });
        let media = media.and_then(valid);
        let crop = crop.and_then(valid);
        match (crop, media) {
            (Some(crop), Some(media)) => {
                let clipped = Self {
                    x0: crop.x0.max(media.x0),
                    y0: crop.y0.max(media.y0),
                    x1: crop.x1.min(media.x1),
                    y1: crop.y1.min(media.y1),
                };
                Some(if clipped.x1 > clipped.x0 && clipped.y1 > clipped.y0 {
                    clipped
                } else {
                    media
                })
            }
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        }
    }
}

impl PageFrame {
    /// The frame of a metadata page, `turned` when the library swapped the
    /// page's geometry. `None` for a page with no usable box, which is
    /// left as the library returned it.
    #[must_use]
    pub fn new(page: &pb::PageGeometry, turned: bool) -> Option<Self> {
        Self::from_crop(CropBox::of(page)?, page.rotation, turned)
    }

    /// The frame of a page the extraction walk measured.
    #[must_use]
    pub fn of_box(page: &PageBox, turned: bool) -> Option<Self> {
        Self::from_crop(
            CropBox::visible(page.media_box, page.crop_box)?,
            page.rotation,
            turned,
        )
    }

    fn from_crop(crop: CropBox, rotation: u32, turned: bool) -> Option<Self> {
        Some(Self {
            crop: [
                f64::from(crop.x0),
                f64::from(crop.y0),
                f64::from(crop.x1),
                f64::from(crop.y1),
            ],
            rotation: match rotation % 360 {
                90 => 90,
                180 => 180,
                270 => 270,
                _ => 0,
            },
            turned,
        })
    }

    /// The frames of every measured page, `turned` naming the pages the
    /// library swapped.
    #[must_use]
    pub fn of_pages(
        boxes: &BTreeMap<u32, PageBox>,
        turned: &BTreeSet<u32>,
    ) -> BTreeMap<u32, PageFrame> {
        boxes
            .iter()
            .filter_map(|(page_no, page)| {
                Self::of_box(page, turned.contains(page_no)).map(|frame| (*page_no, frame))
            })
            .collect()
    }

    /// The displayed page's width and height.
    #[must_use]
    pub fn size(&self) -> (f64, f64) {
        let (width, height) = (self.crop[2] - self.crop[0], self.crop[3] - self.crop[1]);
        if self.rotation == 90 || self.rotation == 270 {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// Where a user-space point is shown.
    fn display(&self, x: f64, y: f64) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.crop;
        match self.rotation {
            90 => (y - y0, x1 - x),
            180 => (x1 - x, y1 - y),
            270 => (y1 - y, x - x0),
            _ => (x - x0, y - y0),
        }
    }

    /// A point of the library's frame back in user space.
    fn user(&self, x: f64, y: f64) -> (f64, f64) {
        if self.turned { (-y, x) } else { (x, y) }
    }

    /// The displayed box of a user-space hull `[x0, y0, x1, y1]`, clipped
    /// to the page.
    #[must_use]
    pub fn place_hull(&self, hull: [f64; 4]) -> pb::Rect {
        let a = self.display(hull[0], hull[1]);
        let b = self.display(hull[2], hull[3]);
        self.clip(rect_between(a, b))
    }

    /// The displayed box of a box in the library's frame, clipped to the
    /// page.
    #[must_use]
    pub fn place_rect(&self, rect: &pb::Rect) -> pb::Rect {
        let a = self.user(rect.x, rect.y);
        let b = self.user(rect.x + rect.width, rect.y + rect.height);
        self.place_hull([a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)])
    }

    /// The displayed box of a run: its user-space hull when the library
    /// measured one, else its frame box.
    #[must_use]
    pub fn place_item(&self, item: &TextItem) -> pb::Rect {
        match item.hull {
            Some(hull) => self.place_hull([
                f64::from(hull[0]),
                f64::from(hull[1]),
                f64::from(hull[2]),
                f64::from(hull[3]),
            ]),
            None => self.place_rect(&raw_rect(item.x, item.y, item.width, item.height)),
        }
    }

    /// The displayed box of a form placement. The library swaps a turned
    /// page's forms with its rectangles, so a form is in the same frame as
    /// the runs and takes the same turn back.
    #[must_use]
    pub fn place_form(&self, form: &PdfForm) -> pb::Rect {
        self.place_rect(&raw_rect(form.x, form.y, form.width, form.height))
    }

    /// Move a page's grids onto the displayed page: their extents, their
    /// column edges, their row edges and their cells.
    ///
    /// The wire's lists are the contract's: `column_boundaries` says where
    /// each column starts, ascending, the last column closed by the
    /// extent's right edge; `row_boundaries` says where each row's band
    /// ends at the bottom, descending, the first row closed by the
    /// extent's top. Neither is a bare list of lines: a turn that mirrors
    /// an axis makes a start edge an end edge, and a quarter turn makes a
    /// column edge a row edge. So each column and each row is taken as
    /// the band between two edges in the frame the grid was read in, each
    /// cell as the crossing of its column's band and its row's, every edge
    /// is placed, and the lists and the cells are rebuilt from where the
    /// bands lie on the displayed page.
    ///
    /// On a page the library read sideways and shown under `/Rotate 270`
    /// its frame is a half turn from the page: the text runs towards
    /// smaller x in that frame, so a column's start is the far end of its
    /// glyphs and its band reaches back to the previous start, and a row's
    /// bottom is its top. The bands are cut that way there.
    pub fn place_tables(&self, tables: &mut pb::PageTables) {
        let reversed = self.turned && self.rotation == 270;
        for table in &mut tables.tables {
            let extent = table.bbox;
            let starts = table.column_boundaries.clone();
            let bottoms = table.row_boundaries.clone();
            // The edges closing the last column and the first row (the
            // first column and the last row on a reversed page): the
            // extent's, unless the list already reaches past it (a ruled
            // table reports its outer rule as one more edge, and the
            // extent is measured from the runs inside it).
            let far_x = extent
                .as_ref()
                .map(|rect| rect.x + rect.width)
                .into_iter()
                .chain(starts.last().copied())
                .reduce(f64::max);
            let far_y = extent
                .as_ref()
                .map(|rect| rect.y + rect.height)
                .into_iter()
                .chain(bottoms.first().copied())
                .reduce(f64::max);
            let near_x = extent
                .as_ref()
                .map(|rect| rect.x)
                .into_iter()
                .chain(starts.first().copied())
                .reduce(f64::min);
            let near_y = extent
                .as_ref()
                .map(|rect| rect.y)
                .into_iter()
                .chain(bottoms.last().copied())
                .reduce(f64::min);
            let column_band = |index: usize| -> Option<(f64, f64)> {
                let start = *starts.get(index)?;
                let other = if reversed {
                    index
                        .checked_sub(1)
                        .and_then(|previous| starts.get(previous).copied())
                        .or(near_x)
                } else {
                    starts.get(index + 1).copied().or(far_x)
                };
                Some((start, other.unwrap_or(start)))
            };
            let row_band = |index: usize| -> Option<(f64, f64)> {
                let bottom = *bottoms.get(index)?;
                let other = if reversed {
                    bottoms.get(index + 1).copied().or(near_y)
                } else {
                    index
                        .checked_sub(1)
                        .and_then(|previous| bottoms.get(previous).copied())
                        .or(far_y)
                };
                Some((bottom, other.unwrap_or(bottom)))
            };
            // A list that already names its outer edge (a ruled table's)
            // is a list of fences, and every fence is kept: both ends of
            // every band go in, so a mirrored axis keeps the far fence.
            let fences_x = !reversed
                && far_x.is_some_and(|far| starts.last().is_some_and(|last| *last >= far - 1e-6));
            let fences_y = !reversed
                && far_y
                    .is_some_and(|far| bottoms.first().is_some_and(|first| *first >= far - 1e-6));
            let mut columns: Vec<f64> = Vec::new();
            let mut rows: Vec<f64> = Vec::new();
            let mut land = |near: Edge, far: Edge, fences: bool| {
                let (axis, a) = self.place_edge(near);
                let (_, b) = self.place_edge(far);
                let list = match axis {
                    Axis::Column => &mut columns,
                    Axis::Row => &mut rows,
                };
                list.push(a.min(b));
                if fences {
                    list.push(a.max(b));
                }
            };
            for index in 0..starts.len() {
                let (a, b) = column_band(index).expect("an index of the list");
                land(Edge::X(a), Edge::X(b), fences_x);
            }
            for index in 0..bottoms.len() {
                let (a, b) = row_band(index).expect("an index of the list");
                land(Edge::Y(a), Edge::Y(b), fences_y);
            }
            columns.sort_by(f64::total_cmp);
            columns.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            rows.sort_by(|a, b| b.total_cmp(a));
            rows.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            table.column_boundaries = columns;
            table.row_boundaries = rows;
            // Each cell is the crossing of its bands, cut in the frame the
            // grid was read in and placed with it, so it keeps its text
            // whichever way the page is shown. A table with no extent
            // places no cells.
            for (row_index, row) in table.rows.iter_mut().enumerate() {
                row.boxes = if extent.is_some() {
                    (0..row.cells.len())
                        .filter_map(|column| {
                            let (x0, x1) = column_band(column)?;
                            let (y0, y1) = row_band(row_index)?;
                            Some(self.place_rect(&pb::Rect {
                                x: x0.min(x1),
                                y: y0.min(y1),
                                width: (x1 - x0).abs(),
                                height: (y1 - y0).abs(),
                            }))
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            }
            if let Some(rect) = table.bbox.as_mut() {
                *rect = self.place_rect(rect);
            }
        }
    }

    /// Where a line of constant x or y in the library's frame lands: on
    /// which displayed axis (a column edge is a line of constant displayed
    /// x, a row edge of constant displayed y) and at what value.
    fn place_edge(&self, edge: Edge) -> (Axis, f64) {
        // In user space the line is X = c or Y = c.
        let (vertical, c) = match (edge, self.turned) {
            (Edge::X(c), false) => (true, c),
            (Edge::Y(c), false) => (false, c),
            // The library's x is the user-space Y, its y the negated X.
            (Edge::X(c), true) => (false, c),
            (Edge::Y(c), true) => (true, -c),
        };
        let [x0, y0, x1, y1] = self.crop;
        let quarter = self.rotation == 90 || self.rotation == 270;
        let value = match (vertical, self.rotation) {
            (true, 90) => x1 - c,
            (true, 180) => x1 - c,
            (true, 270) => c - x0,
            (true, _) => c - x0,
            (false, 90) => c - y0,
            (false, 180) => y1 - c,
            (false, 270) => y1 - c,
            (false, _) => c - y0,
        };
        let axis = if vertical != quarter {
            Axis::Column
        } else {
            Axis::Row
        };
        (axis, value)
    }

    /// `rect` cut down to the displayed page. A box with nothing on the
    /// page collapses to a zero-area box at the nearest point of the edge.
    fn clip(&self, rect: pb::Rect) -> pb::Rect {
        let (width, height) = self.size();
        let x0 = rect.x.clamp(0.0, width);
        let x1 = (rect.x + rect.width).clamp(0.0, width);
        let y0 = rect.y.clamp(0.0, height);
        let y1 = (rect.y + rect.height).clamp(0.0, height);
        pb::Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

/// A line of constant x or y in the library's frame.
#[derive(Clone, Copy)]
enum Edge {
    X(f64),
    Y(f64),
}

/// The displayed axis an edge lies across.
#[derive(Clone, Copy)]
enum Axis {
    Column,
    Row,
}

/// The box with `a` and `b` as opposite corners.
fn rect_between(a: (f64, f64), b: (f64, f64)) -> pb::Rect {
    pb::Rect {
        x: a.0.min(b.0),
        y: a.1.min(b.1),
        width: (a.0 - b.0).abs(),
        height: (a.1 - b.1).abs(),
    }
}

/// A box as the library gave it, with a negative extent turned into an
/// origin and a non-negative extent so a consumer never has to wonder
/// which corner `x`/`y` names.
#[must_use]
pub fn raw_rect(x: f32, y: f32, width: f32, height: f32) -> pb::Rect {
    let (x, width) = normalize(x, width);
    let (y, height) = normalize(y, height);
    pb::Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(width),
        height: f64::from(height),
    }
}

/// Turn an origin and a possibly-negative extent into an origin and a
/// non-negative extent.
fn normalize(origin: f32, extent: f32) -> (f32, f32) {
    if extent < 0.0 {
        (origin + extent, -extent)
    } else {
        (origin, extent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(page_no: u32, rotation: u32, crop: [f64; 4]) -> pb::PageGeometry {
        let rect = pb::Rect {
            x: crop[0],
            y: crop[1],
            width: crop[2] - crop[0],
            height: crop[3] - crop[1],
        };
        pb::PageGeometry {
            page_no,
            media_box: Some(rect),
            crop_box: Some(rect),
            rotation,
            ..Default::default()
        }
    }

    const LETTER: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

    fn frame(rotation: u32, crop: [f64; 4], turned: bool) -> PageFrame {
        PageFrame::new(&geometry(1, rotation, crop), turned).expect("a measured page")
    }

    fn close(rect: &pb::Rect, x: f64, y: f64, width: f64, height: f64) -> bool {
        (rect.x - x).abs() < 1e-3
            && (rect.y - y).abs() < 1e-3
            && (rect.width - width).abs() < 1e-3
            && (rect.height - height).abs() < 1e-3
    }

    fn item(hull: Option<[f32; 4]>, x: f32, y: f32, width: f32, height: f32) -> TextItem {
        TextItem {
            hull,
            text: "run".to_owned(),
            x,
            y,
            width,
            height,
            font: String::new(),
            font_tag: String::new(),
            font_size: height,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: pdf_inspector::types::ItemType::Text,
            mcid: None,
        }
    }

    #[test]
    fn pages_are_measured_as_shown() {
        assert_eq!(frame(0, LETTER, false).size(), (612.0, 792.0));
        assert_eq!(frame(90, LETTER, false).size(), (792.0, 612.0));
        assert_eq!(frame(180, LETTER, false).size(), (612.0, 792.0));
        assert_eq!(frame(270, LETTER, false).size(), (792.0, 612.0));
        assert_eq!(
            frame(0, [36.0, 0.0, 432.0, 396.0], false).size(),
            (396.0, 396.0),
            "a cropped page is its crop box"
        );
    }

    #[test]
    fn a_crop_box_moves_the_origin() {
        // The crop box `[36 0 432 396]`: a run at user space x = 100 is
        // 64 points from the left edge a reader sees.
        let frame = frame(0, [36.0, 0.0, 432.0, 396.0], false);
        let rect = frame.place_item(&item(
            Some([100.0, 50.0, 140.0, 62.0]),
            100.0,
            50.0,
            40.0,
            12.0,
        ));
        assert!(close(&rect, 64.0, 50.0, 40.0, 12.0), "{rect:?}");
    }

    #[test]
    fn a_crop_box_wider_than_the_sheet_is_the_sheet() {
        let page = pb::PageGeometry {
            page_no: 1,
            media_box: Some(pb::Rect {
                x: 0.0,
                y: 0.0,
                width: 612.0,
                height: 792.0,
            }),
            crop_box: Some(pb::Rect {
                x: -100.0,
                y: -100.0,
                width: 1000.0,
                height: 1000.0,
            }),
            ..Default::default()
        };
        assert_eq!(
            PageFrame::new(&page, false).map(|frame| frame.size()),
            Some((612.0, 792.0))
        );
    }

    #[test]
    fn a_run_in_the_library_frame_lands_where_the_page_shows_it() {
        // Distiller's first line on a landscape court opinion: the text
        // matrix puts it at user space (60.52, 54.04), and the page shows it
        // 54 points from the left and 60 points from the top of a 792 x 612
        // page, which is y = 612 - 60.52 from the bottom. The library hands
        // it over swapped, as (54.04, -60.52), with no hull measured.
        let frame = frame(90, LETTER, true);
        let rect = frame.place_item(&item(None, 54.04, -60.52, 40.0, 12.0));
        assert!((rect.x - 54.04).abs() < 1e-3, "{rect:?}");
        assert!((rect.y - 551.48).abs() < 1e-3, "{rect:?}");
    }

    #[test]
    fn a_turned_run_is_placed_by_its_hull() {
        // The same line, measured: drawn with `0 11 -11 0 60.52 54.04 Tm`,
        // its 40 points of advance run up the sheet and its 11 points of
        // height run left, so the hull is 49.52..60.52 by 54.04..94.04 in
        // user space. Shown turned, that is 40 wide and 11 tall with its
        // bottom-left corner at (54.04, 612 - 60.52).
        let frame = frame(90, LETTER, true);
        let rect = frame.place_item(&item(
            Some([49.52, 54.04, 60.52, 94.04]),
            54.04,
            -60.52,
            40.0,
            11.0,
        ));
        assert!(close(&rect, 54.04, 551.48, 40.0, 11.0), "{rect:?}");
    }

    #[test]
    fn an_upright_run_on_a_turned_page_runs_down_the_shown_page() {
        // A running head drawn upright in user space on a /Rotate 90 page
        // (`1 0 0 1 442.95 801.41 Tm`, 70 points wide, 8 tall): shown, it
        // reads downwards along the right edge, 8 wide and 70 tall, with
        // its box starting 801.41 from the left and ending at 612 - 442.95
        // from the bottom. The library's frame put its width along x and
        // sent it 29 points off the page.
        let frame = frame(90, [0.0, 0.0, 595.28, 841.89], true);
        let rect = frame.place_item(&item(
            Some([442.95, 801.41, 512.95, 809.41]),
            801.41,
            -442.95,
            70.0,
            8.0,
        ));
        assert!(close(&rect, 801.41, 595.28 - 512.95, 8.0, 70.0), "{rect:?}");
    }

    #[test]
    fn a_page_turned_the_other_way_is_a_half_turn_of_the_library_frame() {
        // /Rotate 270 with text drawn reading down the sheet
        // (`0 -11.04 11.04 0 511.92 762.12 Tm`, 60.5 points long): shown,
        // the page is 792 x 612 and the run starts 29.88 from the left
        // with its baseline 511.92 up. In user space the run covers
        // 511.92..522.96 by 701.62..762.12.
        let frame = frame(270, LETTER, true);
        let rect = frame.place_item(&item(
            Some([511.92, 701.62, 522.96, 762.12]),
            762.12,
            -511.92,
            71.8,
            11.04,
        ));
        assert!(close(&rect, 29.88, 511.92, 60.5, 11.04), "{rect:?}");
    }

    #[test]
    fn an_upside_down_page_is_mirrored_both_ways() {
        let frame = frame(180, LETTER, false);
        let rect = frame.place_item(&item(
            Some([100.0, 700.0, 200.0, 712.0]),
            100.0,
            700.0,
            100.0,
            12.0,
        ));
        assert!(close(&rect, 412.0, 80.0, 100.0, 12.0), "{rect:?}");
    }

    #[test]
    fn a_run_across_the_edge_is_clipped_and_one_off_the_page_collapses() {
        let frame = frame(0, LETTER, false);
        let rect = frame.place_item(&item(
            Some([602.0, 600.0, 648.0, 610.0]),
            602.0,
            600.0,
            46.0,
            10.0,
        ));
        assert!(close(&rect, 602.0, 600.0, 10.0, 10.0), "{rect:?}");
        let rect = frame.place_item(&item(
            Some([218.0, -14.0, 232.0, -8.0]),
            218.0,
            -14.0,
            14.0,
            6.0,
        ));
        assert!(close(&rect, 218.0, 0.0, 14.0, 0.0), "{rect:?}");
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
        frame(90, LETTER, true).place_tables(&mut tables);
        let table = &tables.tables[0];
        assert!(close(
            table.bbox.as_ref().expect("a box"),
            100.0,
            312.0,
            200.0,
            100.0
        ));
        assert_eq!(table.column_boundaries, [100.0, 200.0]);
        assert_eq!(table.row_boundaries, [392.0, 352.0]);
    }

    #[test]
    fn a_grid_drawn_upright_on_a_turned_sheet_exchanges_its_edges() {
        // A table drawn upright in user space on a /Rotate 90 page: its
        // column edges (constant X) are shown as rows, its row edges as
        // columns.
        let mut tables = pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: 500.0,
                    width: 200.0,
                    height: 100.0,
                }),
                column_boundaries: vec![100.0, 200.0],
                row_boundaries: vec![550.0, 500.0],
                ..Default::default()
            }],
        };
        frame(90, LETTER, false).place_tables(&mut tables);
        let table = &tables.tables[0];
        assert!(close(
            table.bbox.as_ref().expect("a box"),
            500.0,
            312.0,
            100.0,
            200.0
        ));
        // The row bands 550..600 and 500..550 lie across displayed x and
        // start at 500 and 550; the column bands 100..200 and 200..300 lie
        // across displayed y as 412..512 and 312..412, ending at 412 and
        // 312. Each list keeps the contract's order and meaning, so a
        // consumer closing the last column with the extent's right edge
        // (600) and the first row with its top (512) gets every cell.
        assert_eq!(table.column_boundaries, [500.0, 550.0]);
        assert_eq!(table.row_boundaries, [412.0, 312.0]);
    }

    #[test]
    fn a_turned_grid_shown_anticlockwise_keeps_its_columns_ascending() {
        // The library's frame for a /Rotate 270 page is a half turn from
        // the shown page, so a column's start edge becomes its end edge.
        // Columns starting at 100 and 200 in an extent reaching 300 are
        // shown as bands 592..692 and 492..592: starts 492 and 592, with
        // the extent's right edge (692) closing the last.
        let mut tables = pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: -300.0,
                    width: 200.0,
                    height: 100.0,
                }),
                column_boundaries: vec![200.0, 300.0],
                row_boundaries: vec![-220.0, -260.0],
                rows: vec![pb::TableCells {
                    cells: vec!["a".to_owned(), "b".to_owned()],
                    boxes: Vec::new(),
                }],
                ..Default::default()
            }],
        };
        frame(270, LETTER, true).place_tables(&mut tables);
        let table = &tables.tables[0];
        let rect = table.bbox.as_ref().expect("a box");
        assert!(close(rect, 492.0, 200.0, 200.0, 100.0), "{rect:?}");
        // The frame is a half turn from the page: a column's start (200,
        // 300) is the far end of its glyphs, so the bands are 100..200 and
        // 200..300, shown as 592..692 and 492..592.
        assert_eq!(table.column_boundaries, [492.0, 592.0]);
        // Rows ending at -220 and -260 are the tops of bands reaching to
        // the next bottom (-260) and the extent's bottom (-300): user
        // space x 220..260 and 260..300, shown as y 220..260 and 260..300,
        // descending bottoms 260 and 220.
        assert_eq!(table.row_boundaries, [260.0, 220.0]);
        let boxes = &table.rows[0].boxes;
        assert!(close(&boxes[0], 592.0, 220.0, 100.0, 40.0), "{boxes:?}");
        assert!(close(&boxes[1], 492.0, 220.0, 100.0, 40.0), "{boxes:?}");
    }

    #[test]
    fn an_upside_down_grid_rebuilds_both_lists_from_the_far_edges() {
        let mut tables = pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: 500.0,
                    width: 200.0,
                    height: 100.0,
                }),
                column_boundaries: vec![100.0, 200.0],
                row_boundaries: vec![550.0, 500.0],
                ..Default::default()
            }],
        };
        frame(180, LETTER, false).place_tables(&mut tables);
        let table = &tables.tables[0];
        let rect = table.bbox.as_ref().expect("a box");
        assert!(close(rect, 312.0, 192.0, 200.0, 100.0), "{rect:?}");
        assert_eq!(table.column_boundaries, [312.0, 412.0]);
        assert_eq!(table.row_boundaries, [242.0, 192.0]);
    }

    #[test]
    fn a_ruled_grid_keeps_its_outer_fence_when_mirrored() {
        // A ruled table names its outer edges: three fences for two
        // columns, three for two rows. Upside down, every fence is still
        // there, mirrored.
        let mut tables = pb::PageTables {
            page_no: 1,
            tables: vec![pb::TableRegion {
                bbox: Some(pb::Rect {
                    x: 100.0,
                    y: 500.0,
                    width: 200.0,
                    height: 100.0,
                }),
                column_boundaries: vec![100.0, 200.0, 300.0],
                row_boundaries: vec![550.0, 500.0],
                rows: vec![pb::TableCells {
                    cells: vec!["a".to_owned(), "b".to_owned()],
                    boxes: Vec::new(),
                }],
                ..Default::default()
            }],
        };
        frame(180, LETTER, false).place_tables(&mut tables);
        let table = &tables.tables[0];
        assert_eq!(table.column_boundaries, [312.0, 412.0, 512.0]);
        assert_eq!(table.row_boundaries, [242.0, 192.0]);
        // The cells turn with their text: the first cell, top left as
        // read, is bottom right as shown.
        let boxes = &table.rows[0].boxes;
        assert!(close(&boxes[0], 412.0, 192.0, 100.0, 50.0), "{boxes:?}");
        assert!(close(&boxes[1], 312.0, 192.0, 100.0, 50.0), "{boxes:?}");
    }

    #[test]
    fn a_link_annotation_on_a_turned_page_lands_where_the_page_shows_it() {
        // A link's rectangle is user space whatever the library did to the
        // page's runs, and the library marks it so with a hull. /Rect
        // [100 200 150 210] on a letter sheet shown under /Rotate 90 is
        // 200..210 from the left and 612 - 150 .. 612 - 100 up.
        let frame = frame(90, LETTER, true);
        let mut link = item(Some([100.0, 200.0, 150.0, 210.0]), 100.0, 200.0, 50.0, 10.0);
        link.item_type = pdf_inspector::types::ItemType::Link("https://example.invalid".to_owned());
        let rect = frame.place_item(&link);
        assert!(close(&rect, 200.0, 462.0, 10.0, 50.0), "{rect:?}");
    }

    #[test]
    fn a_form_in_the_library_frame_lands_where_the_page_shows_it() {
        // A 50 x 30 form drawn at user space (100, 200) on a letter sheet
        // shown under /Rotate 90. The library hands it over already turned
        // into its frame, as (200, -150) with the sides exchanged, exactly
        // as it hands over the runs; the page shows it 200 points from the
        // left with its bottom edge 612 - 150 = 462 points up.
        let form = PdfForm {
            name: "Fm0".to_owned(),
            x: 200.0,
            y: -150.0,
            width: 30.0,
            height: 50.0,
            page: 1,
        };
        let rect = frame(90, LETTER, true).place_form(&form);
        assert!(close(&rect, 200.0, 462.0, 30.0, 50.0), "{rect:?}");
    }

    #[test]
    fn only_measured_pages_get_a_frame() {
        let frames = PageFrame::of_pages(
            &BTreeMap::from([
                (
                    1,
                    PageBox {
                        media_box: Some([0.0, 0.0, 612.0, 792.0]),
                        crop_box: None,
                        rotation: 90,
                    },
                ),
                (2, PageBox::default()),
            ]),
            &BTreeSet::from([1]),
        );
        assert_eq!(frames.keys().copied().collect::<Vec<_>>(), [1]);
        assert!(frames[&1].turned);
        assert_eq!(frames[&1].size(), (792.0, 612.0));
    }
}
