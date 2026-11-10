//! `page_font_kinds`: the kinds of the fonts a page declares (ADR-0005,
//! step 5.1), from font dictionaries and descriptors only.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::collections::BTreeSet;

use pdf_document::Document;
use pdf_render::{FontKind, page_font_kinds};
use pdf_syntax::{Budget, CancellationToken, LimitKind, MemorySource, ResourceLimits};

/// A one-page file whose `/Resources` are `<< /Font << fonts >> >>`; `more`
/// are objects numbered from 4.
fn file(fonts: &str, more: &[&str]) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /Font << {fonts} >> >> >>"
        ),
    ];
    bodies.extend(more.iter().map(|body| (*body).to_owned()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
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

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn kinds_with(bytes: &[u8], budget: &mut Budget) -> Result<BTreeSet<FontKind>, pdf_render::Error> {
    let source =
        MemorySource::from_vec(bytes.to_vec(), &ResourceLimits::default()).expect("source");
    let document = Document::open(source, &mut self::budget()).expect("document");
    let page = document.page(0).expect("page");
    page_font_kinds(document.file(), page, budget)
}

fn kinds(fonts: &str, more: &[&str]) -> Vec<FontKind> {
    kinds_with(&file(fonts, more), &mut budget())
        .expect("classified")
        .into_iter()
        .collect()
}

const STREAM: &str = "/Length 0 >>\nstream\n\nendstream";

#[test]
fn simple_fonts_are_told_apart_by_their_program() {
    let more = [
        "<< /Type /FontDescriptor /FontFile 9 0 R >>",
        "<< /Type /FontDescriptor /FontFile2 9 0 R >>",
        "<< /Type /FontDescriptor /FontFile3 10 0 R >>",
        "<< /Type /FontDescriptor /FontFile3 11 0 R >>",
        "<< /Type /FontDescriptor /Flags 32 >>",
    ];
    let streams = [
        format!("<< {STREAM}"),
        format!("<< /Subtype /Type1C {STREAM}"),
        format!("<< /Subtype /OpenType {STREAM}"),
    ];
    let mut objects: Vec<&str> = more.to_vec();
    objects.extend(streams.iter().map(String::as_str));
    for (font, kind) in [
        (
            "<< /Type /Font /Subtype /Type1 /BaseFont /A /FontDescriptor 4 0 R >>",
            FontKind::Type1,
        ),
        (
            "<< /Type /Font /Subtype /TrueType /BaseFont /B /FontDescriptor 5 0 R >>",
            FontKind::TrueType,
        ),
        (
            "<< /Type /Font /Subtype /Type1 /BaseFont /C /FontDescriptor 6 0 R >>",
            FontKind::Type1C,
        ),
        (
            "<< /Type /Font /Subtype /TrueType /BaseFont /D /FontDescriptor 7 0 R >>",
            FontKind::OpenType,
        ),
        (
            "<< /Type /Font /Subtype /TrueType /BaseFont /Arial /FontDescriptor 8 0 R >>",
            FontKind::NotEmbedded,
        ),
        (
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            FontKind::Standard14,
        ),
        (
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Courier-Bold >>",
            FontKind::Standard14,
        ),
        (
            "<< /Type /Font /Subtype /MMType1 /BaseFont /Myriad >>",
            FontKind::NotEmbedded,
        ),
        ("<< /Type /Font /Subtype /Type3 >>", FontKind::Type3),
        ("<< /Type /Font /Subtype /Type42 >>", FontKind::Invalid),
        (
            "<< /Type /Font /Subtype /Type1 /FontDescriptor 99 0 R >>",
            FontKind::Invalid,
        ),
    ] {
        assert_eq!(kinds(&format!("/F1 {font}"), &objects), [kind], "{font}");
    }
}

#[test]
fn type0_fonts_take_the_kind_of_their_cid_font() {
    let more = [
        "<< /Type /FontDescriptor /FontFile2 9 0 R >>",
        "<< /Type /FontDescriptor /FontFile3 10 0 R >>",
        "<< /Type /FontDescriptor >>",
        "<< /Type /Font /Subtype /CIDFontType2 /FontDescriptor 4 0 R >>",
        "<< /Type /Font /Subtype /CIDFontType0 /FontDescriptor 5 0 R >>",
    ];
    let streams = [
        format!("<< {STREAM}"),
        format!("<< /Subtype /CIDFontType0C {STREAM}"),
    ];
    let mut objects: Vec<&str> = more.to_vec();
    objects.extend(streams.iter().map(String::as_str));
    for (font, kind) in [
        (
            "<< /Subtype /Type0 /DescendantFonts [7 0 R] >>",
            FontKind::CidTrueType,
        ),
        (
            "<< /Subtype /Type0 /DescendantFonts [8 0 R] >>",
            FontKind::CidType0C,
        ),
        (
            "<< /Subtype /Type0 /DescendantFonts [<< /Subtype /CIDFontType2 /FontDescriptor 6 0 R >>] >>",
            FontKind::CidNotEmbedded,
        ),
        (
            "<< /Subtype /Type0 /DescendantFonts [7 0 R 8 0 R] >>",
            FontKind::Invalid,
        ),
        ("<< /Subtype /Type0 >>", FontKind::Invalid),
    ] {
        assert_eq!(kinds(&format!("/F1 {font}"), &objects), [kind], "{font}");
    }
}

#[test]
fn a_page_lists_each_kind_once() {
    assert_eq!(
        kinds(
            "/F1 << /Subtype /Type1 /BaseFont /Times-Roman >> /F2 << /Subtype /Type1 /BaseFont /Symbol >> /F3 << /Subtype /Type3 >>",
            &[],
        ),
        [FontKind::Standard14, FontKind::Type3]
    );
    assert!(kinds("", &[]).is_empty());
}

#[test]
fn limits_stop_the_classification() {
    let bytes = file("/F1 4 0 R", &["<< /Subtype /Type1 /BaseFont /Helvetica >>"]);
    let mut limits = ResourceLimits::default();
    limits.max_objects_visited = 1;
    let mut short = Budget::new(limits, CancellationToken::new());
    assert!(matches!(
        kinds_with(&bytes, &mut short),
        Err(pdf_render::Error::Syntax(
            pdf_syntax::Error::LimitExceeded {
                kind: LimitKind::ObjectsVisited,
                ..
            }
        ))
    ));
}
