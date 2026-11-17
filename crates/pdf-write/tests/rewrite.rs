//! Documents rewritten from a page plan (ADR-0008, tranche 4.2): every
//! public fixture rewritten unchanged reads back strictly and renders the
//! same, pixel for pixel; a plan without a page leaves that page's content
//! out of the file; the same document gives the same bytes.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, PdfFile, ResourceLimits};
use pdf_write::{PlanPage, identity_plan, rewrite};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn open(bytes: Vec<u8>) -> Document<MemorySource> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

/// Opens with the strict reader: no repair allowed.
fn open_strict(bytes: Vec<u8>) -> Document<MemorySource> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let file = PdfFile::open_strict(source, &mut budget()).expect("strict open");
    Document::from_file(file, &mut budget()).expect("document")
}

const LIMIT: u64 = 64 << 20;

#[test]
fn every_fixture_rewritten_unchanged_renders_the_same() {
    let mut checked = 0;
    for entry in std::fs::read_dir(repo("tests/corpus/public")).expect("fixtures") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "pdf") {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let original = open(std::fs::read(&path).expect("read"));
        let plan = identity_plan(&original);
        let rewritten = rewrite(&original, &plan, LIMIT, &mut budget())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let written = open_strict(rewritten.bytes);
        assert_eq!(written.page_count(), original.page_count(), "{name}");
        for (index, (a, b)) in original
            .pages()
            .iter()
            .zip(written.pages())
            .enumerate()
            .take(6)
        {
            assert_eq!(a.media_box, b.media_box, "{name} page {}", index + 1);
            assert_eq!(a.crop_box, b.crop_box, "{name} page {}", index + 1);
            assert_eq!(a.rotation, b.rotation, "{name} page {}", index + 1);
            let options = RenderOptions::default();
            let before = render_page(original.file(), a, &options, &mut budget());
            let after = render_page(written.file(), b, &options, &mut budget());
            match (before, after) {
                (Ok(before), Ok(after)) => {
                    assert_eq!(before.skipped, after.skipped, "{name} page {}", index + 1);
                    assert!(
                        before.bitmap == after.bitmap,
                        "{name} page {}: pixels differ",
                        index + 1
                    );
                }
                (Err(_), Err(_)) => {}
                (before, after) => panic!(
                    "{name} page {}: before {:?}, after {:?}",
                    index + 1,
                    before.err(),
                    after.err()
                ),
            }
        }
        checked += 1;
    }
    assert!(checked >= 20, "{checked} fixtures");
}

#[test]
fn rewriting_is_deterministic() {
    let original = open(std::fs::read(repo("tests/corpus/public/forms.pdf")).expect("read"));
    let plan = identity_plan(&original);
    let a = rewrite(&original, &plan, LIMIT, &mut budget()).expect("first");
    let b = rewrite(&original, &plan, LIMIT, &mut budget()).expect("second");
    assert_eq!(a, b);
    // Rewriting a rewritten file gives the same bytes again.
    let again = open(a.bytes.clone());
    let c = rewrite(&again, &identity_plan(&again), LIMIT, &mut budget()).expect("third");
    assert_eq!(a.bytes, c.bytes);
}

/// A document of three pages whose contents hold a distinct marker, a
/// link on page 1 to page 3, an outline and a form.
fn three_pages() -> Vec<u8> {
    let content = |n: usize| format!("% marker-of-page-{n}\n0 0 1 rg 10 10 50 50 re f");
    let mut bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines << /Count 0 >> /AcroForm << /Fields [] /SigFlags 3 >> \
         /Lang (fr) >>"
            .to_owned(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 100 200] >>".to_owned(),
    ];
    for n in 0..3 {
        let annots = if n == 0 {
            " /Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [5 0 R /Fit] >>]"
        } else {
            ""
        };
        bodies.push(format!(
            "<< /Type /Page /Parent 2 0 R /Rotate {} /Contents {} 0 R{annots} >>",
            n * 90,
            6 + n
        ));
    }
    for n in 0..3 {
        let c = content(n + 1);
        bodies.push(format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()));
    }
    let mut out = b"%PDF-1.6\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in &offsets {
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_page_left_out_of_the_plan_is_not_in_the_file() {
    // Pages 2 then 1, page 3 left out: its content is not copied, the link
    // on page 1 to it loses its destination, inherited boxes and rotations
    // are kept, and what the catalog loses is listed.
    let original = open(three_pages());
    let plan = [
        PlanPage {
            page: 1,
            rotation: 0,
        },
        PlanPage {
            page: 0,
            rotation: 270,
        },
    ];
    let rewritten = rewrite(&original, &plan, LIMIT, &mut budget()).expect("rewrite");
    assert!(contains(&rewritten.bytes, b"marker-of-page-1"));
    assert!(contains(&rewritten.bytes, b"marker-of-page-2"));
    assert!(!contains(&rewritten.bytes, b"marker-of-page-3"));
    assert_eq!(rewritten.dropped, vec!["AcroForm", "Outlines", "Signature"]);
    let written = open_strict(rewritten.bytes);
    assert_eq!(written.page_count(), 2);
    let (first, second) = (&written.pages()[0], &written.pages()[1]);
    assert_eq!((first.rotation, second.rotation), (90, 270));
    assert_eq!(first.media_box.ury, 200.0);
    assert_eq!(
        written.catalog().get(b"Lang"),
        Some(&pdf_syntax::Object::String(b"fr".to_vec()))
    );
    let annotations = second.annotations.clone().expect("annotations");
    let pdf_syntax::Object::Array(annotations) = annotations else {
        panic!("annotations");
    };
    let pdf_syntax::Object::Dictionary(link) = &annotations[0] else {
        panic!("link");
    };
    assert_eq!(
        link.get(b"Dest")
            .and_then(pdf_syntax::Object::as_array)
            .map(|d| d[0].clone()),
        Some(pdf_syntax::Object::Null)
    );
}

#[test]
fn invalid_plans_are_refused() {
    let original = open(three_pages());
    for plan in [
        vec![],
        vec![PlanPage {
            page: 3,
            rotation: 0,
        }],
        vec![PlanPage {
            page: 0,
            rotation: 45,
        }],
    ] {
        assert_eq!(
            rewrite(&original, &plan, LIMIT, &mut budget()).err(),
            Some(pdf_write::Error::InvalidPlan),
            "{plan:?}"
        );
    }
    // A limit too small to hold the file.
    let plan = identity_plan(&original);
    assert_eq!(
        rewrite(&original, &plan, 200, &mut budget()).err(),
        Some(pdf_write::Error::TooLarge { limit: 200 })
    );
}

#[test]
fn optional_content_properties_are_kept() {
    // Found by the corpus measurement: dropping /OCProperties would make
    // other readers show the layers a document hides. Kept, with the
    // groups it names.
    let pdf = "%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [4 0 R] \
               /D << /OFF [4 0 R] >> >> >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
               3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n\
               4 0 obj\n<< /Type /OCG /Name (Hidden) >>\nendobj\n";
    let mut bytes = pdf.as_bytes().to_vec();
    // A wrong table: the reader rebuilds it (only the objects matter here).
    bytes.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    let original = open(bytes);
    let rewritten =
        rewrite(&original, &identity_plan(&original), LIMIT, &mut budget()).expect("rewrite");
    assert!(rewritten.dropped.is_empty(), "{:?}", rewritten.dropped);
    let written = open_strict(rewritten.bytes);
    let properties = written
        .catalog()
        .get(b"OCProperties")
        .and_then(pdf_syntax::Object::as_dictionary);
    let off = properties
        .and_then(|p| p.get(b"D"))
        .and_then(pdf_syntax::Object::as_dictionary)
        .and_then(|d| d.get(b"OFF"))
        .and_then(pdf_syntax::Object::as_array)
        .and_then(|a| a.first())
        .and_then(pdf_syntax::Object::as_reference)
        .expect("the hidden group");
    let Ok(pdf_syntax::IndirectValue::Object(group)) = written.file().resolve(off, &mut budget())
    else {
        panic!("group");
    };
    assert_eq!(
        group.as_dictionary().and_then(|g| g.get(b"Name")),
        Some(&pdf_syntax::Object::String(b"Hidden".to_vec()))
    );
}

/// A PDF of `bodies` (objects 1, 2...), object 1 the catalog.
fn assemble(bodies: &[&str]) -> Vec<u8> {
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
    for offset in &offsets {
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
fn nothing_left_out_comes_back_through_another_object() {
    // Found in review: page 1 is kept; its annotation refers to the
    // catalog (whose form holds a marker), replies (/IRT) to an annotation
    // of page 2, which is left out, and links to a page object outside the
    // page tree. None of them is copied: each reference becomes null.
    let pdf = assemble(&[
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 9 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R] /AA << /O << /S /Named /N /NextPage >> >> >>",
        "<< /Type /Page /Parent 2 0 R /Annots [6 0 R] >>",
        "<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /X 1 0 R /IRT 6 0 R \
         /A << /S /GoTo /D [7 0 R /Fit] >> >>",
        "<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents (marker-of-annotation-2) >>",
        "<< /Type /Page /MediaBox [0 0 10 10] /Contents 8 0 R >>",
        "<< /Length 21 >>\nstream\n% marker-of-orphan\n\nendstream",
        "<< /Fields [] /DA (marker-of-form) >>",
    ]);
    let original = open(pdf);
    assert_eq!(original.page_count(), 2);
    let plan = [PlanPage {
        page: 0,
        rotation: 0,
    }];
    let rewritten = rewrite(&original, &plan, LIMIT, &mut budget()).expect("rewrite");
    for marker in [
        &b"marker-of-annotation-2"[..],
        b"marker-of-orphan",
        b"marker-of-form",
    ] {
        assert!(
            !contains(&rewritten.bytes, marker),
            "{}",
            String::from_utf8_lossy(marker)
        );
    }
    assert_eq!(rewritten.dropped, vec!["AcroForm"]);
    let written = open_strict(rewritten.bytes);
    let page = &written.pages()[0];
    // The page keeps its own additional actions (document content, never
    // run by this engine).
    let Ok(pdf_syntax::IndirectValue::Object(pdf_syntax::Object::Dictionary(own))) =
        written.file().resolve(page.id, &mut budget())
    else {
        panic!("page");
    };
    assert!(own.get(b"AA").is_some());
    let Some(pdf_syntax::Object::Array(annotations)) = page.annotations.clone() else {
        panic!("annotations");
    };
    let Ok(pdf_syntax::IndirectValue::Object(pdf_syntax::Object::Dictionary(annotation))) =
        written.file().resolve(
            annotations[0].as_reference().expect("reference"),
            &mut budget(),
        )
    else {
        panic!("annotation");
    };
    assert_eq!(annotation.get(b"X"), Some(&pdf_syntax::Object::Null));
    assert_eq!(annotation.get(b"IRT"), Some(&pdf_syntax::Object::Null));
}

#[test]
fn a_page_object_outside_the_tree_is_written_as_null() {
    // The page object reached only through a link, never through the page
    // tree: written as null (its number exists, its content does not).
    let pdf = assemble(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [<< /Subtype /Link /Rect [0 0 1 1] /Dest [4 0 R /Fit] >>] >>",
        "<< /Type /Page /MediaBox [0 0 10 10] /Contents 5 0 R >>",
        "<< /Length 18 >>\nstream\n% marker-of-orphan\nendstream",
    ]);
    let original = open(pdf);
    let rewritten =
        rewrite(&original, &identity_plan(&original), LIMIT, &mut budget()).expect("rewrite");
    assert!(!contains(&rewritten.bytes, b"marker-of-orphan"));
    assert_eq!(open_strict(rewritten.bytes).page_count(), 1);
}

/// An object body for a stream of `data`, `/Length` computed.
fn stream(entries: &str, data: &str) -> String {
    format!(
        "<< {entries} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// Two pages sharing the resources of their page tree node (object 9):
/// page 1 draws `/Im1` (object 5), page 2 `/Im2` (object 6, which holds a
/// marker). Each field replaces a part; `objects` are numbered from 10.
struct Fixture<'a> {
    im1: String,
    content1: String,
    page1: &'a str,
    spaces: &'a str,
    resources: &'a str,
    objects: Vec<String>,
}

fn image(entries: &str, marker: &str) -> String {
    stream(
        &format!(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 {entries} \
             /BitsPerComponent 8 /Marker ({marker})"
        ),
        "\u{7f}",
    )
}

impl Default for Fixture<'_> {
    fn default() -> Self {
        Self {
            im1: image("/ColorSpace /DeviceGray", "marker-of-image-1"),
            content1: stream("", "q 20 0 0 20 0 0 cm /Im1 Do Q"),
            page1: "/Contents 7 0 R",
            spaces: "/DefaultRGB /DeviceRGB /Unused /DeviceGray",
            resources: "",
            objects: Vec::new(),
        }
    }
}

impl Fixture<'_> {
    fn pdf(&self) -> Vec<u8> {
        let page1 = format!("<< /Type /Page /Parent 2 0 R {} >>", self.page1);
        let resources = format!(
            "<< /XObject << /Im1 5 0 R /Im2 6 0 R >> /ColorSpace << {} >> /ProcSet [/PDF] {} >>",
            self.spaces, self.resources
        );
        let image2 = image("/ColorSpace /DeviceGray", "marker-of-image-2");
        let content2 = stream("", "q 20 0 0 20 0 0 cm /Im2 Do Q");
        let mut bodies: Vec<&str> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 20 20] /Resources 9 0 R >>",
            &page1,
            "<< /Type /Page /Parent 2 0 R /Contents 8 0 R >>",
            &self.im1,
            &image2,
            &self.content1,
            &content2,
            &resources,
        ];
        bodies.extend(self.objects.iter().map(String::as_str));
        assemble(&bodies)
    }
}

fn shared_resources(im1: &str, content1: &str) -> Vec<u8> {
    let mut fixture = Fixture {
        content1: content1.to_owned(),
        ..Fixture::default()
    };
    if !im1.is_empty() {
        fixture.im1 = im1.to_owned();
    }
    fixture.pdf()
}

const PAGE_1_ONLY: [PlanPage; 1] = [PlanPage {
    page: 0,
    rotation: 0,
}];

#[test]
fn shared_resources_keep_only_what_the_kept_pages_draw() {
    let pdf = shared_resources(
        "",
        &stream(
            "",
            "q 10 0 0 10 0 0 cm /Im1 Do Q\nq 5 0 0 5 10 10 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \u{1f} EI Q",
        ),
    );
    let original = open(pdf);
    let rewritten = rewrite(&original, &PAGE_1_ONLY, LIMIT, &mut budget()).expect("rewrite");
    assert!(contains(&rewritten.bytes, b"marker-of-image-1"));
    assert!(!contains(&rewritten.bytes, b"marker-of-image-2"));
    let written = open_strict(rewritten.bytes);
    let page = &written.pages()[0];
    let resources = page
        .resources
        .as_deref()
        .and_then(pdf_syntax::Object::as_dictionary);
    let resources = resources.expect("resources");
    // A default colour space is used without being named; other entries
    // are copied as they are.
    let spaces = resources
        .get(b"ColorSpace")
        .and_then(pdf_syntax::Object::as_dictionary);
    let spaces = spaces.expect("color spaces");
    assert!(spaces.get(b"DefaultRGB").is_some());
    assert!(spaces.get(b"Unused").is_none());
    assert!(resources.get(b"ProcSet").is_some());
    let options = RenderOptions::default();
    let before = render_page(
        original.file(),
        &original.pages()[0],
        &options,
        &mut budget(),
    );
    let after = render_page(written.file(), page, &options, &mut budget());
    assert_eq!(before.expect("before").bitmap, after.expect("after").bitmap);
}

#[test]
fn an_identity_plan_prunes_nothing() {
    let pdf = shared_resources("", &stream("", "/Im1 Do"));
    let original = open(pdf);
    let rewritten =
        rewrite(&original, &identity_plan(&original), LIMIT, &mut budget()).expect("rewrite");
    let written = open_strict(rewritten.bytes);
    for page in written.pages() {
        let resources = page.resources.as_deref().expect("resources");
        // Still one shared object, with every entry.
        assert!(matches!(resources, pdf_syntax::Object::Reference(_)));
    }
}

#[test]
fn resources_are_kept_whole_when_what_is_used_cannot_be_told() {
    // Each draws with the page's resources, so may use /Im2: a form, a
    // tiling pattern, a Type 3 font, a soft mask group, an annotation
    // appearance, none with /Resources of its own.
    let form = || stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", "/Im2 Do");
    let content = |text: &str| stream("", text);
    let cases = [
        Fixture {
            im1: form(),
            ..Fixture::default()
        },
        Fixture {
            content1: content("/Pattern cs /P1 scn 0 0 1 1 re f /Im1 Do"),
            resources: "/Pattern << /P1 10 0 R >>",
            objects: vec![stream(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1",
                "/Im2 Do",
            )],
            ..Fixture::default()
        },
        Fixture {
            content1: content("/Im1 Do BT /F1 1 Tf (a) Tj ET"),
            resources: "/Font << /F1 10 0 R >>",
            objects: vec![
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] \
                 /CharProcs << /a 11 0 R >> /Encoding << /Differences [97 /a] >> \
                 /FirstChar 97 /LastChar 97 /Widths [1] >>"
                    .to_owned(),
                stream("", "1 0 d0 /Im2 Do"),
            ],
            ..Fixture::default()
        },
        Fixture {
            content1: content("/G1 gs /Im1 Do"),
            resources: "/ExtGState << /G1 << /SMask << /S /Luminosity /G 10 0 R >> >> >>",
            objects: vec![form()],
            ..Fixture::default()
        },
        Fixture {
            page1: "/Contents 7 0 R /Annots [<< /Subtype /Square /Rect [0 0 1 1] /AP << /N 10 0 R >> >>]",
            objects: vec![form()],
            ..Fixture::default()
        },
        // Content that cannot be decoded, an inline image without its end,
        // a content array with a part that is not a stream.
        Fixture {
            content1: stream("/Filter /NoSuchDecode", "/Im1 Do"),
            ..Fixture::default()
        },
        Fixture {
            content1: content("/Im1 Do BI /W 1 /H 1 ID xyz"),
            ..Fixture::default()
        },
        Fixture {
            page1: "/Contents [7 0 R null]",
            ..Fixture::default()
        },
    ];
    for (index, fixture) in cases.into_iter().enumerate() {
        let original = open(fixture.pdf());
        let rewritten = rewrite(&original, &PAGE_1_ONLY, LIMIT, &mut budget()).expect("rewrite");
        assert!(
            contains(&rewritten.bytes, b"marker-of-image-2"),
            "case {index}"
        );
        assert_eq!(rewritten.resources_kept_whole, vec![0], "case {index}");
    }
}

#[test]
fn a_colour_space_named_by_an_image_is_kept() {
    // Found in review: /CS0 is named by the image, not by the content.
    let fixture = Fixture {
        im1: image("/ColorSpace /CS0", "marker-of-image-1"),
        spaces: "/CS0 [/Indexed /DeviceRGB 255 10 0 R] /Unused /DeviceGray",
        objects: vec![stream("", &"\u{7f}\u{0}\u{0}".repeat(256))],
        ..Fixture::default()
    };
    let original = open(fixture.pdf());
    let rewritten = rewrite(&original, &PAGE_1_ONLY, LIMIT, &mut budget()).expect("rewrite");
    assert!(rewritten.resources_kept_whole.is_empty());
    assert!(!contains(&rewritten.bytes, b"marker-of-image-2"));
    let written = open_strict(rewritten.bytes);
    let options = RenderOptions::default();
    let before = render_page(
        original.file(),
        &original.pages()[0],
        &options,
        &mut budget(),
    );
    let after = render_page(written.file(), &written.pages()[0], &options, &mut budget());
    let (before, after) = (before.expect("before"), after.expect("after"));
    assert!(before.skipped.is_empty(), "{:?}", before.skipped);
    assert_eq!(before.bitmap, after.bitmap);
    assert_eq!(before.skipped, after.skipped);
}

#[test]
fn a_page_given_twice_is_pruned_once_the_same() {
    let original = open(Fixture::default().pdf());
    let plan = [PAGE_1_ONLY[0], PAGE_1_ONLY[0]];
    let rewritten = rewrite(&original, &plan, LIMIT, &mut budget()).expect("rewrite");
    assert!(!contains(&rewritten.bytes, b"marker-of-image-2"));
    assert_eq!(open_strict(rewritten.bytes).page_count(), 2);
}
