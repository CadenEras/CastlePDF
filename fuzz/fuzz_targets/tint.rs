//! Arbitrary bytes as tint transforms (tranche 3.15): the first byte
//! splits the rest into a PostScript calculator program (type 4) for a
//! Separation space over CMYK and the sample table (type 0, 8 bits) of a
//! three-colorant DeviceN space over RGB; a page paints with both at a few
//! tints, as a fill and a stroke. Bits 2 to 4 of the second byte pick
//! the functions' `/Domain` and `/Range` among valid, reversed, odd and
//! empty ones (a reversed one made clipping panic, found in review).
//! Properties: no panic, overflow or unbounded work; a function that
//! cannot be read or evaluated is reported, never fatal.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 16 << 20;
    limits.max_objects_visited = 100_000;
    Budget::new(limits, CancellationToken::new())
}

fn stream(dictionary: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).as_bytes(),
        data,
        b"\nendstream",
    ]
    .concat()
}

/// Domains and ranges of the program (1 input, 4 outputs) and the table
/// (3 inputs, 3 outputs), by `choice`.
fn intervals(choice: u8) -> (&'static str, &'static str, &'static str, &'static str) {
    match choice % 8 {
        0 => (
            "[0 1]",
            "[0 1 0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
        ),
        1 => (
            "[1 0]",
            "[0 1 0 1 0 1 0 1]",
            "[0 1 1 0 0 1]",
            "[0 1 0 1 0 1]",
        ),
        2 => (
            "[0 1]",
            "[1 0 0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
            "[0 1 1 0 0 1]",
        ),
        3 => ("[0 1 2]", "[0 1 0 1 0 1]", "[0 1 0 1]", "[0 1 0 1 0]"),
        4 => ("[]", "[]", "[]", "[]"),
        5 => (
            "[-1e30 1e30]",
            "[-1 2 -1 2 -1 2 -1 2]",
            "[0 0 0 0 0 0]",
            "[1 1 1 1 1 1]",
        ),
        6 => (
            "[0.5 0.5]",
            "[0 1 0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
        ),
        _ => (
            "[0 1]",
            "[0 1 0 1 0 1 0 1]",
            "[0 1 0 1 0 1]",
            "[1 0 1 0 1 0]",
        ),
    }
}

fn pdf(program: &[u8], table: &[u8], size: u8) -> Vec<u8> {
    let (program_domain, program_range, table_domain, table_range) = intervals(size >> 2);
    let content = b"/S cs 0.3 sc 0 0 20 20 re f /S CS 1 SC 4 w 30 30 10 10 re S \
        /N cs 0 0.5 1 sc 20 0 20 20 re f /N cs 0.25 0.75 0.1 scn 40 40 10 10 re f \
        /S cs 0 sc 0 40 10 10 re f /N CS 1 1 1 SC 2 w 0 0 m 64 64 l S";
    // The table's size per input: 1 to 4 samples, so at most 64 x 3.
    let n = u32::from(size % 4) + 1;
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 64] /Resources << /ColorSpace << \
          /S [/Separation /Spot /DeviceCMYK 5 0 R] /N [/DeviceN [/A /B /C] /DeviceRGB 6 0 R] \
          >> >> /Contents 4 0 R >>"
            .to_vec(),
        stream("", content),
        stream(
            &format!("/FunctionType 4 /Domain {program_domain} /Range {program_range}"),
            program,
        ),
        stream(
            &format!(
                "/FunctionType 0 /Domain {table_domain} /Range {table_range} /Size [{n} {n} {n}] /BitsPerSample 8"
            ),
            table,
        ),
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

fuzz_target!(|data: &[u8]| {
    let [split, size, rest @ ..] = data else {
        return;
    };
    let split = (usize::from(*split) * 8).min(rest.len());
    let (program, table) = rest.split_at(split);
    let bytes = pdf(program, table, *size);
    let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
        return;
    };
    let Ok(document) = Document::open(source, &mut budget()) else {
        return;
    };
    let Some(page) = document.page(0) else {
        return;
    };
    let _ = render_page(
        document.file(),
        page,
        &RenderOptions::default(),
        &mut budget(),
    );
});
