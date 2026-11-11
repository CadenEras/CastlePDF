//! Form XObjects (ISO 32000-2, 8.10): matrix, bounding box, resources,
//! transparency groups, nesting, and what a damaged form affects.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{Error, RenderLimitKind, RenderOptions, Rendering, Unsupported, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn render_with(bytes: Vec<u8>, options: &RenderOptions) -> Result<Rendering, Error> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let document = Document::open(source, &mut budget()).expect("document");
    let page = document.page(0).expect("page");
    render_page(document.file(), page, options, &mut budget())
}

fn render(bytes: Vec<u8>) -> Rendering {
    render_with(bytes, &RenderOptions::default()).expect("rendering")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
    std::fs::read(path).expect("fixture")
}

/// RGB of the pixel at user point (x, y) of a page 200 high, at scale 1.
fn at(rendering: &Rendering, x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = rendering.bitmap.pixel(x, 200 - y).expect("inside");
    [r, g, b]
}

fn skipped(rendering: &Rendering) -> Vec<Unsupported> {
    rendering.skipped.iter().copied().collect()
}

/// A stream object body.
fn stream(dictionary: &str, data: &str) -> String {
    format!(
        "<< {dictionary} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// A form XObject body.
fn form(entries: &str, content: &str) -> String {
    stream(&format!("/Type /XObject /Subtype /Form {entries}"), content)
}

/// A 200 x 200 page with `content` and `resources`; `more` objects from 5.
fn pdf(content: &str, resources: &str, more: &[String]) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {resources} >> /Contents 4 0 R >>"
        ),
        stream("", content),
    ];
    bodies.extend(more.iter().cloned());
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
    out
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];
const BLACK: [u8; 3] = [0, 0, 0];

#[test]
fn a_form_is_drawn_with_its_matrix_and_clipped_to_its_bbox() {
    // The form fills far beyond its 50 x 50 box, moved by (20, 20).
    let bytes = pdf(
        "/F Do",
        "/XObject << /F 5 0 R >>",
        &[form(
            "/BBox [0 0 50 50] /Matrix [1 0 0 1 20 20]",
            "1 0 0 rg -1000 -1000 3000 3000 re f",
        )],
    );
    let page = render(bytes);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 40, 40), RED);
    assert_eq!(at(&page, 10, 10), WHITE);
    assert_eq!(at(&page, 80, 80), WHITE);
    // The current transformation applies too, and the form can be reused.
    let bytes = pdf(
        "/F Do q 2 0 0 2 100 100 cm /F Do Q",
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 20 20]", "1 0 0 rg 0 0 20 20 re f")],
    );
    let page = render(bytes);
    assert_eq!(at(&page, 10, 10), RED);
    assert_eq!(at(&page, 130, 130), RED);
    assert_eq!(at(&page, 145, 145), WHITE);
}

#[test]
fn a_form_has_its_own_resources_or_the_page_ones() {
    // Page /F is the form; the form's own /F is an image. Read in the
    // page's scope, the form would draw itself.
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "\u{0}",
    );
    let bytes = pdf(
        "q 100 0 0 100 50 50 cm /F Do Q",
        "/XObject << /F 5 0 R >>",
        &[
            form(
                "/BBox [0 0 1 1] /Resources << /XObject << /F 6 0 R >> >>",
                "/F Do",
            ),
            image.clone(),
        ],
    );
    let page = render(bytes);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 100, 100), BLACK);
    // Without /Resources, the names are the page's (older files).
    let bytes = pdf(
        "q 100 0 0 100 50 50 cm /F Do Q",
        "/XObject << /F 5 0 R /I 6 0 R >>",
        &[form("/BBox [0 0 1 1]", "/I Do"), image],
    );
    assert_eq!(at(&render(bytes), 100, 100), BLACK);
    // After the form, the page's names are the page's again.
    let bytes = pdf(
        "/F Do /G0 gs 0 0 1 rg 0 0 20 20 re f",
        "/XObject << /F 5 0 R >> /ExtGState << /G0 << /ca 1 >> >>",
        &[form(
            "/BBox [0 0 200 200] /Resources << /ExtGState << /G0 << /ca 0 >> >> >>",
            "/G0 gs",
        )],
    );
    assert_eq!(at(&render(bytes), 10, 10), BLUE);
}

#[test]
fn the_graphics_state_is_restored_after_a_form() {
    // The form changes the colour, leaves a q, a text object and marked
    // content open: the page continues in its own state.
    let bytes = pdf(
        "0 0 1 rg /F Do 0 0 20 20 re f",
        "/XObject << /F 5 0 R >>",
        &[form(
            "/BBox [0 0 200 200]",
            "q 1 0 0 rg 2 0 0 2 0 0 cm 50 50 10 10 re f /T BMC BT",
        )],
    );
    let page = render(bytes);
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 10, 10), BLUE);
    assert_eq!(at(&page, 110, 110), RED);
}

#[test]
fn a_damaged_form_affects_only_itself() {
    let red_then = |rest: &str| format!("1 0 0 rg 0 0 50 50 re f {rest}");
    for content in [
        // A Q that would restore the page's state.
        red_then("Q Q"),
        // Invalid operands.
        red_then("/X re"),
        // Unterminated array.
        red_then("[1 2"),
    ] {
        let bytes = pdf(
            "0 0 1 rg /F Do 150 150 20 20 re f",
            "/XObject << /F 5 0 R >>",
            &[form("/BBox [0 0 200 200]", &content)],
        );
        let page = render(bytes);
        assert_eq!(skipped(&page), [Unsupported::XObject], "{content}");
        // Drawn up to its error; the page goes on in its own state.
        assert_eq!(at(&page, 25, 25), RED, "{content}");
        assert_eq!(at(&page, 160, 160), BLUE, "{content}");
    }
    // No /BBox, or a /Matrix that is not one: not drawn.
    for entries in ["", "/BBox [0 0 1]", "/BBox [0 0 200 200] /Matrix [1 0 0]"] {
        let bytes = pdf(
            "/F Do",
            "/XObject << /F 5 0 R >>",
            &[form(entries, "1 0 0 rg 0 0 50 50 re f")],
        );
        let page = render(bytes);
        assert_eq!(skipped(&page), [Unsupported::XObject], "{entries}");
        assert_eq!(at(&page, 25, 25), WHITE, "{entries}");
    }
}

#[test]
fn a_form_inside_itself_or_nested_too_deep_is_skipped() {
    // Form 5 draws a red square then itself.
    let bytes = pdf(
        "/F Do",
        "/XObject << /F 5 0 R >>",
        &[form(
            "/BBox [0 0 200 200] /Resources << /XObject << /F 5 0 R >> >>",
            "1 0 0 rg 0 0 50 50 re f /F Do",
        )],
    );
    let page = render(bytes);
    assert_eq!(skipped(&page), [Unsupported::XObject]);
    assert_eq!(at(&page, 25, 25), RED);
    // A chain of distinct forms: 16 levels are drawn, the 17th is not.
    let chain = |levels: usize| {
        let more: Vec<String> = (0..levels)
            .map(|level| {
                let next = 5 + level + 1;
                if level + 1 == levels {
                    form("/BBox [0 0 200 200]", "1 0 0 rg 0 0 50 50 re f")
                } else {
                    form(
                        &format!(
                            "/BBox [0 0 200 200] /Resources << /XObject << /N {next} 0 R >> >>"
                        ),
                        "/N Do",
                    )
                }
            })
            .collect();
        render(pdf("/F Do", "/XObject << /F 5 0 R >>", &more))
    };
    let deep = chain(16);
    assert!(skipped(&deep).is_empty(), "{:?}", skipped(&deep));
    assert_eq!(at(&deep, 25, 25), RED);
    let deeper = chain(17);
    assert_eq!(skipped(&deeper), [Unsupported::XObject]);
    assert_eq!(at(&deeper, 25, 25), WHITE);
}

#[test]
fn a_transparency_group_is_composited_as_a_whole() {
    // Two overlapping opaque red squares at fill alpha 0.5.
    let squares = "1 0 0 rg 20 20 60 60 re f 50 50 60 60 re f";
    let draw = |group: &str| {
        render(pdf(
            "/H gs /F Do",
            "/XObject << /F 5 0 R >> /ExtGState << /H << /ca 0.5 >> >>",
            &[form(&format!("/BBox [0 0 200 200] {group}"), squares)],
        ))
    };
    // A group: one layer at 0.5, the overlap like the rest.
    let grouped = draw("/Group << /S /Transparency >>");
    assert!(skipped(&grouped).is_empty(), "{:?}", skipped(&grouped));
    let single = at(&grouped, 30, 30);
    assert!(
        single[0] == 255 && single[1].abs_diff(128) <= 1,
        "{single:?}"
    );
    assert_eq!(at(&grouped, 65, 65), single);
    // Not a group: each square at 0.5, the overlap darker.
    let plain = draw("");
    assert_eq!(at(&plain, 30, 30), single);
    assert!(
        at(&plain, 65, 65)[1] < single[1] - 30,
        "{:?}",
        at(&plain, 65, 65)
    );
    // A knockout group is drawn as a plain one and reported.
    let knockout = draw("/Group << /S /Transparency /K true >>");
    assert_eq!(skipped(&knockout), [Unsupported::BlendMode]);
}

#[test]
fn forms_count_against_the_page_limits() {
    // 100 draws of a form of 3 operations: 400 operations, the page's
    // included; one fewer is over the limit.
    let content = "/F Do ".repeat(100);
    let bytes = pdf(
        &content,
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 200 200]", "1 0 0 rg 0 0 1 1 re f")],
    );
    let mut options = RenderOptions::default();
    options.limits.max_operations = 399;
    assert!(matches!(
        render_with(bytes.clone(), &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            ..
        })
    ));
    options.limits.max_operations = 400;
    assert!(render_with(bytes, &options).is_ok());
}

#[test]
fn the_form_fixture_draws_every_kind() {
    let page = render(fixture("forms.pdf"));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    // Rotated form: red inside its box, the blue diagonal, white outside.
    assert_eq!(at(&page, 70, 149), RED);
    assert_eq!(at(&page, 49, 154), BLUE);
    assert_eq!(at(&page, 80, 120), WHITE);
    assert_eq!(at(&page, 20, 130), WHITE);
    // Form with its own resources: the Indexed image, then the nested form.
    assert_eq!(at(&page, 120, 170), [255, 255, 0]);
    assert_eq!(at(&page, 140, 170), [0, 255, 255]);
    assert_eq!(at(&page, 160, 170), [255, 0, 255]);
    assert_eq!(at(&page, 180, 170), BLACK);
    assert_eq!(at(&page, 150, 130), [0, 153, 0]);
    // The same form, scaled by the page.
    assert_eq!(at(&page, 15, 46), [255, 255, 0]);
    assert_eq!(at(&page, 55, 46), BLACK);
    assert_eq!(at(&page, 34, 22), [0, 153, 0]);
    // Transparency group at 0.5: the overlap like the rest.
    let single = at(&page, 120, 20);
    assert!(
        single[0] == 255 && single[1].abs_diff(128) <= 1,
        "{single:?}"
    );
    assert_eq!(at(&page, 150, 50), single);
    assert_eq!(at(&page, 180, 80), single);
}

#[test]
fn clips_and_layers_left_open_by_a_form_are_undone() {
    // A clip at the form's base level, another in an unclosed q; then, in a
    // group at alpha 0.5, the same with an invalid operation. After each,
    // the page paints outside the form's clips, opaque.
    for (group, content, damaged) in [
        ("", "0 0 10 10 re W n q 0 0 5 5 re W n", false),
        (
            "/Group << /S /Transparency >>",
            "0 0 10 10 re W n q 0 0 5 5 re W n /X re",
            true,
        ),
    ] {
        let page = render(pdf(
            "/H gs /F Do /K gs 0 0 1 rg 100 100 50 50 re f",
            "/XObject << /F 5 0 R >> /ExtGState << /H << /ca 0.5 >> /K << /ca 1 >> >>",
            &[form(&format!("/BBox [0 0 200 200] {group}"), content)],
        ));
        let expected: Vec<Unsupported> = if damaged {
            vec![Unsupported::XObject]
        } else {
            vec![]
        };
        assert_eq!(skipped(&page), expected, "{content}");
        assert_eq!(at(&page, 125, 125), BLUE, "{content}");
    }
}

#[test]
fn nested_groups_compose_their_layers() {
    // An outer group at 0.5 holding an inner group, itself drawn at 0.5:
    // the red square ends at 0.25 over white.
    let page = render(pdf(
        "/H gs /F Do",
        "/XObject << /F 5 0 R >> /ExtGState << /H << /ca 0.5 >> >>",
        &[
            form(
                "/BBox [0 0 200 200] /Group << /S /Transparency >> \
                 /Resources << /XObject << /G 6 0 R >> /ExtGState << /H << /ca 0.5 >> >> >>",
                "/H gs /G Do",
            ),
            form(
                "/BBox [0 0 200 200] /Group << /S /Transparency >>",
                "1 0 0 rg 0 0 100 100 re f",
            ),
        ],
    ));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    let [r, g, b] = at(&page, 50, 50);
    assert!(
        r == 255 && g.abs_diff(191) <= 2 && b.abs_diff(191) <= 2,
        "{:?}",
        [r, g, b]
    );
}

#[test]
fn a_group_resets_the_stroke_alpha_too() {
    // CA 0.5 outside; inside the group, a stroke and a fill at alpha 1 over
    // each other: one layer at full opacity.
    let page = render(pdf(
        "/H gs /F Do",
        "/XObject << /F 5 0 R >> /ExtGState << /H << /CA 0.5 >> >>",
        &[form(
            "/BBox [0 0 200 200] /Group << /S /Transparency >>",
            "0 0 1 RG 20 w 0 50 m 200 50 l S",
        )],
    ));
    assert_eq!(at(&page, 100, 50), BLUE);
}

#[test]
fn form_entries_change_what_is_reported_or_inherited() {
    let square = "1 0 0 rg 0 0 50 50 re f";
    // /OC: drawn, reported.
    let page = render(pdf(
        "/F Do",
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 200 200] /OC << /Type /OCG >>", square)],
    ));
    assert_eq!(skipped(&page), [Unsupported::OptionalContent]);
    assert_eq!(at(&page, 25, 25), RED);
    // /Resources null: the enclosing resources.
    let page = render(pdf(
        "/F Do",
        "/XObject << /F 5 0 R >> /ExtGState << /G0 << /ca 1 >> >>",
        &[form(
            "/BBox [0 0 200 200] /Resources null",
            &format!("/G0 gs {square}"),
        )],
    ));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 25, 25), RED);
    // A /Group that is not a transparency group changes nothing; a damaged
    // one is reported.
    let page = render(pdf(
        "/F Do",
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 200 200] /Group << /S /Other >>", square)],
    ));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    let page = render(pdf(
        "/F Do",
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 200 200] /Group 9 0 R", square)],
    ));
    assert_eq!(skipped(&page), [Unsupported::BlendMode]);
    assert_eq!(at(&page, 25, 25), RED);
    // A matrix sending the box out of range: skipped.
    let page = render(pdf(
        "/F Do 0 0 1 rg 100 100 20 20 re f",
        "/XObject << /F 5 0 R >>",
        &[form(
            "/BBox [0 0 200 200] /Matrix [1e15 0 0 1e15 0 0]",
            square,
        )],
    ));
    assert_eq!(skipped(&page), [Unsupported::XObject]);
    assert_eq!(at(&page, 110, 110), BLUE);
    // Marked content of the page cannot be closed by a form.
    let page = render(pdf(
        "/T BMC /F Do EMC",
        "/XObject << /F 5 0 R >>",
        &[form("/BBox [0 0 200 200]", &format!("{square} EMC"))],
    ));
    assert_eq!(skipped(&page), [Unsupported::XObject]);
}

#[test]
fn a_form_is_read_once_per_rendering() {
    // Drawing a form 1 or 200 times visits about as many objects.
    let visited = |draws: usize| {
        let bytes = pdf(
            &"/F Do ".repeat(draws),
            "/XObject << /F 5 0 R >>",
            &[form(
                "/BBox [0 0 200 200] /Resources << /ExtGState << /G << /ca 1 >> >> >>",
                "/G gs 1 0 0 rg 0 0 50 50 re f",
            )],
        );
        let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
        let document = Document::open(source, &mut budget()).expect("document");
        let page = document.page(0).expect("page");
        let mut budget = budget();
        render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget,
        )
        .expect("page");
        budget.objects_visited()
    };
    let (once, often) = (visited(1), visited(200));
    assert!(often <= once + 10, "{once} then {often}");
}

#[test]
fn the_clip_of_a_form_counts_against_the_limits() {
    let bytes = |draws: usize| {
        pdf(
            &"/F Do ".repeat(draws),
            "/XObject << /F 5 0 R >>",
            &[form("/BBox [0 0 200 200]", "")],
        )
    };
    // Clips: each form's own clip is undone after it, so only nesting
    // counts; with max_clips 0 no form can be drawn.
    let mut options = RenderOptions::default();
    options.limits.max_clips = 0;
    assert!(matches!(
        render_with(bytes(1), &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Clips,
            ..
        })
    ));
    options.limits.max_clips = 1;
    assert!(render_with(bytes(1000), &options).is_ok());
    // Path work: each draw charges its clip.
    let work = |draws: usize, limit: u64| {
        let mut options = RenderOptions::default();
        options.limits.max_path_work = limit;
        render_with(bytes(draws), &options)
    };
    let mut fits = 1;
    while work(10, fits).is_err() {
        fits *= 2;
    }
    assert!(matches!(
        work(20, fits),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            ..
        })
    ));
}

#[test]
fn an_image_naming_its_colour_space_follows_each_form() {
    // Image 7 says /ColorSpace /P: red in form 5's palette, blue in form
    // 6's. Each form draws it in its half of the page.
    let palette = |color: &str| format!("/ColorSpace << /P [/Indexed /DeviceRGB 0 <{color}>] >>");
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /P /BitsPerComponent 8",
        "\u{0}",
    );
    let page = render(pdf(
        "/A Do /B Do",
        "/XObject << /A 5 0 R /B 6 0 R >>",
        &[
            form(
                &format!(
                    "/BBox [0 0 1 1] /Matrix [100 0 0 200 0 0] /Resources << /XObject << /I 7 0 R >> {} >>",
                    palette("FF0000")
                ),
                "/I Do",
            ),
            form(
                &format!(
                    "/BBox [0 0 1 1] /Matrix [100 0 0 200 100 0] /Resources << /XObject << /I 7 0 R >> {} >>",
                    palette("0000FF")
                ),
                "/I Do",
            ),
            image,
        ],
    ));
    assert!(skipped(&page).is_empty(), "{:?}", skipped(&page));
    assert_eq!(at(&page, 50, 100), RED);
    assert_eq!(at(&page, 150, 100), BLUE);
}
