//! Type 0 encodings beyond Identity-H (ADR-0006, tranche 3.4a): Identity-V
//! and `/W2`, embedded CMaps with notdef ranges, `usecmap` chains bounded
//! and cycles stopped, predefined parents not supported yet.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{PageText, RenderLimits, RenderOptions, Unsupported, page_text, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn document(bytes: Vec<u8>) -> Document<MemorySource> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn text(bytes: Vec<u8>, page: usize) -> PageText {
    let document = document(bytes);
    let page = document.page(page).expect("page");
    page_text(
        document.file(),
        page,
        &RenderLimits::default(),
        &mut budget(),
    )
    .expect("text")
}

/// A PDF of `bodies` numbered from 1 (bodies may hold binary streams).
fn pdf(bodies: &[Vec<u8>]) -> Vec<u8> {
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

fn stream(data: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!("<< /Length {}{extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// One page showing `content` in a Type 0 Arimo font (object 5) whose
/// `/Encoding` is `encoding` and whose CID font has `cid_extra`; `more` are
/// objects numbered from 8. CIDs are glyph ids.
fn page(content: &str, encoding: &str, cid_extra: &str, more: &[Vec<u8>]) -> Vec<u8> {
    let arimo = std::fs::read(repo("tests/fonts/arimo-subset.ttf")).expect("font");
    let mut bodies = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream(content.as_bytes(), ""),
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /A /Encoding {encoding} /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /A /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /CIDToGIDMap /Identity /DW 500{cid_extra} >>] >>"
        )
        .into_bytes(),
        b"<< /Type /FontDescriptor /FontName /A /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>".to_vec(),
        stream(&arimo, &format!(" /Length1 {}", arimo.len())),
    ];
    bodies.extend(more.iter().cloned());
    pdf(&bodies)
}

fn positions(text: &PageText) -> Vec<(f64, f64)> {
    text.glyphs.iter().map(|glyph| (glyph.x, glyph.y)).collect()
}

#[test]
fn identity_v_writes_down_with_the_default_metrics() {
    // DW2 [880 -1000]: each glyph moves down by the font size.
    let text = text(
        page(
            "BT /F1 20 Tf 100 200 Td <000F0010> Tj ET",
            "/Identity-V",
            "",
            &[],
        ),
        0,
    );
    assert_eq!(positions(&text), [(100.0, 200.0), (100.0, 180.0)]);
    assert!((text.glyphs[1].end_y - 160.0).abs() < 1e-9);
}

#[test]
fn w2_gives_each_cid_its_displacement() {
    // CID 15: -500; CIDs 16 to 20: -2000; others the DW2 default -900.
    let text = text(
        page(
            "BT /F1 10 Tf 0 100 Td <000F00100015> Tj ET",
            "/Identity-V",
            " /DW2 [800 -900] /W2 [15 [-500 250 800] 16 20 -2000 250 800]",
            &[],
        ),
        0,
    );
    let ys: Vec<f64> = positions(&text).iter().map(|(_, y)| *y).collect();
    assert_eq!(ys, [100.0, 95.0, 75.0]);
    assert!((text.glyphs[2].end_y - 66.0).abs() < 1e-9);
}

#[test]
fn a_tj_adjustment_moves_down_in_vertical_writing() {
    let text = text(
        page(
            "BT /F1 10 Tf 0 100 Td [<000F> 500 <0010>] TJ ET",
            "/Identity-V",
            "",
            &[],
        ),
        0,
    );
    // -1000 for the glyph, then -(500 / 1000) * 10 = -5.
    assert_eq!(positions(&text)[1], (0.0, 85.0));
}

#[test]
fn an_embedded_cmap_splits_codes_and_maps_notdef() {
    let cmap = b"begincodespacerange <00> <7F> <8000> <FFFF> endcodespacerange \
        begincidrange <41> <5A> 15 endcidrange \
        begincidchar <8001> 41 endcidchar \
        beginnotdefrange <00> <1F> 3 endnotdefrange";
    let bytes = page(
        "BT /F1 10 Tf 0 100 Td <41428001> Tj <01> Tj ET",
        "8 0 R",
        "",
        &[stream(cmap, " /Type /CMap")],
    );
    let text = text(bytes.clone(), 0);
    assert_eq!(text.glyphs.len(), 4);
    // One-byte codes advance by DW (500), the two-byte code too.
    let xs: Vec<f64> = positions(&text).iter().map(|(x, _)| *x).collect();
    assert_eq!(xs, [0.0, 5.0, 10.0, 15.0]);
    // Rendered without skipping text: CIDs 15, 16, 41 and the notdef CID 3.
    let document = document(bytes);
    let rendering = render_page(
        document.file(),
        document.page(0).expect("page"),
        &RenderOptions::default(),
        &mut budget(),
    )
    .expect("rendered");
    assert!(
        !rendering.skipped.contains(&Unsupported::Text),
        "{:?}",
        rendering.skipped
    );
}

/// An embedded CMap whose `/UseCMap` is object `parent`.
fn chained(parent: usize) -> Vec<u8> {
    stream(
        b"begincodespacerange <0000> <FFFF> endcodespacerange begincidrange <0000> <00FF> 0 endcidrange",
        &format!(" /Type /CMap /UseCMap {parent} 0 R"),
    )
}

#[test]
fn usecmap_chains_are_bounded_and_cycles_stopped() {
    // Objects 8.. form a chain: 8 uses 9, 9 uses 10, and so on.
    let leaf = stream(
        b"begincodespacerange <0000> <FFFF> endcodespacerange begincidrange <0000> <00FF> 0 endcidrange",
        " /Type /CMap",
    );
    let content = "BT /F1 10 Tf 0 100 Td <000F> Tj ET";
    // Four CMaps (the font's and three parents): followed.
    let four = page(
        content,
        "8 0 R",
        "",
        &[chained(9), chained(10), chained(11), leaf.clone()],
    );
    assert_eq!(text(four, 0).glyphs.len(), 1);
    // Five: past the bound, the font is not read and its bytes counted.
    let five = page(
        content,
        "8 0 R",
        "",
        &[chained(9), chained(10), chained(11), chained(12), leaf],
    );
    let five = text(five, 0);
    assert!(five.glyphs.is_empty());
    assert_eq!(five.unread_bytes, 2);
    // A CMap using itself ends at the bound too.
    let cycle = text(page(content, "8 0 R", "", &[chained(8)]), 0);
    assert!(cycle.glyphs.is_empty());
    assert_eq!(cycle.unread_bytes, 2);
}

#[test]
fn identity_parents_are_built_in_and_other_predefined_ones_unsupported() {
    let override_one = |parent: &str| {
        let cmap = format!(
            "/{parent} usecmap begincodespacerange <0000> <FFFF> endcodespacerange begincidchar <0001> 16 endcidchar"
        );
        page(
            "BT /F1 10 Tf 0 100 Td <000F0001> Tj ET",
            "8 0 R",
            "",
            &[stream(cmap.as_bytes(), " /Type /CMap")],
        )
    };
    assert_eq!(text(override_one("Identity-H"), 0).glyphs.len(), 2);
    // A predefined CJK parent comes with tranche 3.7b.
    let unsupported = text(override_one("UniJIS-UCS2-H"), 0);
    assert!(unsupported.glyphs.is_empty());
    assert!(unsupported.skipped.contains(&Unsupported::Text));
}

#[test]
fn the_fixture_renders_without_skipping_text() {
    let bytes = std::fs::read(repo("tests/corpus/public/cmaps.pdf")).expect("fixture");
    let document = document(bytes);
    for index in 0..2 {
        let rendering = render_page(
            document.file(),
            document.page(index).expect("page"),
            &RenderOptions::default(),
            &mut budget(),
        )
        .expect("rendered");
        assert!(
            rendering.skipped.is_empty(),
            "page {}: {:?}",
            index + 1,
            rendering.skipped
        );
    }
}
