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
