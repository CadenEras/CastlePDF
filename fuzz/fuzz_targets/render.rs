//! Renders arbitrary bytes as the content stream of a 64 x 64 page whose
//! resources name ExtGStates (one with stream-valued `/TR` and `/HT`), a
//! device, an ICC and a pattern colour space, an XObject, a shading, a
//! pattern and a property list. The first byte chooses the scale (1 to 8,
//! so up to 512 x 512 pixels); the rest is the content.
//! Properties: no panic in the interpreter or the rasteriser, no unbounded
//! work (operations and path work are limited), a successful rendering has
//! exactly `width * height * 4` bytes, and `render_area` of an inner
//! rectangle (offset off the 4-pixel grid) gives the same pixels within 1.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_render::{PixelRect, RenderOptions, render_area, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

const PAGE: &[u8] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 64] /Resources << \
    /ExtGState << /GS0 5 0 R /GS1 7 0 R >> \
    /ColorSpace << /CS0 /DeviceRGB /CS1 [/ICCBased 6 0 R] /CS2 /Pattern >> \
    /XObject << /X0 6 0 R >> /Shading << /Sh0 6 0 R >> /Pattern << /P0 6 0 R >> \
    /Properties << /MC0 << /Type /OCG >> >> \
    >> /Contents 4 0 R >>";

fn pdf(content: &[u8]) -> Vec<u8> {
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let bodies: [&[u8]; 7] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        PAGE,
        &stream,
        b"<< /Type /ExtGState /LW 2 /ca 0.5 /D [[3 1] 0] >>",
        b"<< /N 3 /Length 0 >>\nstream\n\nendstream",
        b"<< /TR 6 0 R /HT 6 0 R /CA 0.5 /BM /Multiply >>",
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
        let area = PixelRect {
            x: side / 3 + 1,
            y: side / 5 + 1,
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
                assert!(
                    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                    "pixel ({x}, {y}) of the area: {got:?} != {want:?}"
                );
            }
        }
    }
});
