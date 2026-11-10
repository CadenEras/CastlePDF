//! Arbitrary bytes as an image XObject drawn on a small page (ADR-0005).
//! The first four bytes choose its dictionary: width and height (1 to 64),
//! filter (none, Flate, DCT), colour space (Gray, RGB, CMYK, Indexed),
//! bits per component, stencil mask, /Decode and a colour key; the rest is
//! the data.
//! Properties: no panic, overflow or unbounded work; an image that cannot
//! be decoded is reported, never fatal.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 8 << 20;
    limits.max_objects_visited = 100_000;
    Budget::new(limits, CancellationToken::new())
}

fn pdf(choices: [u8; 4], data: &[u8]) -> Vec<u8> {
    let [a, b, c, d] = choices;
    let width = u32::from(a % 64) + 1;
    let height = u32::from(b % 64) + 1;
    let filter = match c % 3 {
        0 => "",
        1 => " /Filter /FlateDecode",
        _ => " /Filter /DCTDecode",
    };
    let space = match (c / 3) % 4 {
        0 => "/DeviceGray",
        1 => "/DeviceRGB",
        2 => "/DeviceCMYK",
        _ => "[/Indexed /DeviceRGB 3 <FF000000FF000000FF808080>]",
    };
    let bits = [1, 2, 4, 8, 16][usize::from(d % 5)];
    let image = if d & 0x80 != 0 {
        format!("/ImageMask true /Width {width} /Height {height}{filter}")
    } else {
        let decode = if d & 0x40 != 0 { " /Decode [1 0 1 0 1 0 1 0]" } else { "" };
        let key = if d & 0x20 != 0 { " /Mask [0 0]" } else { "" };
        format!(
            "/ColorSpace {space} /BitsPerComponent {bits} /Width {width} /Height {height}{filter}{decode}{key}"
        )
    };
    let content = "1 0 0 rg q 40 10 -10 30 12 4 cm /I Do Q q 0.5 0 0 0.5 1 1 cm /I Do Q";
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 48 48] /Resources << /XObject << /I 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        [
            format!("<< /Type /XObject /Subtype /Image {image} /Length {} >>\nstream\n", data.len()).as_bytes(),
            data,
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
    let Some((choices, data)) = data.split_first_chunk::<4>() else {
        return;
    };
    let bytes = pdf(*choices, data);
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
