//! Arbitrary text searched for an arbitrary query (ADR-0006, tranche 3.7):
//! the first byte chooses the options, the bytes up to the first NUL are the
//! query, the rest the page, lines split at line feeds and words at spaces,
//! read as UTF-8 with replacement characters.
//! Properties: no panic, overflow or unbounded work; every occurrence lies
//! within the text, starts before it ends, and they follow each other in
//! reading order without overlapping.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_render::search::{Query, SearchOptions, find_words};

fuzz_target!(|data: &[u8]| {
    let Some((flags, rest)) = data.split_first() else {
        return;
    };
    let options = SearchOptions {
        match_case: flags & 1 != 0,
        match_diacritics: flags & 2 != 0,
    };
    let (query, page) = match rest.iter().position(|byte| *byte == 0) {
        Some(at) => (&rest[..at], &rest[at + 1..]),
        None => (rest, &[][..]),
    };
    let Ok(query) = Query::new(&String::from_utf8_lossy(query), options) else {
        return;
    };
    let page = String::from_utf8_lossy(page);
    let lines: Vec<Vec<&str>> = page
        .split('\n')
        .map(|line| line.split(' ').collect())
        .collect();
    let found = find_words(lines.iter().map(|line| line.iter().copied()), &query, 64);
    let mut previous = None;
    for hit in &found.hits {
        assert!(hit.start < hit.end);
        let word = |line: usize, word: usize| lines.get(line).and_then(|l| l.get(word));
        let start = word(hit.start.line, hit.start.word).expect("start in the text");
        assert!(hit.start.char < start.chars().count());
        let end = word(hit.end.line, hit.end.word).expect("end in the text");
        assert!(hit.end.char <= end.chars().count());
        assert!(previous.is_none_or(|end| end <= hit.start));
        previous = Some(hit.end);
    }
    assert!(found.hits.len() <= 64);
});
