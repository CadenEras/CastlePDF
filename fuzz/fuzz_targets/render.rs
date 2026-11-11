//! Renders arbitrary bytes as the content stream of a 64 x 64 page whose
//! resources name ExtGStates (one with stream-valued `/TR` and `/HT`), a
//! device, an ICC and a pattern colour space, an XObject, two form XObjects
//! whose content is the same bytes, scaled and moved (`/X1`: a
//! transparency group without resources of its own; `/X2`: a plain form
//! whose resources name `/X1` and itself, so that forms nest and repeat),
//! a Type 3 font (`/T3`: codes 32 to 127 show one glyph whose procedure
//! is the same bytes, so that glyphs run `d0`, `d1` and text in their own
//! font, except `e`, an empty procedure), a shading, a pattern and a
//! property list. The first byte chooses the scale (1 to 8, so up to
//! 512 x 512 pixels); the rest is the content.
//! Properties: no panic in the interpreter or the rasteriser, no unbounded
//! work (operations and path work are limited), a successful rendering has
//! exactly `width * height * 4` bytes, and `render_area` of an inner
//! rectangle (off the cell grid) gives exactly the same pixels.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{PixelRect, RenderOptions, render_area, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

const PAGE: &[u8] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 64] /Resources << \
    /ExtGState << /GS0 5 0 R /GS1 7 0 R >> \
    /ColorSpace << /CS0 /DeviceRGB /CS1 [/ICCBased 6 0 R] /CS2 /Pattern >> \
    /XObject << /X0 6 0 R /X1 8 0 R /X2 9 0 R >> /Shading << /Sh0 6 0 R >> /Pattern << /P0 6 0 R >> \
    /Properties << /MC0 << /Type /OCG >> >> /Font << /T3 10 0 R >> \
    >> /Contents 4 0 R >>";

fn pdf(content: &[u8]) -> Vec<u8> {
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let mut form = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 64 64] /Matrix [0.5 0 0 0.5 8 8] \
         /Group << /S /Transparency >> /Length {} >>\nstream\n",
        content.len()
    )
    .into_bytes();
    form.extend_from_slice(content);
    form.extend_from_slice(b"\nendstream");
    let mut plain = format!(
        "<< /Type /XObject /Subtype /Form /BBox [4 4 60 60] /Matrix [0.8 0 0 0.8 6 6] \
         /Resources << /XObject << /X1 8 0 R /X2 9 0 R >> /ExtGState << /GS0 5 0 R >> >> \
         /Length {} >>\nstream\n",
        content.len()
    )
    .into_bytes();
    plain.extend_from_slice(content);
    plain.extend_from_slice(b"\nendstream");
    let names = "/g ".repeat(96);
    let type3 = format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
         /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /g 11 0 R /e 12 0 R >> \
         /Encoding << /Differences [32 {names} 101 /e] >> /FirstChar 32 /LastChar 32 /Widths [600] >>"
    )
    .into_bytes();
    let mut glyph = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    glyph.extend_from_slice(content);
    glyph.extend_from_slice(b"\nendstream");
    let bodies: [&[u8]; 12] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        PAGE,
        &stream,
        b"<< /Type /ExtGState /LW 2 /ca 0.5 /D [[3 1] 0] >>",
        b"<< /N 3 /Length 0 >>\nstream\n\nendstream",
        b"<< /TR 6 0 R /HT 6 0 R /CA 0.5 /BM /Multiply >>",
        &form,
        &plain,
        &type3,
        &glyph,
        b"<< /Length 0 >>\nstream\n\nendstream",
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
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1)
            .as_bytes(),
    );
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((scale, content)) = data.split_first() else {
        return;
    };
    let scale = u32::from(scale % 8) + 1;
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 1 << 20;
    limits.max_objects_visited = 100_000;
    limits.max_nesting_depth = 32;
    let Ok(source) = MemorySource::from_vec(pdf(content), &limits) else {
        return;
    };
    let mut budget = Budget::new(limits, CancellationToken::new());
    let Ok(document) = Document::open(source, &mut budget) else {
        return;
    };
    let Some(page) = document.page(0) else {
        return;
    };
    let mut options = RenderOptions::default();
    options.scale = f64::from(scale);
    options.limits.max_operations = 100_000;
    options.limits.max_path_work = 2_000_000;
    if let Ok(rendering) = render_page(document.file(), page, &options, &mut budget) {
        let bitmap = &rendering.bitmap;
        let side = 64 * scale;
        assert_eq!((bitmap.width(), bitmap.height()), (side, side));
        assert_eq!(bitmap.data().len(), (side * side * 4) as usize);
        // Moved by the input length, so that areas cross cell sides at
        // various places; x + width stays within the page.
        let shift = (content.len() as u32) % (side / 6).max(1);
        let area = PixelRect {
            x: side / 3 + shift,
            y: side / 5 + 1 + shift / 2,
            width: side / 2,
            height: side / 2,
        };
        // A budget of its own: the decoded-bytes limit is cumulative, and the
        // page rendering may have used most of it (images).
        let mut area_budget = Budget::new(limits, CancellationToken::new());
        let part = render_area(document.file(), page, &options, area, &mut area_budget)
            .expect("the area of a rendered page renders");
        assert_eq!(part.skipped, rendering.skipped);
        for y in 0..area.height {
            for x in 0..area.width {
                let got = part.bitmap.pixel(x, y).expect("inside the area");
                let want = bitmap.pixel(area.x + x, area.y + y).expect("inside the page");
                assert_eq!(
                    got, want,
                    "pixel ({x}, {y}) of the area: {got:?} != {want:?}"
                );
            }
        }
    }
});
