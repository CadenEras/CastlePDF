//! Measurement, not a check (ignored by default): on the first pages of a
//! local corpus that still report `ColorSpace`, the colour space families
//! their resources declare (page, and forms and images one level down),
//! and for Separation and DeviceN the type of their tint transform. Files
//! are read from `CASTLE_CENSUS_DIR`, never from the repository; only
//! counts are printed.
//!
//!   CASTLE_CENSUS_DIR=/path/to/pdfs cargo test -p pdf-render --release \
//!     --test color_census -- --ignored --nocapture

#![allow(clippy::print_stderr, reason = "measurement code: results are printed")]

use std::collections::BTreeMap;

use pdf_document::Document;
use pdf_render::{RenderOptions, Unsupported, render_page};
use pdf_syntax::{
    Budget, CancellationToken, Dictionary, IndirectValue, MemorySource, Object, PdfFile,
    ResourceLimits,
};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

/// `object` resolved once; a stream gives its dictionary.
fn direct(file: &PdfFile<MemorySource>, object: &Object) -> Option<Object> {
    match object {
        Object::Reference(id) => match file.resolve(*id, &mut budget()).ok()? {
            IndirectValue::Object(object) => Some(object),
            IndirectValue::Stream(stream) => Some(Object::Dictionary(stream.dictionary)),
        },
        other => Some(other.clone()),
    }
}

fn dictionary(file: &PdfFile<MemorySource>, object: &Object) -> Option<Dictionary> {
    match direct(file, object)? {
        Object::Dictionary(dictionary) => Some(dictionary),
        _ => None,
    }
}

/// The family of a colour space value and, for Separation and DeviceN,
/// the type of its tint transform (`Separation/f4`...); the base of an
/// Indexed space as well.
fn families(file: &PdfFile<MemorySource>, space: &Object, out: &mut Vec<String>) {
    let Some(space) = direct(file, space) else {
        return;
    };
    let items = match &space {
        Object::Name(name) => {
            out.push(String::from_utf8_lossy(name).into_owned());
            return;
        }
        Object::Array(items) => items,
        _ => return,
    };
    let Some(Object::Name(family)) = items.first() else {
        return;
    };
    let family = String::from_utf8_lossy(family).into_owned();
    match family.as_str() {
        "Separation" | "DeviceN" => {
            let function = items.get(3).and_then(|f| match direct(file, f)? {
                Object::Dictionary(d) => d.get(b"FunctionType").and_then(Object::as_integer),
                Object::Array(_) => Some(-1),
                _ => None,
            });
            let alternate = items.get(2).and_then(|a| direct(file, a)).map(|a| match a {
                Object::Name(n) => String::from_utf8_lossy(&n).into_owned(),
                Object::Array(i) => i
                    .first()
                    .and_then(Object::as_name)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default(),
                _ => String::new(),
            });
            out.push(format!(
                "{family}/f{} over {}",
                function.map_or("?".to_owned(), |f| f.to_string()),
                alternate.unwrap_or_default()
            ));
        }
        "Indexed" => {
            out.push(family);
            if let Some(base) = items.get(1) {
                let mut inner = Vec::new();
                families(file, base, &mut inner);
                out.extend(inner.into_iter().map(|f| format!("Indexed base {f}")));
            }
        }
        _ => out.push(family),
    }
}

/// Families declared by `resources`, and by the forms and images it names
/// (one level).
fn census(file: &PdfFile<MemorySource>, resources: &Object, out: &mut Vec<String>, depth: u8) {
    let Some(resources) = dictionary(file, resources) else {
        return;
    };
    if let Some(spaces) = resources
        .get(b"ColorSpace")
        .and_then(|s| dictionary(file, s))
    {
        for (_, space) in spaces.iter() {
            families(file, space, out);
        }
    }
    if let Some(xobjects) = resources.get(b"XObject").and_then(|x| dictionary(file, x)) {
        for (_, xobject) in xobjects.iter() {
            let Some(xobject) = dictionary(file, xobject) else {
                continue;
            };
            if let Some(space) = xobject.get(b"ColorSpace") {
                families(file, space, out);
            }
            if depth < 2
                && let Some(inner) = xobject.get(b"Resources")
            {
                census(file, inner, out, depth + 1);
            }
        }
    }
}

#[test]
#[ignore = "measurement on a local corpus: CASTLE_CENSUS_DIR"]
fn colour_spaces_of_pages_still_reporting_colour_space() {
    let Ok(dir) = std::env::var("CASTLE_CENSUS_DIR") else {
        eprintln!("CASTLE_CENSUS_DIR not set");
        return;
    };
    let mut paths = Vec::new();
    let mut pending = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let (mut pages, mut files) = (0, BTreeMap::<String, usize>::new());
    for path in &paths {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
            continue;
        };
        let Ok(document) = Document::open(source, &mut budget()) else {
            continue;
        };
        let Some(page) = document.page(0) else {
            continue;
        };
        let Ok(rendering) = render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget(),
        ) else {
            continue;
        };
        if !rendering.skipped.contains(&Unsupported::ColorSpace) {
            continue;
        }
        pages += 1;
        let mut found = Vec::new();
        if let Some(resources) = page.resources.as_deref() {
            census(document.file(), resources, &mut found, 0);
        }
        found.sort();
        found.dedup();
        for family in found {
            *files.entry(family).or_default() += 1;
        }
    }
    eprintln!("{pages} first pages reporting ColorSpace; pages declaring:");
    let mut rows: Vec<_> = files.into_iter().collect();
    rows.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (family, count) in rows {
        eprintln!("  {count:5}  {family}");
    }
}

/// Shading types (and their colour space families) declared by
/// `resources`, and by the forms it names (two levels).
fn shading_census(
    file: &PdfFile<MemorySource>,
    resources: &Object,
    out: &mut Vec<String>,
    depth: u8,
) {
    let Some(resources) = dictionary(file, resources) else {
        return;
    };
    if let Some(shadings) = resources.get(b"Shading").and_then(|s| dictionary(file, s)) {
        for (_, shading) in shadings.iter() {
            let Some(shading) = dictionary(file, shading) else {
                continue;
            };
            let kind = shading.get(b"ShadingType").and_then(Object::as_integer);
            let mut space = Vec::new();
            if let Some(cs) = shading.get(b"ColorSpace") {
                families(file, cs, &mut space);
            }
            let function = match shading.get(b"Function").and_then(|f| direct(file, f)) {
                Some(Object::Dictionary(d)) => d
                    .get(b"FunctionType")
                    .and_then(Object::as_integer)
                    .map_or("f?".to_owned(), |t| format!("f{t}")),
                Some(Object::Array(a)) => format!("{} functions", a.len()),
                _ => "no function".to_owned(),
            };
            let extend = shading
                .get(b"Extend")
                .map_or("extend default".to_owned(), |e| {
                    format!("{e:?}").replace("Boolean", "")
                });
            out.push(format!(
                "type {} {}",
                kind.map_or("?".to_owned(), |k| k.to_string()),
                function
            ));
            out.push(format!(
                "type {} in {}",
                kind.unwrap_or(0),
                space.first().cloned().unwrap_or_default()
            ));
            out.push(format!("type {} {extend}", kind.unwrap_or(0)));
        }
    }
    if let Some(xobjects) = resources.get(b"XObject").and_then(|x| dictionary(file, x)) {
        for (_, xobject) in xobjects.iter() {
            if depth < 2
                && let Some(xobject) = dictionary(file, xobject)
                && let Some(inner) = xobject.get(b"Resources")
            {
                shading_census(file, inner, out, depth + 1);
            }
        }
    }
}

#[test]
#[ignore = "measurement on a local corpus: CASTLE_CENSUS_DIR"]
fn shadings_of_pages_reporting_shading() {
    let Ok(dir) = std::env::var("CASTLE_CENSUS_DIR") else {
        eprintln!("CASTLE_CENSUS_DIR not set");
        return;
    };
    let mut paths = Vec::new();
    let mut pending = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let (mut pages, mut counts) = (0, BTreeMap::<String, usize>::new());
    for path in &paths {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
            continue;
        };
        let Ok(document) = Document::open(source, &mut budget()) else {
            continue;
        };
        let Some(page) = document.page(0) else {
            continue;
        };
        let Ok(rendering) = render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget(),
        ) else {
            continue;
        };
        if !rendering.skipped.contains(&Unsupported::Shading) {
            continue;
        }
        pages += 1;
        let mut found = Vec::new();
        if let Some(resources) = page.resources.as_deref() {
            shading_census(document.file(), resources, &mut found, 0);
        }
        found.sort();
        found.dedup();
        for item in found {
            *counts.entry(item).or_default() += 1;
        }
    }
    eprintln!("{pages} first pages reporting Shading; pages declaring:");
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (item, count) in rows {
        eprintln!("  {count:5}  {item}");
    }
}
