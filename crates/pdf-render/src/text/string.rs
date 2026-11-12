//! PDF text strings (ISO 32000-2, 7.9.2.2): UTF-16BE after a byte order
//! mark, UTF-8 after one (PDF 2.0), otherwise PDFDocEncoding (annex D.3).
//! Bytes that are not valid in their encoding, and codes PDFDocEncoding
//! leaves undefined, become U+FFFD: decoding never fails.

/// PDFDocEncoding codes 0x80 to 0xA0 (annex D.3); 0x9F is undefined.
const HIGH: [char; 33] = [
    '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}',
    '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}',
    '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{0141}', '\u{0152}', '\u{0160}',
    '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}',
    '\u{20AC}',
];

/// PDFDocEncoding codes 0x18 to 0x1F: spacing accents (annex D.3).
const ACCENTS: [char; 8] = [
    '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}', '\u{02DC}',
];

/// The text of a PDF text string.
pub(crate) fn text_string(bytes: &[u8]) -> String {
    if let Some(units) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = units
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair));
        let mut text: String = char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        if units_left(bytes) {
            text.push(char::REPLACEMENT_CHARACTER);
        }
        return text;
    }
    if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(utf8).into_owned();
    }
    bytes.iter().map(|byte| pdf_doc(*byte)).collect()
}

/// A UTF-16 string with an odd byte count: the last byte is a broken unit.
fn units_left(bytes: &[u8]) -> bool {
    !bytes.len().is_multiple_of(2)
}

fn pdf_doc(byte: u8) -> char {
    match byte {
        0x18..=0x1F => ACCENTS
            .get(usize::from(byte - 0x18))
            .copied()
            .unwrap_or(char::REPLACEMENT_CHARACTER),
        0x7F | 0xAD => char::REPLACEMENT_CHARACTER,
        0x80..=0xA0 => HIGH
            .get(usize::from(byte - 0x80))
            .copied()
            .unwrap_or(char::REPLACEMENT_CHARACTER),
        _ => char::from(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_encodings() {
        assert_eq!(text_string(b"Caf\xe9 \x80 \x93 \xa0"), "Café • ﬁ €");
        assert_eq!(text_string(b"\x18\x1f"), "\u{02D8}\u{02DC}");
        assert_eq!(text_string(b"\x7f\x9f\xad"), "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(text_string(b"\xfe\xff\x00A\xd8\x35\xdc\x00"), "A\u{1D400}");
        assert_eq!(text_string("\u{FEFF}é".as_bytes()), "é");
        assert_eq!(text_string(b""), "");
    }

    #[test]
    fn broken_input_becomes_replacement_characters() {
        // A lone surrogate, then an odd byte.
        assert_eq!(
            text_string(b"\xfe\xff\xd8\x00\x00A\x00"),
            "\u{FFFD}A\u{FFFD}"
        );
        assert_eq!(text_string(b"\xef\xbb\xbf\xff"), "\u{FFFD}");
    }
}
