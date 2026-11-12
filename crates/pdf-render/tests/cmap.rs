//! CMap reader (ADR-0006, tranche 3.2): codespaces, `bfchar`, `bfrange`,
//! `cidchar`, `cidrange`, notdef ranges, `usecmap`, `/WMode`; invalid
//! entries skipped and counted; limits and cancellation.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_render::{CMap, CMapError, Code, MAX_CID, MAX_CODESPACES, MAX_DESTINATION_UNITS};
use pdf_syntax::{Budget, CancellationToken, LimitKind, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn parse(data: &str) -> CMap {
    CMap::parse(data.as_bytes(), &mut budget()).expect("CMap")
}

fn code(value: u32, len: u8) -> Code {
    Code { value, len }
}

fn unicode(cmap: &CMap, value: u32, len: u8) -> Option<String> {
    cmap.unicode(code(value, len))
}

/// The example of ISO 32000-2, 9.10.3 (example 2), with its preamble.
const SPEC_TO_UNICODE: &str = "/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS2) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS2 def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfrange
<0000> <005E> <0020>
<005F> <0061> [<00660066> <00660069> <00660066006C>]
endbfrange
1 beginbfchar
<3A51> <D840DC3E>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

#[test]
fn the_example_of_the_standard() {
    let cmap = parse(SPEC_TO_UNICODE);
    // Base range: each code adds one to the destination.
    assert_eq!(unicode(&cmap, 0x0000, 2).as_deref(), Some(" "));
    assert_eq!(unicode(&cmap, 0x0021, 2).as_deref(), Some("A"));
    assert_eq!(unicode(&cmap, 0x005E, 2).as_deref(), Some("~"));
    // Array range: one string per code, ligatures expanded.
    assert_eq!(unicode(&cmap, 0x005F, 2).as_deref(), Some("ff"));
    assert_eq!(unicode(&cmap, 0x0060, 2).as_deref(), Some("fi"));
    assert_eq!(unicode(&cmap, 0x0061, 2).as_deref(), Some("ffl"));
    // A surrogate pair: U+2003E.
    assert_eq!(unicode(&cmap, 0x3A51, 2).as_deref(), Some("\u{2003E}"));
    // Not mapped, or another length.
    assert_eq!(unicode(&cmap, 0x0062, 2), None);
    assert_eq!(unicode(&cmap, 0x21, 1), None);
    assert_eq!(cmap.skipped_entries(), 0);
    assert!(!cmap.is_vertical());
    assert_eq!(cmap.parent(), None);
}

#[test]
fn codes_of_mixed_lengths_follow_the_codespace() {
    // The codespace of ISO 32000-2, 9.7.6.2 (example), as in 90ms-RKSJ-H.
    let cmap = parse(
        "begincodespacerange <00> <80> <8140> <9FFC> <A0> <DF> <E040> <FBFC> endcodespacerange",
    );
    assert!(cmap.has_codespace());
    let next = |bytes: &[u8]| cmap.next_code(bytes);
    assert_eq!(next(b"\x41\x81\x40"), Some(code(0x41, 1)));
    assert_eq!(next(b"\x81\x40\x41"), Some(code(0x8140, 2)));
    assert_eq!(next(b"\xA5"), Some(code(0xA5, 1)));
    assert_eq!(next(b"\xE0\x40"), Some(code(0xE040, 2)));
    // Byte by byte, not numeric: 0x81FF is outside <8140> <9FFC>.
    // It matches one leading byte of a two-byte range: two bytes.
    assert_eq!(next(b"\x81\xFF"), Some(code(0x81FF, 2)));
    // No range starts with 0x90 0x30... 0x90 is in <8140> <9FFC>'s first
    // byte, so two bytes again; a byte in no range at all takes the
    // shortest length.
    assert_eq!(next(b"\x90\x30"), Some(code(0x9030, 2)));
    assert_eq!(next(b"\xFE\x41"), Some(code(0xFE, 1)));
    // Never more bytes than remain.
    assert_eq!(next(b"\x81"), Some(code(0x81, 1)));
    assert_eq!(next(b""), None);
    // Without codespace, one byte.
    assert_eq!(parse("").next_code(b"\x12\x34"), Some(code(0x12, 1)));
}

#[test]
fn cid_ranges_and_notdef() {
    let cmap = parse(
        "/WMode 1 def
        begincodespacerange <0000> <FFFF> endcodespacerange
        begincidrange <0020> <007E> 1 <8140> <817E> 633 endcidrange
        begincidchar <0041> 9999 endcidchar
        beginnotdefrange <0000> <001F> 1 endnotdefrange
        beginnotdefchar <FFFF> 7 endnotdefchar",
    );
    assert!(cmap.is_vertical());
    assert_eq!(cmap.cid(code(0x0020, 2)), Some(1));
    assert_eq!(cmap.cid(code(0x0040, 2)), Some(33));
    // Defined later: the cidchar wins over the range.
    assert_eq!(cmap.cid(code(0x0041, 2)), Some(9999));
    assert_eq!(cmap.cid(code(0x0042, 2)), Some(35));
    assert_eq!(cmap.cid(code(0x8141, 2)), Some(634));
    assert_eq!(cmap.cid(code(0x0010, 2)), None);
    assert_eq!(cmap.notdef(code(0x0010, 2)), Some(1));
    assert_eq!(cmap.notdef(code(0xFFFF, 2)), Some(7));
    assert_eq!(cmap.notdef(code(0x0020, 2)), None);
}

#[test]
fn the_latest_definition_wins_where_ranges_overlap() {
    let cmap = parse(
        "beginbfrange <00> <FF> <0041> endbfrange
        beginbfrange <10> <1F> <0061> endbfrange
        beginbfchar <15> <005A> endbfchar
        beginbfrange <00> <0F> <0030> endbfrange",
    );
    assert_eq!(unicode(&cmap, 0x05, 1).as_deref(), Some("5"));
    assert_eq!(unicode(&cmap, 0x10, 1).as_deref(), Some("a"));
    assert_eq!(unicode(&cmap, 0x15, 1).as_deref(), Some("Z"));
    assert_eq!(unicode(&cmap, 0x16, 1).as_deref(), Some("g"));
    assert_eq!(unicode(&cmap, 0x20, 1).as_deref(), Some("a"));
}

#[test]
fn invalid_entries_are_skipped_and_counted() {
    let cmap = parse(
        "begincodespacerange <00> <FF> <0000> <FF> <FF> <00> <0000000000> <FFFFFFFFFF> endcodespacerange
        beginbfchar
          <01> <0041>
          <02> /space
          <03> <004142>
          <04> <>
          <0000000000> <0041>
          <05> <D800>
        endbfchar
        beginbfrange
          <10> <0F> <0041>
          <10> <0011> <0041>
          <20> <22> [<0061> 5 <0063>]
          <30> <31> [<0061>]
          <40> <41> <0041>
        endbfrange
        begincidrange <00> <0F> -1 <10> <1F> 2 endcidrange",
    );
    assert_eq!(unicode(&cmap, 0x01, 1).as_deref(), Some("A"));
    // A lone high surrogate is stored but is not valid text.
    assert_eq!(unicode(&cmap, 0x05, 1), None);
    // An array range shorter than the range: codes past it are unmapped.
    assert_eq!(unicode(&cmap, 0x30, 1).as_deref(), Some("a"));
    assert_eq!(unicode(&cmap, 0x31, 1), None);
    assert_eq!(unicode(&cmap, 0x41, 1).as_deref(), Some("B"));
    assert_eq!(cmap.cid(code(0x11, 1)), Some(3));
    // Codespaces: 3 invalid; bfchar: 4; bfrange: 3; cidrange: 1.
    assert_eq!(cmap.skipped_entries(), 11);
    assert_eq!(cmap.next_code(b"\x41"), Some(code(0x41, 1)));
}

#[test]
fn a_single_byte_destination_is_latin_text() {
    let cmap = parse("beginbfchar <01> <E9> endbfchar beginbfrange <02> <03> <41> endbfrange");
    assert_eq!(unicode(&cmap, 0x01, 1).as_deref(), Some("é"));
    assert_eq!(unicode(&cmap, 0x03, 1).as_deref(), Some("B"));
}

#[test]
fn a_range_past_the_last_utf16_unit_maps_nothing_beyond() {
    let cmap = parse("beginbfrange <0000> <0003> <FFFE> endbfrange");
    assert_eq!(unicode(&cmap, 0, 2).as_deref(), Some("\u{FFFE}"));
    assert_eq!(unicode(&cmap, 1, 2).as_deref(), Some("\u{FFFF}"));
    assert_eq!(unicode(&cmap, 2, 2), None);
}

#[test]
fn usecmap_names_the_parent_and_inherit_merges_it_below() {
    let mut child = parse(
        "/Base usecmap
        begincodespacerange <00> <7F> endcodespacerange
        begincidchar <41> 500 endcidchar",
    );
    assert_eq!(child.parent(), Some(&b"Base"[..]));
    let parent = parse(
        "begincodespacerange <8140> <9FFC> endcodespacerange
        begincidrange <20> <7E> 1 <8140> <8142> 633 endcidrange
        beginbfchar <42> <0042> endbfchar",
    );
    child.inherit(&parent, &mut budget()).expect("merged");
    assert_eq!(child.cid(code(0x41, 1)), Some(500));
    assert_eq!(child.cid(code(0x42, 1)), Some(35));
    assert_eq!(child.cid(code(0x8141, 2)), Some(634));
    assert_eq!(unicode(&child, 0x42, 1).as_deref(), Some("B"));
    assert_eq!(child.next_code(b"\x81\x40"), Some(code(0x8140, 2)));
    assert_eq!(child.next_code(b"\x41"), Some(code(0x41, 1)));
}

#[test]
fn an_unterminated_block_is_an_error() {
    for data in [
        "beginbfchar <01> <0041>",
        "beginbfchar <01> <0041> beginbfrange endbfrange endbfchar",
        "begincidrange <00> <01> 1 endcmap",
    ] {
        assert!(
            matches!(
                CMap::parse(data.as_bytes(), &mut budget()),
                Err(CMapError::Unterminated { offset: 0 })
            ),
            "{data}"
        );
    }
}

#[test]
fn a_lexer_error_is_an_error() {
    let error = CMap::parse(b"beginbfchar <01> <00G1> endbfchar", &mut budget());
    assert!(matches!(
        error,
        Err(CMapError::Syntax(pdf_syntax::Error::Malformed { .. }))
    ));
}

#[test]
fn every_entry_is_charged_to_the_budget() {
    let entries: String = (0..1000)
        .map(|n| format!("<{n:04X}> <{n:04X}>\n"))
        .collect();
    let data = format!("beginbfchar\n{entries}endbfchar");
    let mut measured = budget();
    CMap::parse(data.as_bytes(), &mut measured).expect("CMap");
    let used = measured.decoded_bytes();
    assert!(used > 1000 * 16, "{used}");
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = used - 1;
    let mut short = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        CMap::parse(data.as_bytes(), &mut short),
        Err(CMapError::Syntax(pdf_syntax::Error::LimitExceeded {
            kind: LimitKind::DecodedBytes,
            ..
        }))
    ));
    let mut exact = Budget::new(
        {
            let mut limits = ResourceLimits::default();
            limits.max_decoded_bytes = used;
            limits
        },
        CancellationToken::new(),
    );
    assert!(CMap::parse(data.as_bytes(), &mut exact).is_ok());
}

#[test]
fn a_wide_range_is_not_expanded() {
    let mut measured = budget();
    let cmap = CMap::parse(
        b"begincidrange <00000000> <FFFFFFFF> 0 endcidrange",
        &mut measured,
    )
    .expect("CMap");
    assert!(measured.decoded_bytes() < 1024);
    assert_eq!(cmap.cid(code(0x0000_FFFF, 4)), Some(MAX_CID));
    // Past the highest CID (annex C): no CID.
    assert_eq!(cmap.cid(code(0x0001_0000, 4)), None);
    assert_eq!(cmap.cid(code(u32::MAX, 4)), None);
}

#[test]
fn cids_above_the_highest_are_skipped() {
    let cmap = parse(
        "begincidchar <01> 65535 <02> 65536 endcidchar begincidrange <10> <1F> 70000 endcidrange",
    );
    assert_eq!(cmap.cid(code(0x01, 1)), Some(65_535));
    assert_eq!(cmap.cid(code(0x02, 1)), None);
    assert_eq!(cmap.cid(code(0x10, 1)), None);
    assert_eq!(cmap.skipped_entries(), 2);
}

#[test]
fn a_bfrange_adds_to_the_last_utf16_unit() {
    // <00F0> + 0x10 = U+0100: the unit is incremented, not only its last byte.
    let cmap = parse("beginbfrange <00> <1F> <00F0> endbfrange");
    assert_eq!(unicode(&cmap, 0x10, 1).as_deref(), Some("\u{0100}"));
    assert_eq!(unicode(&cmap, 0x1F, 1).as_deref(), Some("\u{010F}"));
}

#[test]
fn a_misaligned_block_is_skipped_whole() {
    // A missing destination would pair 02 with 03 and 03 with 0043.
    let cmap = parse("beginbfchar <01> <0041> <02> <03> <0043> endbfchar");
    for value in 1..=3 {
        assert_eq!(unicode(&cmap, value, 1), None, "{value}");
    }
    assert_eq!(cmap.skipped_entries(), 3);
    let cmap = parse("begincidrange <00> <0F> 1 <10> endcidrange");
    assert_eq!(cmap.cid(code(0x01, 1)), None);
    assert_eq!(cmap.skipped_entries(), 2);
}

#[test]
fn a_destination_longer_than_the_limit_is_skipped() {
    let longest = "0041".repeat(MAX_DESTINATION_UNITS);
    let cmap = parse(&format!(
        "beginbfchar <01> <{longest}> <02> <{longest}0041> endbfchar"
    ));
    assert_eq!(
        unicode(&cmap, 0x01, 1).map(|text| text.len()),
        Some(MAX_DESTINATION_UNITS)
    );
    assert_eq!(unicode(&cmap, 0x02, 1), None);
    assert_eq!(cmap.skipped_entries(), 1);
}

#[test]
fn codespaces_are_deduplicated_and_bounded() {
    let many: String = (0..=MAX_CODESPACES)
        .map(|n| format!("<{:04X}> <{:04X}> <00> <FF> ", n, n))
        .collect();
    let cmap = parse(&format!("begincodespacerange {many} endcodespacerange"));
    // 1 one-byte range (repeated, kept once) and MAX_CODESPACES + 1
    // two-byte ranges: 258 distinct, the last 2 over the bound.
    assert_eq!(cmap.skipped_entries(), 2);
    assert_eq!(cmap.next_code(b"\x41"), Some(code(0x41, 1)));
}

#[test]
fn a_block_of_tokens_is_charged_before_it_grows() {
    let data = format!("beginbfchar {} endbfchar", "[".repeat(1_000_000));
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 1 << 20;
    let mut short = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        CMap::parse(data.as_bytes(), &mut short),
        Err(CMapError::Syntax(pdf_syntax::Error::LimitExceeded {
            kind: LimitKind::DecodedBytes,
            ..
        }))
    ));
}

#[test]
fn lookups_stay_fast_under_a_wide_range_with_many_exceptions() {
    // Identity plus exceptions: every lookup above the exceptions used to
    // walk all of them.
    let exceptions: String = (0..50_000u32)
        .map(|n| format!("<{:04X}> {}\n", n * 2, n))
        .collect();
    let cmap = parse(&format!(
        "begincidrange <0000> <FFFF> 0 endcidrange begincidchar\n{exceptions}endcidchar"
    ));
    let start = std::time::Instant::now();
    for value in 0..=0xFFFFu32 {
        let expected = if value % 2 == 0 && value < 100_000 {
            value / 2
        } else {
            value
        };
        assert_eq!(cmap.cid(code(value, 2)), Some(expected), "{value}");
    }
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn inherit_is_charged_to_the_budget() {
    let mut child = parse("begincidchar <41> 500 endcidchar");
    let parent = parse(SPEC_TO_UNICODE);
    let mut measured = budget();
    child
        .clone()
        .inherit(&parent, &mut measured)
        .expect("merged");
    let used = measured.decoded_bytes();
    assert!(used > 0);
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = used - 1;
    let mut short = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        child.clone().inherit(&parent, &mut short),
        Err(CMapError::Syntax(pdf_syntax::Error::LimitExceeded { .. }))
    ));
    limits.max_decoded_bytes = used;
    let mut exact = Budget::new(limits, CancellationToken::new());
    child
        .inherit(&parent, &mut exact)
        .expect("merged at the limit");
    assert_eq!(unicode(&child, 0x0021, 2).as_deref(), Some("A"));
}

#[test]
fn cancellation_stops_the_reading() {
    let token = CancellationToken::new();
    token.cancel();
    let mut cancelled = Budget::new(ResourceLimits::default(), token);
    assert!(matches!(
        CMap::parse(SPEC_TO_UNICODE.as_bytes(), &mut cancelled),
        Err(CMapError::Syntax(pdf_syntax::Error::Cancelled))
    ));
}
