//! Differential rendering against `pdftoppm` (poppler, GPL): a test oracle
//! only, never shipped nor linked (ADR-0003). Skipped unless
//! `CASTLE_PDFTOPPM` names the executable.
//!
//! Tolerance: rasterisers anti-alias edges differently and may place an
//! edge one pixel apart, so a pixel matches when each of its channels lies
//! within `MARGIN` of the range of the other image's pixels in its 3 x 3
//! neighbourhood. The test runs both ways (ours against the reference, and
//! the reference against ours), so that a thin line present in only one of
//! the images is caught even where the other image's neighbourhood spans
//! both colours. A page matches when, in each direction, at most
//! `MAX_MISMATCHES` of its pixels do not. Controlled mutations of our image
//! (an erased dash, a 2-pixel shift, a filled hole) must be detected: they
//! prove that the tolerance does not hide such defects.
//!
//! Text is compared at scale 3 only, with its own tolerance: poppler draws
//! glyphs through FreeType, which fits stems to the pixel grid, while
//! CastlePDF draws unhinted outlines (ADR-0005). At 72 dpi a stem one pixel
//! wide is then darker in poppler (0.28 % of `truetype.pdf` measured); at
//! 216 dpi the difference falls to 0.010 %. An erased glyph stays detected.
//!
//! DeviceCMYK is not compared (page 3 of `shapes.pdf`): without colour
//! management, CastlePDF applies the conversion of ISO 32000-2, 10.4.2.4
//! (cyan is (0, 255, 255)) and poppler approximates a press profile (cyan is
//! (0, 173, 239)). Probes check that page in `tests/render.rs`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

const MARGIN: u8 = 16;
/// Measured: no mismatching pixel on these fixtures with poppler 22.02. The
/// threshold leaves room for another poppler version (4 pixels of page 1 at
/// scale 1) while the smallest controlled defect, an erased 6 x 2 dash
/// (0.03 %), stays well above it.
const MAX_MISMATCHES: f64 = 0.0001;

/// Text pages, at 216 dpi: three times the 0.010 % measured with poppler
/// 22.02 (grid fitting of stems, see above). Erasing the narrow "I" of
/// "Identity" gives 0.078 %.
const MAX_TEXT_MISMATCHES: f64 = 0.0003;

/// Fixtures, page indices, scales and largest share of mismatching pixels.
const CASES: [(&str, usize, f64, f64); 9] = [
    ("shapes.pdf", 0, 1.0, MAX_MISMATCHES),
    ("shapes.pdf", 0, 2.5, MAX_MISMATCHES),
    ("shapes.pdf", 1, 1.0, MAX_MISMATCHES),
    ("shapes.pdf", 1, 3.0, MAX_MISMATCHES),
    ("truetype.pdf", 0, 3.0, MAX_TEXT_MISMATCHES),
    ("cff.pdf", 0, 3.0, MAX_TEXT_MISMATCHES),
    ("images.pdf", 0, 2.0, MAX_MISMATCHES),
    ("images.pdf", 1, 2.0, MAX_MISMATCHES),
    ("jpeg.pdf", 0, 2.0, MAX_MISMATCHES),
];

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

/// Parses a binary PPM (P6, maxval 255) as written by `pdftoppm`.
fn read_ppm(bytes: &[u8]) -> (u32, u32, &[u8]) {
    let mut fields = Vec::new();
    let mut position = 0;
    while fields.len() < 4 {
        while bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        let start = position;
        while !bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..position]).expect("header"));
    }
    assert_eq!((fields[0], fields[3]), ("P6", "255"), "PPM header");
    let width = fields[1].parse().expect("width");
    let height = fields[2].parse().expect("height");
    (width, height, &bytes[position + 1..])
}

/// Renders with `pdftoppm` to `<prefix>.ppm` (`prefix` without extension).
fn pdftoppm(executable: &Path, pdf: &Path, page: usize, scale: f64, prefix: &Path) -> Vec<u8> {
    let page = (page + 1).to_string();
    let status = Command::new(executable)
        .args(["-r", &(72.0 * scale).to_string(), "-f", &page, "-l", &page])
        .args(["-cropbox", "-singlefile"])
        .arg(pdf)
        .arg(prefix)
        .status()
        .expect("pdftoppm runs");
    assert!(status.success(), "pdftoppm failed");
    let mut output = prefix.as_os_str().to_owned();
    output.push(".ppm");
    std::fs::read(output).expect("pdftoppm output")
}

/// Pixels of `image` outside the per-channel range (widened by `MARGIN`)
/// of the 3 x 3 neighbourhood of the same pixel in `other`.
fn outside(image: &[[u8; 3]], other: &[[u8; 3]], w: usize, h: usize) -> usize {
    let mut count = 0;
    for y in 0..h {
        for x in 0..w {
            let pixel = image[y * w + x];
            let matches = (0..3).all(|channel| {
                let mut low = u8::MAX;
                let mut high = u8::MIN;
                for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                    for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                        let value = other[ny * w + nx][channel];
                        low = low.min(value);
                        high = high.max(value);
                    }
                }
                let value = pixel[channel];
                value >= low.saturating_sub(MARGIN) && value <= high.saturating_add(MARGIN)
            });
            count += usize::from(!matches);
        }
    }
    count
}

/// Larger of the two directions' mismatch shares.
fn mismatch(ours: &[[u8; 3]], reference: &[[u8; 3]], w: usize, h: usize) -> f64 {
    let worst = outside(ours, reference, w, h).max(outside(reference, ours, w, h));
    worst as f64 / (w * h) as f64
}

/// Our rendering and `pdftoppm`'s of `page_index` at `scale`, as RGB pixels,
/// with the width and height.
fn renderings(
    executable: &Path,
    name: &str,
    page_index: usize,
    scale: f64,
) -> (Vec<[u8; 3]>, Vec<[u8; 3]>, usize, usize) {
    let path = corpus(name);
    let prefix = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "oracle-{}-{page_index}-{}",
        name.replace('.', "_"),
        (scale * 100.0).round()
    ));
    let expected_file = pdftoppm(executable, &path, page_index, scale, &prefix);
    let (width, height, expected) = read_ppm(&expected_file);

    let limits = ResourceLimits::default();
    let mut budget = Budget::new(limits, CancellationToken::new());
    let bytes = std::fs::read(&path).expect("fixture");
    let source = MemorySource::from_vec(bytes, &limits).expect("source");
    let document = Document::open(source, &mut budget).expect("document");
    let page = document.page(page_index).expect("page");
    let mut options = RenderOptions::default();
    options.scale = scale;
    let rendering = render_page(document.file(), page, &options, &mut budget).expect("render");
    let bitmap = &rendering.bitmap;
    assert_eq!(
        (bitmap.width(), bitmap.height()),
        (width, height),
        "{name} page {} at {scale}: size",
        page_index + 1
    );
    let (ours, _) = bitmap.data().as_chunks::<4>();
    let ours: Vec<[u8; 3]> = ours.iter().map(|[r, g, b, _]| [*r, *g, *b]).collect();
    let (reference, _) = expected.as_chunks::<3>();
    assert_eq!(ours.len(), reference.len());
    (ours, reference.to_vec(), width as usize, height as usize)
}

/// Mismatch share of one case, printed.
fn mismatch_ratio(executable: &Path, name: &str, page_index: usize, scale: f64) -> f64 {
    let (ours, reference, w, h) = renderings(executable, name, page_index, scale);
    let ratio = mismatch(&ours, &reference, w, h);
    eprintln!(
        "{name} page {} at {scale}: {:.3} % pixels outside the other image's neighbourhood",
        page_index + 1,
        ratio * 100.0
    );
    ratio
}

#[test]
fn pages_match_pdftoppm() {
    let Some(executable) = std::env::var_os("CASTLE_PDFTOPPM") else {
        eprintln!("pdftoppm oracle skipped: CASTLE_PDFTOPPM is not set");
        return;
    };
    let executable = PathBuf::from(executable);
    for (name, page_index, scale, max) in CASES {
        let ratio = mismatch_ratio(&executable, name, page_index, scale);
        assert!(
            ratio <= max,
            "{name} page {} at {scale} differs from pdftoppm",
            page_index + 1
        );
    }

    // Sensitivity: controlled defects in our page 1 at scale 1 must be
    // detected (device row = 200 - user y).
    let (ours, reference, w, h) = renderings(&executable, "shapes.pdf", 0, 1.0);
    let background = [230, 230, 230];
    let mutate = |label: &str, change: &dyn Fn(&mut Vec<[u8; 3]>)| {
        let mut mutated = ours.clone();
        change(&mut mutated);
        let ratio = mismatch(&mutated, &reference, w, h);
        eprintln!("mutation {label}: {:.3} % mismatching", ratio * 100.0);
        assert!(ratio > MAX_MISMATCHES, "mutation {label} not detected");
    };
    // The first dash of the [6 4] line (x 130..136, y 181..183).
    mutate("erased dash", &|image| {
        for y in 16..20 {
            for x in 129..138 {
                image[y * w + x] = background;
            }
        }
    });
    mutate("shift by 2 pixels", &|image| {
        let copy = image.clone();
        for y in 0..h {
            for x in 2..w {
                image[y * w + x] = copy[y * w + x - 2];
            }
        }
    });
    // The hole of the even-odd ring (x 98..112, y 178..186).
    mutate("filled hole", &|image| {
        for y in 14..22 {
            for x in 98..112 {
                image[y * w + x] = [0, 0, 0];
            }
        }
    });
    // Text: the "I" of "Identity" (x 10..13, baseline 50, cap height 17)
    // erased must exceed the text tolerance.
    let (ours, reference, w, h) = renderings(&executable, "truetype.pdf", 0, 3.0);
    let mut erased = ours.clone();
    for y in 246..303 {
        for x in 28..44 {
            erased[y * w + x] = [255, 255, 255];
        }
    }
    let ratio = mismatch(&erased, &reference, w, h);
    eprintln!("mutation erased glyph: {:.3} % mismatching", ratio * 100.0);
    assert!(
        ratio > MAX_TEXT_MISMATCHES,
        "an erased glyph is not detected"
    );
    // The known CMYK disagreement is detected as well.
    let cmyk = mismatch_ratio(&executable, "shapes.pdf", 2, 1.0);
    assert!(
        cmyk > MAX_MISMATCHES,
        "the CMYK page should differ from pdftoppm"
    );
    eprintln!("fixtures: rendering matches pdftoppm");
}
