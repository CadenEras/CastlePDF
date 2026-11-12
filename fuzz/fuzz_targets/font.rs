//! Arbitrary bytes embedded as a font program, then every code shown on a
//! small page (ADR-0005). The first byte chooses: bit 0, a simple font or a
//! Type 0 font; bit 1, a TrueType program (`/FontFile2`) or a bare CFF one
//! (`/FontFile3` `/Type1C` or `/CIDFontType0C`); bits 2 and 3, the Type 0
//! encoding (ADR-0006, tranche 3.4a): `Identity-H`, `Identity-V` with
//! `/W2`, an embedded CMap mixing one- and two-byte codes over `Identity-H`
//! by `usecmap`, or the same CMap vertical.
//! Properties: no panic, overflow or unbounded work; a font that cannot be
//! read is reported as `Text`, never fatal.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{RenderLimits, RenderOptions, page_text, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 4 << 20;
    limits.max_objects_visited = 100_000;
    Budget::new(limits, CancellationToken::new())
}

/// An embedded encoding CMap: one-byte codes 00-7F, two-byte codes
/// 8000-FFFF over `Identity-H`, a notdef range.
const CMAP: &str = "/Identity-H usecmap begincodespacerange <00> <7F> <8000> <FFFF> endcodespacerange \
    begincidrange <20> <7E> 3 endcidrange begincidchar <8001> 9 endcidchar \
    beginnotdefrange <00> <1F> 1 endnotdefrange";

fn pdf(program: &[u8], cid: bool, cff: bool, encoding: u8) -> Vec<u8> {
    let (simple, descendant, file, stream) = if cff {
        ("Type1", "CIDFontType0", "FontFile3", if cid { " /Subtype /CIDFontType0C" } else { " /Subtype /Type1C" })
    } else {
        ("TrueType", "CIDFontType2", "FontFile2", "")
    };
    let (font, codes) = if cid {
        (
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /F /Encoding {} /DescendantFonts [<< /Type /Font /Subtype /{descendant} /BaseFont /F /FontDescriptor 6 0 R /W [0 [500 600] 10 300 700] /W2 [3 [-800 250 880] 5 9 -1200 300 900] >>] >>",
                match encoding {
                    0 => "/Identity-H",
                    1 => "/Identity-V",
                    _ => "8 0 R",
                }
            ),
            (0u16..300).map(|c| format!("{c:04x}")).collect::<String>(),
        )
    } else {
        (
            format!("<< /Type /Font /Subtype /{simple} /BaseFont /F /FontDescriptor 6 0 R /Encoding << /Differences [65 /A /uni00E9 /g3] >> >>"),
            (0u16..256).map(|c| format!("{c:02x}")).collect::<String>(),
        )
    };
    let content = format!("BT /F1 9 Tf 1 Tr 2 30 Td 0.3 Tc 80 Tz <{codes}> Tj 0 -10 Td [<{codes}> -250 <41>] TJ ET");
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 40] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        font.into_bytes(),
        format!("<< /Type /FontDescriptor /FontName /F /Flags 4 /{file} 7 0 R >>").into_bytes(),
        [
            format!("<< /Length {}{stream} >>\nstream\n", program.len()).as_bytes(),
            program,
            b"\nendstream",
        ]
        .concat(),
        format!(
            "<< /Type /CMap{} /Length {} >>\nstream\n{CMAP}\nendstream",
            if encoding == 3 { " /WMode 1" } else { "" },
            CMAP.len()
        )
        .into_bytes(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes());
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

fuzz_target!(|data: &[u8]| {
    let Some((first, program)) = data.split_first() else {
        return;
    };
    let bytes = pdf(program, first & 1 == 1, first & 2 == 2, (first >> 2) & 3);
    let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
        return;
    };
    let Ok(document) = Document::open(source, &mut budget()) else {
        return;
    };
    let Some(page) = document.page(0) else {
        return;
    };
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    let _ = render_page(document.file(), page, &options, &mut budget());
    let _ = page_text(document.file(), page, &RenderLimits::default(), &mut budget());
});
