//! Documents rewritten from a page plan (ADR-0008, tranches 4.2 to 4.4):
//! the objects reachable from a new catalog, renumbered, written whole with
//! `FileWriter`. A plan takes its pages from one source (`rewrite`) or from
//! several (`merge`): each source's objects are numbered apart, so two
//! sources never share an object, even the same file given twice.
//!
//! Rules that keep a page left out of the plan out of the file, and every
//! loss said:
//! - a reference to what is not written becomes `null`: a page not in the
//!   plan (a link to it, an annotation's `/P`), its annotations (a reply's
//!   `/IRT`, a `/Popup`), the source's catalog and page tree nodes, and the
//!   roots of the catalog entries not kept (form, outlines, names...), so
//!   that none is copied through another object;
//! - a page object reached that is not in the plan (one outside the page
//!   tree, left by an incremental update) is written as `null`;
//! - the catalog keeps only entries that name no page, taken from the
//!   source of the plan's first page; the others are listed in
//!   `Rewritten::dropped` (ADR-0008, point 3). Optional content
//!   (`OCProperties`) is merged from every source, so that a layer hidden
//!   in one stays hidden;
//! - stream data is copied as stored, still encoded;
//! - when the plan leaves out a page of a source, each page of that source
//!   keeps only the resources its content names (`prune`), so that
//!   resources shared with a page left out do not carry what only it
//!   draws; the pages where that cannot be told for sure keep them whole
//!   and are listed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use pdf_document::{Document, Page};
use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, ObjectId, Source};

use crate::prune::Pruner;
use crate::{Error, FileWriter};

/// One page of a one-source plan: page `page` of the source (0-based),
/// turned by `rotation` degrees clockwise (a multiple of 90) on top of its
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanPage {
    pub page: usize,
    pub rotation: u16,
}

/// One page of a plan over several sources: page `page` (0-based) of
/// source `document` (its index in the sources given), turned by
/// `rotation` as in `PlanPage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePage {
    pub document: usize,
    pub page: usize,
    pub rotation: u16,
}

/// A rewritten document: its bytes, and what the sources had that it does
/// not keep (catalog entries by name; `Signature` when a source is signed,
/// a signature being invalidated by any rewrite). With several sources,
/// each entry names its source: `Outlines (document 2)`, from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    pub bytes: Vec<u8>,
    pub dropped: Vec<String>,
    /// Pages, as (source, page), 0-based, in plan order, once each, whose
    /// resources were copied whole when the plan leaves out a page of
    /// their source: what they use could not be told for sure, so
    /// resources shared with a page left out may still carry what it drew
    /// (see `prune`).
    pub resources_kept_whole: Vec<(usize, usize)>,
}

/// Catalog entries kept: none of them names a page. `OCProperties` lists
/// the optional content groups and which are hidden: without it, other
/// readers would show layers the document hides (found by the corpus
/// measurement, on 78 and 125 files).
const KEPT_CATALOG_KEYS: [&[u8]; 8] = [
    b"Metadata",
    b"ViewerPreferences",
    b"PageLayout",
    b"PageMode",
    b"Lang",
    b"OutputIntents",
    b"OCProperties",
    b"Extensions",
];

/// The identity plan of `document`: every page, in order, unturned.
pub fn identity_plan<S: Source>(document: &Document<S>) -> Vec<PlanPage> {
    (0..document.page_count())
        .map(|page| PlanPage { page, rotation: 0 })
        .collect()
}

/// Writes the pages of `plan`, from `document`, as a new file of at most
/// `limit` bytes: `merge` with one source.
pub fn rewrite<S: Source>(
    document: &Document<S>,
    plan: &[PlanPage],
    limit: u64,
    budget: &mut Budget,
) -> Result<Rewritten, Error> {
    let plan: Vec<SourcePage> = plan
        .iter()
        .map(|entry| SourcePage {
            document: 0,
            page: entry.page,
            rotation: entry.rotation,
        })
        .collect();
    merge(&[document], &plan, limit, budget)
}

/// Most pages a plan may have: object numbers stay far below `u32::MAX`.
pub const MAX_PLAN_PAGES: usize = 1 << 20;

/// Writes the pages of `plan`, taken from `documents`, as a new file of at
/// most `limit` bytes. An empty plan, one of more than `MAX_PLAN_PAGES`, a source or page out of range, or a
/// rotation that is not a multiple of 90 is `InvalidPlan`. Reading the
/// sources is charged to `budget` and stops at its limits or cancellation.
pub fn merge<S: Source>(
    documents: &[&Document<S>],
    plan: &[SourcePage],
    limit: u64,
    budget: &mut Budget,
) -> Result<Rewritten, Error> {
    let first = plan.first().ok_or(Error::InvalidPlan)?.document;
    if plan.len() > MAX_PLAN_PAGES
        || plan.iter().any(|p| {
            documents
                .get(p.document)
                .is_none_or(|document| p.page >= document.page_count())
                || p.rotation % 90 != 0
        })
    {
        return Err(Error::InvalidPlan);
    }
    // The sources the plan uses, in order of first use.
    let mut used: Vec<usize> = Vec::new();
    for entry in plan {
        if !used.contains(&entry.document) {
            used.push(entry.document);
        }
    }
    let several = used.len() > 1;
    let name = |key: &str, source: usize| {
        if several {
            format!("{key} (document {})", source + 1)
        } else {
            key.to_owned()
        }
    };
    let mut copier = Copier::new(documents, plan);
    for &source in &used {
        let document = documents.get(source).ok_or(Error::InvalidPlan)?;
        let pages: BTreeSet<usize> = plan
            .iter()
            .filter(|p| p.document == source)
            .map(|p| p.page)
            .collect();
        let excluded = excluded(document, &pages, budget)?;
        if let (Some(prune), Some(slot)) = (
            copier.prune.get_mut(source),
            copier.excluded.get_mut(source),
        ) {
            *prune = pages.len() < document.page_count();
            *slot = excluded;
        }
    }
    let mut major_minor = (1, 4);
    for &source in &used {
        let document = documents.get(source).ok_or(Error::InvalidPlan)?;
        major_minor = major_minor.max(version(document));
    }
    let mut writer = FileWriter::new(major_minor.0, major_minor.1, limit)?;

    // The pages, each as its own new object.
    let mut kids = Vec::with_capacity(plan.len());
    let mut pruners: Vec<Pruner<'_, S>> = documents
        .iter()
        .map(|document| Pruner::new(document.file()))
        .collect();
    let mut resources_kept_whole = Vec::new();
    let mut listed = BTreeSet::new();
    for (index, entry) in plan.iter().enumerate() {
        let document = documents.get(entry.document).ok_or(Error::InvalidPlan)?;
        let page = document.page(entry.page).ok_or(Error::InvalidPlan)?;
        let number = copier
            .page_numbers
            .get(index)
            .copied()
            .ok_or(Error::InvalidPlan)?;
        let pruner = pruners.get_mut(entry.document).ok_or(Error::InvalidPlan)?;
        let (dictionary, kept_whole) = copier.page_dictionary(
            entry.document,
            document,
            pruner,
            page,
            entry.rotation,
            budget,
        )?;
        let key = (entry.document, entry.page);
        if kept_whole && listed.insert(key) {
            resources_kept_whole.push(key);
        }
        writer.object(number, &Object::Dictionary(dictionary))?;
        kids.push(reference(number));
    }
    // The catalog: the first source's kept entries, optional content from
    // every source, the others listed as dropped.
    let mut catalog = Dictionary::new();
    catalog.insert(b"Type".to_vec(), Object::Name(b"Catalog".to_vec()));
    catalog.insert(b"Pages".to_vec(), reference(PAGES));
    let mut dropped = Vec::new();
    for &source in &used {
        let document = documents.get(source).ok_or(Error::InvalidPlan)?;
        for (key, value) in document.catalog().iter() {
            if matches!(key, b"Type" | b"Pages" | b"Version") {
                continue;
            }
            if source == first && KEPT_CATALOG_KEYS.contains(&key) && key != b"OCProperties" {
                let value = copier.map(source, value.clone(), budget)?;
                catalog.insert(key.to_vec(), value);
            } else if key != b"OCProperties" {
                dropped.push(name(&String::from_utf8_lossy(key), source));
            }
        }
        if is_signed(document, budget)? {
            dropped.push(name("Signature", source));
        }
        if source != first && document.file().trailer().get(b"Info").is_some() {
            dropped.push(name("Info", source));
        }
    }
    if several {
        let (properties, lost) = merged_optional_content(&mut copier, documents, &used, budget)?;
        if let Some(properties) = properties {
            catalog.insert(b"OCProperties".to_vec(), properties);
        }
        dropped.extend(lost);
    } else if let Some(properties) = documents
        .get(first)
        .and_then(|document| document.catalog().get(b"OCProperties"))
    {
        let properties = copier.map(first, properties.clone(), budget)?;
        catalog.insert(b"OCProperties".to_vec(), properties);
    }
    // The document information dictionary, if any, of the first source.
    let info = match documents
        .get(first)
        .and_then(|document| document.file().trailer().get(b"Info"))
    {
        Some(info) => Some(copier.map(first, info.clone(), budget)?),
        None => None,
    };
    // Every object reached, until none is left.
    while let Some((source, id, number)) = copier.pending.pop_front() {
        budget.check_cancelled()?;
        let document = documents.get(source).ok_or(Error::InvalidPlan)?;
        match document.file().resolve(id, budget) {
            // A page (or page tree node) not in the plan, reached through
            // another object: not copied.
            Ok(IndirectValue::Object(Object::Dictionary(dictionary)))
                if matches!(
                    dictionary.get(b"Type").and_then(Object::as_name),
                    Some(b"Page" | b"Pages")
                ) =>
            {
                writer.object(number, &Object::Null)?;
            }
            Ok(IndirectValue::Object(object)) => {
                let object = copier.map(source, object, budget)?;
                writer.object(number, &object)?;
            }
            Ok(IndirectValue::Stream(stream)) => {
                let mut dictionary = stream.dictionary.clone();
                // Written again by `FileWriter` from the data.
                dictionary.remove(b"Length");
                let Object::Dictionary(dictionary) =
                    copier.map(source, Object::Dictionary(dictionary), budget)?
                else {
                    return Err(Error::InvalidPlan);
                };
                match document.file().stream_data(&stream) {
                    Ok(data) => writer.stream(number, &dictionary, &data)?,
                    // Data that cannot be read: null, as a damaged object.
                    Err(
                        pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. },
                    ) => writer.object(number, &Object::Null)?,
                    Err(other) => return Err(Error::Syntax(other)),
                }
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
        resources_kept_whole,
    })
}

/// The header version a source asks for: the file's or the catalog's
/// `/Version`, the later one (7.2.2), at least 1.4.
fn version<S: Source>(document: &Document<S>) -> (u8, u8) {
    let file_version = document.file().version();
    let catalog_version = document
        .catalog()
        .get(b"Version")
        .and_then(Object::as_name)
        .and_then(|name| std::str::from_utf8(name).ok())
        .and_then(|name| name.split_once('.'))
        .and_then(|(major, minor)| Some((major.parse::<u8>().ok()?, minor.parse::<u8>().ok()?)));
    let (major, minor) = catalog_version
        .filter(|v| *v > (file_version.major, file_version.minor) && v.0 <= 2)
        .unwrap_or((file_version.major, file_version.minor));
    if major <= 1 {
        (1, minor.max(4))
    } else {
        (major, minor)
    }
}

const CATALOG: u32 = 1;
const PAGES: u32 = 2;

fn reference(number: u32) -> Object {
    Object::Reference(ObjectId {
        number,
        generation: 0,
    })
}

/// Renumbers the objects reached from what is written, each source apart,
/// and keeps the pages out of the plan out.
struct Copier {
    /// New number of each source object reached, by (source, object),
    /// pages of the plan first.
    numbers: BTreeMap<(usize, ObjectId), u32>,
    /// Source objects reached but not written yet, with their numbers.
    pending: VecDeque<(usize, ObjectId, u32)>,
    /// The new number of each plan entry, in order.
    page_numbers: Vec<u32>,
    /// What is never copied, by source: a reference to it becomes `null`
    /// (see the module's rules).
    excluded: Vec<BTreeSet<ObjectId>>,
    /// Whether the plan leaves out a page of each source: page resources
    /// are then pruned.
    prune: Vec<bool>,
    next: u32,
}

impl Copier {
    fn new<S: Source>(documents: &[&Document<S>], plan: &[SourcePage]) -> Self {
        let mut copier = Self {
            numbers: BTreeMap::new(),
            pending: VecDeque::new(),
            page_numbers: Vec::with_capacity(plan.len()),
            excluded: vec![BTreeSet::new(); documents.len()],
            prune: vec![false; documents.len()],
            next: PAGES + 1,
        };
        for entry in plan {
            let number = copier.next;
            copier.next = copier.next.saturating_add(1);
            copier.page_numbers.push(number);
            // A page given twice is one object for what refers to it.
            if let Some(page) = documents
                .get(entry.document)
                .and_then(|document| document.page(entry.page))
            {
                copier
                    .numbers
                    .entry((entry.document, page.id))
                    .or_insert(number);
            }
        }
        copier
    }

    /// `object` of `source` with its references renumbered, the objects
    /// they reach queued, and a reference to what is excluded made `null`.
    fn map(&mut self, source: usize, object: Object, budget: &mut Budget) -> Result<Object, Error> {
        Ok(match object {
            Object::Reference(id) => {
                if let Some(number) = self.numbers.get(&(source, id)) {
                    reference(*number)
                } else if self
                    .excluded
                    .get(source)
                    .is_some_and(|excluded| excluded.contains(&id))
                {
                    Object::Null
                } else {
                    budget.check_cancelled()?;
                    let number = self.next;
                    self.next = self
                        .next
                        .checked_add(1)
                        .ok_or(Error::InvalidObjectNumber(u32::MAX))?;
                    self.numbers.insert((source, id), number);
                    self.pending.push_back((source, id, number));
                    reference(number)
                }
            }
            Object::Array(items) => Object::Array(
                items
                    .into_iter()
                    .map(|item| self.map(source, item, budget))
                    .collect::<Result<_, _>>()?,
            ),
            Object::Dictionary(dictionary) => {
                let mut out = Dictionary::new();
                for (key, value) in dictionary.iter() {
                    out.insert(key.to_vec(), self.map(source, value.clone(), budget)?);
                }
                Object::Dictionary(out)
            }
            other => other,
        })
    }

    /// The dictionary of `page` as written: its own entries, its inherited
    /// attributes (boxes, rotation, resources) made explicit from the
    /// model, its rotation turned by `rotation`, its `/Parent` the new page
    /// tree. Left out: articles (`/B`, with the threads they belong to).
    /// With it, whether its resources had to be kept whole although the
    /// plan prunes them.
    fn page_dictionary<S: Source>(
        &mut self,
        source: usize,
        document: &Document<S>,
        pruner: &mut Pruner<'_, S>,
        page: &Page,
        rotation: u16,
        budget: &mut Budget,
    ) -> Result<(Dictionary, bool), Error> {
        let own = match document.file().resolve(page.id, budget)? {
            IndirectValue::Object(Object::Dictionary(dictionary)) => dictionary,
            _ => Dictionary::new(),
        };
        let mut dictionary = Dictionary::new();
        for (key, value) in own.iter() {
            if matches!(
                key,
                b"Parent" | b"MediaBox" | b"CropBox" | b"Rotate" | b"Resources" | b"B"
            ) {
                continue;
            }
            dictionary.insert(key.to_vec(), self.map(source, value.clone(), budget)?);
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
        // Required of a page (7.7.3.3); an empty dictionary when the source
        // has none, even inherited.
        let mut kept_whole = false;
        let prune = self.prune.get(source).copied().unwrap_or(false);
        let resources = match page.resources.as_deref() {
            Some(resources) if prune => match pruner.pruned_resources(page, resources, budget)? {
                Some(pruned) => self.map(source, Object::Dictionary(pruned), budget)?,
                None => {
                    kept_whole = true;
                    self.map(source, resources.clone(), budget)?
                }
            },
            Some(resources) => self.map(source, resources.clone(), budget)?,
            None => Object::Dictionary(Dictionary::new()),
        };
        dictionary.insert(b"Resources".to_vec(), resources);
        dictionary.insert(b"Parent".to_vec(), reference(PAGES));
        Ok((dictionary, kept_whole))
    }
}

/// The optional content of several sources as one `OCProperties` (8.11.4):
/// every group, every source's default configuration's hidden groups
/// hidden (whatever its `/BaseState`), the orders, radio button groups,
/// locked groups and usage applications one after the other. With it, what
/// cannot be merged: the other configurations (`/Configs`) of a source.
fn merged_optional_content<S: Source>(
    copier: &mut Copier,
    documents: &[&Document<S>],
    used: &[usize],
    budget: &mut Budget,
) -> Result<(Option<Object>, Vec<String>), Error> {
    let mut groups = Vec::new();
    let mut off = Vec::new();
    let mut order = Vec::new();
    let mut lists: BTreeMap<&[u8], Vec<Object>> = BTreeMap::new();
    let mut rest: Option<Dictionary> = None;
    let mut rest_source = Dictionary::new();
    let mut lost = Vec::new();
    let mut any_order = false;
    let mut sources = Vec::new();
    for &source in used {
        let Some(document) = documents.get(source) else {
            continue;
        };
        let Some(properties) = document.catalog().get(b"OCProperties") else {
            continue;
        };
        let Some(Object::Dictionary(properties)) = resolved(document, properties, budget)? else {
            continue;
        };
        let configuration = match properties.get(b"D") {
            Some(d) => match resolved(document, d, budget)? {
                Some(Object::Dictionary(d)) => d,
                _ => Dictionary::new(),
            },
            None => Dictionary::new(),
        };
        any_order |= configuration.get(b"Order").is_some();
        sources.push((source, *document, properties, configuration));
    }
    if sources.is_empty() {
        return Ok((None, lost));
    }
    for (source, document, properties, configuration) in sources {
        let array =
            |key: &[u8], dictionary: &Dictionary, budget: &mut Budget| match dictionary.get(key) {
                Some(value) => match resolved(document, value, budget) {
                    Ok(Some(Object::Array(items))) => Ok(items),
                    Ok(_) => Ok(Vec::new()),
                    Err(e) => Err(e),
                },
                None => Ok(Vec::new()),
            };
        let own_groups = array(b"OCGs", &properties, budget)?;
        let shown = array(b"ON", &configuration, budget)?;
        let hidden = if configuration.get(b"BaseState").and_then(Object::as_name) == Some(b"OFF") {
            own_groups
                .iter()
                .filter(|group| !shown.contains(group))
                .cloned()
                .collect()
        } else {
            array(b"OFF", &configuration, budget)?
        };
        for group in hidden {
            off.push(copier.map(source, group, budget)?);
        }
        let own_order = array(b"Order", &configuration, budget)?;
        let listed = if own_order.is_empty() && any_order {
            own_groups.clone()
        } else {
            own_order
        };
        for item in listed {
            order.push(copier.map(source, item, budget)?);
        }
        for key in [b"RBGroups".as_slice(), b"Locked", b"AS"] {
            for item in array(key, &configuration, budget)? {
                let item = copier.map(source, item, budget)?;
                lists.entry(key).or_default().push(item);
            }
        }
        for group in own_groups {
            groups.push(copier.map(source, group, budget)?);
        }
        let merged_keys = |key: &[u8]| {
            matches!(
                key,
                b"BaseState" | b"ON" | b"OFF" | b"Order" | b"RBGroups" | b"Locked" | b"AS"
            )
        };
        match &rest {
            None => {
                let mut kept = Dictionary::new();
                for (key, value) in configuration.iter() {
                    if !merged_keys(key) {
                        kept.insert(key.to_vec(), copier.map(source, value.clone(), budget)?);
                    }
                }
                rest = Some(kept);
                rest_source = configuration.clone();
            }
            // The other sources' own settings (name, intent...): the first
            // one's are kept, a different one is listed.
            Some(_) => {
                for (key, value) in configuration.iter() {
                    if !merged_keys(key) && rest_source.get(key) != Some(value) {
                        lost.push(format!(
                            "OCProperties D/{} (document {})",
                            String::from_utf8_lossy(key),
                            source + 1
                        ));
                    }
                }
            }
        }
        if properties.get(b"Configs").is_some() {
            lost.push(format!("OCProperties Configs (document {})", source + 1));
        }
    }
    let mut configuration = rest.unwrap_or_default();
    configuration.insert(b"OFF".to_vec(), Object::Array(off));
    if any_order {
        configuration.insert(b"Order".to_vec(), Object::Array(order));
    }
    for (key, items) in lists {
        configuration.insert(key.to_vec(), Object::Array(items));
    }
    let mut properties = Dictionary::new();
    properties.insert(b"OCGs".to_vec(), Object::Array(groups));
    properties.insert(b"D".to_vec(), Object::Dictionary(configuration));
    Ok((Some(Object::Dictionary(properties)), lost))
}

/// What a rewrite must never copy from `document`, reached from what it
/// writes: its pages not in `kept` and their annotations, its catalog and
/// page tree nodes, and the roots of its catalog entries not kept.
fn excluded<S: Source>(
    document: &Document<S>,
    kept: &BTreeSet<usize>,
    budget: &mut Budget,
) -> Result<BTreeSet<ObjectId>, Error> {
    let mut excluded = BTreeSet::new();
    if let Some(catalog) = document.catalog_id() {
        excluded.insert(catalog);
    }
    for (key, value) in document.catalog().iter() {
        if !KEPT_CATALOG_KEYS.contains(&key)
            && let Object::Reference(id) = value
        {
            excluded.insert(*id);
        }
    }
    // The page tree's nodes, from the catalog's /Pages down.
    let mut nodes: Vec<Object> = document
        .catalog()
        .get(b"Pages")
        .cloned()
        .into_iter()
        .collect();
    while let Some(node) = nodes.pop() {
        budget.check_cancelled()?;
        let Object::Reference(id) = node else {
            continue;
        };
        if !excluded.insert(id) {
            continue;
        }
        if let Some(Object::Dictionary(dictionary)) =
            resolved(document, &Object::Reference(id), budget)?
            && let Some(kids) = dictionary.get(b"Kids")
            && let Some(Object::Array(kids)) = resolved(document, kids, budget)?
        {
            nodes.extend(kids);
        }
    }
    // The pages left out, and their annotations.
    for (index, page) in document.pages().iter().enumerate() {
        if kept.contains(&index) {
            excluded.remove(&page.id);
            continue;
        }
        excluded.insert(page.id);
        if let Some(annotations) = &page.annotations
            && let Some(Object::Array(annotations)) = resolved(document, annotations, budget)?
        {
            if let Object::Reference(id) = annotations_ref(page) {
                excluded.insert(id);
            }
            for annotation in annotations {
                if let Object::Reference(id) = annotation {
                    excluded.insert(id);
                }
            }
        }
    }
    Ok(excluded)
}

/// The `/Annots` value of `page` when it is a reference (to its array).
fn annotations_ref(page: &Page) -> Object {
    match &page.annotations {
        Some(reference @ Object::Reference(_)) => reference.clone(),
        _ => Object::Null,
    }
}

/// `object`, resolved if it is a reference; `None` for a stream or what
/// cannot be read (damaged syntax).
fn resolved<S: Source>(
    document: &Document<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Object>, Error> {
    match object {
        Object::Reference(id) => match document.file().resolve(*id, budget) {
            Ok(IndirectValue::Object(object)) => Ok(Some(object)),
            Ok(IndirectValue::Stream(_))
            | Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                Ok(None)
            }
            Err(other) => Err(Error::Syntax(other)),
        },
        other => Ok(Some(other.clone())),
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
