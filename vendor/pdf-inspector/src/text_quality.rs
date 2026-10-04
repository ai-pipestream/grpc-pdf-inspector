//! Text-quality detection: deciding when an extracted text layer is too broken
//! to serve and a page should fall back to OCR.
//!
//! Extraction can produce plausible-looking bytes that are actually garbage —
//! failed CID→Unicode mappings, broken ToUnicode CMaps, mojibake. These
//! detectors catch that and let callers set `needs_ocr`. They come in two
//! layers, sharing the same primitives:
//!
//! - **Markdown-level** ([`detect_encoding_issues`], [`is_garbage_text`],
//!   [`is_cid_garbage`]) run on a page's final markdown string. Used as a
//!   backstop on the region-extraction and whole-document paths.
//! - **Item/span-level** ([`analyze_text_quality`],
//!   [`region_items_have_decoding_issue`]) run on individual `TextItem`s and
//!   accumulate per-page evidence, so localized garbled spans on an otherwise
//!   clean page are caught without a single span having to condemn the page.
//!
//! Detection classes, roughly by signal:
//! - **Replacement runs**: U+FFFD clusters ([`has_replacement_text_run`]).
//! - **Private-use / C1-control runs**: CID passthrough landing in PUA or the
//!   C1 block ([`has_private_use_text_run`], [`has_cid_control_token`]).
//! - **Dollar-as-space**: `Word$Word$Word` from broken CMaps
//!   ([`has_dollar_as_space_pattern`]).
//! - **Non-alphanumeric dominance**: symbol soup ([`is_garbage_text`]).
//! - **Substitution-cipher letter statistics**: pure-ASCII output whose letter
//!   distribution is a permutation of natural language ([`CipherGarbleStats`]).
//! - **Symbol soup on the page's runs**: glyph codes passed through as low
//!   ASCII, so a page of prose reads as `!"#$%&` ([`SymbolSoupStats`]).

use crate::types::TextItem;
use crate::{add_ocr_reason, OCR_REASON_SUSPECTED_GARBLED_TEXT};
use std::collections::BTreeMap;

/// Detect broken font encodings in extracted markdown text.
///
/// Two heuristics:
/// 1. **U+FFFD**: Any replacement character indicates decode failures.
/// 2. **Dollar-as-space**: Pattern like `Word$Word$Word` where `$` is used as a
///    word separator due to broken ToUnicode CMaps. Triggers when either:
///    - More than 50% of `$` are between letters (clear substitution pattern), OR
///    - More than 20 letter-dollar-letter occurrences (even if some `$` are also
///      used as trailing/leading separators, 20+ is far beyond normal financial text).
pub fn detect_encoding_issues(markdown: &str) -> bool {
    // Heuristic 1: U+FFFD replacement characters
    if markdown.contains('\u{FFFD}') {
        return true;
    }

    // Heuristic 2: dollar-as-space pattern
    if has_dollar_as_space_pattern(markdown) {
        return true;
    }

    // Heuristic 3: substitution-cipher letter statistics (broken ToUnicode)
    let mut stats = CipherGarbleStats::default();
    stats.add_text(markdown);
    stats.looks_garbled()
}

fn has_dollar_as_space_pattern(markdown: &str) -> bool {
    let total_dollars = markdown.matches('$').count();
    if total_dollars > 10 {
        let bytes = markdown.as_bytes();
        let mut letter_dollar_letter = 0usize;
        for i in 1..bytes.len().saturating_sub(1) {
            if bytes[i] == b'$'
                && bytes[i - 1].is_ascii_alphabetic()
                && bytes[i + 1].is_ascii_alphabetic()
            {
                letter_dollar_letter += 1;
            }
        }
        if letter_dollar_letter > 20 || letter_dollar_letter * 2 > total_dollars {
            return true;
        }
    }

    false
}

/// English letter frequencies (percent, a–z). Used as a natural-language
/// reference: every Latin-script language in the eval corpus (Swedish,
/// Finnish, Turkish, German, romaji) scores ≥ 0.80 cosine similarity against
/// it, while substitution-cipher text scores ~0.53.
const ENGLISH_LETTER_FREQ: [f64; 26] = [
    8.2, 1.5, 2.8, 4.3, 12.7, 2.2, 2.0, 6.1, 7.0, 0.15, 0.8, 4.0, 2.4, 6.7, 7.5, 1.9, 0.1, 6.0,
    6.3, 9.1, 2.8, 1.0, 2.4, 0.15, 2.0, 0.07,
];

/// Letter statistics for detecting substitution-cipher garbling: broken
/// ToUnicode CMaps that shift every character by a per-range constant (e.g.
/// `Certificate` extracted as `8VceZWZTReV`). Such text is 100% printable
/// ASCII with word-like token lengths, so it defeats `is_garbage_text` and
/// produces no replacement characters — it needs its own discriminator.
#[derive(Debug, Default)]
struct CipherGarbleStats {
    /// Case-folded ASCII letter histogram.
    letter_counts: [u32; 26],
    ascii_letters: usize,
    ascii_vowels: usize,
    /// Accented Latin letters (Latin-1 Supplement through Latin Extended-B,
    /// plus Latin Extended Additional). Count toward Latin dominance only.
    latin_ext_letters: usize,
    non_latin_letters: usize,
    /// Adjacent ASCII-letter pairs, and how many of them switch from
    /// lowercase straight to uppercase mid-word.
    letter_bigrams: usize,
    case_shift_bigrams: usize,
}

impl CipherGarbleStats {
    fn add_text(&mut self, text: &str) {
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if ch.is_ascii_alphabetic() {
                let idx = (ch.to_ascii_lowercase() as u8 - b'a') as usize;
                self.letter_counts[idx] += 1;
                self.ascii_letters += 1;
                if matches!(ch.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u') {
                    self.ascii_vowels += 1;
                }
                if let Some(p) = prev {
                    self.letter_bigrams += 1;
                    if p.is_ascii_lowercase() && ch.is_ascii_uppercase() {
                        self.case_shift_bigrams += 1;
                    }
                }
                prev = Some(ch);
            } else {
                if ch.is_alphabetic() {
                    if matches!(ch as u32, 0xC0..=0x24F | 0x1E00..=0x1EFF) {
                        self.latin_ext_letters += 1;
                    } else {
                        self.non_latin_letters += 1;
                    }
                }
                prev = None;
            }
        }
    }

    /// Cosine similarity between the observed letter histogram and English
    /// letter frequencies. A shifted alphabet permutes the histogram, which
    /// destroys the similarity regardless of the shift amount.
    fn english_cosine(&self) -> f64 {
        if self.ascii_letters == 0 {
            return 1.0;
        }
        let n = self.ascii_letters as f64;
        let mut dot = 0.0;
        let mut norm_obs = 0.0;
        for (count, freq) in self.letter_counts.iter().zip(ENGLISH_LETTER_FREQ) {
            let p = *count as f64 / n;
            dot += p * freq;
            norm_obs += p * p;
        }
        let norm_en = ENGLISH_LETTER_FREQ
            .iter()
            .map(|f| f * f)
            .sum::<f64>()
            .sqrt();
        dot / (norm_obs.sqrt() * norm_en)
    }

    /// Cosine similarity between the observed histogram and English
    /// frequencies after sorting BOTH descending — i.e. comparing the *shape*
    /// of the frequency profile, ignoring which letter sits where. A
    /// substitution cipher is a bijection, so it preserves this shape exactly
    /// (att10k 0.97, arbitrary shifts 0.99) regardless of case or offset.
    /// Non-linguistic ASCII has a different profile: a small alphabet is far
    /// steeper (random DNA 0.74, hex dumps 0.81), so the shape diverges.
    fn english_shape_cosine(&self) -> f64 {
        if self.ascii_letters == 0 {
            return 1.0;
        }
        let n = self.ascii_letters as f64;
        let mut obs: [f64; 26] = std::array::from_fn(|i| self.letter_counts[i] as f64 / n);
        obs.sort_unstable_by(|a, b| b.total_cmp(a));
        let mut en = ENGLISH_LETTER_FREQ;
        en.sort_unstable_by(|a, b| b.total_cmp(a));

        let dot: f64 = obs.iter().zip(en).map(|(o, e)| o * e).sum();
        let norm_obs = obs.iter().map(|o| o * o).sum::<f64>().sqrt();
        let norm_en = en.iter().map(|e| e * e).sum::<f64>().sqrt();
        dot / (norm_obs * norm_en)
    }

    /// Thresholds validated against the 380-document pdf-evals snapshot
    /// corpus (0 false positives) and the garbled ParseBench `att10k` page
    /// (vowel ratio 0.245, case-shift rate 0.225, cosine 0.532). Closest
    /// legitimate document on each axis: vowel ratio 0.264 (circuit
    /// schematic), case-shift rate 0.021, cosine 0.801.
    fn looks_garbled(&self) -> bool {
        // Need a statistically meaningful, Latin-dominant sample.
        if self.ascii_letters < MIN_LETTERS_FOR_GARBLE_SCORE
            || self.non_latin_letters > self.ascii_letters + self.latin_ext_letters
        {
            return false;
        }

        // Real Latin-script text keeps vowels above ~30% of letters even in
        // acronym- and part-number-heavy documents; shifted text starves them.
        let vowel_ratio = self.ascii_vowels as f64 / self.ascii_letters as f64;
        if vowel_ratio > 0.30 {
            return false;
        }

        // Signal 1: lowercase→uppercase transitions inside words. A shifted
        // lowercase alphabet straddles the ASCII uppercase block ('i'→'Z',
        // 't'→'e'), so garbled words flip case constantly. Real documents
        // stay ≤ 0.02 even with camelCase identifiers.
        let case_shifts = self.letter_bigrams >= 100
            && self.case_shift_bigrams as f64 >= self.letter_bigrams as f64 * 0.10;

        // Signal 2: the histogram is a permutation of natural language — an
        // English-like frequency SHAPE (sorted cosine high) but with letters
        // in the wrong POSITIONS (unsorted cosine low). This is the signature
        // of a substitution cipher and is case-independent, so it catches
        // all-lowercase and all-uppercase shifts as well as case-straddling
        // ones. Genuinely non-linguistic ASCII that is merely "unlike English"
        // fails one of the two halves: DNA/hex dumps have too steep a profile
        // (shape cosine < 0.90), while protein sequences, ticker symbols and
        // base64 are not sufficiently unlike English in position (unsorted
        // cosine ≥ 0.60) — so none of them are routed to OCR.
        let permuted_language = self.english_cosine() < 0.60 && self.english_shape_cosine() >= 0.90;

        case_shifts || permuted_language
    }

    /// The letter statistics as a caller sees them.
    fn score(&self) -> LetterFrequencyScore {
        LetterFrequencyScore {
            ascii_letters: self.ascii_letters,
            english_cosine: self.english_cosine(),
            english_shape_cosine: self.english_shape_cosine(),
            looks_garbled: self.looks_garbled(),
        }
    }
}

/// Fewest ASCII letters a page must contribute before its letter statistics
/// mean anything.
///
/// Below this the ratios are noise: a caption or a folio number can be a
/// long way from any language's letter distribution without being garbled,
/// so the verdict never fires and a caller reporting a score should say
/// nothing rather than report one.
pub const MIN_LETTERS_FOR_GARBLE_SCORE: usize = 200;

/// One page's letter-frequency evidence: the correlation that separates
/// substitution-cipher garble from natural text, and the verdict it feeds.
///
/// A broken ToUnicode CMap shifts every character by a per-range constant,
/// so `Certificate` extracts as `8VceZWZTReV`: printable ASCII, word-like
/// token lengths, no replacement characters, and a letter histogram that is
/// a permutation of a natural one. That is what these two numbers measure,
/// and until they were public only the boolean they produce escaped the
/// crate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LetterFrequencyScore {
    /// ASCII letters the page's runs contributed. Compare against
    /// [`MIN_LETTERS_FOR_GARBLE_SCORE`] before reading anything into the
    /// rest.
    pub ascii_letters: usize,
    /// Cosine similarity between the page's letter histogram and English
    /// letter frequencies, which every Latin-script language in the crate's
    /// eval corpus scores at least 0.80 against. Substitution-cipher text
    /// scores around 0.53. 1.0 for a page with no ASCII letters at all.
    pub english_cosine: f64,
    /// The same comparison with both histograms sorted descending, so it
    /// measures the *shape* of the frequency profile and ignores which
    /// letter sits where. A substitution cipher is a bijection and
    /// preserves this exactly; non-linguistic ASCII (hex dumps, DNA) does
    /// not.
    pub english_shape_cosine: f64,
    /// Whether this page's statistics fire the crate's own garble verdict,
    /// which is [`MIN_LETTERS_FOR_GARBLE_SCORE`], a Latin-dominance check,
    /// a vowel-ratio floor and the two cosines above taken together.
    pub looks_garbled: bool,
}

impl LetterFrequencyScore {
    /// Whether the page contributed enough letters for the statistics to
    /// carry information.
    #[must_use]
    pub const fn is_measurable(&self) -> bool {
        self.ascii_letters >= MIN_LETTERS_FOR_GARBLE_SCORE
    }
}

/// What [`analyze_text_quality`] concluded about a set of extracted items.
#[derive(Debug, Default)]
pub struct TextQualityReport {
    /// 1-indexed pages whose text layer this analysis judged unusable.
    pub pages_needing_ocr: Vec<u32>,
    /// Whether any page reached a verdict at all.
    pub has_encoding_issues: bool,
    /// Why, per page. The reason strings are the `OCR_REASON_*` constants.
    pub reasons_by_page: BTreeMap<u32, Vec<String>>,
    /// The letter-frequency evidence behind the substitution-cipher half of
    /// the verdict, for every page that contributed text. A page with a
    /// clean text layer appears here too, with the score that says so.
    pub letter_frequency: BTreeMap<u32, LetterFrequencyScore>,
}

#[derive(Debug, Default)]
struct PageTextQualityEvidence {
    chars: usize,
    replacement_chars: usize,
    replacement_spans: usize,
    longest_replacement_run: usize,
    cipher_garble: CipherGarbleStats,
    symbol_soup: SymbolSoupStats,
}

/// Fewest counted characters a page must contribute before its symbol share
/// is read as a verdict. Below this a short caption full of operators or a
/// lone formula could carry a high share without the layer being broken.
pub const MIN_CHARS_FOR_SYMBOL_SOUP: usize = 200;

/// The share of a page's counted characters that must be rare ASCII symbols
/// before the page is called symbol soup.
///
/// Measured on real documents (2026-10-04): 2,812 pages of born-digital PDFs
/// (DP-Bench, DocLayNet, NapierOne) peak at 0.111 once numeric `%`, `$` and
/// `#` are set aside, the one page above that being a genuinely broken layer
/// at 0.24; 1,264 pages of Acrobat Distiller court opinions whose Type 3
/// fonts carry no ToUnicode sit at a median of 0.36. 0.15 keeps a margin on
/// both sides.
const SYMBOL_SOUP_SHARE: f64 = 0.15;

/// Symbol-soup statistics: how much of a page is ASCII punctuation that
/// prose almost never uses.
///
/// A font with no ToUnicode CMap and a custom encoding hands its glyph codes
/// straight through. When the codes are small integers (Type 3 fonts number
/// their glyphs from 1; subset Type 1C fonts from 3) they land on `!"#$%&'`,
/// so a page of prose extracts as `’!!"!9"5&%9 !" !!`: printable ASCII, no
/// replacement character, and too few letters for [`CipherGarbleStats`] to
/// measure. What gives it away is the symbols themselves. Prose uses `!`,
/// `"`, `&`, `*`, `+`, `<`, `>`, `@`, brackets and braces sparingly; this
/// output is a third symbols or more.
///
/// Two kinds of legitimate symbol runs are set aside before counting: dot
/// and dash leaders (any character repeated three or more times, as a table
/// of contents draws them), and the numeric uses of `%`, `$` and `#` (`49.8%`,
/// `$12`, `#3`), which a statistics table repeats in every cell.
#[derive(Debug, Default)]
struct SymbolSoupStats {
    counted: usize,
    symbols: usize,
}

impl SymbolSoupStats {
    fn add_text(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let ch = chars[i];
            let mut run_end = i + 1;
            while run_end < chars.len() && chars[run_end] == ch {
                run_end += 1;
            }
            if ch.is_whitespace() || run_end - i >= 3 {
                i = run_end;
                continue;
            }
            for at in i..run_end {
                self.counted += 1;
                if is_rare_symbol(&chars, at) {
                    self.symbols += 1;
                }
            }
            i = run_end;
        }
    }

    fn share(&self) -> Option<f64> {
        (self.counted >= MIN_CHARS_FOR_SYMBOL_SOUP)
            .then(|| self.symbols as f64 / self.counted as f64)
    }

    fn looks_garbled(&self) -> bool {
        self.share().is_some_and(|share| share >= SYMBOL_SOUP_SHARE)
    }
}

/// Whether the character at `at` is a symbol prose rarely uses, leaving out
/// `%` after a digit and `$` or `#` before one.
fn is_rare_symbol(chars: &[char], at: usize) -> bool {
    let ch = chars[at];
    if !matches!(
        ch,
        '!' | '"' | '#' | '$' | '%' | '&' | '*' | '+' | '<' | '=' | '>' | '@' | '[' | '\\'
            | ']' | '^' | '{' | '|' | '}' | '~'
    ) {
        return false;
    }
    let before_digit = chars.get(at + 1).is_some_and(char::is_ascii_digit);
    let after_digit = at > 0 && chars[at - 1].is_ascii_digit();
    match ch {
        '%' => !after_digit,
        '$' | '#' => !before_digit,
        _ => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextSpanIssueKind {
    Replacement,
    Strong,
}

pub fn analyze_text_quality(items: &[TextItem]) -> TextQualityReport {
    let mut reasons_by_page = BTreeMap::new();
    let mut evidence_by_page = BTreeMap::<u32, PageTextQualityEvidence>::new();

    for item in items {
        if !matches!(item.item_type, crate::types::ItemType::Text) {
            continue;
        }

        let evidence = evidence_by_page.entry(item.page).or_default();
        evidence.chars += item.text.chars().filter(|ch| !ch.is_whitespace()).count();
        evidence.cipher_garble.add_text(&item.text);
        evidence.symbol_soup.add_text(&item.text);

        match text_span_decoding_issue_kind(&item.text) {
            Some(TextSpanIssueKind::Strong) => {
                add_ocr_reason(
                    &mut reasons_by_page,
                    item.page,
                    OCR_REASON_SUSPECTED_GARBLED_TEXT,
                );
            }
            Some(TextSpanIssueKind::Replacement) => {
                let stats = replacement_text_stats(&item.text);
                evidence.replacement_chars += stats.0;
                evidence.replacement_spans += 1;
                evidence.longest_replacement_run = evidence.longest_replacement_run.max(stats.1);
            }
            None => {}
        }
    }

    let mut letter_frequency = BTreeMap::new();
    for (page, evidence) in evidence_by_page {
        letter_frequency.insert(page, evidence.cipher_garble.score());
        if reasons_by_page.contains_key(&page) {
            continue;
        }
        if page_replacement_evidence_needs_ocr(&evidence)
            || evidence.cipher_garble.looks_garbled()
            || evidence.symbol_soup.looks_garbled()
        {
            add_ocr_reason(
                &mut reasons_by_page,
                page,
                OCR_REASON_SUSPECTED_GARBLED_TEXT,
            );
        }
    }

    let pages_needing_ocr: Vec<u32> = reasons_by_page.keys().copied().collect();
    TextQualityReport {
        has_encoding_issues: !pages_needing_ocr.is_empty(),
        pages_needing_ocr,
        reasons_by_page,
        letter_frequency,
    }
}

pub(crate) fn region_items_have_decoding_issue(items: &[TextItem]) -> bool {
    items.iter().any(|item| {
        matches!(item.item_type, crate::types::ItemType::Text)
            && text_span_has_decoding_issue(&item.text)
    })
}

fn text_span_has_decoding_issue(text: &str) -> bool {
    text_span_decoding_issue_kind(text).is_some()
}

fn text_span_decoding_issue_kind(text: &str) -> Option<TextSpanIssueKind> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    if has_dollar_as_space_pattern(text)
        || has_private_use_text_run(text)
        || is_cid_garbage(text)
        || has_cid_control_token(text)
    {
        return Some(TextSpanIssueKind::Strong);
    }

    if has_replacement_text_run(text) {
        return Some(TextSpanIssueKind::Replacement);
    }

    None
}

fn replacement_text_stats(text: &str) -> (usize, usize) {
    let mut replacement = 0usize;
    let mut current_run = 0usize;
    let mut longest_run = 0usize;

    for ch in text.chars() {
        if ch == '\u{FFFD}' {
            replacement += 1;
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }

    (replacement, longest_run)
}

fn page_replacement_evidence_needs_ocr(evidence: &PageTextQualityEvidence) -> bool {
    if evidence.replacement_chars == 0 || evidence.chars == 0 {
        return false;
    }

    // If the entire page is only a short broken text layer, even a short
    // replacement run is enough evidence. On otherwise text-heavy pages,
    // require density so math formulas do not force full-page OCR.
    if evidence.chars <= 80 && evidence.longest_replacement_run >= 2 {
        return true;
    }

    let replacement_density_bps = evidence.replacement_chars * 10_000 / evidence.chars;
    let enough_bad_text = evidence.replacement_chars >= 12 && replacement_density_bps >= 500;
    let repeated_bad_spans = evidence.replacement_spans >= 3 && replacement_density_bps >= 250;
    let long_bad_run = evidence.longest_replacement_run >= 8 && replacement_density_bps >= 250;

    enough_bad_text || repeated_bad_spans || long_bad_run
}

fn has_replacement_text_run(text: &str) -> bool {
    let (replacement, longest_run) = replacement_text_stats(text);
    longest_run >= 2 || replacement >= 3
}

fn has_private_use_text_run(text: &str) -> bool {
    let mut total = 0usize;
    let mut private_use = 0usize;
    let mut current_run = 0usize;
    let mut longest_run = 0usize;

    for ch in text.chars() {
        if ch.is_whitespace() {
            current_run = 0;
            continue;
        }
        total += 1;
        if is_private_use_char(ch) {
            private_use += 1;
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }

    if private_use == 0 {
        return false;
    }

    longest_run >= 3 || (total >= 5 && private_use >= 2 && private_use * 2 >= total)
}

fn has_cid_control_token(text: &str) -> bool {
    text.split_whitespace().any(token_has_cid_control)
}

fn token_has_cid_control(token: &str) -> bool {
    let mut total = 0usize;
    let mut c1_control = 0usize;

    for ch in token.chars() {
        total += 1;
        if ('\u{0080}'..='\u{009F}').contains(&ch) {
            c1_control += 1;
        }
    }

    total >= 5 && c1_control >= 2 && c1_control * 20 >= total
}

fn is_private_use_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

/// Check if extracted text is predominantly garbage (non-alphanumeric).
///
/// Broken font encodings produce text like "----1-.-.-.___  --.-. .._ I_---."
/// where most characters are punctuation/symbols. Real text in any language
/// has >50% alphanumeric characters.
pub(crate) fn is_garbage_text(markdown: &str) -> bool {
    let mut alphanum = 0usize;
    let mut non_alphanum = 0usize;

    let chars: Vec<char> = markdown.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let mut run_end = i + 1;
        while run_end < chars.len() && chars[run_end] == ch {
            run_end += 1;
        }

        let is_decorative_leader = matches!(ch, '.' | '_' | '·') && run_end - i >= 3;
        if !is_decorative_leader {
            for &run_ch in &chars[i..run_end] {
                if run_ch.is_whitespace() {
                    continue;
                }
                // Skip markdown syntax chars that we add (not from the PDF)
                if matches!(run_ch, '#' | '*' | '|' | '-' | '\n') {
                    continue;
                }
                if run_ch.is_alphanumeric() {
                    alphanum += 1;
                } else {
                    non_alphanum += 1;
                }
            }
        }
        i = run_end;
    }

    let total = alphanum + non_alphanum;
    total >= 50 && alphanum * 2 < total
}

/// Detect garbage from failed CID-to-Unicode mapping on Identity-H fonts.
///
/// When CID values don't correspond to Unicode codepoints, the raw bytes often
/// produce characters in the C1 control range (U+0080–U+009F) or Private Use
/// Area, mixed with random Latin Extended characters.  Valid text in any
/// language almost never contains C1 controls.  We also fall back to the
/// general `is_garbage_text` check for non-alphanumeric-heavy patterns.
pub(crate) fn is_cid_garbage(text: &str) -> bool {
    if is_garbage_text(text) {
        return true;
    }
    let mut total = 0usize;
    let mut c1_control = 0usize;
    let mut high_latin = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        total += 1;
        // C1 control characters (U+0080–U+009F) — almost never in real text
        if ch == '·' {
            continue;
        }
        if ('\u{0080}'..='\u{009F}').contains(&ch) {
            c1_control += 1;
        }
        // High Latin-1 (U+00A0–U+00FF) — legitimate in Western European text
        // but when combined with ASCII in CID passthrough, indicates mojibake
        // from CID values being misinterpreted as Latin-1 characters.
        if ('\u{00A0}'..='\u{00FF}').contains(&ch) {
            high_latin += 1;
        }
    }
    if total < 5 {
        return false;
    }
    // If ≥5% of non-whitespace chars are C1 controls, it's garbage
    if c1_control >= 2 && c1_control * 20 >= total {
        return true;
    }
    // If ≥40% of non-whitespace chars are high Latin-1 AND the text has few
    // ASCII letters, it's likely CID-as-Latin-1 mojibake (Japanese/CJK PDFs
    // where CID values 0x80-0xFF become accented Latin characters).  Keep a
    // minimum length so short math tokens like "2×()×" do not route a clean
    // page to OCR.
    let ascii_letters = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    total >= 20 && high_latin * 5 >= total * 2 && ascii_letters * 3 < total
}

#[cfg(test)]
mod symbol_soup_tests {
    use super::SymbolSoupStats;

    fn stats(text: &str) -> SymbolSoupStats {
        let mut stats = SymbolSoupStats::default();
        stats.add_text(text);
        stats
    }

    const PROSE: &str = "The court held that the district court did not abuse its discretion \
        when it denied the motion to suppress, because the officers had probable cause to \
        search the vehicle once the dog alerted. We therefore affirm the judgment of the \
        district court in all respects and remand for resentencing consistent with this opinion.";

    #[test]
    fn prose_is_not_symbol_soup() {
        let stats = stats(PROSE);
        assert!(stats.share().expect("long enough to measure") < 0.01);
        assert!(!stats.looks_garbled());
    }

    #[test]
    fn glyph_codes_passed_through_as_low_ascii_are() {
        // A Distiller court opinion's Type 3 body text, as extracted.
        let soup = "::!&%0%/’ ’!!\"!9\"5&%9 !\" !! !\"%! !! &’, -!% &!!% 2\" #F-2G ! \"& H! \" !! \
            H\"8!9’$% % 5& ’ ! !\"%!!\"!!’& &\" !! & #F!!7G () $% # # %#& # #, # 3%\" \
            ())*+%H,%!’!\"\"’& !\"\"’ ! !. !! & !!&JFKL% ! KL !! !!&%& & \" KL G %012+- \
            3 ! &’ &!!!’ &’!!\"!!% ! \" ! ’&!! &’!&’ !!28\"% !!!\"\" &1\"!!# E-5<<A):C::5<%!9";
        let stats = stats(soup);
        assert!(stats.share().expect("long enough to measure") > 0.30);
        assert!(stats.looks_garbled());
    }

    #[test]
    fn leaders_and_numeric_symbols_do_not_count() {
        let toc = "1. Executive Summary ........................................ 4\n\
            2. Methodology ############################################## 9\n";
        let table = "Entry Level 49.8% 7.2% 2.6% 2.2% 6.4% 6.8% 8.0% 1.4% 1.5% 1.3% 4.0% \
            SFL L1 52.0% 9.4% 1.6% 3.0% 5.3% 5.1% 9.6% 4.3% 2.0% 0.3% 3.6% 3.1% 0.8% \
            Revenue $12,400 $9,100 $3,300 item #3 item #4 item #5 total $24,800 ";
        let stats = stats(&format!("{toc}{table}{table}"));
        assert_eq!(stats.symbols, 0, "{stats:?}");
        assert!(!stats.looks_garbled());
    }

    #[test]
    fn a_short_run_is_not_measured() {
        let stats = stats("!\"#$%& <=> @[]^");
        assert_eq!(stats.share(), None);
        assert!(!stats.looks_garbled());
    }
}
