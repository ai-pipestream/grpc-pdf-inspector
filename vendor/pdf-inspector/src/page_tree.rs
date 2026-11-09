//! The page tree, walked without believing what it says about its size.
//!
//! lopdf's page iterator reports the `/Count` of every `/Pages` node still
//! ahead of it as its size hint, and [`Document::get_pages`] collects that
//! iterator through a `Vec` that reserves the hinted length before it has
//! seen a second page. A node claiming 2^40 descendants therefore asks the
//! allocator for terabytes, which aborts the process, and a node claiming
//! 2^60 overflows the capacity arithmetic, which panics. Neither number is
//! read for any other purpose, so the walk here inserts pages one at a time
//! and costs what the tree actually holds.

use std::collections::BTreeMap;

use lopdf::{Document, ObjectId};

/// Every page of `doc`, numbered from 1 in page-tree order.
///
/// The bounded counterpart of [`Document::get_pages`]: the same pages in
/// the same order, found by the same iterator, without the up-front
/// reservation its size hint invites.
pub fn pages(doc: &Document) -> BTreeMap<u32, ObjectId> {
    let mut pages = BTreeMap::new();
    for (index, page_id) in doc.page_iter().enumerate() {
        pages.insert(index as u32 + 1, page_id);
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object};

    /// One page, beside an empty `/Pages` node whose `/Count` is `count`.
    fn with_count_lie(count: i64) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        let liar_id = doc.add_object(dictionary! {
            "Type" => "Pages",
            "Parent" => pages_id,
            "Kids" => Vec::<Object>::new(),
            "Count" => count,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id), Object::Reference(liar_id)],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc
    }

    #[test]
    fn a_count_the_tree_does_not_hold_costs_nothing() {
        for count in [1i64 << 40, 1i64 << 60, i64::MAX] {
            let doc = with_count_lie(count);
            let pages = pages(&doc);
            assert_eq!(pages.len(), 1, "count {count}");
            assert_eq!(pages.keys().copied().collect::<Vec<_>>(), [1]);
        }
    }

    #[test]
    fn pages_are_numbered_from_one_in_tree_order() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let kids: Vec<Object> = (0..3)
            .map(|_| {
                Object::Reference(doc.add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                }))
            })
            .collect();
        let expected: Vec<ObjectId> = kids.iter().map(|kid| kid.as_reference().unwrap()).collect();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);

        let pages = pages(&doc);
        assert_eq!(pages.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(pages.values().copied().collect::<Vec<_>>(), expected);
    }
}
