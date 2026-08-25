// SPDX-License-Identifier: Apache-2.0

//! The roles a tagged document gives its own content.
//!
//! A tagged PDF carries a structure tree: the author's own statement that
//! this run is a second-level heading, that one is a list item, this region
//! is a table cell. The markdown pipeline reads that tree internally and
//! uses it to decide how many `#` characters to print, and the roles
//! themselves never leave the library — so what reaches a consumer is a
//! rendering of the evidence rather than the evidence.
//!
//! `extract_structure_elements_mem` returns the tree resolved through the
//! document's `/RoleMap` and keyed by `(page, mcid)`, which is exactly the
//! key every extracted run already carries. Joining the two turns a heading
//! level that was inferred from type size into one the document states.

use std::collections::BTreeMap;

use pdf_inspector::StructureElement;

use crate::proto::v1 as pb;

/// Group the document's structure elements by their 1-indexed page.
#[must_use]
pub fn by_page(elements: Vec<StructureElement>) -> BTreeMap<u32, pb::PageStructure> {
    let mut pages: BTreeMap<u32, pb::PageStructure> = BTreeMap::new();
    for element in elements {
        pages
            .entry(element.page)
            .or_insert_with(|| pb::PageStructure {
                page_no: element.page,
                elements: Vec::new(),
            })
            .elements
            .push(pb::StructureElement {
                mcid: element.mcid,
                role: role(&element.role).into(),
                // Always set, even for a standard role: a consumer should
                // never have to turn the enum back into the name the
                // document used.
                role_raw: element.role,
            });
    }
    pages
}

/// The heading depth a role states, when it states one.
///
/// `H` is a heading whose depth the document does not give; it counts as
/// the outermost, because a document that uses `H` uses it for everything
/// and any other choice would invent a hierarchy.
#[must_use]
pub fn heading_level(role: pb::StructureRole) -> Option<i32> {
    match role {
        pb::StructureRole::H | pb::StructureRole::H1 => Some(1),
        pb::StructureRole::H2 => Some(2),
        pb::StructureRole::H3 => Some(3),
        pb::StructureRole::H4 => Some(4),
        pb::StructureRole::H5 => Some(5),
        pb::StructureRole::H6 => Some(6),
        _ => None,
    }
}

/// Map a resolved role name onto the wire enum.
///
/// A name with no standard mapping is `UNSPECIFIED`; `role_raw` carries it
/// verbatim, so nothing is lost by the enum not knowing it.
#[must_use]
pub fn role(name: &str) -> pb::StructureRole {
    match name {
        "Document" => pb::StructureRole::Document,
        "Part" => pb::StructureRole::Part,
        "Art" => pb::StructureRole::Art,
        "Sect" => pb::StructureRole::Sect,
        "Div" => pb::StructureRole::Div,
        "BlockQuote" => pb::StructureRole::BlockQuote,
        "Caption" => pb::StructureRole::Caption,
        "TOC" => pb::StructureRole::Toc,
        "TOCI" => pb::StructureRole::Toci,
        "Index" => pb::StructureRole::Index,
        "NonStruct" => pb::StructureRole::NonStruct,
        "Private" => pb::StructureRole::Private,
        "H" => pb::StructureRole::H,
        "H1" => pb::StructureRole::H1,
        "H2" => pb::StructureRole::H2,
        "H3" => pb::StructureRole::H3,
        "H4" => pb::StructureRole::H4,
        "H5" => pb::StructureRole::H5,
        "H6" => pb::StructureRole::H6,
        "P" => pb::StructureRole::P,
        "L" => pb::StructureRole::L,
        "LI" => pb::StructureRole::Li,
        "Lbl" => pb::StructureRole::Lbl,
        "LBody" => pb::StructureRole::Lbody,
        "Table" => pb::StructureRole::Table,
        "TR" => pb::StructureRole::Tr,
        "TH" => pb::StructureRole::Th,
        "TD" => pb::StructureRole::Td,
        "THead" => pb::StructureRole::Thead,
        "TBody" => pb::StructureRole::Tbody,
        "TFoot" => pb::StructureRole::Tfoot,
        "Span" => pb::StructureRole::Span,
        "Quote" => pb::StructureRole::Quote,
        "Note" => pb::StructureRole::Note,
        "Reference" => pb::StructureRole::Reference,
        "BibEntry" => pb::StructureRole::BibEntry,
        "Code" => pb::StructureRole::Code,
        "Link" => pb::StructureRole::Link,
        "Annot" => pb::StructureRole::Annot,
        "Figure" => pb::StructureRole::Figure,
        "Formula" => pb::StructureRole::Formula,
        "Form" => pb::StructureRole::Form,
        "Ruby" => pb::StructureRole::Ruby,
        "RB" => pb::StructureRole::Rb,
        "RT" => pb::StructureRole::Rt,
        "RP" => pb::StructureRole::Rp,
        "Warichu" => pb::StructureRole::Warichu,
        "WT" => pb::StructureRole::Wt,
        "WP" => pb::StructureRole::Wp,
        _ => pb::StructureRole::Unspecified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(page: u32, mcid: i64, role: &str) -> StructureElement {
        StructureElement {
            page,
            mcid,
            role: role.to_owned(),
        }
    }

    #[test]
    fn elements_group_by_page_keeping_their_join_key() {
        let pages = by_page(vec![
            element(1, 0, "H1"),
            element(2, 0, "P"),
            element(1, 1, "P"),
        ]);
        assert_eq!(pages.keys().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(pages[&1].elements.len(), 2);
        assert_eq!(pages[&1].elements[0].mcid, 0);
        assert_eq!(pages[&1].elements[0].role, pb::StructureRole::H1 as i32);
    }

    #[test]
    fn a_custom_tag_keeps_its_name_even_without_an_enum_value() {
        let pages = by_page(vec![element(1, 4, "MyCompanyCallout")]);
        let only = &pages[&1].elements[0];
        assert_eq!(only.role, pb::StructureRole::Unspecified as i32);
        assert_eq!(only.role_raw, "MyCompanyCallout");
    }

    #[test]
    fn a_standard_role_still_carries_its_own_name() {
        let pages = by_page(vec![element(1, 0, "BlockQuote")]);
        let only = &pages[&1].elements[0];
        assert_eq!(only.role, pb::StructureRole::BlockQuote as i32);
        assert_eq!(only.role_raw, "BlockQuote");
    }

    #[test]
    fn heading_roles_state_their_depth_and_nothing_else_does() {
        assert_eq!(heading_level(pb::StructureRole::H1), Some(1));
        assert_eq!(heading_level(pb::StructureRole::H4), Some(4));
        assert_eq!(
            heading_level(pb::StructureRole::H),
            Some(1),
            "a heading with no stated depth is the outermost"
        );
        assert_eq!(heading_level(pb::StructureRole::P), None);
        assert_eq!(heading_level(pb::StructureRole::Unspecified), None);
    }
}
