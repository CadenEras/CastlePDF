//! A document rewritten from a page plan (ADR-0008, tranche 4.2): the
//! objects reachable from a new catalog, renumbered, written whole with
//! `FileWriter`.
//!
//! Rules that keep a page left out of the plan out of the file, and every
//! loss said:
//! - a reference to a page not in the plan becomes `null` (a link to it,
//!   an annotation's `/P`), so that its content is never copied;
//! - `/Parent` is removed from every copied dictionary and set again on the
//!   pages only: the old page tree, and the form field tree reached from a
//!   widget, are not copied through it;
//! - the catalog keeps only entries that name no page; the others are
//!   listed in `Rewritten::dropped` (ADR-0008, point 3);
//! - stream data is copied as stored, still encoded.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use pdf_document::{Document, Page};
use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, ObjectId, Source};

use crate::{Error, FileWriter};

/// One page of the plan: page `page` of the source (0-based), turned by
/// `rotation` degrees clockwise (a multiple of 90) on top of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanPage {
    pub page: usize,
    pub rotation: u16,
}

/// A rewritten document: its bytes, and what the source had that it does
/// not keep (catalog entries by name; `Signature` when the source is
/// signed, a signature being invalidated by any rewrite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    pub bytes: Vec<u8>,
    pub dropped: Vec<String>,
}

/// Catalog entries kept: none of them names a page.
const KEPT_CATALOG_KEYS: [&[u8]; 6] = [
    b"Metadata",
    b"ViewerPreferences",
    b"PageLayout",
    b"PageMode",
    b"Lang",
    b"OutputIntents",
];

/// The identity plan of `document`: every page, in order, unturned.
pub fn identity_plan<S: Source>(document: &Document<S>) -> Vec<PlanPage> {
    (0..document.page_count())
        .map(|page| PlanPage { page, rotation: 0 })
        .collect()
}

/// Writes the pages of `plan`, from `document`, as a new file of at most
/// `limit` bytes. An empty plan, a page out of range or a rotation that is
/// not a multiple of 90 is `InvalidPlan`. Reading the source is charged to
/// `budget` and stops at its limits or cancellation.
pub fn rewrite<S: Source>(
    document: &Document<S>,
    plan: &[PlanPage],
    limit: u64,
    budget: &mut Budget,
) -> Result<Rewritten, Error> {
    if plan.is_empty()
        || plan
            .iter()
            .any(|p| p.page >= document.page_count() || p.rotation % 90 != 0)
    {
        return Err(Error::InvalidPlan);
    }
    let mut copier = Copier::new(document, plan);
    let version = document.file().version();
    let minor = if version.major == 1 { version.minor.max(4) } else { version.minor };
    let mut writer = FileWriter::new(version.major.max(1), minor, limit)?;

    // The pages, each as its own new object.
    let mut kids = Vec::with_capacity(plan.len());
    for (index, entry) in plan.iter().enumerate() {
        let page = document.page(entry.page).ok_or(Error::InvalidPlan)?;
        let number = copier.page_numbers.get(index).copied().ok_or(Error::InvalidPlan)?;
        let dictionary = copier.page_dictionary(document, page, entry.rotation, budget)?;
        writer.object(number, &Object::Dictionary(dictionary))?;
        kids.push(reference(number));
    }
    // The catalog: kept entries, the others listed as dropped.
    let mut catalog = Dictionary::new();
    catalog.insert(b"Type".to_vec(), Object::Name(b"Catalog".to_vec()));
    catalog.insert(b"Pages".to_vec(), reference(PAGES));
    let mut dropped = Vec::new();
    for (key, value) in document.catalog().iter() {
        if KEPT_CATALOG_KEYS.contains(&key) {
            let value = copier.map(value.clone(), budget)?;
            catalog.insert(key.to_vec(), value);
        } else if !matches!(key, b"Type" | b"Pages" | b"Version") {
            dropped.push(String::from_utf8_lossy(key).into_owned());
        }
    }
    if is_signed(document, budget)? {
        dropped.push("Signature".to_owned());
    }
    // The document information dictionary, if any.
    let info = match document.file().trailer().get(b"Info") {
        Some(info) => Some(copier.map(info.clone(), budget)?),
        None => None,
    };
    // Every object reached, until none is left.
    while let Some((id, number)) = copier.pending.pop_front() {
        budget.check_cancelled()?;
        match document.file().resolve(id, budget) {
            Ok(IndirectValue::Object(object)) => {
                let object = copier.map(object, budget)?;
                writer.object(number, &object)?;
            }
            Ok(IndirectValue::Stream(stream)) => {
                let mut dictionary = stream.dictionary.clone();
                // Written again by `FileWriter` from the data.
                dictionary.remove(b"Length");
                let Object::Dictionary(dictionary) =
                    copier.map(Object::Dictionary(dictionary), budget)?
                else {
                    return Err(Error::InvalidPlan);
                };
                let data = document.file().stream_data(&stream)?;
                writer.stream(number, &dictionary, &data)?;
            }
            // A damaged object is written as null, as a reader reads it.
            Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                writer.object(number, &Object::Null)?;
            }
            Err(other) => return Err(Error::Syntax(other)),
        }
    }
    writer.object(CATALOG, &Object::Dictionary(catalog))?;
    let mut pages = Dictionary::new();
    pages.insert(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
    pages.insert(b"Count".to_vec(), Object::Integer(kids.len() as i64));
    pages.insert(b"Kids".to_vec(), Object::Array(kids));
    writer.object(PAGES, &Object::Dictionary(pages))?;
    let mut trailer = Dictionary::new();
    trailer.insert(b"Root".to_vec(), reference(CATALOG));
    if let Some(info) = info {
        trailer.insert(b"Info".to_vec(), info);
    }
    Ok(Rewritten {
        bytes: writer.finish(&trailer)?,
        dropped,
    })
}

const CATALOG: u32 = 1;
const PAGES: u32 = 2;

fn reference(number: u32) -> Object {
    Object::Reference(ObjectId {
        number,
        generation: 0,
    })
}

/// Renumbers the objects reached from what is written, and keeps the
/// pages out of the plan out.
struct Copier {
    /// New number of each source object reached, pages of the plan first.
    numbers: BTreeMap<ObjectId, u32>,
    /// Source objects reached but not written yet, with their numbers.
    pending: VecDeque<(ObjectId, u32)>,
    /// The new number of each plan entry, in order.
    page_numbers: Vec<u32>,
    /// Every page of the source: one not in the plan is never copied.
    pages: BTreeSet<ObjectId>,
    next: u32,
}

impl Copier {
    fn new<S: Source>(document: &Document<S>, plan: &[PlanPage]) -> Self {
        let mut copier = Self {
            numbers: BTreeMap::new(),
            pending: VecDeque::new(),
            page_numbers: Vec::with_capacity(plan.len()),
            pages: document.pages().iter().map(|p| p.id).collect(),
            next: PAGES + 1,
        };
        for entry in plan {
            let number = copier.next;
            copier.next += 1;
            copier.page_numbers.push(number);
            // A page given twice is one object for what refers to it.
            if let Some(page) = document.page(entry.page) {
                copier.numbers.entry(page.id).or_insert(number);
            }
        }
        copier
    }

    /// `object` with its references renumbered, the objects they reach
    /// queued, a reference to a page out of the plan made `null`, and
    /// `/Parent` removed from its dictionaries.
    fn map(&mut self, object: Object, budget: &mut Budget) -> Result<Object, Error> {
        Ok(match object {
            Object::Reference(id) => {
                if let Some(number) = self.numbers.get(&id) {
                    reference(*number)
                } else if self.pages.contains(&id) {
                    Object::Null
                } else {
                    budget.check_cancelled()?;
                    let number = self.next;
                    self.next = self.next.checked_add(1).ok_or(Error::InvalidObjectNumber(u32::MAX))?;
                    self.numbers.insert(id, number);
                    self.pending.push_back((id, number));
                    reference(number)
                }
            }
            Object::Array(items) => Object::Array(
                items
                    .into_iter()
                    .map(|item| self.map(item, budget))
                    .collect::<Result<_, _>>()?,
            ),
            Object::Dictionary(dictionary) => {
                let mut out = Dictionary::new();
                for (key, value) in dictionary.iter() {
                    if key == b"Parent" {
                        continue;
                    }
                    out.insert(key.to_vec(), self.map(value.clone(), budget)?);
                }
                Object::Dictionary(out)
            }
            other => other,
        })
    }

    /// The dictionary of `page` as written: its own entries, its inherited
    /// attributes (boxes, rotation, resources) made explicit from the
    /// model, its rotation turned by `rotation`, its `/Parent` the new page
    /// tree. Left out: articles (`/B`, with the threads they belong to)
    /// and additional actions (`/AA`, which may run JavaScript and are not
    /// kept by this writer).
    fn page_dictionary<S: Source>(
        &mut self,
        document: &Document<S>,
        page: &Page,
        rotation: u16,
        budget: &mut Budget,
    ) -> Result<Dictionary, Error> {
        let own = match document.file().resolve(page.id, budget)? {
            IndirectValue::Object(Object::Dictionary(dictionary)) => dictionary,
            _ => Dictionary::new(),
        };
        let mut dictionary = Dictionary::new();
        for (key, value) in own.iter() {
            if matches!(
                key,
                b"Parent" | b"MediaBox" | b"CropBox" | b"Rotate" | b"Resources" | b"B" | b"AA"
            ) {
                continue;
            }
            dictionary.insert(key.to_vec(), self.map(value.clone(), budget)?);
        }
        let rectangle = |r: &pdf_document::Rectangle| {
            Object::Array(vec![
                number(r.llx),
                number(r.lly),
                number(r.urx),
                number(r.ury),
            ])
        };
        dictionary.insert(b"Type".to_vec(), Object::Name(b"Page".to_vec()));
        dictionary.insert(b"MediaBox".to_vec(), rectangle(&page.media_box));
        if page.crop_box != page.media_box {
            dictionary.insert(b"CropBox".to_vec(), rectangle(&page.crop_box));
        }
        let turned = (u32::from(page.rotation) + u32::from(rotation)) % 360;
        if turned != 0 {
            dictionary.insert(b"Rotate".to_vec(), Object::Integer(i64::from(turned)));
        }
        if let Some(resources) = page.resources.as_deref() {
            dictionary.insert(b"Resources".to_vec(), self.map(resources.clone(), budget)?);
        }
        dictionary.insert(b"Parent".to_vec(), reference(PAGES));
        Ok(dictionary)
    }
}

/// A box coordinate: an integer when it is one, a real otherwise.
fn number(value: f64) -> Object {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        Object::Integer(value as i64)
    } else {
        Object::Real(value)
    }
}

/// Whether the source carries a digital signature (an `/AcroForm` whose
/// `/SigFlags` has bit 1, 12.7.3), which a rewrite invalidates.
fn is_signed<S: Source>(document: &Document<S>, budget: &mut Budget) -> Result<bool, Error> {
    let Some(form) = document.catalog().get(b"AcroForm") else {
        return Ok(false);
    };
    let form = match form {
        Object::Reference(id) => match document.file().resolve(*id, budget) {
            Ok(IndirectValue::Object(object)) => object,
            Ok(IndirectValue::Stream(_))
            | Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                return Ok(false);
            }
            Err(other) => return Err(Error::Syntax(other)),
        },
        other => other.clone(),
    };
    Ok(form
        .as_dictionary()
        .and_then(|form| form.get(b"SigFlags"))
        .and_then(Object::as_integer)
        .is_some_and(|flags| flags & 1 == 1))
}
