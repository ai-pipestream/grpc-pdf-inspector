//! Keeping the page's own characters apart from the markup the renderer
//! writes around them.
//!
//! The renderer builds one string. It writes `**` around a bold run, `#`
//! in front of a heading and a fence around code, and it writes the page's
//! text between them exactly as the page has it. A page whose text contains
//! an asterisk, a `#`, a backtick or a `<` therefore produces a string in
//! which nothing says which of those characters were drawn on the page and
//! which were written by the renderer. A reader of that string has to
//! guess, and a wrong guess removes characters the page drew.
//!
//! The renderer is the one place that knows, at the moment it copies a
//! run's text into the output. When [`MarkdownOptions::mark_literals`] is
//! set it copies those four characters as stand-ins, the cleanup passes run
//! over a string in which every `*`, `#`, backtick and `<` is markup, and
//! [`unshield`] puts the characters back and returns where they are.
//!
//! The stand-ins are Unicode noncharacters, which are reserved for exactly
//! this (private use inside one process) and are never valid in text that
//! is interchanged. A page whose text decodes to one of them anyway has it
//! replaced with U+FFFD by [`scrub`] before rendering, so no stand-in in
//! the string can have come from a page.
//!
//! [`MarkdownOptions::mark_literals`]: super::MarkdownOptions::mark_literals

use std::borrow::Cow;

use crate::types::TextItem;

/// Stands in for an asterisk the page drew.
pub(crate) const ASTERISK: char = '\u{FDD0}';
/// Stands in for a `#` the page drew.
pub(crate) const HASH: char = '\u{FDD1}';
/// Stands in for a backtick the page drew.
pub(crate) const BACKTICK: char = '\u{FDD2}';
/// Stands in for a `<` the page drew.
pub(crate) const LESS_THAN: char = '\u{FDD3}';

/// The stand-in for a character the page drew, when it is one the renderer
/// also writes as markup.
const fn stand_in(character: char) -> Option<char> {
    match character {
        '*' => Some(ASTERISK),
        '#' => Some(HASH),
        '`' => Some(BACKTICK),
        '<' => Some(LESS_THAN),
        _ => None,
    }
}

/// The character a stand-in stands for.
const fn original(character: char) -> Option<char> {
    match character {
        ASTERISK => Some('*'),
        HASH => Some('#'),
        BACKTICK => Some('`'),
        LESS_THAN => Some('<'),
        _ => None,
    }
}

/// Page text as it goes into the output: with stand-ins when `mark`, and
/// untouched otherwise.
pub(crate) fn shield_if(mark: bool, text: &str) -> Cow<'_, str> {
    if mark && text.chars().any(|character| stand_in(character).is_some()) {
        Cow::Owned(
            text.chars()
                .map(|character| stand_in(character).unwrap_or(character))
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// Take any stand-in code point out of text that is about to be rendered.
///
/// A noncharacter is not text, so one in a run is a decoding accident, and
/// U+FFFD is what it is replaced with wherever text is exchanged. Doing it
/// here is what makes every stand-in in the rendered string the renderer's
/// own.
pub(crate) fn scrub(items: &mut [TextItem]) {
    for item in items {
        if item
            .text
            .chars()
            .any(|character| original(character).is_some())
        {
            item.text = item
                .text
                .chars()
                .map(|character| {
                    if original(character).is_some() {
                        char::REPLACEMENT_CHARACTER
                    } else {
                        character
                    }
                })
                .collect();
        }
    }
}

/// Put the page's characters back, and say where they are.
///
/// Returns the markdown and the byte offset of each character in it that
/// is page text although markdown would read it as markup, in order.
pub(crate) fn unshield(marked: &str) -> (String, Vec<usize>) {
    let mut markdown = String::with_capacity(marked.len());
    let mut literals = Vec::new();
    for character in marked.chars() {
        match original(character) {
            Some(drawn) => {
                literals.push(markdown.len());
                markdown.push(drawn);
            }
            None => markdown.push(character),
        }
    }
    (markdown, literals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_text_comes_back_with_its_offsets() {
        let marked = format!("**bold** x{ASTERISK}y C{HASH} {LESS_THAN}u> {BACKTICK}");
        let (markdown, literals) = unshield(&marked);
        assert_eq!(markdown, "**bold** x*y C# <u> `");
        assert_eq!(literals, [10, 14, 16, 20]);
        for offset in literals {
            assert!(matches!(
                markdown.as_bytes()[offset],
                b'*' | b'#' | b'<' | b'`'
            ));
        }
    }

    #[test]
    fn offsets_are_bytes_of_the_markdown_that_is_returned() {
        // The stand-ins are three bytes each and the characters they stand
        // for are one; text before them may be wider than either.
        let marked = format!("\u{e9}t\u{e9} {ASTERISK} \u{2014} {HASH}");
        let (markdown, literals) = unshield(&marked);
        assert_eq!(markdown, "\u{e9}t\u{e9} * \u{2014} #");
        assert_eq!(&markdown[literals[0]..=literals[0]], "*");
        assert_eq!(&markdown[literals[1]..=literals[1]], "#");
    }

    /// One run of 11 pt text on page 1.
    fn run(text: &str, x: f32, y: f32) -> TextItem {
        TextItem {
            hull: None,
            text: text.into(),
            x,
            y,
            width: text.len() as f32 * 5.5,
            height: 11.0,
            font: "Helvetica".into(),
            font_tag: "F1".into(),
            font_size: 11.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: crate::types::ItemType::Text,
            mcid: None,
        }
    }

    /// A page of prose that draws every character the renderer also
    /// writes, with one run set bold and one italic, under a large heading
    /// that ends in a hash.
    fn page() -> Vec<TextItem> {
        let mut heading = run("Programming in C#", 72.0, 720.0);
        heading.font_size = 20.0;
        heading.height = 20.0;
        heading.is_bold = true;
        let mut bold = run("a bold phrase", 72.0, 666.0);
        bold.is_bold = true;
        let mut italic = run("an italic phrase", 72.0, 652.0);
        italic.is_italic = true;
        vec![
            heading,
            run("The area is x*y and the volume is z*w in every case.", 72.0, 680.0),
            bold,
            italic,
            run("Press <u>enter</u> to continue, see https://example.org/a#b<c", 72.0, 638.0),
            run("and `ticks` close the paragraph of the page here.", 72.0, 624.0),
            run("# of units sold was five in total this year.", 72.0, 560.0),
            run("and it was more than the year before that one.", 72.0, 546.0),
        ]
    }

    #[test]
    fn the_marked_rendering_is_the_plain_rendering() {
        use crate::markdown::{
            to_marked_markdown_from_items_with_rects_and_page_count,
            to_markdown_from_items_with_rects_and_page_count, MarkdownOptions,
        };
        let plain = to_markdown_from_items_with_rects_and_page_count(
            page(),
            MarkdownOptions::default(),
            &[],
            1,
        );
        let marked = to_marked_markdown_from_items_with_rects_and_page_count(
            page(),
            MarkdownOptions::default(),
            &[],
            1,
        );
        assert_eq!(marked.markdown, plain, "the marks must not change a byte");
        assert!(
            !plain.contains(|c| original(c).is_some()),
            "and a rendering nobody asked to mark carries no stand-in: {plain:?}"
        );
    }

    #[test]
    fn the_offsets_name_the_characters_the_page_drew_and_no_others() {
        use crate::markdown::{
            to_marked_markdown_from_items_with_rects_and_page_count, MarkdownOptions,
        };
        let marked = to_marked_markdown_from_items_with_rects_and_page_count(
            page(),
            MarkdownOptions::default(),
            &[],
            1,
        );
        let markdown = marked.markdown.as_str();
        let drawn: Vec<(usize, char)> = marked
            .literals
            .iter()
            .map(|offset| (*offset, markdown[*offset..].chars().next().expect("a character")))
            .collect();
        let named: String = drawn.iter().map(|(_, character)| *character).collect();
        // C#, x*y, z*w, <u>, </u>, a#b twice (the URL becomes a link, and
        // is its text and its target), <c, two backticks, the leading #.
        assert_eq!(named, "#**<<##<``#", "{markdown}");
        assert!(
            markdown.contains("[https://example.org/a#b](https://example.org/a#b)<c"),
            "a drawn `<` ends a URL as it always did: {markdown}"
        );

        // Everything else that looks like markup is the renderer's: the
        // heading marker, and the markers around the bold and italic runs.
        let written: Vec<usize> = markdown
            .char_indices()
            .filter(|(offset, character)| {
                matches!(character, '*' | '#' | '`' | '<') && !marked.literals.contains(offset)
            })
            .map(|(offset, _)| offset)
            .collect();
        assert!(markdown.starts_with("# Programming in C#"), "{markdown}");
        assert!(written.contains(&0), "the heading marker is markup");
        assert!(markdown.contains("**a bold phrase**"), "{markdown}");
        assert!(markdown.contains("*an italic phrase*"), "{markdown}");
        // One `#`, four `*` for the bold run, two for the italic run.
        assert_eq!(written.len(), 7, "{written:?} in {markdown}");
    }

    #[test]
    fn a_noncharacter_in_a_run_cannot_pass_for_a_stand_in() {
        use crate::markdown::{
            to_marked_markdown_from_items_with_rects_and_page_count, MarkdownOptions,
        };
        let marked = to_marked_markdown_from_items_with_rects_and_page_count(
            vec![run(&format!("garbage {ASTERISK}{HASH} decoded here"), 72.0, 700.0)],
            MarkdownOptions::default(),
            &[],
            1,
        );
        assert!(marked.literals.is_empty(), "{:?}", marked.literals);
        assert!(!marked.markdown.contains('*'), "{}", marked.markdown);
        assert!(marked.markdown.contains('\u{FFFD}'), "{}", marked.markdown);
    }

    #[test]
    fn nothing_is_shielded_unless_asked() {
        assert_eq!(shield_if(false, "x*y"), "x*y");
        assert_eq!(shield_if(true, "plain"), "plain");
        assert_eq!(shield_if(true, "x*y"), format!("x{ASTERISK}y"));
    }
}
