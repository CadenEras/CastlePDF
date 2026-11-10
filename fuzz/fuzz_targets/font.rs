//! Arbitrary bytes embedded as a TrueType program (`/FontFile2`), in a
//! simple font (first byte even) or a Type 0 `Identity-H` font (odd), then
//! every code shown on a small page (ADR-0005).
//! Properties: no panic, overflow or unbounded work; a font that cannot be
//! read is reported as `Text`, never fatal.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 4 << 20;
    limits.max_objects_visited = 100_000;
    Budget::new(limits, CancellationToken::new())
}

fn pdf(program: &[u8], cid: bool) -> Vec<u8> {
    let (font, codes) = if cid {
        (
            "<< /Type /Font /Subtype /Type0 /BaseFont /F /Encoding /Identity-H /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /F /FontDescriptor 6 0 R /W [0 [500 600] 10 300 700] >>] >>".to_owned(),
            (0u16..300).map(|c| format!("{c:04x}")).collect::<String>(),
        )
    } else {
        (
            "<< /Type /Font /Subtype /TrueType /BaseFont /F /FontDescriptor 6 0 R /Encoding << /Differences [65 /A /uni00E9 /g3] >> >>".to_owned(),
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
        b"<< /Type /FontDescriptor /FontName /F /Flags 4 /FontFile2 7 0 R >>".to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", program.len()).as_bytes(),
            program,
            b"\nendstream",
        ]
        .concat(),
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
    let bytes = pdf(program, first % 2 == 1);
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
});
