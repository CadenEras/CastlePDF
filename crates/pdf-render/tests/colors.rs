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
    // What cannot be read stays reported, not painted: a tint transform of
    // the wrong arity (1 input for 2 colorants, 4 outputs for an RGB
    // alternate), a missing one, an alternate that is a Pattern, a Lab
    // space without white point, an ICCBased profile of 2 components, an
    // Indexed space over Lab.
    let function = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>";
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    for space in [
        "[/DeviceN [/A /B] /DeviceCMYK 6 0 R]",
        "[/Separation /Spot /DeviceRGB 6 0 R]",
        "[/Separation /Spot /DeviceCMYK 9 0 R]",
        "[/Separation /Spot /Pattern 6 0 R]",
        "[/Lab << >>]",
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

/// Whether two colours are within `tolerance` on every channel.
fn near(got: [u8; 3], want: [u8; 3], tolerance: u8) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| g.abs_diff(w) <= tolerance)
}

#[test]
fn separation_and_device_n_colours_go_through_their_tint_transform() {
    // Tranche 3.15. Each space fills a square at tint 0.5 (or its own
    // values), with an exponential, a sampled and a calculator function,
    // over CMYK, gray, RGB, an ICC profile and Lab.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let magenta = "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1 >>";
    let gray = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";
    // Sampled, 2 samples of 3 outputs (8 bits): black then red.
    let sampled = "<< /FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2] \
                   /BitsPerSample 8 /Filter /ASCIIHexDecode /Length 13 >>\nstream\n000000FF0000>\nendstream";
    // Calculator: two colorants onto cyan and magenta.
    let program = "{ 0 0 }";
    let calculator = format!(
        "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n{program}\nendstream",
        program.len()
    );
    // Onto Lab: tint 1 is the L*a*b* of sRGB red.
    let lab_red = "<< /FunctionType 2 /Domain [0 1] /C0 [100 0 0] /C1 [53.24 80.09 67.2] /N 1 >>";
    let spaces = format!(
        "/M [/Separation /Magenta /DeviceCMYK 6 0 R] /G [/Separation /Black /DeviceGray 7 0 R] \
         /R [/Separation /Red /DeviceRGB 8 0 R] /N [/DeviceN [/Cyan /Magenta] /DeviceCMYK 9 0 R] \
         /I [/Separation /Gray [/ICCBased 10 0 R] 7 0 R] \
         /L [/Separation /Red [/Lab << {WHITE_POINT} >>] 11 0 R]"
    );
    let gray_profile = profile(1);
    let more = [
        form,
        magenta,
        gray,
        sampled,
        &calculator,
        &gray_profile,
        lab_red,
    ];
    for (content, expected, tolerance) in [
        ("/M cs 0.5 sc", [255, 128, 255], 1),
        ("/G cs 0.25 sc", [191, 191, 191], 1),
        ("/R cs 1 sc", [255, 0, 0], 0),
        ("/R cs 0.5 sc", [128, 0, 0], 1),
        ("/N cs 1 0.5 sc", [0, 128, 255], 1),
        ("/I cs 1 sc", [0, 0, 0], 0),
        ("/L cs 1 sc", [255, 0, 0], 3),
        // `cs` alone: every tint 1 (8.6.8).
        ("/M cs", [255, 0, 255], 0),
        ("/M CS 8 w 50 50 20 20 re S /M cs", [255, 0, 255], 0),
    ] {
        let document = document(&format!("{content} 0 0 100 100 re f"), &spaces, &more);
        let page = render(&document, &mut RenderCache::default());
        assert!(skipped(&page).is_empty(), "{content}: {:?}", skipped(&page));
        assert!(
            near(at(&page, 20, 20), expected, tolerance),
            "{content}: {:?}",
            at(&page, 20, 20)
        );
    }
}

#[test]
fn lab_colours_are_converted_to_srgb() {
    // L*a*b* of sRGB primaries and white, relative to D65 (and to D50,
    // adapted to D65 by the white point); a* and b* clamped to /Range.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    for (space, values, expected) in [
        (WHITE_POINT, "53.24 80.09 67.2", [255, 0, 0]),
        (WHITE_POINT, "87.73 -86.18 83.18", [0, 255, 0]),
        // b* beyond the default range (-100 to 100): a range that holds it.
        (
            "/WhitePoint [0.9505 1 1.089] /Range [-128 127 -128 127]",
            "32.3 79.19 -107.86",
            [0, 0, 255],
        ),
        (WHITE_POINT, "32.3 79.19 -107.86", [67, 0, 241]),
        (WHITE_POINT, "100 0 0", [255, 255, 255]),
        (WHITE_POINT, "0 0 0", [0, 0, 0]),
        ("/WhitePoint [0.9642 1 0.8249]", "100 0 0", [255, 255, 255]),
        (
            "/WhitePoint [0.9505 1 1.089] /Range [0 0 0 0]",
            "53.24 80.09 67.2",
            [126, 126, 126],
        ),
    ] {
        let document = document(
            &format!("/L cs {values} sc 0 0 100 100 re f"),
            &format!("/L [/Lab << {space} >>]"),
            &[form],
        );
        let page = render(&document, &mut RenderCache::default());
        assert!(skipped(&page).is_empty(), "{values}: {:?}", skipped(&page));
        assert!(
            near(at(&page, 20, 20), expected, 3),
            "{space} {values}: {:?}",
            at(&page, 20, 20)
        );
    }
}

#[test]
fn the_none_colorant_paints_nothing() {
    // Separation /None (8.6.6.4): neither painted nor reported. Its tint
    // transform gives black, the page stays white; the same transform
    // under a named colorant paints black.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let function = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";
    for (colorant, expected) in [("None", [255, 255, 255]), ("Black", [0, 0, 0])] {
        let document = document(
            "/S cs 1 sc 20 20 60 60 re f",
            &format!("/S [/Separation /{colorant} /DeviceGray 6 0 R]"),
            &[form, function],
        );
        let page = render(&document, &mut RenderCache::default());
        assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
        assert_eq!(at(&page, 50, 50), expected, "{colorant}");
    }
}

#[test]
fn a_tint_transform_that_fails_paints_black_and_says_so() {
    // A calculator program, 1 / tint, that divides by zero at tint 0: no
    // colour, so black, and the colour space reported; tint 1 gives white.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let program = "{ 1 exch div }";
    let function = format!(
        "<< /FunctionType 4 /Domain [0 1] /Range [0 1] /Length {} >>\nstream\n{program}\nendstream",
        program.len()
    );
    for (tint, expected, reported) in [("1", [255, 255, 255], false), ("0", [0, 0, 0], true)] {
        let document = document(
            &format!("/S cs {tint} sc 0 0 100 100 re f"),
            "/S [/Separation /Spot /DeviceGray 6 0 R]",
            &[form, &function],
        );
        let page = render(&document, &mut RenderCache::default());
        assert_eq!(
            skipped(&page),
            if reported {
                vec![Unsupported::ColorSpace]
            } else {
                vec![]
            },
            "{tint}"
        );
        assert_eq!(at(&page, 50, 50), expected, "{tint}");
    }
}

#[test]
fn stitching_and_bounded_sampled_functions_read_from_the_file() {
    // A stitching function (type 3) of two exponential ones: white to
    // black over tints 0 to 0.5, then black. A sampled table of 2^21 x 3
    // samples (over the bound of 2^22) is refused before it is read: the
    // space is reported.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let stitching = "<< /FunctionType 3 /Domain [0 1] /Bounds [0.5] /Encode [0 1 0 1] \
                     /Functions [<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >>] >>";
    let huge = "<< /FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2097152] \
                /BitsPerSample 8 /Length 0 >>\nstream\n\nendstream";
    let spaces = "/T [/Separation /T /DeviceGray 6 0 R] /H [/Separation /H /DeviceRGB 7 0 R]";
    for (content, expected, reported) in [
        ("/T cs 0.25 sc", [128, 128, 128], false),
        ("/T cs 0.75 sc", [0, 0, 0], false),
        ("/H cs 0.5 sc", [255, 255, 255], true),
    ] {
        let document = document(
            &format!("{content} 0 0 100 100 re f"),
            spaces,
            &[form, stitching, huge],
        );
        let page = render(&document, &mut RenderCache::default());
        assert_eq!(
            skipped(&page),
            if reported {
                vec![Unsupported::ColorSpace]
            } else {
                vec![]
            },
            "{content}"
        );
        assert!(
            near(at(&page, 50, 50), expected, 1),
            "{content}: {:?}",
            at(&page, 50, 50)
        );
    }
}

#[test]
fn reversed_domains_and_ranges_are_refused_not_fatal() {
    // Found in review: /Domain [1 0] or /Range [1 0] made the clipping of
    // an input or output panic (min above max). Such a function is
    // refused, the space reported; also inside a stitching function.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    for function in [
        "<< /FunctionType 2 /Domain [1 0] /C0 [0] /C1 [1] /N 1 >>",
        "<< /FunctionType 2 /Domain [0 1] /Range [1 0] /C0 [0] /C1 [1] /N 1 >>",
        "<< /FunctionType 3 /Domain [0 1] /Bounds [] /Encode [0 1] \
         /Functions [<< /FunctionType 2 /Domain [1 0] /C0 [0] /C1 [1] /N 1 >>] >>",
    ] {
        let document = document(
            "/S cs 0.5 sc 0 0 100 100 re f",
            "/S [/Separation /Spot /DeviceGray 6 0 R]",
            &[form, function],
        );
        let page = render(&document, &mut RenderCache::default());
        assert_eq!(skipped(&page), vec![Unsupported::ColorSpace], "{function}");
    }
}

#[test]
fn malformed_function_entries_refuse_the_function() {
    // Found in review: an invalid entry was read as absent, so its default
    // applied (/C0 [true] as [0]). It refuses the function now, as do
    // stitching bounds outside the domain or out of order.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let exponential = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";
    for function in [
        "<< /FunctionType 2 /Domain [0 1] /C0 [true] /C1 [1] /N 1 >>",
        "<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 (one) /N 1 >>",
        "<< /FunctionType 2 /Domain [0 1 2] /C0 [0] /C1 [1] /N 1 >>",
        "<< /FunctionType 3 /Domain [0 1] /Bounds [1.5] /Encode [0 1 0 1] /Functions [7 0 R 7 0 R] >>",
        "<< /FunctionType 3 /Domain [0 1] /Bounds [/x] /Encode [0 1 0 1] /Functions [7 0 R 7 0 R] >>",
        "<< /FunctionType 3 /Domain [0 1] /Bounds [0.6 0.4] /Encode [0 1 0 1 0 1] \
         /Functions [7 0 R 7 0 R 7 0 R] >>",
    ] {
        let document = document(
            "/S cs 0.5 sc 0 0 100 100 re f",
            "/S [/Separation /Spot /DeviceGray 6 0 R]",
            &[form, function, exponential],
        );
        let page = render(&document, &mut RenderCache::default());
        assert_eq!(skipped(&page), vec![Unsupported::ColorSpace], "{function}");
    }
}

#[test]
fn tint_evaluations_count_against_the_operation_limit() {
    // Found in review: every `sc` evaluates its tint transform, up to 4096
    // calculator operators, unlike any other operator. Each evaluation now
    // counts its steps in `max_operations`: 100 colours through a program
    // of 41 operators exceed 2 000 operations; through an exponential
    // function (1 step each) they do not.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let program = format!("{{ {} }}", "1 add ".repeat(20) + "20 sub");
    let calculator = format!(
        "<< /FunctionType 4 /Domain [0 1] /Range [0 1] /Length {} >>\nstream\n{program}\nendstream",
        program.len()
    );
    let exponential = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";
    let content = "/S cs 0.5 sc ".repeat(100);
    let mut options = RenderOptions::default();
    options.limits.max_operations = 2000;
    for (function, fails) in [(calculator.as_str(), true), (exponential, false)] {
        let document = document(
            &content,
            "/S [/Separation /Spot /DeviceGray 6 0 R]",
            &[form, function],
        );
        let page = document.page(0).expect("page");
        let result = render_page_cached(
            document.file(),
            page,
            &options,
            &mut RenderCache::default(),
            &mut budget(),
        );
        assert_eq!(
            matches!(
                result,
                Err(pdf_render::Error::LimitExceeded {
                    kind: pdf_render::RenderLimitKind::Operations,
                    ..
                })
            ),
            fails,
            "{function}"
        );
    }
}

#[test]
fn sample_tables_are_bounded_before_they_are_read() {
    // 2^22 samples at most, counted before the table is read: a table of
    // exactly 2^22 (one output) is read and used, one sample more is
    // refused although its data is all there.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream";
    let samples = 1usize << 22;
    let data = "@".repeat(samples + 1);
    for (size, drawn) in [(samples, true), (samples + 1, false)] {
        let table = format!(
            "<< /FunctionType 0 /Domain [0 1] /Range [0 1] /Size [{size}] /BitsPerSample 8 \
             /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        );
        let document = document(
            "/S cs 0.5 sc 0 0 100 100 re f",
            "/S [/Separation /Spot /DeviceGray 6 0 R]",
            &[form, &table],
        );
        let page = render(&document, &mut RenderCache::default());
        if drawn {
            // '@' is 64 of 255: gray 64.
            assert!(skipped(&page).is_empty(), "{size}: {:?}", skipped(&page));
            assert!(
                near(at(&page, 50, 50), [64, 64, 64], 1),
                "{:?}",
                at(&page, 50, 50)
            );
        } else {
            assert_eq!(skipped(&page), vec![Unsupported::ColorSpace], "{size}");
        }
    }
}
