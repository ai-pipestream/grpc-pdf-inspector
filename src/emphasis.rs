// SPDX-License-Identifier: Apache-2.0

//! Lifting the markdown renderer's emphasis markers off a block's text.
//!
//! The renderer flattens per-run facts into markers inside the text it
//! prints: `**` around bold, `*` around italic, `<u>` around an underlined
//! run and `<s>` around a struck one. That is the right thing for a
//! markdown file and the wrong thing for `TextItemBase.text`, whose
//! contract is the words the page shows. A consumer indexing that text
//! meets an asterisk the page never drew, and a consumer rendering it
//! escapes the tag and prints `&lt;u&gt;` in the middle of a heading.
//!
//! The markers are not thrown away. They are the renderer's report of
//! which characters the emphasis covered, so they become
//! `InlineSpan.formatting` over exactly those characters of the plain text
//! — the typed home the schema gives that fact — and the text keeps only
//! what was printed.
//!
//! The renderer never escapes a literal asterisk, so `*2000` in a card
//! number and `*T*` in a formula look the same to a scanner. What tells
//! them apart is what an asterisk touches: a marker that opens emphasis
//! is followed by the text it emphasizes and one that closes it follows
//! that text, the way the renderer prints them, whereas `*2000` at the
//! end of a block opens nothing that ever closes. A marker that opens
//! and never closes, or closes nothing, is a character of the text.

use crate::proto::ai::pipestream::document::v1 as doc;

/// A block's text with its emphasis lifted onto spans.
#[derive(Debug, Default, PartialEq)]
pub struct Lifted {
    /// The text with every marker removed.
    pub text: String,
    /// The emphasis the markers described, as formatting spans over
    /// character ranges of `text`. Empty when the block had none.
    pub spans: Vec<doc::InlineSpan>,
}

/// The emphasis in force over one stretch of characters.
#[derive(Clone, Copy, Default, PartialEq)]
struct State {
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

impl State {
    const fn is_plain(self) -> bool {
        !self.bold && !self.italic && !self.underline && !self.strikeout
    }

    const fn get(self, kind: Kind) -> bool {
        match kind {
            Kind::Bold => self.bold,
            Kind::Italic => self.italic,
            Kind::Underline => self.underline,
            Kind::Strikeout => self.strikeout,
        }
    }

    const fn toggled(self, kind: Kind) -> Self {
        match kind {
            Kind::Bold => Self {
                bold: !self.bold,
                ..self
            },
            Kind::Italic => Self {
                italic: !self.italic,
                ..self
            },
            Kind::Underline => Self {
                underline: !self.underline,
                ..self
            },
            Kind::Strikeout => Self {
                strikeout: !self.strikeout,
                ..self
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bold,
    Italic,
    Underline,
    Strikeout,
}

/// One piece of the block as scanned: a character of text, or a marker
/// with the emphasis it would toggle.
enum Token {
    Text(char),
    /// A marker, its source text (for when it turns out to be literal),
    /// and whether the text around it lets it open or close.
    Marker {
        kind: Kind,
        source: &'static str,
        can_open: bool,
        can_close: bool,
    },
}

/// Scan `text` into tokens. The tags name their side; an asterisk run can
/// open when text follows it and close when text precedes it.
fn scan(text: &str) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(text.len());
    let mut rest = text;
    let mut previous: Option<char> = None;
    while !rest.is_empty() {
        let (kind, source, open_side, close_side) = if rest.starts_with("**") {
            (Kind::Bold, "**", None, None)
        } else if rest.starts_with('*') {
            (Kind::Italic, "*", None, None)
        } else if rest.starts_with("<u>") {
            (Kind::Underline, "<u>", Some(true), Some(false))
        } else if rest.starts_with("</u>") {
            (Kind::Underline, "</u>", Some(false), Some(true))
        } else if rest.starts_with("<s>") {
            (Kind::Strikeout, "<s>", Some(true), Some(false))
        } else if rest.starts_with("</s>") {
            (Kind::Strikeout, "</s>", Some(false), Some(true))
        } else {
            let character = rest.chars().next().expect("non-empty");
            tokens.push(Token::Text(character));
            previous = Some(character);
            rest = &rest[character.len_utf8()..];
            continue;
        };
        let after = rest[source.len()..].chars().next();
        let flanks_text = |side: Option<char>| side.is_some_and(|c| !c.is_whitespace());
        tokens.push(Token::Marker {
            kind,
            source,
            can_open: open_side.unwrap_or_else(|| flanks_text(after)),
            can_close: close_side.unwrap_or_else(|| flanks_text(previous)),
        });
        previous = None;
        rest = &rest[source.len()..];
    }
    tokens
}

/// Lift the emphasis markers out of `text`.
#[must_use]
pub fn lift(text: &str) -> Lifted {
    let tokens = scan(text);
    // First pass: which markers pair. A closer takes the emphasis that is
    // open; an opener opens one that is not; anything else is literal, and
    // so is an opener nothing ever closes.
    let mut role: Vec<Option<Kind>> = vec![None; tokens.len()];
    let mut open_at: [Option<usize>; 4] = [None; 4];
    let slot = |kind: Kind| kind as usize;
    let mut state = State::default();
    for (index, token) in tokens.iter().enumerate() {
        let Token::Marker {
            kind,
            can_open,
            can_close,
            ..
        } = token
        else {
            continue;
        };
        if state.get(*kind) && *can_close {
            role[index] = Some(*kind);
            open_at[slot(*kind)] = None;
            state = state.toggled(*kind);
        } else if !state.get(*kind) && *can_open {
            role[index] = Some(*kind);
            open_at[slot(*kind)] = Some(index);
            state = state.toggled(*kind);
        }
    }
    for opener in open_at.into_iter().flatten() {
        role[opener] = None;
    }

    // Second pass: the plain text, and the emphasis in force over each
    // stretch of it.
    let mut plain = String::with_capacity(text.len());
    let mut chars = 0usize;
    let mut state = State::default();
    let mut run_start = 0usize;
    let mut runs: Vec<(usize, usize, State)> = Vec::new();
    for (token, role) in tokens.iter().zip(role) {
        match (token, role) {
            (Token::Marker { kind, .. }, Some(_)) => {
                if chars > run_start && !state.is_plain() {
                    runs.push((run_start, chars, state));
                }
                run_start = chars;
                state = state.toggled(*kind);
            }
            (Token::Marker { source, .. }, None) => {
                plain.push_str(source);
                chars += source.chars().count();
            }
            (Token::Text(character), _) => {
                plain.push(*character);
                chars += 1;
            }
        }
    }
    if chars > run_start && !state.is_plain() {
        runs.push((run_start, chars, state));
    }
    Lifted {
        text: plain,
        spans: runs.into_iter().map(span).collect(),
    }
}

/// One emphasized stretch as a formatting span over `[start, end)`.
fn span((start, end, state): (usize, usize, State)) -> doc::InlineSpan {
    doc::InlineSpan {
        range: Some(doc::IntSpan {
            start: i32::try_from(start).unwrap_or(i32::MAX),
            end: i32::try_from(end).unwrap_or(i32::MAX),
        }),
        formatting: Some(doc::Formatting {
            bold: state.bold,
            italic: state.italic,
            underline: state.underline,
            strikethrough: state.strikeout,
            ..doc::Formatting::default()
        }),
        ..doc::InlineSpan::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(lifted: &Lifted, span: &doc::InlineSpan) -> String {
        let range = span.range.as_ref().expect("a range");
        lifted
            .text
            .chars()
            .skip(range.start as usize)
            .take((range.end - range.start) as usize)
            .collect()
    }

    fn formatting(span: &doc::InlineSpan) -> (bool, bool, bool, bool) {
        let f = span.formatting.as_ref().expect("formatting");
        (f.bold, f.italic, f.underline, f.strikethrough)
    }

    #[test]
    fn plain_text_is_untouched_and_carries_no_spans() {
        let lifted = lift("Your order has been placed!");
        assert_eq!(lifted.text, "Your order has been placed!");
        assert!(lifted.spans.is_empty());
    }

    #[test]
    fn bold_lifts_onto_a_formatting_span_over_the_plain_text() {
        let lifted = lift("Please **click here** to follow");
        assert_eq!(lifted.text, "Please click here to follow");
        assert_eq!(lifted.spans.len(), 1);
        assert_eq!(covered(&lifted, &lifted.spans[0]), "click here");
        assert_eq!(formatting(&lifted.spans[0]), (true, false, false, false));
    }

    #[test]
    fn underline_tags_leave_no_html_in_the_text() {
        let lifted = lift("View the Privacy Policies for <u>Clover</u>");
        assert_eq!(lifted.text, "View the Privacy Policies for Clover");
        assert_eq!(covered(&lifted, &lifted.spans[0]), "Clover");
        assert_eq!(formatting(&lifted.spans[0]), (false, false, true, false));
    }

    #[test]
    fn a_whole_underlined_heading_is_one_span() {
        let lifted = lift("<u>Your receipt from GEEK SHOP</u>");
        assert_eq!(lifted.text, "Your receipt from GEEK SHOP");
        assert_eq!(lifted.spans.len(), 1);
        let range = lifted.spans[0].range.as_ref().expect("a range");
        assert_eq!((range.start, range.end), (0, 27));
    }

    #[test]
    fn italic_and_nested_emphasis_keep_their_own_ranges() {
        let lifted = lift("**Diffusion Models** A *latent* variable");
        assert_eq!(lifted.text, "Diffusion Models A latent variable");
        assert_eq!(lifted.spans.len(), 2);
        assert_eq!(covered(&lifted, &lifted.spans[0]), "Diffusion Models");
        assert_eq!(formatting(&lifted.spans[0]), (true, false, false, false));
        assert_eq!(covered(&lifted, &lifted.spans[1]), "latent");
        assert_eq!(formatting(&lifted.spans[1]), (false, true, false, false));
    }

    #[test]
    fn a_literal_asterisk_stays_when_it_does_not_pair() {
        let lifted = lift("**Payment method**: American Express *2000");
        assert_eq!(lifted.text, "Payment method: American Express *2000");
        assert_eq!(lifted.spans.len(), 1);
        assert_eq!(covered(&lifted, &lifted.spans[0]), "Payment method");
    }

    #[test]
    fn a_literal_asterisk_does_not_cost_the_block_its_italics() {
        let lifted = lift("follow *the* steps. Card *2000");
        assert_eq!(lifted.text, "follow the steps. Card *2000");
        assert_eq!(lifted.spans.len(), 1);
        assert_eq!(covered(&lifted, &lifted.spans[0]), "the");
        assert_eq!(formatting(&lifted.spans[0]), (false, true, false, false));
    }

    #[test]
    fn a_closer_with_nothing_open_is_text() {
        let lifted = lift("rated 5* by critics");
        assert_eq!(lifted.text, "rated 5* by critics");
        assert!(lifted.spans.is_empty());
    }

    #[test]
    fn unpaired_bold_reads_as_literal_too() {
        let lifted = lift("rated ** by critics");
        assert_eq!(lifted.text, "rated ** by critics");
        assert!(lifted.spans.is_empty());
    }

    #[test]
    fn an_unpaired_tag_is_text() {
        let lifted = lift("the <u> element **matters**");
        assert_eq!(lifted.text, "the <u> element matters");
        assert_eq!(lifted.spans.len(), 1);
        assert_eq!(covered(&lifted, &lifted.spans[0]), "matters");
    }

    #[test]
    fn ranges_count_characters_not_bytes() {
        let lifted = lift("x*t−*1 = *α*¯");
        assert_eq!(lifted.text, "xt−1 = α¯");
        assert_eq!(covered(&lifted, &lifted.spans[0]), "t−");
        assert_eq!(covered(&lifted, &lifted.spans[1]), "α");
    }

    #[test]
    fn struck_text_is_strikethrough() {
        let lifted = lift("was <s>ten</s> now nine");
        assert_eq!(lifted.text, "was ten now nine");
        assert_eq!(formatting(&lifted.spans[0]), (false, false, false, true));
    }
}
