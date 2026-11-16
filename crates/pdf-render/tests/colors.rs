//! Colour spaces of paths and text (ISO 32000-2, 8.6; tranche 3.14):
//! ICCBased by its number of components, CalGray and CalRGB, Indexed, and
//! `Default` spaces, drawn as for images, without colour management. Lab,
//! Separation and DeviceN are still reported (`ColorSpace`), not drawn.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_document::Document;
use pdf_render::{RenderCache, RenderOptions, Rendering, Unsupported, render_page_cached};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

/// A 100 x 100 page whose content is `content`, with `/ColorSpace`
/// `spaces` (dictionary entries) in its resources and `more` objects from
/// 5; the page's own resources hold form /F (object 5) when `more` has it.
fn document(content: &str, spaces: &str, more: &[&str]) -> Document<MemorySource> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << /ColorSpace << {spaces} >> /XObject << /F 5 0 R >> >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
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
    let source = MemorySource::from_vec(out, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn render(document: &Document<MemorySource>, cache: &mut RenderCache) -> Rendering {
    let page = document.page(0).expect("page");
    render_page_cached(
        document.file(),
        page,
        &RenderOptions::default(),
        cache,
        &mut budget(),
    )
    .expect("render")
}

fn at(rendering: &Rendering, x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = rendering.bitmap.pixel(x, y).expect("inside");
    [r, g, b]
}

fn skipped(rendering: &Rendering) -> Vec<Unsupported> {
    rendering.skipped.iter().copied().collect()
}

/// An ICC profile stream of `n` components (its data is not read).
fn profile(n: u8) -> String {
    format!("<< /N {n} /Length 0 >>\nstream\n\nendstream")
}

const WHITE_POINT: &str = "/WhitePoint [0.9505 1 1.089]";

#[test]
fn paths_and_text_in_icc_and_cie_spaces_are_drawn() {
    // A square filled, then one stroked, in each space; RGB and gray as
    // their device spaces, CMYK by the conversion of 10.4.2.4.
    let gray = profile(1);
    let rgb = profile(3);
    let cmyk = profile(4);
    let spaces = format!(
        "/G1 [/ICCBased 6 0 R] /R1 [/ICCBased 7 0 R] /K1 [/ICCBased 8 0 R] \
         /G2 [/CalGray << {WHITE_POINT} >>] /R2 [/CalRGB << {WHITE_POINT} >>]"
    );
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    for (space, values, expected) in [
        ("/R1", "1 0 0", [255, 0, 0]),
        ("/G1", "0.6", [153, 153, 153]),
        ("/K1", "0 1 0 0", [255, 0, 255]),
        ("/G2", "0.2", [51, 51, 51]),
        ("/R2", "0 0 1", [0, 0, 255]),
    ] {
        let content = format!(
            "{space} cs {values} sc 10 10 30 30 re f \
             {space} CS {values} SC 8 w 64 14 22 22 re S"
        );
        let document = document(&content, &spaces, &[form, &gray, &rgb, &cmyk]);
        let page = render(&document, &mut RenderCache::default());
        assert!(skipped(&page).is_empty(), "{space}: {:?}", skipped(&page));
        assert_eq!(at(&page, 25, 75), expected, "{space} fill");
        assert_eq!(at(&page, 64, 75), expected, "{space} stroke");
    }
}

#[test]
fn indexed_colours_come_from_their_palette() {
    // Indexed over DeviceRGB and over an ICCBased gray: the index is
    // rounded and clamped to the palette (8.6.6.3).
    let gray = profile(1);
    let spaces = "/P [/Indexed /DeviceRGB 2 <FF0000 00FF00 0000FF>] \
                  /Q [/Indexed [/ICCBased 6 0 R] 1 <00 CC>]";
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let content = "/P cs 1 sc 0 0 20 20 re f 2.4 sc 20 0 20 20 re f 9 sc 40 0 20 20 re f \
                   -3 sc 60 0 20 20 re f /Q cs 1 sc 80 0 20 20 re f";
    let document = document(content, spaces, &[form, &gray]);
    let page = render(&document, &mut RenderCache::default());
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 10, 90), [0, 255, 0]);
    assert_eq!(at(&page, 30, 90), [0, 0, 255]);
    assert_eq!(at(&page, 50, 90), [0, 0, 255]);
    assert_eq!(at(&page, 70, 90), [255, 0, 0]);
    assert_eq!(at(&page, 90, 90), [204, 204, 204]);
}

#[test]
fn palettes_stay_right_across_renders_and_forms() {
    // A form with its own Indexed space, drawn by many renders sharing the
    // document's cache, as the tiles of a viewer do (the form's scope is
    // kept, its Indexed spaces resolved again by each render): every
    // render paints the same colours. Ten more palettes on the page: kept
    // across renders, 500 renders would have reached `MAX_PALETTES`.
    let mut content = String::from("/F Do /P cs 50 0 50 100 re f");
    let mut spaces = String::from("/P [/Indexed /DeviceRGB 0 <0000FF>]");
    for n in 0..10 {
        content.push_str(&format!(" /I{n} cs 0 sc 0 0 1 1 re f"));
        spaces.push_str(&format!(" /I{n} [/Indexed /DeviceGray 0 <{n:02X}>]"));
    }
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 50 100] \
                /Resources << /ColorSpace << /P [/Indexed /DeviceRGB 1 <FF0000 00FF00>] >> >> \
                /Length 26 >>\nstream\n/P cs 1 sc 0 0 50 100 re f\nendstream";
    let document = document(&content, &spaces, &[form]);
    let mut cache = RenderCache::default();
    for _ in 0..500 {
        let page = render(&document, &mut cache);
        assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
        assert_eq!(at(&page, 25, 50), [0, 255, 0]);
        // `cs` without `sc`: index 0 (8.6.8).
        assert_eq!(at(&page, 75, 50), [0, 0, 255]);
    }
    // A cache that keeps nothing draws the same.
    let page = render(&document, &mut RenderCache::new(0));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 25, 50), [0, 255, 0]);
    assert_eq!(at(&page, 75, 50), [0, 0, 255]);
}

#[test]
fn default_spaces_of_the_same_kind_keep_the_device_space() {
    // DefaultRGB as an RGB profile or CalRGB: drawn as DeviceRGB, not
    // reported (as for an image in that space). DefaultRGB as Lab, or as
    // a gray profile: another kind, reported and not painted.
    let content = "1 0 0 rg 0 0 100 100 re f";
    for (default, more, drawn) in [
        ("[/ICCBased 6 0 R]", profile(3), true),
        (
            "[/CalRGB << /WhitePoint [0.9505 1 1.089] >>]",
            profile(3),
            true,
        ),
        (
            "[/Lab << /WhitePoint [0.9505 1 1.089] >>]",
            profile(3),
            false,
        ),
        ("[/ICCBased 6 0 R]", profile(1), false),
    ] {
        let form =
            "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
        let document = document(content, &format!("/DefaultRGB {default}"), &[form, &more]);
        let page = render(&document, &mut RenderCache::default());
        if drawn {
            assert!(skipped(&page).is_empty(), "{default}: {:?}", skipped(&page));
            assert_eq!(at(&page, 50, 50), [255, 0, 0], "{default}");
        } else {
            assert_eq!(skipped(&page), vec![Unsupported::ColorSpace], "{default}");
            assert_eq!(at(&page, 50, 50), [255, 255, 255], "{default}");
        }
    }
}

#[test]
fn other_spaces_are_still_reported() {
    // Lab, Separation, DeviceN, an ICCBased profile of 2 components, an
    // Indexed space over Lab: reported, not painted (tranche 3.14 leaves
    // them for the tint functions).
    let function = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>";
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    for space in [
        "[/Lab << /WhitePoint [0.9505 1 1.089] >>]",
        "[/Separation /Spot /DeviceCMYK 6 0 R]",
        "[/DeviceN [/A /B] /DeviceCMYK 6 0 R]",
        "[/ICCBased 7 0 R]",
        "[/Indexed [/Lab << /WhitePoint [0.9505 1 1.089] >>] 0 <000000>]",
    ] {
        let document = document(
            "/S cs 0 0 100 100 re f",
            &format!("/S {space}"),
            &[form, function, &profile(2)],
        );
        let page = render(&document, &mut RenderCache::default());
        assert_eq!(skipped(&page), vec![Unsupported::ColorSpace], "{space}");
        assert_eq!(at(&page, 50, 50), [255, 255, 255], "{space}");
    }
}
