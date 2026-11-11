//! Fonts that replace a font the PDF names but does not embed, or whose
//! program is not read (Type 1) or damaged (ADR-0005, step 5.3): Arimo
//! (Helvetica, Arial), Tinos (Times) and Cousine (Courier), SIL OFL 1.1,
//! four styles each, reduced to the characters of the Standard, WinAnsi and
//! MacRoman encodings (`crates/pdf-render/fonts`, built by
//! `scripts/build-standard-fonts.sh`). Their widths match those of the
//! fonts they replace. Symbol and ZapfDingbats are replaced by CastlePDF
//! Symbols, glyphs of four Noto fonts (SIL OFL 1.1) merged by
//! `scripts/build-symbol-font.sh`; their widths come from Adobe's metrics.
//! The glyphs only resemble the original ones, so text drawn with these
//! fonts is reported as `FontSubstituted`.
//!
//! Without the `standard-fonts` feature no font is embedded and such text
//! is not drawn (`Text`).

/// A replacement font family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Sans,
    Serif,
    Mono,
}

/// Family and style that replace the font named \`name\` (subset prefix
/// removed) with descriptor \`flags\`, \`weight\` (\`/FontWeight\`) and
/// \`italic_angle\`: from the name first, then the descriptor (ISO 32000-2,
/// 9.8.2, table 121: \`FixedPitch\` 1, \`Serif\` 2, \`Italic\` 64, \`ForceBold\`
/// 262 144).
pub(crate) fn choose(
    name: &[u8],
    flags: i64,
    weight: Option<f64>,
    italic_angle: f64,
) -> (Family, bool, bool) {
    let lower = String::from_utf8_lossy(name).to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    let family = if has(&["courier", "mono", "consolas", "typewriter"]) || flags & 1 != 0 {
        Family::Mono
    } else if has(&[
        "times", "serif", "roman", "georgia", "garamond", "cambria", "palatino", "antiqua",
    ]) && !has(&["sans"])
        || (flags & 2 != 0 && !has(&["sans", "arial", "helvetica", "verdana", "calibri"]))
    {
        Family::Serif
    } else {
        Family::Sans
    };
    let bold = has(&["bold", "black", "heavy", "semibold", "demi"])
        || flags & 262_144 != 0
        || weight.is_some_and(|w| w >= 600.0);
    let italic = has(&["italic", "oblique"]) || flags & 64 != 0 || italic_angle != 0.0;
    (family, bold, italic)
}

/// The program of a replacement font, if the feature embeds them.
#[cfg(feature = "standard-fonts")]
pub(crate) fn program(family: Family, bold: bool, italic: bool) -> Option<&'static [u8]> {
    macro_rules! font {
        ($file:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/", $file)).as_slice()
        };
    }
    Some(match (family, bold, italic) {
        (Family::Sans, false, false) => font!("Arimo-Regular.ttf"),
        (Family::Sans, true, false) => font!("Arimo-Bold.ttf"),
        (Family::Sans, false, true) => font!("Arimo-Italic.ttf"),
        (Family::Sans, true, true) => font!("Arimo-BoldItalic.ttf"),
        (Family::Serif, false, false) => font!("Tinos-Regular.ttf"),
        (Family::Serif, true, false) => font!("Tinos-Bold.ttf"),
        (Family::Serif, false, true) => font!("Tinos-Italic.ttf"),
        (Family::Serif, true, true) => font!("Tinos-BoldItalic.ttf"),
        (Family::Mono, false, false) => font!("Cousine-Regular.ttf"),
        (Family::Mono, true, false) => font!("Cousine-Bold.ttf"),
        (Family::Mono, false, true) => font!("Cousine-Italic.ttf"),
        (Family::Mono, true, true) => font!("Cousine-BoldItalic.ttf"),
    })
}

#[cfg(not(feature = "standard-fonts"))]
pub(crate) fn program(_: Family, _: bool, _: bool) -> Option<&'static [u8]> {
    None
}

/// The program that replaces Symbol and ZapfDingbats, if the feature
/// embeds it.
#[cfg(feature = "standard-fonts")]
pub(crate) fn symbols() -> Option<&'static [u8]> {
    Some(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fonts/CastleSymbols-Regular.ttf"
        ))
        .as_slice(),
    )
}

#[cfg(not(feature = "standard-fonts"))]
pub(crate) fn symbols() -> Option<&'static [u8]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_14_standard_names_choose_their_family_and_style() {
        for (name, expected) in [
            (&b"Helvetica"[..], (Family::Sans, false, false)),
            (b"Helvetica-BoldOblique", (Family::Sans, true, true)),
            (b"Times-Roman", (Family::Serif, false, false)),
            (b"Times-BoldItalic", (Family::Serif, true, true)),
            (b"Courier-Oblique", (Family::Mono, false, true)),
            (b"Courier-Bold", (Family::Mono, true, false)),
        ] {
            assert_eq!(choose(name, 0, None, 0.0), expected, "{name:?}");
        }
    }

    #[test]
    fn other_names_and_flags_are_mapped() {
        assert_eq!(
            choose(b"Arial,Bold", 32, None, 0.0),
            (Family::Sans, true, false)
        );
        assert_eq!(
            choose(b"TimesNewRomanPSMT", 34, None, 0.0),
            (Family::Serif, false, false)
        );
        assert_eq!(
            choose(b"Georgia", 32, None, -12.0),
            (Family::Serif, false, true)
        );
        assert_eq!(
            choose(b"CourierNewPSMT", 33, None, 0.0),
            (Family::Mono, false, false)
        );
        assert_eq!(
            choose(b"NotoSans", 2, Some(700.0), 0.0),
            (Family::Sans, true, false)
        );
        // The flags decide when the name says nothing.
        assert_eq!(choose(b"F1", 2, None, 0.0), (Family::Serif, false, false));
        assert_eq!(
            choose(b"F2", 1 | 64, None, 0.0),
            (Family::Mono, false, true)
        );
    }

    #[cfg(feature = "standard-fonts")]
    #[test]
    fn every_style_is_a_font() {
        for family in [Family::Sans, Family::Serif, Family::Mono] {
            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                let program = program(family, bold, italic).expect("embedded");
                assert!(
                    skrifa::FontRef::new(program).is_ok(),
                    "{family:?} {bold} {italic}"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "standard-fonts"))]
mod width_tests {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::metrics::GlyphMetrics;
    use skrifa::raw::TableProvider;
    use skrifa::{FontRef, MetadataProvider};

    use super::*;

    /// Family, bold, italic, and (character, AFM width) pairs.
    type Case = (Family, bool, bool, &'static [(char, f64)]);

    /// Widths of the Adobe Core 14 AFM files (thousandths of an em) that the
    /// replacements must reproduce: the PDF positions text with them when a
    /// standard font has no `/Widths`.
    #[test]
    fn replacements_keep_the_standard_widths() {
        let cases: [Case; 5] = [
            (
                Family::Sans,
                false,
                false,
                &[
                    ('A', 667.0),
                    ('a', 556.0),
                    ('M', 833.0),
                    ('i', 222.0),
                    ('W', 944.0),
                    (' ', 278.0),
                ],
            ),
            (
                Family::Sans,
                true,
                false,
                &[('A', 722.0), ('a', 556.0), ('i', 278.0), ('M', 833.0)],
            ),
            (
                Family::Serif,
                false,
                false,
                &[
                    ('A', 722.0),
                    ('a', 444.0),
                    ('M', 889.0),
                    ('i', 278.0),
                    ('W', 944.0),
                    (' ', 250.0),
                ],
            ),
            (
                Family::Serif,
                true,
                false,
                &[('A', 722.0), ('a', 500.0), ('M', 944.0)],
            ),
            (
                Family::Mono,
                false,
                false,
                &[('A', 600.0), ('i', 600.0), ('W', 600.0), (' ', 600.0)],
            ),
        ];
        for (family, bold, italic, widths) in cases {
            let font =
                FontRef::new(program(family, bold, italic).expect("embedded")).expect("font");
            let units = f64::from(font.head().expect("head").units_per_em());
            let metrics = GlyphMetrics::new(&font, Size::unscaled(), LocationRef::default());
            let charmap = font.charmap();
            for (character, expected) in widths {
                let glyph = charmap.map(*character).expect("mapped");
                let advance = metrics.advance_width(glyph).expect("advance");
                let width = f64::from(advance) * 1000.0 / units;
                assert!(
                    (width - expected).abs() <= 1.0,
                    "{family:?} bold {bold}: {character:?} {width} != {expected}"
                );
            }
        }
    }
}
