//! Search in the text of a page (ADR-0006, tranche 3.7): occurrences of a
//! query in the words of its lines, in reading order.
//!
//! The page reads as its words joined by single spaces, lines too, so that
//! a phrase can run from one line to the next. Text and query are compared
//! after the same folding, character by character:
//! - always: ligatures (`ﬁ` and the like) become their letters, typographic
//!   apostrophes and quotes their plain forms, any white space a single
//!   space, and soft hyphens vanish;
//! - unless `match_case`: letters are lowercased (`char::to_lowercase`);
//! - unless `match_diacritics`: then letters of Latin-1 and Latin Extended-A
//!   lose their accents (`é` is `e`, `œ` is `oe`), and combining marks
//!   vanish.
//!
//! Not handled, such text being found only as written: a word cut by a
//! hyphen at the end of a line; accents outside the two Latin blocks (Greek,
//! Vietnamese...); `ß` is not `ss`; and no Unicode normalization, so that
//! with `match_diacritics` a precomposed `é` differs from `e` followed by a
//! combining accent.
//!
//! Memory follows the page's folded text, at most `MAX_SEARCH_CHARS`
//! characters (12 bytes of place each): text beyond is not searched, and the
//! result says so (`Found::truncated`).

use crate::TextLine;

/// Longest query, in characters after folding.
pub const MAX_QUERY_CHARS: usize = 256;
/// Most characters of a page's folded text searched: an `/ActualText` of
/// tens of megabytes is not folded whole.
pub const MAX_SEARCH_CHARS: usize = 1 << 22;

/// How text and query are compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Upper and lower case differ.
    pub match_case: bool,
    /// Accented and plain letters differ.
    pub match_diacritics: bool,
}

/// A query ready to be searched: folded, white space collapsed, trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    chars: Vec<char>,
    /// Knuth-Morris-Pratt failure function of `chars`.
    failure: Vec<usize>,
    options: SearchOptions,
}

/// Why a query cannot be searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// Nothing left once folded and trimmed.
    Empty,
    /// Longer than `MAX_QUERY_CHARS` once folded.
    TooLong,
}

impl Query {
    pub fn new(text: &str, options: SearchOptions) -> Result<Self, QueryError> {
        let mut chars = Vec::new();
        let mut too_long = false;
        for c in text.chars() {
            fold(c, options, &mut |folded| {
                if is_space(folded) {
                    if chars.last().is_some_and(|last| *last != ' ') {
                        chars.push(' ');
                    }
                } else {
                    chars.push(folded);
                }
            });
            // Checked as it grows: an enormous query stops early.
            if chars.len() > MAX_QUERY_CHARS + 1 {
                too_long = true;
                break;
            }
        }
        if chars.last() == Some(&' ') {
            chars.pop();
        }
        if too_long || chars.len() > MAX_QUERY_CHARS {
            return Err(QueryError::TooLong);
        }
        if chars.is_empty() {
            return Err(QueryError::Empty);
        }
        let failure = failure(&chars);
        Ok(Self {
            chars,
            failure,
            options,
        })
    }

    pub fn options(&self) -> SearchOptions {
        self.options
    }
}

/// A place in the text of a page: a character of a word of a line, each
/// counted from 0 in reading order (`reading_lines`); `char` counts Unicode
/// scalar values of the word's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    pub line: usize,
    pub word: usize,
    pub char: usize,
}

/// An occurrence: from the character at `start` to the character before
/// `end`, in the same word or across words and lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHit {
    pub start: TextPosition,
    pub end: TextPosition,
}

/// The occurrences found in a page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub hits: Vec<SearchHit>,
    /// Occurrences may be left out: more than the limit, or text beyond
    /// `MAX_SEARCH_CHARS`.
    pub truncated: bool,
}

/// The occurrences of `query` in `lines`, in reading order, at most `limit`
/// of them; they never overlap, even where a ligature folds into two
/// (`ﬀ` is one occurrence of `f`, not two).
pub fn find(lines: &[TextLine], query: &Query, limit: usize) -> Found {
    find_words(
        lines
            .iter()
            .map(|line| line.words.iter().map(|word| word.text.as_str())),
        query,
        limit,
    )
}

/// As `find`, over lines given as the text of their words.
pub fn find_words<'a, L, W>(lines: L, query: &Query, limit: usize) -> Found
where
    L: IntoIterator<Item = W>,
    W: IntoIterator<Item = &'a str>,
{
    find_bounded(lines, query, limit, MAX_SEARCH_CHARS)
}

/// Place of a folded character: line, word and character indices; `line` is
/// `SPACE` for the space between words and lines.
type Place = [u32; 3];
const SPACE: u32 = u32::MAX;

fn find_bounded<'a, L, W>(lines: L, query: &Query, limit: usize, max_chars: usize) -> Found
where
    L: IntoIterator<Item = W>,
    W: IntoIterator<Item = &'a str>,
{
    let mut found = Found::default();
    // The folded text of the page, each character with the place it comes
    // from, built once.
    let mut text: Vec<char> = Vec::new();
    let mut origin: Vec<Place> = Vec::new();
    // Words, to stretch an occurrence over the marks that follow it.
    let mut words: Vec<Vec<&str>> = Vec::new();
    'lines: for (line_index, line) in lines.into_iter().enumerate() {
        let Some(line_place) = u32::try_from(line_index).ok().filter(|l| *l != SPACE) else {
            found.truncated = true;
            break;
        };
        let mut line_words = Vec::new();
        for (word_index, word) in line.into_iter().enumerate() {
            let Ok(word_place) = u32::try_from(word_index) else {
                found.truncated = true;
                words.push(line_words);
                break 'lines;
            };
            line_words.push(word);
            separate(&mut text, &mut origin);
            for (char_index, c) in word.chars().enumerate() {
                let (true, Ok(char_place)) = (text.len() < max_chars, u32::try_from(char_index))
                else {
                    found.truncated = true;
                    words.push(line_words);
                    break 'lines;
                };
                let place = [line_place, word_place, char_place];
                fold(c, query.options, &mut |folded| {
                    if is_space(folded) {
                        separate(&mut text, &mut origin);
                    } else {
                        text.push(folded);
                        origin.push(place);
                    }
                });
            }
        }
        words.push(line_words);
    }

    let position = |place: &Place| {
        let [line, word, char] = place.map(|index| index as usize);
        TextPosition { line, word, char }
    };
    let pattern = &query.chars;
    let mut matched = 0;
    let mut last_end: Option<TextPosition> = None;
    for (index, c) in text.iter().enumerate() {
        while matched > 0 && pattern.get(matched) != Some(c) {
            matched = query.failure.get(matched - 1).copied().unwrap_or(0);
        }
        if pattern.get(matched) == Some(c) {
            matched += 1;
        }
        if matched < pattern.len() {
            continue;
        }
        // Not overlapping: the next occurrence starts after this one.
        matched = 0;
        let first = index + 1 - pattern.len();
        let (Some(start), Some(last)) = (origin.get(first), origin.get(index)) else {
            continue;
        };
        if start[0] == SPACE || last[0] == SPACE {
            continue;
        }
        let start = position(start);
        // Inside the characters of the previous occurrence (a ligature
        // folded into several letters): not another one.
        if last_end.is_some_and(|end| start < end) {
            continue;
        }
        if found.hits.len() == limit {
            found.truncated = true;
            break;
        }
        let mut end = position(last);
        end.char += 1;
        if !query.options.match_diacritics {
            // Marks folded away still belong to the letter they follow.
            let word = words
                .get(end.line)
                .and_then(|line| line.get(end.word))
                .copied()
                .unwrap_or("");
            end.char += word
                .chars()
                .skip(end.char)
                .take_while(|c| is_mark(*c))
                .count();
        }
        last_end = Some(end);
        found.hits.push(SearchHit { start, end });
    }
    found
}

/// Adds the space between two words, once, never at the start.
fn separate(text: &mut Vec<char>, origin: &mut Vec<Place>) {
    if text.last().is_some_and(|last| *last != ' ') {
        text.push(' ');
        origin.push([SPACE; 3]);
    }
}

fn failure(pattern: &[char]) -> Vec<usize> {
    let mut failure = vec![0; pattern.len()];
    let mut length = 0;
    for index in 1..pattern.len() {
        while length > 0 && pattern.get(index) != pattern.get(length) {
            length = failure.get(length - 1).copied().unwrap_or(0);
        }
        if pattern.get(index) == pattern.get(length) {
            length += 1;
        }
        if let Some(slot) = failure.get_mut(index) {
            *slot = length;
        }
    }
    failure
}

fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// Combining diacritical marks (U+0300 to U+036F).
fn is_mark(c: char) -> bool {
    ('\u{300}'..='\u{36F}').contains(&c)
}

/// Base letters of U+00C0 to U+017F; `-` keeps the character as it is
/// (signs, letters without a base, and those spelled with two letters,
/// handled in `unaccented`).
const LATIN_BASES: &str = concat!(
    // U+00C0 to U+00FF.
    "AAAAAA-CEEEEIIIIDNOOOOO-OUUUUY--",
    "aaaaaa-ceeeeiiiidnooooo-ouuuuy-y",
    // U+0100 to U+017F.
    "AaAaAaCcCcCcCcDdDdEeEeEeEeEeGgGg",
    "GgGgHhHhIiIiIiIiIi--JjKk-LlLlLlL",
    "lLlNnNnNnn--OoOoOo--RrRrRrSsSsSs",
    "SsTtTtTtUuUuUuUuUuUuWwYyYZzZzZzs",
);

/// The letters of `c` without their accent, if it has one.
fn unaccented(c: char) -> Option<&'static str> {
    match c {
        'Æ' => Some("AE"),
        'æ' => Some("ae"),
        'Œ' => Some("OE"),
        'œ' => Some("oe"),
        'Ĳ' => Some("IJ"),
        'ĳ' => Some("ij"),
        '\u{C0}'..='\u{17F}' => {
            let index = (c as usize) - 0xC0;
            LATIN_BASES.get(index..=index).filter(|base| *base != "-")
        }
        _ => None,
    }
}

/// Folds one character into the characters it is compared as.
fn fold(c: char, options: SearchOptions, out: &mut impl FnMut(char)) {
    let compatible: &str = match c {
        '\u{AD}' => return,
        'ﬀ' => "ff",
        'ﬁ' => "fi",
        'ﬂ' => "fl",
        'ﬃ' => "ffi",
        'ﬄ' => "ffl",
        'ﬅ' | 'ﬆ' => "st",
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{2BC}' => "'",
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => "\"",
        _ => {
            fold_letter(c, options, out);
            return;
        }
    };
    for c in compatible.chars() {
        fold_letter(c, options, out);
    }
}

/// Lowercased first, so that a letter whose lowercase is accented (the
/// angstrom sign is `å`) loses its accent too.
fn fold_letter(c: char, options: SearchOptions, out: &mut impl FnMut(char)) {
    if options.match_case {
        unaccent(c, options, out);
    } else {
        for lower in c.to_lowercase() {
            unaccent(lower, options, out);
        }
    }
}

fn unaccent(c: char, options: SearchOptions, out: &mut impl FnMut(char)) {
    if !options.match_diacritics {
        if is_mark(c) {
            return;
        }
        if let Some(letters) = unaccented(c) {
            letters.chars().for_each(out);
            return;
        }
    }
    out(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOSE: SearchOptions = SearchOptions {
        match_case: false,
        match_diacritics: false,
    };

    fn search(lines: &[&[&str]], query: &str, options: SearchOptions) -> Vec<SearchHit> {
        let query = Query::new(query, options).expect("valid query");
        find_words(lines.iter().map(|line| line.iter().copied()), &query, 100).hits
    }

    fn at(line: usize, word: usize, char: usize) -> TextPosition {
        TextPosition { line, word, char }
    }

    #[test]
    fn latin_bases_cover_both_blocks() {
        assert_eq!(LATIN_BASES.chars().count(), 0x180 - 0xC0);
        assert!(LATIN_BASES.is_ascii());
        for (c, base) in [
            ('À', "A"),
            ('é', "e"),
            ('ç', "c"),
            ('ÿ', "y"),
            ('Ā', "A"),
            ('č', "c"),
            ('Đ', "D"),
            ('ę', "e"),
            ('ğ', "g"),
            ('İ', "I"),
            ('ı', "i"),
            ('ł', "l"),
            ('ň', "n"),
            ('ő', "o"),
            ('ř', "r"),
            ('š', "s"),
            ('ť', "t"),
            ('ů', "u"),
            ('ŵ', "w"),
            ('Ÿ', "Y"),
            ('ž', "z"),
            ('ſ', "s"),
            ('œ', "oe"),
        ] {
            assert_eq!(unaccented(c), Some(base), "{c}");
        }
        for c in ['×', '÷', 'ß', 'Þ', 'ĸ', 'Ŋ'] {
            assert_eq!(unaccented(c), None, "{c}");
        }
    }

    #[test]
    fn finds_words_and_phrases_across_lines() {
        let lines: &[&[&str]] = &[&["Le", "château", "domine"], &["la", "vallée."]];
        assert_eq!(
            search(lines, "chateau", LOOSE),
            [SearchHit {
                start: at(0, 1, 0),
                end: at(0, 1, 7)
            }]
        );
        // From one line to the next, any white space in the query.
        assert_eq!(
            search(lines, "domine \t la  VALLEE", LOOSE),
            [SearchHit {
                start: at(0, 2, 0),
                end: at(1, 1, 6)
            }]
        );
        // Inside a word, and several times.
        assert_eq!(
            search(lines, "a", LOOSE)
                .iter()
                .map(|hit| hit.start)
                .collect::<Vec<_>>(),
            [at(0, 1, 2), at(0, 1, 5), at(1, 0, 1), at(1, 1, 1)]
        );
        assert!(search(lines, "château domine la vallée. et", LOOSE).is_empty());
    }

    #[test]
    fn case_and_accents_matter_only_when_asked() {
        let lines: &[&[&str]] = &[&["Cœur", "ÉTÉ", "e\u{301}te\u{301}"]];
        assert_eq!(search(lines, "coeur", LOOSE).len(), 1);
        assert_eq!(search(lines, "ete", LOOSE).len(), 2);
        // A combining mark after the last letter is part of the occurrence.
        assert_eq!(
            search(lines, "ete", LOOSE)[1],
            SearchHit {
                start: at(0, 2, 0),
                end: at(0, 2, 5)
            }
        );
        let case = SearchOptions {
            match_case: true,
            ..LOOSE
        };
        assert_eq!(search(lines, "ete", case).len(), 1);
        assert_eq!(search(lines, "ETE", case).len(), 1);
        let accents = SearchOptions {
            match_diacritics: true,
            ..LOOSE
        };
        assert!(search(lines, "coeur", accents).is_empty());
        assert_eq!(search(lines, "cœur", accents).len(), 1);
        assert_eq!(search(lines, "été", accents).len(), 1);
    }

    #[test]
    fn ligatures_quotes_and_soft_hyphens_fold() {
        let lines: &[&[&str]] = &[&["l’eﬃcacité", "pré\u{AD}vue"]];
        assert_eq!(
            search(lines, "l'efficacite", LOOSE),
            [SearchHit {
                start: at(0, 0, 0),
                end: at(0, 0, 10)
            }]
        );
        // Inside the ligature: the occurrence covers the whole glyph.
        assert_eq!(
            search(lines, "ic", LOOSE),
            [SearchHit {
                start: at(0, 0, 3),
                end: at(0, 0, 5)
            }]
        );
        assert_eq!(search(lines, "prevue", LOOSE).len(), 1);
    }

    #[test]
    fn a_ligature_holds_one_occurrence_of_each_letter_it_folds_into() {
        let lines: &[&[&str]] = &[&["ﬀ", "œ"]];
        assert_eq!(
            search(lines, "f", LOOSE),
            [SearchHit {
                start: at(0, 0, 0),
                end: at(0, 0, 1)
            }]
        );
        assert_eq!(search(lines, "o", LOOSE).len(), 1);
        assert_eq!(search(lines, "e", LOOSE).len(), 1);
        // A letter whose lowercase only is accented.
        assert_eq!(
            search(&[&["\u{212B}ngstr\u{F6}m"]], "angstrom", LOOSE).len(),
            1
        );
    }

    #[test]
    fn the_folded_text_is_bounded() {
        let query = Query::new("b", LOOSE).unwrap();
        // Three characters searched: "a", " " and "a" of the second word.
        let found = find_bounded([["a", "ab"]], &query, 10, 3);
        assert!(found.hits.is_empty());
        assert!(found.truncated);
        let found = find_bounded([["a", "ab"]], &query, 10, 4);
        assert_eq!(found.hits.len(), 1);
        assert!(!found.truncated);
    }

    #[test]
    fn occurrences_never_overlap_and_stop_at_the_limit() {
        let lines: &[&[&str]] = &[&["aaaa"]];
        assert_eq!(search(lines, "aa", LOOSE).len(), 2);
        let query = Query::new("a", LOOSE).unwrap();
        let found = find_words([["aaaa"]], &query, 3);
        assert_eq!(found.hits.len(), 3);
        assert!(found.truncated);
        let found = find_words([["aaa"]], &query, 3);
        assert!(!found.truncated);
        // Partial matches fall back on the failure function.
        assert_eq!(search(&[&["aabaab", "aabaaab"]], "aabaaa", LOOSE).len(), 1);
        assert_eq!(search(&[&["abababc"]], "ababc", LOOSE).len(), 1);
    }

    #[test]
    fn white_space_inside_a_word_separates() {
        // An /ActualText may hold several words.
        let lines: &[&[&str]] = &[&["deux mots"], &["ici"]];
        assert_eq!(
            search(lines, "mots ici", LOOSE),
            [SearchHit {
                start: at(0, 0, 5),
                end: at(1, 0, 3)
            }]
        );
    }

    #[test]
    fn queries_are_bounded() {
        assert_eq!(Query::new(" \t\u{AD} ", LOOSE), Err(QueryError::Empty));
        let longest = "a".repeat(MAX_QUERY_CHARS);
        assert!(Query::new(&longest, LOOSE).is_ok());
        assert_eq!(
            Query::new(&format!("{longest}a"), LOOSE),
            Err(QueryError::TooLong)
        );
        // Folding can lengthen a query: counted after it.
        let ligatures = "ﬃ".repeat(MAX_QUERY_CHARS / 3 + 1);
        assert_eq!(Query::new(&ligatures, LOOSE), Err(QueryError::TooLong));
        // Trailing white space is not counted.
        assert!(Query::new(&format!("{longest}    "), LOOSE).is_ok());
        assert!(Query::new(&"a".repeat(1 << 20), LOOSE).is_err());
    }
}
