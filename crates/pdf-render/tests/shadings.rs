//! Shadings painted by `sh` (ISO 32000-2, 8.7.4; tranche 3.16): axial and
//! radial ones are drawn, with their extension, under the current clip and
//! their `/BBox`; other types are reported (`Shading`).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_document::Document;
use pdf_render::{PixelRect, RenderOptions, Rendering, Unsupported, render_area, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

/// A `size` x `size` page whose content is `content`, with shading /Sh
/// (object 5, `shading`) and `more` objects from 6.
fn document(size: u32, content: &str, shading: &str, more: &[&str]) -> Document<MemorySource> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {size} {size}] \
             /Resources << /Shading << /Sh 5 0 R >> >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        shading.to_owned(),
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

fn render(document: &Document<MemorySource>) -> Rendering {
    let page = document.page(0).expect("page");
    render_page(
        document.file(),
        page,
        &RenderOptions::default(),
        &mut budget(),
    )
    .expect("render")
}

/// The colour at (x, y) in user space of a page `size` high (y up).
fn at(rendering: &Rendering, size: u32, x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = rendering.bitmap.pixel(x, size - 1 - y).expect("inside");
    [r, g, b]
}

fn near(got: [u8; 3], want: [u8; 3], tolerance: u8) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| g.abs_diff(w) <= tolerance)
}

fn skipped(rendering: &Rendering) -> Vec<Unsupported> {
    rendering.skipped.iter().copied().collect()
}

/// Red to blue in RGB, linear over the parameter.
const RED_TO_BLUE: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";

fn axial(coords: &str, extend: &str) -> String {
    format!(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [{coords}] {extend} /Function 6 0 R >>"
    )
}

#[test]
fn axial_shadings_follow_their_axis() {
    // Across the page, extended: red on the left, blue on the right, the
    // mix in the middle, constant along the axis' normal.
    let document = document(
        100,
        "/Sh sh",
        &axial("0 0 100 0", "/Extend [true true]"),
        &[RED_TO_BLUE],
    );
    let page = render(&document);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert!(
        near(at(&page, 100, 0, 50), [255, 0, 0], 3),
        "{:?}",
        at(&page, 100, 0, 50)
    );
    assert!(
        near(at(&page, 100, 99, 50), [0, 0, 255], 3),
        "{:?}",
        at(&page, 100, 99, 50)
    );
    assert!(
        near(at(&page, 100, 50, 10), [128, 0, 127], 3),
        "{:?}",
        at(&page, 100, 50, 10)
    );
    assert_eq!(at(&page, 100, 50, 10), at(&page, 100, 50, 90));
}

#[test]
fn shadings_extend_only_where_they_say_so() {
    // The axis from x = 25 to 75: beyond it, the end colour where the
    // shading extends, nothing (the page) where it does not.
    for (extend, left, right) in [
        ("", [255, 255, 255], [255, 255, 255]),
        ("/Extend [true false]", [255, 0, 0], [255, 255, 255]),
        ("/Extend [false true]", [255, 255, 255], [0, 0, 255]),
        ("/Extend [true true]", [255, 0, 0], [0, 0, 255]),
    ] {
        let document = document(100, "/Sh sh", &axial("25 0 75 0", extend), &[RED_TO_BLUE]);
        let page = render(&document);
        assert!(skipped(&page).is_empty(), "{extend}: {:?}", skipped(&page));
        assert_eq!(at(&page, 100, 10, 50), left, "{extend}");
        assert_eq!(at(&page, 100, 90, 50), right, "{extend}");
        assert!(near(at(&page, 100, 50, 50), [128, 0, 127], 3), "{extend}");
    }
}

#[test]
fn radial_shadings_go_from_circle_to_circle() {
    // From a point at the centre to a circle of radius 40: red at the
    // centre, the mix at radius 20, blue at 40 when extended, the page
    // beyond it otherwise.
    for (extend, outside) in [("", [255, 255, 255]), ("/Extend [true true]", [0, 0, 255])] {
        let shading = format!(
            "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 50 0 50 50 40] {extend} /Function 6 0 R >>"
        );
        let page = render(&document(100, "/Sh sh", &shading, &[RED_TO_BLUE]));
        assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
        assert!(
            near(at(&page, 100, 50, 50), [255, 0, 0], 8),
            "{:?}",
            at(&page, 100, 50, 50)
        );
        assert!(
            near(at(&page, 100, 70, 50), [128, 0, 127], 8),
            "{:?}",
            at(&page, 100, 70, 50)
        );
        assert!(
            near(at(&page, 100, 50, 30), [128, 0, 127], 8),
            "{:?}",
            at(&page, 100, 50, 30)
        );
        assert_eq!(at(&page, 100, 98, 98), outside, "{extend}");
    }
}

#[test]
fn shadings_paint_within_the_clip_and_their_bbox() {
    // The current clip (left half), then the shading's own /BBox (bottom
    // half, in the shading's space): only their intersection is painted.
    let clip = "0 0 50 100 re W n /Sh sh";
    let page = render(&document(
        100,
        clip,
        &axial("0 0 100 0", "/Extend [true true]"),
        &[RED_TO_BLUE],
    ));
    assert!(
        near(at(&page, 100, 10, 50), [230, 0, 25], 4),
        "{:?}",
        at(&page, 100, 10, 50)
    );
    assert_eq!(at(&page, 100, 80, 50), [255, 255, 255]);
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] \
                   /BBox [0 0 100 50] /Function 6 0 R >>";
    let page = render(&document(100, clip, shading, &[RED_TO_BLUE]));
    assert!(
        near(at(&page, 100, 10, 20), [230, 0, 25], 4),
        "{:?}",
        at(&page, 100, 10, 20)
    );
    assert_eq!(at(&page, 100, 10, 80), [255, 255, 255]);
    assert_eq!(at(&page, 100, 80, 20), [255, 255, 255]);
}

#[test]
fn shadings_take_the_colour_spaces_and_functions_of_paints() {
    // A DeviceN shading (the corpus' most frequent kind after RGB and ICC)
    // with a calculator tint transform, a stitching function onto it, and
    // an ICC-based one with n functions of one output.
    let program = "{ 0 0 }";
    let tint = format!(
        "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n{program}\nendstream",
        program.len()
    );
    let stitching = "<< /FunctionType 3 /Domain [0 1] /Bounds [0.5] /Encode [0 1 0 1] \
                     /Functions [<< /FunctionType 2 /Domain [0 1] /C0 [1 0] /C1 [0 1] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0 1] /C1 [0 1] /N 1 >>] >>";
    let device_n = "<< /ShadingType 2 /ColorSpace [/DeviceN [/Cyan /Magenta] /DeviceCMYK 6 0 R] \
                    /Coords [0 0 100 0] /Extend [true true] /Function 7 0 R >>";
    let page = render(&document(100, "/Sh sh", device_n, &[&tint, stitching]));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // Cyan at the start, magenta from the middle.
    assert!(
        near(at(&page, 100, 0, 50), [0, 255, 255], 3),
        "{:?}",
        at(&page, 100, 0, 50)
    );
    assert!(
        near(at(&page, 100, 75, 50), [255, 0, 255], 3),
        "{:?}",
        at(&page, 100, 75, 50)
    );
    let profile = "<< /N 3 /Length 0 >>\nstream\n\nendstream";
    let one =
        |c0: u8, c1: u8| format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>");
    let icc = "<< /ShadingType 2 /ColorSpace [/ICCBased 6 0 R] /Coords [0 0 100 0] \
               /Extend [true true] /Function [7 0 R 8 0 R 9 0 R] >>";
    let page = render(&document(
        100,
        "/Sh sh",
        icc,
        &[profile, &one(0, 1), &one(1, 1), &one(0, 0)],
    ));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert!(
        near(at(&page, 100, 0, 50), [0, 255, 0], 3),
        "{:?}",
        at(&page, 100, 0, 50)
    );
    assert!(
        near(at(&page, 100, 99, 50), [255, 255, 0], 3),
        "{:?}",
        at(&page, 100, 99, 50)
    );
}

#[test]
fn other_shadings_are_reported() {
    // Function-based (1), mesh (4 to 7) and damaged shadings: reported, the
    // page drawn without them.
    for shading in [
        "<< /ShadingType 1 /ColorSpace /DeviceRGB /Function 6 0 R >>".to_owned(),
        "<< /ShadingType 7 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
         /BitsPerFlag 8 /Decode [0 1 0 1 0 1 0 1 0 1] /Length 0 >>\nstream\n\nendstream"
            .to_owned(),
        axial("0 0 100", "/Extend [true true]"),
        axial("0 0 100 0", "/Extend [true]"),
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function 9 0 R >>"
            .to_owned(),
        "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Function 6 0 R >>"
            .to_owned(),
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [0 0 -1 0 0 5] /Function 6 0 R >>"
            .to_owned(),
    ] {
        let page = render(&document(
            100,
            "0 g 0 0 10 10 re f /Sh sh",
            &shading,
            &[RED_TO_BLUE],
        ));
        assert_eq!(skipped(&page), vec![Unsupported::Shading], "{shading}");
        assert_eq!(at(&page, 100, 5, 5), [0, 0, 0]);
    }
}

#[test]
fn shaded_tiles_join_into_the_page() {
    // Tiles of 64 pixels give the pixels of the whole page.
    let shading = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [40 60 5 70 30 60] \
                   /Extend [true true] /Function 6 0 R >>";
    let document = document(150, "/Sh sh", shading, &[RED_TO_BLUE]);
    let page = document.page(0).expect("page");
    let whole = render(&document);
    for (x, y) in [(0, 0), (64, 0), (0, 64), (64, 64), (128, 128)] {
        let area = PixelRect {
            x,
            y,
            width: 64.min(150 - x),
            height: 64.min(150 - y),
        };
        let tile = render_area(
            document.file(),
            page,
            &RenderOptions::default(),
            area,
            &mut budget(),
        )
        .expect("tile");
        for ty in 0..area.height {
            for tx in 0..area.width {
                assert_eq!(
                    tile.bitmap.pixel(tx, ty),
                    whole.bitmap.pixel(x + tx, y + ty),
                    "({}, {})",
                    x + tx,
                    y + ty
                );
            }
        }
    }
}

#[test]
fn a_large_area_is_shaded_in_a_smaller_image() {
    // A page of 2 000 x 2 000 pixels (4 Mpx) is shaded through an image of
    // at most 1 Mpx, smoothed: the colours stay those of the shading.
    let document = document(
        2000,
        "/Sh sh",
        &axial("0 0 2000 0", "/Extend [true true]"),
        &[RED_TO_BLUE],
    );
    let page = render(&document);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert!(
        near(at(&page, 2000, 1000, 1000), [128, 0, 127], 3),
        "{:?}",
        at(&page, 2000, 1000, 1000)
    );
    assert!(
        near(at(&page, 2000, 500, 1000), [191, 0, 64], 3),
        "{:?}",
        at(&page, 2000, 500, 1000)
    );
}

#[test]
fn a_shading_costs_the_area_of_its_clip() {
    // Found by the corpus measurement: each `sh` was computed over the
    // whole canvas, so a page with many small shaded shapes went over
    // `max_path_work` and was refused. It is computed over the rectangle
    // of the current clip: 200 shadings in 10-point squares of a page of
    // 2 000 points render, each square shaded.
    let mut content = String::new();
    for n in 0..200 {
        let (x, y) = ((n % 20) * 100, (n / 20) * 100);
        content.push_str(&format!("q {x} {y} 10 10 re W n /Sh sh Q "));
    }
    let document = document(
        2000,
        &content,
        &axial("0 0 2000 0", "/Extend [true true]"),
        &[RED_TO_BLUE],
    );
    let page = render(&document);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert!(
        near(at(&page, 2000, 5, 5), [255, 0, 0], 3),
        "{:?}",
        at(&page, 2000, 5, 5)
    );
    assert!(
        near(at(&page, 2000, 1905, 905), [12, 0, 243], 3),
        "{:?}",
        at(&page, 2000, 1905, 905)
    );
    assert_eq!(at(&page, 2000, 50, 50), [255, 255, 255]);
}

#[test]
fn a_shading_painted_many_times_is_read_once() {
    // Found by the corpus measurement: a page painting its 63 shadings
    // 1 403 times made each `sh` evaluate its colours again (functions and
    // tint transform, counted in `max_operations`) and went over the
    // limit. A shading is read once per render: 500 `sh` of one shading
    // through a calculator tint cost its 256 colours once.
    let program = format!("{{ {} }}", "dup pop ".repeat(30) + "0 0");
    let tint = format!(
        "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1] /Length {} >>\nstream\n{program}\nendstream",
        program.len()
    );
    let ramp = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0] /C1 [0 1] /N 1 >>";
    let shading = "<< /ShadingType 2 /ColorSpace [/DeviceN [/Cyan /Magenta] /DeviceCMYK 6 0 R] \
                   /Coords [0 0 100 0] /Extend [true true] /Function 7 0 R >>";
    let content = "q 0 0 10 10 re W n /Sh sh Q ".repeat(500);
    let document = document(100, &content, shading, &[&tint, ramp]);
    let page = document.page(0).expect("page");
    let mut options = RenderOptions::default();
    options.limits.max_operations = 100_000;
    let rendering = render_page(document.file(), page, &options, &mut budget())
        .expect("render within the limit");
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    // At x = 1.5 the parameter is 0.015: nearly cyan.
    assert!(
        near(at(&rendering, 100, 1, 1), [0, 255, 255], 6),
        "{:?}",
        at(&rendering, 100, 1, 1)
    );
}

#[test]
fn a_shading_under_a_text_clip_is_reported_not_painted() {
    // Found in review: shaded text (`7 Tr`, then `sh`) would have painted a
    // full rectangle, the text clip not being applied. Reported instead,
    // as it was before shadings were drawn; `Q` ends it.
    let content = "q BT 7 Tr ET /Sh sh Q 0 0 10 10 re W n /Sh sh";
    let page = render(&document(
        100,
        content,
        &axial("0 0 100 0", "/Extend [true true]"),
        &[RED_TO_BLUE],
    ));
    assert!(
        skipped(&page).contains(&Unsupported::Shading),
        "{:?}",
        skipped(&page)
    );
    assert!(
        skipped(&page).contains(&Unsupported::TextClip),
        "{:?}",
        skipped(&page)
    );
    assert_eq!(at(&page, 100, 50, 50), [255, 255, 255]);
    // After `Q`, the shading in a plain clip is painted.
    assert!(
        near(at(&page, 100, 5, 5), [243, 0, 12], 4),
        "{:?}",
        at(&page, 100, 5, 5)
    );
}

#[test]
fn a_shading_in_scopes_with_other_default_spaces_is_read_in_each() {
    // Found in review: one shading object painted by the page, then by a
    // form whose DefaultRGB is Lab (another kind), took the page's
    // colours from the cache. Read again where a Default space exists: the
    // form's paints nothing and is reported, the page's is drawn.
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                /Resources << /Shading << /Sh 5 0 R >> /ColorSpace << \
                /DefaultRGB [/Lab << /WhitePoint [0.9505 1 1.089] >>] >> >> \
                /Length 25 >>\nstream\n50 0 50 100 re W n /Sh sh\nendstream";
    let content = "q 0 0 50 100 re W n /Sh sh Q /F Do";
    let mut bodies = document_with_form(content, form);
    let page = render(&bodies.remove(0));
    assert_eq!(skipped(&page), vec![Unsupported::Shading]);
    assert!(
        near(at(&page, 100, 10, 50), [230, 0, 25], 4),
        "{:?}",
        at(&page, 100, 10, 50)
    );
    assert_eq!(at(&page, 100, 90, 50), [255, 255, 255]);
}

/// The page of `document` with a form /F (object 7) as well.
fn document_with_form(content: &str, form: &str) -> Vec<Document<MemorySource>> {
    let shading = axial("0 0 100 0", "/Extend [true true]");
    let page_resources = "/Resources << /Shading << /Sh 5 0 R >> /XObject << /F 7 0 R >> >>";
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {page_resources} /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        shading,
        RED_TO_BLUE.to_owned(),
        form.to_owned(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.drain(..).enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    let source = MemorySource::from_vec(out, &ResourceLimits::default()).expect("source");
    vec![Document::open(source, &mut budget()).expect("document")]
}

#[test]
fn radial_shadings_with_tangent_circles_and_negative_radii() {
    // Tangent circles (radius difference equal to the distance: the
    // equation is linear) and a cone whose larger root has a negative
    // radius on one side, where the smaller root is the one painted.
    let tangent = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [30 50 10 50 50 30] \
                   /Function 6 0 R >>";
    let page = render(&document(100, "/Sh sh", tangent, &[RED_TO_BLUE]));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // Inside the small circle only circles before the start (s < 0) pass:
    // not extended, nothing. Near the big circle's far side, s = 0.95.
    assert_eq!(at(&page, 100, 25, 50), [255, 255, 255]);
    assert!(
        near(at(&page, 100, 78, 50), [13, 0, 242], 8),
        "{:?}",
        at(&page, 100, 78, 50)
    );
    assert_eq!(at(&page, 100, 95, 95), [255, 255, 255]);
    // A cone from radius 20 at x = 20 to radius 0 at x = 80, extended at
    // the start. Each point takes the larger s whose circle passes there:
    // at x = 5, s = 0.0625 (not the circle of s = -0.875 also passing);
    // at x = 50, s = 0.625 (not 0.25); beyond the tip, nothing.
    let cone = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [20 50 20 80 50 0] \
                /Extend [true false] /Function 6 0 R >>";
    let page = render(&document(100, "/Sh sh", cone, &[RED_TO_BLUE]));
    assert!(
        near(at(&page, 100, 5, 50), [239, 0, 16], 4),
        "{:?}",
        at(&page, 100, 5, 50)
    );
    assert!(
        near(at(&page, 100, 50, 50), [96, 0, 159], 4),
        "{:?}",
        at(&page, 100, 50, 50)
    );
    assert_eq!(at(&page, 100, 95, 50), [255, 255, 255]);
    // Only circles before the start (s = -0.25 and -0.5625) reach (5, 75):
    // extended, red.
    assert!(
        near(at(&page, 100, 5, 75), [255, 0, 0], 4),
        "{:?}",
        at(&page, 100, 5, 75)
    );
}
