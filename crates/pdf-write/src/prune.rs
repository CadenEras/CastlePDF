//! A page's resources pruned to what its content names (tranche 4.3), so
//! that a resource dictionary shared with a page left out of the plan (an
//! `/XObject` dictionary inherited from the page tree, say) does not carry
//! what only that page draws into the file.
//!
//! Every name token of the page's content counts as used, whatever its
//! operator, and so does every name in the `/ColorSpace` of a kept image,
//! shading or shading pattern: keeping too much costs bytes, keeping too
//! little would change the page. The resources are kept whole, unpruned,
//! when that cannot be told for sure: content that cannot be read or
//! tokenized, an inline image whose end is not found, or a kept resource
//! that has no `/Resources` of its own and so draws with the page's (a
//! form, a tiling pattern, a Type 3 font, a soft mask group, an annotation
//! appearance). `Rewritten::resources_kept_whole` lists those pages.
//!
//! Only the page's own dictionary is pruned: a kept form whose `/Resources`
//! is the same shared dictionary still copies it whole.
//!
//! What is resolved or read is kept for the whole rewrite (`Pruner`): a
//! resource dictionary or content stream shared by many pages is read once.

use std::collections::{BTreeMap, BTreeSet};

use pdf_document::Page;
use pdf_syntax::{
    Budget, Dictionary, IndirectValue, Lexer, Object, ObjectId, PdfFile, Source, Token,
    is_delimiter, is_whitespace,
};

use crate::Error;

/// Resource categories named by content operators (7.8.3, table 34).
const PRUNED: [&[u8]; 7] = [
    b"ExtGState",
    b"ColorSpace",
    b"Pattern",
    b"Shading",
    b"XObject",
    b"Font",
    b"Properties",
];

/// Colour spaces used without being named (8.6.5.6).
const IMPLICIT_COLOR_SPACES: [&[u8]; 3] = [b"DefaultGray", b"DefaultRGB", b"DefaultCMYK"];

/// Prunes the resources of the pages of one rewrite, remembering what it
/// resolved and read.
pub(crate) struct Pruner<'a, S> {
    file: &'a PdfFile<S>,
    /// Objects resolved, `None` for what cannot be read.
    resolved: BTreeMap<ObjectId, Option<IndirectValue>>,
    /// The names of each content stream read, `None` when it cannot be.
    parts: BTreeMap<ObjectId, Option<BTreeSet<Vec<u8>>>>,
    /// Each page's pruned resources, for a page given twice.
    pages: BTreeMap<ObjectId, Option<Dictionary>>,
}

impl<'a, S: Source> Pruner<'a, S> {
    pub(crate) fn new(file: &'a PdfFile<S>) -> Self {
        Self {
            file,
            resolved: BTreeMap::new(),
            parts: BTreeMap::new(),
            pages: BTreeMap::new(),
        }
    }

    /// `resources` of `page` with every category of `PRUNED` keeping only
    /// the names used (see the module); `None` when they must be kept
    /// whole. Limits, cancellation and I/O errors stop the rewrite.
    pub(crate) fn pruned_resources(
        &mut self,
        page: &Page,
        resources: &Object,
        budget: &mut Budget,
    ) -> Result<Option<Dictionary>, Error> {
        if let Some(pruned) = self.pages.get(&page.id) {
            return Ok(pruned.clone());
        }
        let pruned = self.prune(page, resources, budget)?;
        self.pages.insert(page.id, pruned.clone());
        Ok(pruned)
    }

    fn prune(
        &mut self,
        page: &Page,
        resources: &Object,
        budget: &mut Budget,
    ) -> Result<Option<Dictionary>, Error> {
        let Some(resources) = self.dictionary(resources, budget)? else {
            return Ok(None);
        };
        let Some(mut used) = self.content_names(page.contents.as_ref(), budget)? else {
            return Ok(None);
        };
        self.named_color_spaces(&resources, &mut used, budget)?;
        if self.annotations_borrow(page, budget)? {
            return Ok(None);
        }
        let mut out = Dictionary::new();
        for (key, value) in resources.iter() {
            if !PRUNED.contains(&key) {
                out.insert(key.to_vec(), value.clone());
                continue;
            }
            let Some(category) = self.dictionary(value, budget)? else {
                // Not a dictionary: nothing can be drawn from it anyway.
                out.insert(key.to_vec(), value.clone());
                continue;
            };
            let mut kept = Dictionary::new();
            for (name, entry) in category.iter() {
                let implicit = key == b"ColorSpace" && IMPLICIT_COLOR_SPACES.contains(&name);
                if !implicit && !used.contains(name) {
                    continue;
                }
                if self.borrows_resources(key, entry, budget)? {
                    return Ok(None);
                }
                kept.insert(name.to_vec(), entry.clone());
            }
            out.insert(key.to_vec(), Object::Dictionary(kept));
        }
        Ok(Some(out))
    }

    /// Adds to `used` the colour spaces named, not by the content, but by
    /// what it uses: an image's `/ColorSpace` (8.9.5), a shading's, a
    /// shading pattern's shading's. Every name in such a value counts.
    fn named_color_spaces(
        &mut self,
        resources: &Dictionary,
        used: &mut BTreeSet<Vec<u8>>,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let mut found = BTreeSet::new();
        for key in [b"XObject".as_slice(), b"Shading", b"Pattern"] {
            let Some(category) = resources.get(key) else {
                continue;
            };
            let Some(category) = self.dictionary(category, budget)? else {
                continue;
            };
            for (name, entry) in category.iter() {
                if !used.contains(name) {
                    continue;
                }
                budget.check_cancelled()?;
                let Some(mut entry) = self.own_dictionary(entry, budget)? else {
                    continue;
                };
                if key == b"Pattern" {
                    let Some(shading) = entry.get(b"Shading") else {
                        continue;
                    };
                    let Some(shading) = self.own_dictionary(shading, budget)? else {
                        continue;
                    };
                    entry = shading;
                }
                let space = match entry.get(b"ColorSpace") {
                    Some(reference @ Object::Reference(_)) => {
                        match self.resolve(reference, budget)? {
                            Some(IndirectValue::Object(object)) => object,
                            _ => continue,
                        }
                    }
                    Some(object) => object.clone(),
                    None => continue,
                };
                collect_names(&space, &mut found);
            }
        }
        used.extend(found);
        Ok(())
    }

    /// `object` resolved; `Ok(None)` for what is not there or cannot be read.
    fn resolve(
        &mut self,
        object: &Object,
        budget: &mut Budget,
    ) -> Result<Option<IndirectValue>, Error> {
        match object {
            Object::Reference(id) => {
                if let Some(value) = self.resolved.get(id) {
                    return Ok(value.clone());
                }
                let value = match self.file.resolve(*id, budget) {
                    Ok(value) => Some(value),
                    Err(e) if fatal(&e) => return Err(Error::Syntax(e)),
                    Err(_) => None,
                };
                self.resolved.insert(*id, value.clone());
                Ok(value)
            }
            Object::Null => Ok(None),
            other => Ok(Some(IndirectValue::Object(other.clone()))),
        }
    }

    /// `object` as a dictionary (a stream's own dictionary not included).
    fn dictionary(
        &mut self,
        object: &Object,
        budget: &mut Budget,
    ) -> Result<Option<Dictionary>, Error> {
        Ok(match self.resolve(object, budget)? {
            Some(IndirectValue::Object(Object::Dictionary(dictionary))) => Some(dictionary),
            _ => None,
        })
    }

    /// The dictionary of `object`, or of the stream it is.
    fn own_dictionary(
        &mut self,
        object: &Object,
        budget: &mut Budget,
    ) -> Result<Option<Dictionary>, Error> {
        Ok(match self.resolve(object, budget)? {
            Some(IndirectValue::Stream(stream)) => Some(stream.dictionary),
            Some(IndirectValue::Object(Object::Dictionary(dictionary))) => Some(dictionary),
            _ => None,
        })
    }

    /// Whether the dictionary of a form, pattern, font or group lacks
    /// usable `/Resources`: a reader then draws it with the page's.
    fn lacks_resources(
        &mut self,
        dictionary: &Dictionary,
        budget: &mut Budget,
    ) -> Result<bool, Error> {
        Ok(match dictionary.get(b"Resources") {
            None => true,
            Some(resources) => self.dictionary(resources, budget)?.is_none(),
        })
    }

    /// Whether a kept resource `entry` of `category` draws with the page's
    /// resources (see the module).
    fn borrows_resources(
        &mut self,
        category: &[u8],
        entry: &Object,
        budget: &mut Budget,
    ) -> Result<bool, Error> {
        let value = self.resolve(entry, budget)?;
        match (category, value) {
            (b"XObject", Some(IndirectValue::Stream(stream))) => {
                let form =
                    stream.dictionary.get(b"Subtype").and_then(Object::as_name) == Some(b"Form");
                Ok(form && self.lacks_resources(&stream.dictionary, budget)?)
            }
            // A tiling pattern is a stream; a shading pattern a dictionary.
            (b"Pattern", Some(IndirectValue::Stream(stream))) => {
                self.lacks_resources(&stream.dictionary, budget)
            }
            (b"Font", Some(IndirectValue::Object(Object::Dictionary(font)))) => {
                let type3 = font.get(b"Subtype").and_then(Object::as_name) == Some(b"Type3");
                Ok(type3 && self.lacks_resources(&font, budget)?)
            }
            (b"ExtGState", Some(IndirectValue::Object(Object::Dictionary(state)))) => {
                let Some(mask) = state.get(b"SMask") else {
                    return Ok(false);
                };
                let Some(mask) = self.dictionary(mask, budget)? else {
                    return Ok(false);
                };
                let Some(group) = mask.get(b"G") else {
                    return Ok(false);
                };
                match self.resolve(group, budget)? {
                    Some(IndirectValue::Stream(group)) => {
                        self.lacks_resources(&group.dictionary, budget)
                    }
                    _ => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    /// Whether an appearance stream of one of the page's annotations has no
    /// `/Resources` of its own.
    fn annotations_borrow(&mut self, page: &Page, budget: &mut Budget) -> Result<bool, Error> {
        let Some(annotations) = &page.annotations else {
            return Ok(false);
        };
        let Some(IndirectValue::Object(Object::Array(annotations))) =
            self.resolve(annotations, budget)?
        else {
            return Ok(false);
        };
        for annotation in &annotations {
            budget.check_cancelled()?;
            let Some(annotation) = self.dictionary(annotation, budget)? else {
                continue;
            };
            let Some(appearances) = annotation.get(b"AP") else {
                continue;
            };
            let Some(appearances) = self.dictionary(appearances, budget)? else {
                continue;
            };
            for kind in [b"N".as_slice(), b"R", b"D"] {
                let Some(appearance) = appearances.get(kind) else {
                    continue;
                };
                let streams = match self.resolve(appearance, budget)? {
                    Some(IndirectValue::Stream(stream)) => vec![stream],
                    // A dictionary of states, each an appearance stream.
                    Some(IndirectValue::Object(Object::Dictionary(states))) => {
                        let mut streams = Vec::new();
                        for (_, state) in states.iter() {
                            if let Some(IndirectValue::Stream(stream)) =
                                self.resolve(state, budget)?
                            {
                                streams.push(stream);
                            }
                        }
                        streams
                    }
                    _ => Vec::new(),
                };
                for stream in streams {
                    if self.lacks_resources(&stream.dictionary, budget)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Every name in the page's content streams (7.8.2); `None` when the
    /// content cannot be read or tokenized to its end.
    fn content_names(
        &mut self,
        contents: Option<&Object>,
        budget: &mut Budget,
    ) -> Result<Option<BTreeSet<Vec<u8>>>, Error> {
        let ids = match contents {
            None | Some(Object::Null) => return Ok(Some(BTreeSet::new())),
            Some(Object::Reference(id)) => match self.resolve(&Object::Reference(*id), budget)? {
                Some(IndirectValue::Stream(_)) => vec![*id],
                Some(IndirectValue::Object(Object::Array(items))) => {
                    let mut ids = Vec::with_capacity(items.len());
                    for item in &items {
                        let Object::Reference(id) = item else {
                            return Ok(None);
                        };
                        ids.push(*id);
                    }
                    ids
                }
                _ => return Ok(None),
            },
            Some(Object::Array(items)) => {
                let mut ids = Vec::with_capacity(items.len());
                for item in items {
                    let Object::Reference(id) = item else {
                        return Ok(None);
                    };
                    ids.push(*id);
                }
                ids
            }
            Some(_) => return Ok(None),
        };
        let mut names = BTreeSet::new();
        for id in ids {
            budget.check_cancelled()?;
            match self.part_names_of(id, budget)? {
                Some(part) => names.extend(part.iter().cloned()),
                None => return Ok(None),
            }
        }
        Ok(Some(names))
    }

    /// The names of content stream `id`, read once per rewrite. Parts
    /// divide at token boundaries (7.8.2): each is read alone.
    fn part_names_of(
        &mut self,
        id: ObjectId,
        budget: &mut Budget,
    ) -> Result<Option<&BTreeSet<Vec<u8>>>, Error> {
        if !self.parts.contains_key(&id) {
            let names = match self.resolve(&Object::Reference(id), budget)? {
                Some(IndirectValue::Stream(stream)) => {
                    match self.file.decoded_stream_data(&stream, budget) {
                        Ok(data) => {
                            let mut names = BTreeSet::new();
                            part_names(&data, &mut names, budget)?.then_some(names)
                        }
                        Err(e) if fatal(&e) => return Err(Error::Syntax(e)),
                        Err(_) => None,
                    }
                }
                _ => None,
            };
            // A stream's data is not kept with its dictionary.
            self.resolved.remove(&id);
            self.parts.insert(id, names);
        }
        Ok(self.parts.get(&id).and_then(Option::as_ref))
    }
}

/// Every name in `object`, its arrays and dictionaries.
fn collect_names(object: &Object, names: &mut BTreeSet<Vec<u8>>) {
    match object {
        Object::Name(name) => {
            names.insert(name.clone());
        }
        Object::Array(items) => items.iter().for_each(|item| collect_names(item, names)),
        Object::Dictionary(dictionary) => {
            dictionary
                .iter()
                .for_each(|(_, value)| collect_names(value, names));
        }
        _ => {}
    }
}

/// Errors that end the rewrite; any other means "cannot tell".
fn fatal(error: &pdf_syntax::Error) -> bool {
    matches!(
        error,
        pdf_syntax::Error::LimitExceeded { .. }
            | pdf_syntax::Error::Cancelled
            | pdf_syntax::Error::Io(_)
    )
}

/// Adds the names of one content stream to `names`; `false` when it
/// cannot be read to its end.
fn part_names(
    data: &[u8],
    names: &mut BTreeSet<Vec<u8>>,
    budget: &mut Budget,
) -> Result<bool, Error> {
    let mut lexer = Lexer::new(data);
    let mut image: Option<InlineImage> = None;
    loop {
        let token = match lexer.next_token(budget) {
            Ok(Some(spanned)) => spanned.token,
            Ok(None) => break,
            Err(e) if fatal(&e) => return Err(Error::Syntax(e)),
            Err(_) => return Ok(false),
        };
        if let Token::Name(name) = &token {
            names.insert(name.clone());
        }
        match (&mut image, &token) {
            (None, Token::Keyword(b"BI")) => image = Some(InlineImage::default()),
            (Some(state), Token::Keyword(b"ID")) => {
                let length = state.length;
                image = None;
                let Some(end) = inline_image_end(data, lexer.position(), length, budget)? else {
                    return Ok(false);
                };
                lexer.seek(end).map_err(Error::Syntax)?;
            }
            (Some(state), token) => state.step(token),
            (None, Token::Keyword(b"ID")) => return Ok(false),
            _ => {}
        }
    }
    Ok(image.is_none())
}

/// An inline image dictionary being read, between `BI` and `ID`: only its
/// `/L` (or `/Length`) matters here, as the renderer reads it.
#[derive(Default)]
struct InlineImage {
    /// Nesting of arrays and dictionaries in a value.
    depth: usize,
    /// Whether the next token at depth 0 is a value (after a key).
    value: bool,
    /// Whether that value is the one of `/L` or `/Length`.
    length_key: bool,
    length: Option<usize>,
}

impl InlineImage {
    fn step(&mut self, token: &Token<'_>) {
        match token {
            Token::ArrayStart | Token::DictStart => {
                if self.depth == 0 && self.value && self.length_key {
                    self.length = None;
                }
                self.depth += 1;
            }
            Token::ArrayEnd | Token::DictEnd => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0 {
                    self.value = false;
                }
            }
            _ if self.depth > 0 => {}
            Token::Name(name) if !self.value => {
                self.value = true;
                self.length_key = matches!(name.as_slice(), b"L" | b"Length");
            }
            Token::Integer(length) if self.length_key => {
                self.length = usize::try_from(*length).ok();
                self.value = false;
            }
            _ => {
                if self.length_key {
                    self.length = None;
                }
                self.value = false;
            }
        }
    }
}

/// Offset just past the `EI` that ends inline image data, `ID` ending at
/// `after_id`, as the renderer finds it: after `length` data bytes when
/// `EI` follows them, otherwise the first `EI` preceded by white space and
/// followed by white space, a delimiter or the end. `None` when not found.
fn inline_image_end(
    data: &[u8],
    after_id: u64,
    length: Option<usize>,
    budget: &mut Budget,
) -> Result<Option<u64>, Error> {
    let Ok(after_id) = usize::try_from(after_id) else {
        return Ok(None);
    };
    let data_start = match data.get(after_id) {
        Some(byte) if is_whitespace(*byte) => after_id + 1,
        Some(_) => after_id,
        None => return Ok(None),
    };
    if let Some(data_end) = length
        .and_then(|length| data_start.checked_add(length))
        .filter(|end| *end <= data.len())
    {
        let mut lexer = Lexer::new(data);
        lexer.seek(data_end as u64).map_err(Error::Syntax)?;
        match lexer.next_token(budget) {
            Ok(Some(spanned)) if spanned.token == Token::Keyword(b"EI") => {
                return Ok(Some(lexer.position()));
            }
            Ok(_) => {}
            Err(e) if fatal(&e) => return Err(Error::Syntax(e)),
            Err(_) => return Ok(None),
        }
    }
    let Some(tail) = data.get(data_start..) else {
        return Ok(None);
    };
    Ok(tail.windows(2).enumerate().find_map(|(i, pair)| {
        let at = data_start + i;
        let before = at.checked_sub(1).and_then(|b| data.get(b)).copied();
        let after = data.get(at + 2).copied();
        let ends = after.is_none_or(|b| is_whitespace(b) || is_delimiter(b));
        (pair == b"EI" && before.is_some_and(is_whitespace) && ends).then_some((at + 2) as u64)
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

    use super::part_names;

    fn names(content: &[u8]) -> Option<Vec<String>> {
        let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
        let mut names = BTreeSet::new();
        part_names(content, &mut names, &mut budget)
            .expect("no limit")
            .then(|| {
                names
                    .iter()
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect()
            })
    }

    #[test]
    fn inline_image_data_ends_as_the_renderer_reads_it() {
        // The first delimited EI; data that only looks like one is skipped.
        assert_eq!(
            names(b"BI /W 1 ID xEIx EI /Im1 Do"),
            Some(vec!["Im1".into(), "W".into()])
        );
        // /L: the data's length, even when the data holds a delimited EI.
        assert_eq!(
            names(b"BI /W 6 /H 1 /L 6 ID a EI % EI /Im1 Do"),
            Some(vec!["H".into(), "Im1".into(), "L".into(), "W".into()])
        );
        // A wrong /L: the first delimited EI, as the renderer falls back.
        assert_eq!(
            names(b"BI /L 2 ID abc EI /Im2 Do"),
            Some(vec!["Im2".into(), "L".into()])
        );
        // /L in a nested value does not count.
        assert_eq!(
            names(b"BI /DP << /L 6 >> ID a EI /Im3 Do"),
            Some(vec!["DP".into(), "Im3".into(), "L".into()])
        );
        assert_eq!(names(b"ID abc EI"), None);
        assert_eq!(names(b"BI /W 1 ID abc"), None);
        assert_eq!(names(b"BI /W 1"), None);
        assert_eq!(names(b"/A#20B Do"), Some(vec!["A B".into()]));
    }
}
