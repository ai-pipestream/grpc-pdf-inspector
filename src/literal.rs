// SPDX-License-Identifier: Apache-2.0

//! Reading a page's markdown without taking the page's own characters for
//! markup.
//!
//! The renderer copies page text into the markdown as it is. A page that
//! drew `x*y and z*w`, a heading that ends in `C#` or a line that opens
//! with `# of units` therefore renders to a string in which nothing says
//! which asterisks and hashes were drawn and which the renderer wrote. A
//! reader that guesses takes drawn characters out of the text and reports
//! emphasis and headings the page never had.
//!
//! It does not have to guess. `PageMarkdown.literal_offsets` is the
//! renderer's own account of which `*`, `#`, backtick and `<` characters
//! are page text. [`Shielded`] swaps exactly those characters for stand-ins
//! before the markdown is read, so the reader meets markup only where the
//! renderer wrote markup, and puts them back in the text each block ends
//! up carrying.
//!
//! A page event that names no offsets is read as it always was, by what an
//! asterisk touches (see [`emphasis`](crate::emphasis)). That is the
//! password fallback's whole-document event, and any event from a server
//! older than the field.

/// The characters the renderer both copies from pages and writes as markup.
const MARKUP: [char; 4] = ['*', '#', '`', '<'];

/// A page's markdown with its page-text markup characters swapped for
/// stand-ins.
pub struct Shielded {
    /// The markdown, stand-ins in place.
    text: String,
    /// The stand-in for each of [`MARKUP`], in its order; `None` when
    /// nothing was swapped.
    stand_ins: Option<[char; 4]>,
}

impl Shielded {
    /// Swap the characters of `markdown` at `literal_offsets` for
    /// stand-ins.
    ///
    /// The offsets are UTF-8 byte offsets, as the wire defines them. One
    /// that does not land on one of the four characters names nothing and
    /// is passed over: the field is the renderer's, and an offset that
    /// disagrees with the string beside it is not evidence about the page.
    #[must_use]
    pub fn new(markdown: &str, literal_offsets: &[u32]) -> Self {
        let unmarked = || Self {
            text: markdown.to_owned(),
            stand_ins: None,
        };
        if literal_offsets.is_empty() {
            return unmarked();
        }
        let Some(stand_ins) = free_stand_ins(markdown) else {
            return unmarked();
        };
        let mut offsets: Vec<usize> = literal_offsets
            .iter()
            .filter_map(|offset| usize::try_from(*offset).ok())
            .collect();
        offsets.sort_unstable();
        let mut offsets = offsets.into_iter().peekable();
        let mut text = String::with_capacity(markdown.len());
        for (offset, character) in markdown.char_indices() {
            while offsets.next_if(|next| *next < offset).is_some() {}
            let named = offsets.next_if_eq(&offset).is_some();
            let swapped = named
                .then(|| MARKUP.iter().position(|markup| *markup == character))
                .flatten()
                .map(|index| stand_ins[index]);
            text.push(swapped.unwrap_or(character));
        }
        Self {
            text,
            stand_ins: Some(stand_ins),
        }
    }

    /// The markdown to read: every `*`, `#`, backtick and `<` left in it
    /// is markup.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Put the page's characters back into text cut from
    /// [`text`](Self::text).
    ///
    /// A stand-in is one character and so is what it stands for, so
    /// character ranges measured over the text before are ranges over the
    /// text after.
    #[must_use]
    pub fn restore(&self, text: String) -> String {
        let Some(stand_ins) = self.stand_ins else {
            return text;
        };
        if !text.contains(stand_ins) {
            return text;
        }
        text.chars()
            .map(|character| {
                stand_ins
                    .iter()
                    .position(|stand_in| *stand_in == character)
                    .map_or(character, |index| MARKUP[index])
            })
            .collect()
    }
}

/// Four code points that occur nowhere in `markdown`.
///
/// Unicode's noncharacters come first: they are reserved for use inside
/// one process and are not text, so a page's markdown has none of them
/// unless its fonts decoded to nonsense. The private-use plane after them
/// is only there so that even such a page has four code points to spare;
/// a string that uses every one of the 65,000 candidates is the only one
/// this returns `None` for.
fn free_stand_ins(markdown: &str) -> Option<[char; 4]> {
    let mut free = ('\u{FDD0}'..='\u{FDEF}')
        .chain('\u{10_0000}'..='\u{10_FFFD}')
        .filter(|candidate| !markdown.contains(*candidate));
    Some([free.next()?, free.next()?, free.next()?, free.next()?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_characters_are_swapped() {
        // The first asterisk pair is the renderer's italic; the last two
        // asterisks are the page's.
        let markdown = "*lead* x*y and z*w";
        let shielded = Shielded::new(markdown, &[8, 16]);
        let asterisks = shielded.text().matches('*').count();
        assert_eq!(asterisks, 2, "{:?}", shielded.text());
        assert_eq!(shielded.restore(shielded.text().to_owned()), markdown);
    }

    #[test]
    fn a_page_with_no_offsets_is_left_as_it_is() {
        let shielded = Shielded::new("x*y and z*w", &[]);
        assert_eq!(shielded.text(), "x*y and z*w");
        assert_eq!(shielded.restore("x*y".to_owned()), "x*y");
    }

    #[test]
    fn an_offset_that_names_no_markup_character_swaps_nothing() {
        // Offset 1 is inside the two-byte e-acute, 2 is a letter, 99 is
        // past the end.
        let markdown = "\u{e9}t\u{e9} *x*";
        let shielded = Shielded::new(markdown, &[1, 2, 99]);
        assert_eq!(shielded.text(), markdown);
    }

    #[test]
    fn stand_ins_never_collide_with_the_pages_own_code_points() {
        // A page whose text decoded to the first noncharacters.
        let markdown = "\u{FDD0}\u{FDD1}\u{FDD2}\u{FDD3} x*y";
        let offset = u32::try_from(markdown.find('*').expect("an asterisk")).expect("small");
        let shielded = Shielded::new(markdown, &[offset]);
        assert!(!shielded.text().contains('*'));
        assert!(
            shielded
                .text()
                .starts_with("\u{FDD0}\u{FDD1}\u{FDD2}\u{FDD3}")
        );
        assert_eq!(shielded.restore(shielded.text().to_owned()), markdown);
    }
}
