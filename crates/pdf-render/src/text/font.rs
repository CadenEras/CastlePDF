//! Fonts whose program the renderer reads (ADR-0005, steps 5.2a and 5.2b):
//! simple fonts (`/TrueType`, `/Type1`, `/MMType1`) and Type 0 fonts with a
//! `CIDFontType2` or `CIDFontType0` descendant, whose embedded program is
//! TrueType (`/FontFile2`) or OpenType (`/FontFile3 /OpenType`), read by
//! `skrifa`, or bare CFF (`/FontFile3 /Type1C` or `/CIDFontType0C`), read by
//! `read-fonts`.
//!
//! A font is loaded once per rendering from its dictionary: widths (ISO
//! 32000-2, 9.2.4, 9.7.4.3), the mapping from character codes to glyph ids
//! and the program bytes. Simple TrueType and OpenType fonts map codes
//! through `cmap` (9.6.5.4); simple CFF fonts through glyph names and the
//! CFF charset, or the CFF's built-in encoding where the PDF gives no name
//! (9.6.5.2); CID fonts through `/CIDToGIDMap` or the charset of a
//! CID-keyed CFF. Any other font (Type 1 program, Type 3, not embedded), an
//! encoding other than `Identity-H`, or a program that cannot be opened is
//! `Unsupported`: its text is not drawn and the page reports `Text`.

use std::collections::BTreeMap;
use std::sync::Arc;

use pdf_syntax::{Budget, Dictionary, Object, PdfFile, Source};
use skrifa::instance::{LocationRef, Size};
use skrifa::metrics::GlyphMetrics;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::raw::ps::cff::CffFontRef;
use skrifa::raw::tables::cmap::{CmapSubtable, PlatformId};
use skrifa::{FontRef, GlyphId, GlyphNames, MetadataProvider};

use super::encodings::{GLYPH_LIST, MAC_EXPERT, MAC_ROMAN, STANDARD, WIN_ANSI};
use super::{resolve, resolve_dictionary, stream_bytes};
use crate::Error;
use crate::geometry::{Point, Segment};

/// Most entries read from a `/W` array of a CID font.
const MAX_W_ENTRIES: usize = 65_536;
/// Most entries read from a `/Differences` array.
const MAX_DIFFERENCES: usize = 4096;
/// CIDs are two bytes (`Identity-H`): entries of `/CIDToGIDMap` beyond are unused.
const MAX_CIDS: usize = 65_536;

/// A font ready to show text.
#[derive(Debug)]
pub(crate) struct Font {
    program: Arc<[u8]>,
    format: Format,
    units_per_em: f64,
    mapping: Mapping,
}

/// How the program bytes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// TrueType or OpenType file, read by `skrifa`.
    OpenType,
    /// Bare CFF (Compact Font Format), read by `read-fonts`.
    Cff,
}

#[derive(Debug)]
enum Mapping {
    /// One byte per code: glyph id and width (glyph space, 1/1000 of text
    /// space) of each code.
    Simple {
        glyphs: Box<[Option<u16>; 256]>,
        /// Codes whose glyph name names no glyph of the program: their text
        /// is not drawn and is reported.
        missing: Box<[bool; 256]>,
        widths: Box<[f64; 256]>,
    },
    /// Two bytes per code (`Identity-H`): the code is the CID.
    Cid {
        /// `None` for `/Identity`.
        cid_to_gid: Option<Vec<u16>>,
        widths: CidWidths,
    },
}

/// `/W` and `/DW` of a CID font.
#[derive(Debug, Default)]
struct CidWidths {
    default: f64,
    /// Non-overlapping runs sorted by first CID: first, last, widths (one
    /// for a whole range, or one per CID).
    runs: Vec<(u32, u32, Vec<f64>)>,
}

impl CidWidths {
    fn width(&self, cid: u32) -> f64 {
        let index = self.runs.partition_point(|(_, last, _)| *last < cid);
        match self.runs.get(index) {
            Some((first, _, widths)) if *first <= cid => match widths.as_slice() {
                [single] => *single,
                list => usize::try_from(cid - first)
                    .ok()
                    .and_then(|i| list.get(i))
                    .copied()
                    .unwrap_or(self.default),
            },
            _ => self.default,
        }
    }
}

/// One code of a shown string.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Code {
    pub glyph: Option<u16>,
    /// The code names a glyph the program does not have.
    pub missing: bool,
    /// Horizontal displacement in glyph space (thousandths of text space).
    pub width: f64,
    /// A single-byte code 32: word spacing applies (9.3.3).
    pub is_space: bool,
}

impl Font {
    pub fn units_per_em(&self) -> f64 {
        self.units_per_em
    }

    /// The codes of `string`: one byte each for a simple font, two bytes
    /// (big-endian) for `Identity-H`; a trailing odd byte is ignored.
    pub fn codes<'s>(&'s self, string: &'s [u8]) -> Box<dyn Iterator<Item = Code> + 's> {
        match &self.mapping {
            Mapping::Simple {
                glyphs,
                missing,
                widths,
            } => Box::new(string.iter().map(|byte| Code {
                glyph: glyphs.get(usize::from(*byte)).copied().flatten(),
                missing: missing.get(usize::from(*byte)).copied().unwrap_or(false),
                width: widths.get(usize::from(*byte)).copied().unwrap_or(0.0),
                is_space: *byte == b' ',
            })),
            Mapping::Cid { cid_to_gid, widths } => {
                Box::new(string.as_chunks::<2>().0.iter().map(move |pair| {
                    let cid = u16::from_be_bytes(*pair);
                    let glyph = match cid_to_gid {
                        None => Some(cid),
                        Some(map) => map.get(usize::from(cid)).copied(),
                    };
                    Code {
                        glyph: glyph.filter(|gid| *gid != 0 || cid == 0),
                        missing: false,
                        width: widths.width(u32::from(cid)),
                        is_space: false,
                    }
                }))
            }
        }
    }

    /// The outline of `glyph` in font units, y up. `Ok(None)` when the
    /// program has no such glyph or cannot draw it; an outline with more
    /// than `max_segments` segments is `Err(())`.
    pub fn outline(&self, glyph: u16, max_segments: usize) -> Result<Option<Vec<Segment>>, ()> {
        let mut pen = Pen::new(max_segments);
        let glyph = GlyphId::new(u32::from(glyph));
        let drawn = match self.format {
            Format::OpenType => {
                let Ok(font) = FontRef::new(&self.program) else {
                    return Ok(None);
                };
                let Some(outline) = font.outline_glyphs().get(glyph) else {
                    return Ok(None);
                };
                let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
                outline.draw(settings, &mut pen).is_ok()
            }
            Format::Cff => {
                // Unscaled: font units, the CFF `FontMatrix` applied.
                let Ok(cff) = CffFontRef::new_cff(&self.program, 0, None) else {
                    return Ok(None);
                };
                let Some(subfont) = cff
                    .subfont_index(glyph)
                    .and_then(|index| cff.subfont(index, &[]).ok())
                else {
                    return Ok(None);
                };
                cff.draw(&subfont, glyph, &[], None, &mut pen).is_ok()
            }
        };
        if !drawn {
            return Ok(None);
        }
        if pen.overflow {
            return Err(());
        }
        pen.segments.shrink_to_fit();
        Ok(Some(pen.segments))
    }
}

/// Collects an outline as segments, quadratic curves raised to cubic ones.
struct Pen {
    segments: Vec<Segment>,
    current: Point,
    start: Point,
    max: usize,
    overflow: bool,
}

impl Pen {
    fn new(max: usize) -> Self {
        Self {
            segments: Vec::new(),
            current: Point::new(0.0, 0.0),
            start: Point::new(0.0, 0.0),
            max,
            overflow: false,
        }
    }

    fn push(&mut self, segment: Segment) {
        if self.segments.len() >= self.max {
            self.overflow = true;
        } else {
            self.segments.push(segment);
        }
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = Point::new(f64::from(x), f64::from(y));
        self.current = point;
        self.start = point;
        self.push(Segment::MoveTo(point));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = Point::new(f64::from(x), f64::from(y));
        self.current = point;
        self.push(Segment::LineTo(point));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, q) = (self.current, Point::new(f64::from(cx), f64::from(cy)));
        let end = Point::new(f64::from(x), f64::from(y));
        let c1 = Point::new(
            p0.x + 2.0 / 3.0 * (q.x - p0.x),
            p0.y + 2.0 / 3.0 * (q.y - p0.y),
        );
        let c2 = Point::new(
            end.x + 2.0 / 3.0 * (q.x - end.x),
            end.y + 2.0 / 3.0 * (q.y - end.y),
        );
        self.current = end;
        self.push(Segment::CurveTo(c1, c2, end));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let end = Point::new(f64::from(x), f64::from(y));
        self.current = end;
        self.push(Segment::CurveTo(
            Point::new(f64::from(cx0), f64::from(cy0)),
            Point::new(f64::from(cx1), f64::from(cy1)),
            end,
        ));
    }

    fn close(&mut self) {
        self.current = self.start;
        self.push(Segment::Close);
    }
}

/// Loads the font dictionary `font`. `Ok(None)` when it is not a kind read
/// yet or is damaged: its text is skipped. Limits and cancellation are
/// errors.
pub(crate) fn load<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    match font.get(b"Subtype").and_then(Object::as_name) {
        Some(b"TrueType" | b"Type1" | b"MMType1") => simple(file, font, budget),
        Some(b"Type0") => cid(file, font, budget),
        _ => Ok(None),
    }
}

/// Glyph of each code of a simple font, and the codes whose glyph name
/// names no glyph of the program.
type SimpleGlyphs = (Box<[Option<u16>; 256]>, Box<[bool; 256]>);

/// A font program, how to read it, and its units per em.
type Program = (Arc<[u8]>, Format, f64);

/// The embedded program of a descriptor, if it can be opened: `/FontFile2`
/// (TrueType), or `/FontFile3` with `/Subtype /OpenType`, `/Type1C` or
/// `/CIDFontType0C`. `/FontFile` (Type 1) is not read (ADR-0005).
fn program<S: Source>(
    file: &PdfFile<S>,
    descriptor: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Program>, Error> {
    let (object, format) = if let Some(object) = descriptor.get(b"FontFile2") {
        (object, Format::OpenType)
    } else if let Some(object) = descriptor.get(b"FontFile3") {
        let subtype = match resolve_dictionary(file, object, budget)? {
            Some(stream) => stream
                .get(b"Subtype")
                .and_then(Object::as_name)
                .map(<[u8]>::to_vec),
            None => None,
        };
        match subtype.as_deref() {
            Some(b"OpenType") => (object, Format::OpenType),
            Some(b"Type1C" | b"CIDFontType0C") => (object, Format::Cff),
            _ => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    let Some(bytes) = stream_bytes(file, object, budget)? else {
        return Ok(None);
    };
    let units_per_em = match format {
        Format::OpenType => match FontRef::new(&bytes).and_then(|font| font.head()) {
            Ok(head) => f64::from(head.units_per_em()),
            Err(_) => return Ok(None),
        },
        Format::Cff => match CffFontRef::new_cff(&bytes, 0, None) {
            Ok(cff) => f64::from(cff.upem()),
            Err(_) => return Ok(None),
        },
    };
    // 16 to 16 384 units per em (OpenType `head`); anything else is damaged.
    if !(16.0..=16_384.0).contains(&units_per_em) {
        return Ok(None);
    }
    Ok(Some((Arc::from(bytes), format, units_per_em)))
}

/// A simple font: one byte per code.
fn simple<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    let Some(descriptor) = font.get(b"FontDescriptor") else {
        return Ok(None);
    };
    let Some(descriptor) = resolve_dictionary(file, descriptor, budget)? else {
        return Ok(None);
    };
    let Some((program, format, units_per_em)) = program(file, &descriptor, budget)? else {
        return Ok(None);
    };
    let flags = descriptor
        .get(b"Flags")
        .and_then(Object::as_integer)
        .unwrap_or(0);
    let symbolic = flags & 4 != 0;
    let encoding = match font.get(b"Encoding") {
        Some(object) => resolve(file, object, budget)?,
        None => None,
    };
    let (glyphs, missing, font_ref) = match format {
        Format::OpenType => {
            let default = if symbolic { None } else { Some(&STANDARD) };
            let names = encoding_names(file, encoding.as_ref(), default, budget)?;
            let Ok(font_ref) = FontRef::new(&program) else {
                return Ok(None);
            };
            let (glyphs, missing) = simple_glyphs(&font_ref, &names, encoding.is_some(), symbolic);
            (glyphs, missing, Some(font_ref))
        }
        Format::Cff => {
            // Without a base encoding, the font's own (built-in) one applies.
            let names = encoding_names(file, encoding.as_ref(), None, budget)?;
            let Ok(cff) = CffFontRef::new_cff(&program, 0, None) else {
                return Ok(None);
            };
            let (glyphs, missing) = cff_glyphs(&cff, &names, budget)?;
            (glyphs, missing, None)
        }
    };
    let widths = simple_widths(
        file,
        font,
        &descriptor,
        font_ref.as_ref(),
        &glyphs,
        units_per_em,
        budget,
    )?;
    Ok(Some(Font {
        program: Arc::clone(&program),
        format,
        units_per_em,
        mapping: Mapping::Simple {
            glyphs,
            missing,
            widths,
        },
    }))
}

/// Glyph of each code of a simple CFF font: the name the encoding gives,
/// looked up among the CFF charset names; a code without a name uses the
/// CFF's built-in encoding. A name the font lacks is reported (`missing`).
fn cff_glyphs(
    cff: &CffFontRef<'_>,
    names: &[Option<Vec<u8>>],
    budget: &mut Budget,
) -> Result<SimpleGlyphs, Error> {
    let mut by_name: BTreeMap<&[u8], u16> = BTreeMap::new();
    if let Some(charset) = cff.charset() {
        for (gid, sid) in charset.iter() {
            budget.charge_object()?;
            if let (Some(name), Ok(gid)) = (cff.string(sid), u16::try_from(gid.to_u32())) {
                by_name.entry(name).or_insert(gid);
            }
        }
    }
    let builtin = cff.encoding();
    let mut glyphs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    for ((code, slot), lost) in (0_u8..=255).zip(glyphs.iter_mut()).zip(missing.iter_mut()) {
        let name = names.get(usize::from(code)).and_then(Option::as_deref);
        let glyph = match name {
            Some(name) => by_name.get(name).copied(),
            None => builtin
                .as_ref()
                .and_then(|encoding| encoding.map(code))
                .and_then(|gid| u16::try_from(gid.to_u32()).ok()),
        }
        .filter(|gid| *gid != 0);
        *slot = glyph;
        *lost = glyph.is_none() && name.is_some_and(|name| name != b".notdef");
    }
    Ok((glyphs, missing))
}

/// Glyph name of each code: `/BaseEncoding` (or `default`) with
/// `/Differences` applied. Codes WinAnsi or MacRoman leave undefined take
/// their Standard name (ISO 32000-2, 9.6.5.4).
fn encoding_names<S: Source>(
    file: &PdfFile<S>,
    encoding: Option<&Object>,
    default: Option<&'static [&'static str; 256]>,
    budget: &mut Budget,
) -> Result<Vec<Option<Vec<u8>>>, Error> {
    let base = |name: &[u8]| -> Option<&'static [&'static str; 256]> {
        match name {
            b"WinAnsiEncoding" => Some(&WIN_ANSI),
            b"MacRomanEncoding" => Some(&MAC_ROMAN),
            b"MacExpertEncoding" => Some(&MAC_EXPERT),
            b"StandardEncoding" => Some(&STANDARD),
            _ => None,
        }
    };
    let (table, differences) = match encoding {
        Some(Object::Name(name)) => (base(name).or(default), None),
        Some(Object::Dictionary(dictionary)) => {
            let table = dictionary
                .get(b"BaseEncoding")
                .and_then(Object::as_name)
                .and_then(base)
                .or(default);
            let differences = match dictionary.get(b"Differences") {
                Some(object) => resolve(file, object, budget)?,
                None => None,
            };
            (table, differences)
        }
        _ => (default, None),
    };
    let fill = table.is_some_and(|t| std::ptr::eq(t, &WIN_ANSI) || std::ptr::eq(t, &MAC_ROMAN));
    let mut names: Vec<Option<Vec<u8>>> = (0..256)
        .map(|code: usize| {
            table
                .and_then(|table| table.get(code))
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    fill.then(|| STANDARD.get(code))
                        .flatten()
                        .filter(|name| !name.is_empty())
                })
                .map(|name| name.as_bytes().to_vec())
        })
        .collect();
    if let Some(Object::Array(items)) = differences {
        let mut code: Option<usize> = None;
        for item in items.iter().take(MAX_DIFFERENCES) {
            match item {
                Object::Integer(start) => code = usize::try_from(*start).ok(),
                Object::Name(name) => {
                    if let Some(slot) = code.and_then(|c| names.get_mut(c)) {
                        *slot = Some(name.clone());
                    }
                    code = code.map(|c| c.saturating_add(1));
                }
                _ => {}
            }
        }
    }
    Ok(names)
}

/// The Unicode code point of a glyph name: Adobe Glyph List, `uniXXXX` or
/// `uXXXX[XX]` (AGL specification, simplified to a single code point).
pub(crate) fn unicode(name: &[u8]) -> Option<u32> {
    let name = std::str::from_utf8(name).ok()?;
    let base = name.split('.').next().unwrap_or(name);
    if let Ok(index) = GLYPH_LIST.binary_search_by(|(entry, _)| (*entry).cmp(base)) {
        return GLYPH_LIST.get(index).map(|(_, code)| *code);
    }
    let hex = |digits: &str| {
        (digits.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(digits, 16).ok())
            .flatten()
    };
    if let Some(digits) = base.strip_prefix("uni").filter(|d| d.len() == 4) {
        return hex(digits);
    }
    if let Some(digits) = base
        .strip_prefix('u')
        .filter(|d| (4..=6).contains(&d.len()))
    {
        return hex(digits).filter(|c| *c <= 0x10_FFFF);
    }
    None
}

/// Glyph id of each code of a simple TrueType font (ISO 32000-2, 9.6.5.4):
/// through the glyph name and the (3,1) Unicode `cmap` when the font is not
/// symbolic or has an encoding, otherwise (or failing that) the (3,0) symbol
/// `cmap` at code, 0xF000 + code, 0xF100 + code or 0xF200 + code, then the
/// (1,0) Macintosh `cmap` (through MacRoman when the name is known). Without
/// any of those `cmap`s, the code is the glyph id.
/// Returns the glyphs and the codes whose name finds no glyph. A name that
/// is not in the Adobe Glyph List as written (`a.sc`, `g12`) is first
/// looked up among the program's own glyph names (`post` table).
fn simple_glyphs(
    font: &FontRef<'_>,
    names: &[Option<Vec<u8>>],
    has_encoding: bool,
    symbolic: bool,
) -> SimpleGlyphs {
    let mut unicode_map = None;
    let mut symbol_map = None;
    let mut mac_map = None;
    if let Ok(cmap) = font.cmap() {
        for (index, record) in cmap.encoding_records().iter().enumerate() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let slot = match (record.platform_id(), record.encoding_id()) {
                (PlatformId::Windows, 1) => &mut unicode_map,
                (PlatformId::Windows, 0) => &mut symbol_map,
                (PlatformId::Macintosh, 0) => &mut mac_map,
                _ => continue,
            };
            if slot.is_none()
                && let Ok(subtable) = cmap.subtable(index)
            {
                *slot = Some(subtable);
            }
        }
    }
    let map = |subtable: &Option<CmapSubtable<'_>>, code: u32| {
        subtable
            .as_ref()
            .and_then(|s| s.map_codepoint(code))
            .and_then(|gid| u16::try_from(gid.to_u32()).ok())
            .filter(|gid| *gid != 0)
    };
    let none = unicode_map.is_none() && symbol_map.is_none() && mac_map.is_none();
    // The program's own glyph names, read once if a name needs them.
    let mut post: Option<BTreeMap<String, u16>> = None;
    let mut post_glyph = |name: &[u8]| -> Option<u16> {
        let names = post.get_or_insert_with(|| {
            GlyphNames::new(font)
                .iter()
                .filter(|(_, name)| !name.is_synthesized())
                .filter_map(|(gid, name)| {
                    u16::try_from(gid.to_u32())
                        .ok()
                        .map(|gid| (name.as_str().to_owned(), gid))
                })
                .collect()
        });
        std::str::from_utf8(name)
            .ok()
            .and_then(|name| names.get(name))
            .copied()
            .filter(|gid| *gid != 0)
    };
    let mut glyphs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    for ((code, slot), lost) in (0_u32..).zip(glyphs.iter_mut()).zip(missing.iter_mut()) {
        let name = names.get(code as usize).and_then(Option::as_deref);
        let mut glyph = None;
        let listed = name.is_some_and(|name| {
            std::str::from_utf8(name).is_ok_and(|name| {
                GLYPH_LIST
                    .binary_search_by(|(entry, _)| (*entry).cmp(name))
                    .is_ok()
            })
        });
        if let Some(name) = name
            && !listed
        {
            glyph = post_glyph(name);
        }
        if glyph.is_none()
            && (!symbolic || has_encoding)
            && let Some(point) = name.and_then(unicode)
        {
            glyph = map(&unicode_map, point);
        }
        if glyph.is_none() {
            glyph = [0, 0xF000, 0xF100, 0xF200]
                .into_iter()
                .find_map(|offset| map(&symbol_map, offset + code));
        }
        // The code itself is looked up only when it has no name: a name the
        // program lacks is reported, not replaced by another glyph.
        if glyph.is_none() {
            let mac_code = match name {
                Some(name) => MAC_ROMAN
                    .iter()
                    .position(|n| n.as_bytes() == name)
                    .and_then(|c| u32::try_from(c).ok()),
                None => Some(code),
            };
            glyph = mac_code.and_then(|c| map(&mac_map, c));
        }
        if glyph.is_none() && name.is_none() {
            glyph = map(&unicode_map, code);
        }
        if glyph.is_none() && none {
            glyph = u16::try_from(code).ok().filter(|c| *c != 0);
        }
        if glyph.is_none() && name.is_some() {
            glyph = name.and_then(&mut post_glyph);
        }
        *slot = glyph;
        // `.notdef` and unnamed codes legitimately draw nothing.
        *lost = glyph.is_none() && name.is_some_and(|name| name != b".notdef");
    }
    (glyphs, missing)
}

/// `/FirstChar` and `/Widths`, `/MissingWidth` outside them; without
/// `/Widths`, the advance widths of an OpenType program (a CFF program
/// keeps `/MissingWidth`).
fn simple_widths<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    descriptor: &Dictionary,
    font_ref: Option<&FontRef<'_>>,
    glyphs: &[Option<u16>; 256],
    units_per_em: f64,
    budget: &mut Budget,
) -> Result<Box<[f64; 256]>, Error> {
    let missing = descriptor
        .get(b"MissingWidth")
        .and_then(number)
        .unwrap_or(0.0);
    let mut widths = Box::new([missing; 256]);
    let listed = match font.get(b"Widths") {
        Some(object) => resolve(file, object, budget)?,
        None => None,
    };
    match listed {
        Some(Object::Array(items)) => {
            let first = font
                .get(b"FirstChar")
                .and_then(Object::as_integer)
                .and_then(|f| usize::try_from(f).ok())
                .unwrap_or(0);
            for (offset, item) in items.iter().take(256).enumerate() {
                let value = match item {
                    Object::Reference(_) => resolve(file, item, budget)?.as_ref().and_then(number),
                    other => number(other),
                };
                if let (Some(width), Some(slot)) =
                    (value, widths.get_mut(first.saturating_add(offset)))
                {
                    *slot = width;
                }
            }
        }
        _ => {
            let Some(font_ref) = font_ref else {
                return Ok(widths);
            };
            let metrics = GlyphMetrics::new(font_ref, Size::unscaled(), LocationRef::default());
            for (slot, glyph) in widths.iter_mut().zip(glyphs.iter()) {
                if let Some(advance) =
                    glyph.and_then(|gid| metrics.advance_width(GlyphId::new(u32::from(gid))))
                {
                    *slot = f64::from(advance) * 1000.0 / units_per_em;
                }
            }
        }
    }
    Ok(widths)
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

/// A Type 0 font with `Identity-H` and a `CIDFontType2` or `CIDFontType0`
/// descendant.
fn cid<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    if font.get(b"Encoding").and_then(Object::as_name) != Some(b"Identity-H") {
        return Ok(None);
    }
    let descendants = match font.get(b"DescendantFonts") {
        Some(object) => resolve(file, object, budget)?,
        None => None,
    };
    let Some(Object::Array(descendants)) = descendants else {
        return Ok(None);
    };
    let [descendant] = descendants.as_slice() else {
        return Ok(None);
    };
    let Some(descendant) = resolve_dictionary(file, descendant, budget)? else {
        return Ok(None);
    };
    if !matches!(
        descendant.get(b"Subtype").and_then(Object::as_name),
        Some(b"CIDFontType2" | b"CIDFontType0")
    ) {
        return Ok(None);
    }
    let Some(descriptor) = descendant.get(b"FontDescriptor") else {
        return Ok(None);
    };
    let Some(descriptor) = resolve_dictionary(file, descriptor, budget)? else {
        return Ok(None);
    };
    let Some((program, format, units_per_em)) = program(file, &descriptor, budget)? else {
        return Ok(None);
    };
    let cid_to_gid = match (format, descendant.get(b"CIDToGIDMap")) {
        // A CID-keyed CFF maps CIDs itself; otherwise CID = glyph id.
        (Format::Cff, _) => {
            let Ok(cff) = CffFontRef::new_cff(&program, 0, None) else {
                return Ok(None);
            };
            if cff.is_cid() {
                let Some(charset) = cff.charset() else {
                    return Ok(None);
                };
                let mut map = Vec::new();
                for (gid, cid) in charset.iter() {
                    budget.charge_object()?;
                    let Ok(gid) = u16::try_from(gid.to_u32()) else {
                        continue;
                    };
                    let cid = usize::from(cid.to_u16());
                    if map.len() <= cid {
                        map.resize(cid + 1, 0);
                    }
                    if let Some(slot) = map.get_mut(cid) {
                        *slot = gid;
                    }
                }
                Some(map)
            } else {
                None
            }
        }
        (Format::OpenType, None) => None,
        (Format::OpenType, Some(Object::Name(name))) if name.as_slice() == b"Identity" => None,
        (Format::OpenType, Some(object)) => {
            let Some(bytes) = stream_bytes(file, object, budget)? else {
                return Ok(None);
            };
            Some(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(MAX_CIDS)
                    .map(|pair| u16::from_be_bytes(*pair))
                    .collect(),
            )
        }
    };
    let widths = cid_widths(file, &descendant, budget)?;
    Ok(Some(Font {
        program,
        format,
        units_per_em,
        mapping: Mapping::Cid { cid_to_gid, widths },
    }))
}

/// `/DW` (1000 by default) and `/W` (9.7.4.3): `c [w1 w2 …]` or
/// `cfirst clast w`. Malformed entries end the array; ranges are sorted and
/// later overlapping ones dropped.
fn cid_widths<S: Source>(
    file: &PdfFile<S>,
    descendant: &Dictionary,
    budget: &mut Budget,
) -> Result<CidWidths, Error> {
    let default = descendant.get(b"DW").and_then(number).unwrap_or(1000.0);
    let listed = match descendant.get(b"W") {
        Some(object) => resolve(file, object, budget)?,
        None => None,
    };
    let mut runs = BTreeMap::new();
    if let Some(Object::Array(items)) = listed {
        let mut items = items.iter().take(MAX_W_ENTRIES).peekable();
        while let Some(first) = items.next() {
            budget.charge_object()?;
            let Some(first) = number(first).and_then(|f| u32::try_from(f as i64).ok()) else {
                break;
            };
            match items.next() {
                Some(Object::Array(widths)) => {
                    // A non-number ends the run: later widths would be shifted.
                    let widths: Vec<f64> = widths.iter().map_while(number).collect();
                    let Some(count) = u32::try_from(widths.len()).ok().filter(|c| *c > 0) else {
                        continue;
                    };
                    let last = first.saturating_add(count - 1);
                    runs.entry(first).or_insert((last, widths));
                }
                Some(last) => {
                    let (Some(last), Some(width)) = (
                        number(last).and_then(|l| u32::try_from(l as i64).ok()),
                        items.next().and_then(number),
                    ) else {
                        break;
                    };
                    if last >= first {
                        runs.entry(first).or_insert((last, vec![width]));
                    }
                }
                None => break,
            }
        }
    }
    let mut sorted: Vec<(u32, u32, Vec<f64>)> = Vec::with_capacity(runs.len());
    for (first, (last, widths)) in runs {
        if sorted
            .last()
            .is_none_or(|(_, previous, _)| *previous < first)
        {
            sorted.push((first, last, widths));
        }
    }
    Ok(CidWidths {
        default,
        runs: sorted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names_give_code_points() {
        assert_eq!(unicode(b"A"), Some(0x41));
        assert_eq!(unicode(b"Euro"), Some(0x20AC));
        assert_eq!(unicode(b"a.sc"), Some(0x61));
        assert_eq!(unicode(b"uni00E9"), Some(0xE9));
        assert_eq!(unicode(b"u1F600"), Some(0x1F600));
        assert_eq!(unicode(b"u110000"), None);
        assert_eq!(unicode(b"uniZZZZ"), None);
        assert_eq!(unicode(b"g123"), None);
    }

    #[test]
    fn the_glyph_list_is_sorted_and_encodings_are_full() {
        assert!(GLYPH_LIST.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(WIN_ANSI[0x41], "A");
        assert_eq!(WIN_ANSI[0x80], "Euro");
        assert_eq!(MAC_ROMAN[0x8E], "eacute");
        assert_eq!(STANDARD[0x27], "quoteright");
    }

    #[test]
    fn cid_widths_follow_runs_and_default() {
        let widths = CidWidths {
            default: 1000.0,
            runs: vec![(1, 3, vec![500.0, 600.0, 700.0]), (10, 20, vec![250.0])],
        };
        let at = |cid| widths.width(cid);
        assert_eq!(
            [at(0), at(1), at(3), at(4), at(10), at(20), at(21)],
            [1000.0, 500.0, 700.0, 1000.0, 250.0, 250.0, 1000.0]
        );
    }
}
