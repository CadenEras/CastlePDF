//! Text extraction (ADR-0006, tranche 3.3): each glyph's Unicode text from
//! `/ToUnicode` or its glyph name, unknown text reported, positions, forms,
//! limits, and no image decoded.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    Error, PageText, RenderLimitKind, RenderLimits, RenderOptions, UnicodeSource, Unsupported,
    page_text, render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
    std::fs::read(path).expect("fixture")
}

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn document(bytes: Vec<u8>) -> Document<MemorySource> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn text_with(bytes: Vec<u8>, page: usize, limits: &RenderLimits) -> Result<PageText, Error> {
    let document = document(bytes);
    let page = document.page(page).expect("page");
    page_text(document.file(), page, limits, &mut budget())
}

fn text(bytes: Vec<u8>, page: usize) -> PageText {
    text_with(bytes, page, &RenderLimits::default()).expect("text")
}

/// The glyphs' texts joined per baseline, unknown ones as U+FFFD.
fn lines(text: &PageText) -> Vec<String> {
    let mut lines: Vec<(f64, String)> = Vec::new();
    for glyph in &text.glyphs {
        let part = glyph.text.clone().unwrap_or_else(|| "\u{FFFD}".to_owned());
        match lines.last_mut() {
            Some((y, line)) if (*y - glyph.y).abs() < 1.0 => line.push_str(&part),
            _ => lines.push((glyph.y, part)),
        }
    }
    lines.into_iter().map(|(_, line)| line).collect()
}

fn count(text: &PageText, source: UnicodeSource) -> usize {
    text.glyphs.iter().filter(|g| g.source == source).count()
}

#[test]
fn to_unicode_and_glyph_names_give_the_text() {
    let page = text(fixture("text.pdf"), 0);
    assert_eq!(
        lines(&page),
        [
            "ToUnicode fine",
            "Identity text\u{1D400}",
            "Other length",
            "Café € çon fi"
        ]
    );
    // F1 (13 glyphs, the ligature one) and F2 (14) through /ToUnicode, F3
    // through codes of another length (12), F4 through glyph names (12).
    assert_eq!(count(&page, UnicodeSource::ToUnicode), 13 + 14);
    assert_eq!(count(&page, UnicodeSource::ToUnicodeOtherLength), 12);
    assert_eq!(count(&page, UnicodeSource::GlyphName), 12);
    assert_eq!(count(&page, UnicodeSource::Unknown), 0);
    assert_eq!(page.unreadable_to_unicode, 0);
}

#[test]
fn unknown_text_is_reported_not_guessed() {
    let page = text(fixture("text.pdf"), 1);
    assert_eq!(
        lines(&page),
        ["\u{FFFD}".repeat(7).as_str(), "Fallback", "Hidden"]
    );
    // A Type 0 font without /ToUnicode: no standard correspondence.
    assert_eq!(count(&page, UnicodeSource::Unknown), 7);
    // A damaged /ToUnicode is not used: glyph names instead, reported once.
    assert_eq!(page.unreadable_to_unicode, 1);
    assert_eq!(count(&page, UnicodeSource::GlyphName), 8);
}

#[test]
fn glyphs_carry_their_position_and_advance() {
    let page = text(fixture("text.pdf"), 0);
    let first = &page.glyphs[0];
    assert_eq!(first.text.as_deref(), Some("T"));
    assert!((first.x - 10.0).abs() < 1e-9 && (first.y - 170.0).abs() < 1e-9);
    assert!((first.size - 16.0).abs() < 1e-9);
    // The next glyph starts where this one ends, on the same baseline.
    let second = &page.glyphs[1];
    assert!(first.end_x > first.x);
    assert!((second.x - first.end_x).abs() < 1e-9);
    assert!((second.y - first.end_y).abs() < 1e-9);
}

#[test]
fn builtin_cff_codes_take_their_charset_names() {
    let page = text(fixture("cff.pdf"), 0);
    let lines = lines(&page);
    assert_eq!(lines[0], "CastlePDF CFF Type1C");
    assert_eq!(lines[1], "Builtin encoding");
    assert_eq!(lines[3], "OpenType CFF");
    // The CID-keyed font has no /ToUnicode: unknown, not guessed.
    assert!(lines[2].chars().all(|c| c == '\u{FFFD}'), "{}", lines[2]);
}

#[test]
fn type3_glyph_names_outside_the_glyph_list_are_unknown() {
    let page = text(fixture("type3.pdf"), 0);
    // /tri is no AGL name; /bar is "|".
    assert_eq!(lines(&page)[0], "\u{FFFD}||\u{FFFD}");
}

#[test]
fn text_inside_forms_is_extracted() {
    let content = "q 1 0 0 1 50 60 cm /X1 Do Q BT /F1 10 Tf 5 5 Td (Page) Tj ET";
    let form = "BT /F1 12 Tf 0 0 Td (Form) Tj ET";
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 6 0 R >> /XObject << /X1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 40] /Resources << /Font << /F1 6 0 R >> >> /Length {} >>\nstream\n{form}\nendstream",
            form.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let page = text(pdf(&bodies), 0);
    assert_eq!(lines(&page), ["Form", "Page"]);
    // The form's text is placed by the current transformation matrix.
    assert!((page.glyphs[0].x - 50.0).abs() < 1e-9 && (page.glyphs[0].y - 60.0).abs() < 1e-9);
}

#[test]
fn images_are_not_decoded_for_text() {
    // A 600 x 600 grey image (360 000 bytes) and some text, under a budget
    // of 200 000 decoded bytes: rendering decodes the image and fails,
    // extracting the text does not decode it.
    let content = "q 100 0 0 100 0 0 cm /Im1 Do Q BT /F1 10 Tf 5 5 Td (Text) Tj ET";
    let image = vec![0x80u8; 600 * 600];
    let mut image_object = format!(
        "<< /Type /XObject /Subtype /Image /Width 600 /Height 600 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",
        image.len()
    )
    .into_bytes();
    image_object.extend_from_slice(&image);
    image_object.extend_from_slice(b"\nendstream");
    let bodies = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        image_object,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let document = document(pdf_bytes(&bodies));
    let page = document.page(0).expect("page");
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 200_000;
    let mut rendering = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut rendering
        ),
        Err(Error::Syntax(pdf_syntax::Error::LimitExceeded { .. }))
    ));
    let mut extraction = Budget::new(limits, CancellationToken::new());
    let text = page_text(
        document.file(),
        page,
        &RenderLimits::default(),
        &mut extraction,
    )
    .expect("text");
    assert_eq!(lines(&text), ["Text"]);
    assert!(!text.skipped.contains(&Unsupported::Image));
}

#[test]
fn glyphs_are_bounded() {
    let mut limits = RenderLimits::default();
    limits.max_text_glyphs = 51;
    assert_eq!(
        text_with(fixture("text.pdf"), 0, &limits)
            .expect("text")
            .glyphs
            .len(),
        51
    );
    limits.max_text_glyphs = 50;
    assert!(matches!(
        text_with(fixture("text.pdf"), 0, &limits),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::TextGlyphs,
            limit: 50
        })
    ));
}

#[test]
fn glyphs_are_charged_to_the_budget() {
    let document = document(fixture("text.pdf"));
    let page = document.page(0).expect("page");
    let mut measured = budget();
    page_text(
        document.file(),
        page,
        &RenderLimits::default(),
        &mut measured,
    )
    .expect("text");
    let used = measured.decoded_bytes();
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = used - 1;
    let mut short = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        page_text(document.file(), page, &RenderLimits::default(), &mut short),
        Err(Error::Syntax(pdf_syntax::Error::LimitExceeded { .. }))
    ));
    let token = CancellationToken::new();
    token.cancel();
    let mut cancelled = Budget::new(ResourceLimits::default(), token);
    assert!(matches!(
        page_text(
            document.file(),
            page,
            &RenderLimits::default(),
            &mut cancelled
        ),
        Err(Error::Syntax(pdf_syntax::Error::Cancelled))
    ));
}

/// A PDF of `bodies` numbered from 1, with a classic cross-reference table.
fn pdf(bodies: &[String]) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = bodies.iter().map(|body| body.as_bytes().to_vec()).collect();
    pdf_bytes(&bodies)
}

/// `pdf` with bodies that may hold binary stream data.
fn pdf_bytes(bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// One page showing `content` with the fonts `fonts` (`/Fn` → object).
fn one_page(content: &str, fonts: &str, more: &[&str]) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << {fonts} >> >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    bodies.extend(more.iter().map(|body| (*body).to_owned()));
    pdf(&bodies)
}

#[test]
fn a_null_or_named_to_unicode_counts_as_absent() {
    for entry in ["null", "/Identity-H"] {
        let font =
            format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode {entry} >>");
        let page = text(
            one_page("BT /F1 10 Tf (Ab) Tj ET", "/F1 5 0 R", &[&font]),
            0,
        );
        assert_eq!(lines(&page), ["Ab"], "{entry}");
        assert_eq!(page.unreadable_to_unicode, 0, "{entry}");
        assert_eq!(count(&page, UnicodeSource::GlyphName), 2, "{entry}");
    }
}

#[test]
fn text_in_a_font_that_cannot_be_read_is_counted() {
    // A Type 0 font with a predefined CMap other than Identity-H is not read
    // yet (tranche 3.4): its glyphs are absent, their bytes counted.
    let font = "<< /Type /Font /Subtype /Type0 /BaseFont /F /Encoding /UniJIS-UCS2-H /DescendantFonts [<< /Subtype /CIDFontType2 >>] >>";
    let page = text(
        one_page(
            "BT /F1 10 Tf <00410042> Tj [<0043> 5 <0044>] TJ /F2 10 Tf (x) Tj ET",
            "/F1 5 0 R /F2 6 0 R",
            &[
                font,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            ],
        ),
        0,
    );
    assert_eq!(lines(&page), ["x"]);
    assert_eq!(page.unread_bytes, 8);
    assert!(page.skipped.contains(&Unsupported::Text));
}
