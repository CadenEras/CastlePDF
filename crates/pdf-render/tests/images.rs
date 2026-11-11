//! Image XObjects (ADR-0005, step 5.4a): colour spaces, bit depths, masks,
//! JPEG, limits checked before allocation, and what is reported instead of
//! drawn.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    Error, PixelRect, RenderOptions, Rendering, Unsupported, render_area, render_page,
};
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

fn render_with(bytes: Vec<u8>, page: usize, options: &RenderOptions) -> Result<Rendering, Error> {
    let document = document(bytes);
    let page = document.page(page).expect("page");
    render_page(document.file(), page, options, &mut budget())
}

fn render(bytes: Vec<u8>, page: usize) -> Rendering {
    render_with(bytes, page, &RenderOptions::default()).expect("rendering")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(repo(&format!("tests/corpus/public/{name}"))).expect("fixture")
}

/// RGB of the pixel at user point (x, y) of a page 200 high, at scale 1.
fn at(rendering: &Rendering, x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = rendering.bitmap.pixel(x, 200 - y).expect("inside");
    [r, g, b]
}

fn skipped(rendering: &Rendering) -> Vec<Unsupported> {
    rendering.skipped.iter().copied().collect()
}

/// A 200 x 200 page drawing image /I (`image`: dictionary entries and
/// data) over (50, 50, 100, 100) after `before`; `more` objects from 6.
fn with_image(before: &str, image: &str, data: &[u8], more: &[&str]) -> Vec<u8> {
    let content = format!("{before} q 100 0 0 100 50 50 cm /I Do Q");
    let mut bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /I 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        [
            format!(
                "<< /Type /XObject /Subtype /Image {image} /Length {} >>\nstream\n",
                data.len()
            )
            .as_bytes(),
            data,
            b"\nendstream",
        ]
        .concat(),
    ];
    bodies.extend(more.iter().map(|body| body.as_bytes().to_vec()));
    assemble(&bodies)
}

/// A 200 x 200 page whose content is `content` (bytes: inline image data
/// is binary), with `resources` (dictionary entries); `more` objects from 5.
fn with_content(content: &[u8], resources: &str, more: &[&str]) -> Vec<u8> {
    let mut bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {resources} >> /Contents 4 0 R >>").into_bytes(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            content,
            b"\nendstream",
        ]
        .concat(),
    ];
    bodies.extend(more.iter().map(|body| body.as_bytes().to_vec()));
    assemble(&bodies)
}

/// A PDF file of objects 1, 2, ... with `bodies`; object 1 is the catalog.
fn assemble(bodies: &[Vec<u8>]) -> Vec<u8> {
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

#[test]
fn the_image_fixture_draws_every_kind() {
    let page = render(fixture("images.pdf"), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // RGB rows (top red, then green, blue), gray ramp at the bottom.
    assert_eq!(at(&page, 50, 180), [255, 0, 0]);
    assert_eq!(at(&page, 50, 160), [0, 255, 0]);
    assert_eq!(at(&page, 50, 140), [0, 0, 255]);
    assert_eq!(at(&page, 15, 115), [0, 0, 0]);
    assert_eq!(at(&page, 60, 115), [170, 170, 170]);
    assert_eq!(at(&page, 80, 115), [255, 255, 255]);
    // Checkerboard: top-left cell white (bit 1), next black.
    assert_eq!(at(&page, 115, 185), [255, 255, 255]);
    assert_eq!(at(&page, 125, 185), [0, 0, 0]);
    // Indexed: yellow, cyan, magenta, black.
    assert_eq!(at(&page, 20, 50), [255, 255, 0]);
    assert_eq!(at(&page, 40, 50), [0, 255, 255]);
    assert_eq!(at(&page, 60, 50), [255, 0, 255]);
    assert_eq!(at(&page, 80, 50), [0, 0, 0]);
    // Stencil: the band is blue, the rest of the square the page.
    assert_eq!(at(&page, 115, 85), [0, 0, 255]);
    assert_eq!(at(&page, 180, 85), [255, 255, 255]);

    let page = render(fixture("images.pdf"), 1);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // SMask: left opaque green, right transparent (red background).
    assert_eq!(at(&page, 30, 150), [0, 160, 0]);
    assert_eq!(at(&page, 70, 150), [255, 0, 0]);
    // Colour key: value 0 masked out, 255 drawn white.
    assert_eq!(at(&page, 115, 150), [255, 0, 0]);
    assert_eq!(at(&page, 135, 150), [255, 255, 255]);
    // 16 bits: white then blue.
    assert_eq!(at(&page, 30, 50), [255, 255, 255]);
    assert_eq!(at(&page, 70, 50), [0, 0, 255]);
    // /Decode [1 0]: sample 0 is white.
    assert_eq!(at(&page, 130, 50), [255, 255, 255]);
    assert_eq!(at(&page, 170, 50), [0, 0, 0]);
}

#[test]
fn jpeg_images_are_decoded() {
    let page = render(fixture("jpeg.pdf"), 0);
    assert!(skipped(&page).is_empty());
    let near = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 6);
    assert!(
        near(at(&page, 50, 140), [220, 0, 0]),
        "{:?}",
        at(&page, 50, 140)
    );
    assert!(
        near(at(&page, 150, 140), [0, 0, 220]),
        "{:?}",
        at(&page, 150, 140)
    );
    assert!(
        near(at(&page, 100, 185), [0, 160, 0]),
        "{:?}",
        at(&page, 100, 185)
    );
    assert!(at(&page, 25, 80)[0] < 20 && at(&page, 175, 80)[0] > 235);
    // Adobe CMYK with /Decode [1 0 ...]: cyan and yellow, converted as for
    // paths (ISO 32000-2, 10.4.2.4).
    let page = render(fixture("jpeg.pdf"), 1);
    assert!(
        near(at(&page, 50, 150), [0, 255, 255]),
        "{:?}",
        at(&page, 50, 150)
    );
    assert!(
        near(at(&page, 150, 150), [255, 255, 0]),
        "{:?}",
        at(&page, 150, 150)
    );
}

#[test]
fn image_size_is_checked_before_decoding() {
    let gray = "/ColorSpace /DeviceGray /BitsPerComponent 8 /Width 4 /Height 4";
    let renders = |limit: u64| {
        let mut options = RenderOptions::default();
        options.limits.max_image_pixels = limit;
        render_with(with_image("", gray, &[128; 16], &[]), 0, &options)
    };
    let drawn = renders(16).expect("rendering");
    assert!(skipped(&drawn).is_empty());
    // One pixel over: the image is not decoded but reported; the rest of
    // the page is drawn.
    let refused = renders(15).expect("rendering");
    assert_eq!(skipped(&refused), [Unsupported::Image]);
    assert_eq!(at(&refused, 100, 100), [255, 255, 255]);
    // A huge declared size with a few bytes: not decoded, nothing allocated.
    let huge = "/ColorSpace /DeviceRGB /BitsPerComponent 8 /Width 65535 /Height 65535";
    let page = render(with_image("", huge, &[0; 3], &[]), 0);
    assert_eq!(skipped(&page), [Unsupported::Image]);
}

#[test]
fn what_is_not_drawn_is_reported() {
    let base = "/BitsPerComponent 8 /Width 2 /Height 2";
    for (image, data, more, expected) in [
        (
            format!("{base} /ColorSpace /DeviceGray /Filter /JPXDecode"),
            &[0u8; 4][..],
            vec![],
            Unsupported::Image,
        ),
        (
            format!("{base} /ColorSpace [/Lab << /WhitePoint [1 1 1] >>]"),
            &[0u8; 12][..],
            vec![],
            Unsupported::ColorSpace,
        ),
        // A colour space name absent from the resources: only the image
        // depends on it.
        (
            format!("{base} /ColorSpace /Missing"),
            &[0u8; 4][..],
            vec![],
            Unsupported::ColorSpace,
        ),
        (
            format!("{base} /ColorSpace /DeviceGray /Filter /DCTDecode"),
            &b"not a jpeg"[..],
            vec![],
            Unsupported::Image,
        ),
        (
            format!("{base} /ColorSpace /DeviceGray /SMask 6 0 R"),
            &[0u8; 4][..],
            vec![
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /JPXDecode /Length 4 >>\nstream\n\0\0\0\0\nendstream",
            ],
            Unsupported::SoftMask,
        ),
    ] {
        let page = render(with_image("", &image, data, &more), 0);
        assert_eq!(skipped(&page), [expected], "{image}");
        assert_eq!(at(&page, 100, 100), [255, 255, 255], "{image}");
    }
    // A form XObject without its required /BBox is not drawn (forms:
    // tests/forms.rs).
    let mut form = with_image("", "/BitsPerComponent 8 /Width 1 /Height 1", &[0], &[]);
    let text = String::from_utf8_lossy(&form).replace("/Subtype /Image", "/Subtype /Form");
    form = text.into_bytes();
    assert_eq!(skipped(&render(form, 0)), [Unsupported::XObject]);
}

#[test]
fn short_image_data_reads_as_zero() {
    // 2 x 2 gray, one byte given: the rest is black, drawn, not refused.
    let page = render(
        with_image(
            "",
            "/ColorSpace /DeviceGray /BitsPerComponent 8 /Width 2 /Height 2",
            &[255],
            &[],
        ),
        0,
    );
    assert!(skipped(&page).is_empty());
    assert_eq!(at(&page, 75, 125), [255, 255, 255]);
    assert_eq!(at(&page, 125, 125), [0, 0, 0]);
    assert_eq!(at(&page, 75, 75), [0, 0, 0]);
}

#[test]
fn a_stencil_mask_takes_each_fill_colour() {
    // The same 1 x 1 mask drawn red, then blue in another place.
    let bytes = with_image(
        "1 0 0 rg q 40 0 0 40 0 0 cm /I Do Q 0 0 1 rg",
        "/ImageMask true /Width 1 /Height 1",
        &[0],
        &[],
    );
    let page = render(bytes, 0);
    assert_eq!(at(&page, 20, 20), [255, 0, 0]);
    assert_eq!(at(&page, 100, 100), [0, 0, 255]);
}

#[test]
fn image_tiles_assemble_into_the_page() {
    let document = document(fixture("images.pdf"));
    let page = document.page(0).expect("page");
    let mut options = RenderOptions::default();
    options.scale = 2.3;
    let whole = render_page(document.file(), page, &options, &mut budget()).expect("page");
    let area = PixelRect {
        x: 97,
        y: 131,
        width: 256,
        height: 200,
    };
    let part = render_area(document.file(), page, &options, area, &mut budget()).expect("tile");
    for y in 0..area.height {
        for x in 0..area.width {
            let got = part.bitmap.pixel(x, y).expect("tile pixel");
            let want = whole
                .bitmap
                .pixel(area.x + x, area.y + y)
                .expect("page pixel");
            assert_eq!(got, want, "({x}, {y}) {got:?} != {want:?}");
        }
    }
}

#[test]
fn matte_colours_are_restored() {
    // A gray 1 x 1 image, premultiplied against a black matte with alpha
    // 127/255: stored 64 (c' = 0 + a x (128 - 0)), restored to about 128 and
    // drawn half transparent over red.
    let page = render(
        with_image(
            "1 0 0 rg 0 0 200 200 re f",
            "/ColorSpace /DeviceGray /BitsPerComponent 8 /Width 1 /Height 1 /SMask 6 0 R",
            &[64],
            &[
                "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Matte [0] /Length 1 >>\nstream\n\x7f\nendstream",
            ],
        ),
        0,
    );
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    let [r, g, b] = at(&page, 100, 100);
    // 0.5 x 128 + 0.5 x (255, 0, 0).
    assert!(
        r.abs_diff(192) <= 2 && g.abs_diff(64) <= 2 && b.abs_diff(64) <= 2,
        "{:?}",
        [r, g, b]
    );
}

/// `BI <entries> ID <data> EI` drawn over (50, 50, 100, 100) after `before`.
fn inline(before: &str, entries: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("{before} q 100 0 0 100 50 50 cm BI {entries} ID ").as_bytes(),
        data,
        b" EI Q",
    ]
    .concat()
}

#[test]
fn the_inline_image_fixture_draws_every_kind() {
    let page = render(fixture("inline-images.pdf"), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // RGB: red, green / blue, white.
    assert_eq!(at(&page, 30, 170), [255, 0, 0]);
    assert_eq!(at(&page, 70, 170), [0, 255, 0]);
    assert_eq!(at(&page, 30, 130), [0, 0, 255]);
    assert_eq!(at(&page, 70, 130), [255, 255, 255]);
    // Indexed through a resource, Flate: yellow, cyan, magenta, black.
    assert_eq!(at(&page, 120, 150), [255, 255, 0]);
    assert_eq!(at(&page, 140, 150), [0, 255, 255]);
    assert_eq!(at(&page, 160, 150), [255, 0, 255]);
    assert_eq!(at(&page, 180, 150), [0, 0, 0]);
    // Stencil with /D [1 0]: the band from top left is blue, the rest white.
    assert_eq!(at(&page, 15, 85), [0, 0, 255]);
    assert_eq!(at(&page, 80, 85), [255, 255, 255]);
    // Gray JPEG ramp.
    assert!(at(&page, 112, 50)[0] < 30, "{:?}", at(&page, 112, 50));
    assert!(at(&page, 188, 50)[0] > 225, "{:?}", at(&page, 188, 50));
}

#[test]
fn inline_images_are_drawn() {
    // Abbreviated keys and colour space: red then blue.
    let content = inline("", "/W 2 /H 1 /BPC 8 /CS /RGB", &[255, 0, 0, 0, 0, 255]);
    let page = render(with_content(&content, "", &[]), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 75, 100), [255, 0, 0]);
    assert_eq!(at(&page, 125, 100), [0, 0, 255]);
    assert_eq!(at(&page, 25, 100), [255, 255, 255]);
    // Full names are accepted too.
    let content = inline(
        "",
        "/Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray",
        &[0],
    );
    assert_eq!(
        at(&render(with_content(&content, "", &[]), 0), 100, 100),
        [0, 0, 0]
    );
}

#[test]
fn inline_image_colour_spaces_and_masks() {
    // /I over /G with a lookup string: entry 0 black, 1 white, 1 bit each.
    let content = inline("", "/W 2 /H 1 /BPC 1 /CS [/I /G 1 <00ff>]", &[0b0100_0000]);
    let page = render(with_content(&content, "", &[]), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 75, 100), [0, 0, 0]);
    assert_eq!(at(&page, 125, 100), [255, 255, 255]);
    // A colour space named in the resources.
    let content = inline("", "/W 1 /H 1 /BPC 8 /CS /Sp", &[1]);
    let resources = "/ColorSpace << /Sp [/Indexed /DeviceRGB 1 <ff0000 00ff00>] >>";
    let page = render(with_content(&content, resources, &[]), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 100, 100), [0, 255, 0]);
    // A stencil mask in the fill colour; /D [1 0] inverts it.
    let content = inline("0 0 1 rg", "/IM true /W 2 /H 1", &[0b0100_0000]);
    let page = render(with_content(&content, "", &[]), 0);
    assert_eq!(at(&page, 75, 100), [0, 0, 255]);
    assert_eq!(at(&page, 125, 100), [255, 255, 255]);
    let content = inline("0 0 1 rg", "/IM true /W 2 /H 1 /D [1 0]", &[0b0100_0000]);
    let page = render(with_content(&content, "", &[]), 0);
    assert_eq!(at(&page, 75, 100), [255, 255, 255]);
    assert_eq!(at(&page, 125, 100), [0, 0, 255]);
}

#[test]
fn inline_jpeg_images_are_decoded() {
    // `/F /DCT` with the PDF 2.0 length: JPEG data may contain ` EI `.
    let jpeg = std::fs::read(repo("tests/images/gray.jpg")).expect("jpeg");
    let content = inline(
        "",
        &format!("/W 32 /H 16 /BPC 8 /CS /G /F /DCT /L {}", jpeg.len()),
        &jpeg,
    );
    let page = render(with_content(&content, "", &[]), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // Gradient from black (left) to white (right).
    assert!(at(&page, 52, 100)[0] < 30, "{:?}", at(&page, 52, 100));
    assert!(at(&page, 148, 100)[0] > 225, "{:?}", at(&page, 148, 100));
}

#[test]
fn inline_images_not_drawn_are_reported() {
    for (entries, data, expected) in [
        // A filter of P3, a missing colour space, a missing size.
        (
            "/W 1 /H 1 /BPC 8 /CS /G /F /AHx",
            &b"00>"[..],
            Unsupported::Image,
        ),
        ("/W 1 /H 1 /BPC 8", &[0][..], Unsupported::ColorSpace),
        ("/W 1 /BPC 8 /CS /G", &[0][..], Unsupported::Image),
        (
            "/W 1 /H 1 /BPC 8 /CS [/Lab << /WhitePoint [1 1 1] >>]",
            &[0, 0, 0][..],
            Unsupported::ColorSpace,
        ),
    ] {
        let page = render(with_content(&inline("", entries, data), "", &[]), 0);
        assert_eq!(skipped(&page), [expected], "{entries}");
        assert_eq!(at(&page, 100, 100), [255, 255, 255], "{entries}");
    }
    // The size limit applies before decoding.
    let content = inline("", "/W 4 /H 4 /BPC 8 /CS /G", &[0; 16]);
    let mut options = RenderOptions::default();
    options.limits.max_image_pixels = 15;
    let page = render_with(with_content(&content, "", &[]), 0, &options).expect("rendering");
    assert_eq!(skipped(&page), [Unsupported::Image]);
    options.limits.max_image_pixels = 16;
    let page = render_with(with_content(&content, "", &[]), 0, &options).expect("rendering");
    assert!(skipped(&page).is_empty());
    // A colour space name absent from the resources: the image alone is
    // skipped, the rest of the page drawn.
    let content = inline(
        "0 0 1 rg 0 0 20 20 re f",
        "/W 1 /H 1 /BPC 8 /CS /Missing",
        &[0],
    );
    let page = render(with_content(&content, "", &[]), 0);
    assert_eq!(skipped(&page), [Unsupported::ColorSpace]);
    assert_eq!(at(&page, 10, 10), [0, 0, 255]);
    assert_eq!(at(&page, 100, 100), [255, 255, 255]);
    // A stencil mask has no colour space: a wrong /CS is not looked up.
    let content = inline("0 0 1 rg", "/IM true /W 1 /H 1 /CS /Missing", &[0]);
    let page = render(with_content(&content, "", &[]), 0);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 100, 100), [0, 0, 255]);
}

#[test]
fn a_damaged_xobject_is_skipped_not_fatal() {
    // /I names an object that is not a stream.
    let bytes = with_image("", "/Width 1 /Height 1", &[0], &[]);
    let text = String::from_utf8_lossy(&bytes)
        .replace("/XObject << /I 5 0 R >>", "/XObject << /I 1 0 R >>");
    let page = render(text.into_bytes(), 0);
    assert_eq!(skipped(&page), [Unsupported::XObject]);
}
