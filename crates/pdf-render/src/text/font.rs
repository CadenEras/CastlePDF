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
//! CID-keyed CFF. A simple font whose program is not read (none embedded, a
//! Type 1 program, a damaged one) is replaced (`standard`); Type 3 fonts are
//! read by `type3`. A Type 0 font's encoding is `Identity-H`, `Identity-V`
//! or an embedded CMap (ADR-0006, tranche 3.4a), whose `usecmap` chain is
//! followed at most `MAX_USECMAP_DEPTH` deep; vertical fonts use `/W2` and
//! `/DW2` (9.7.4.3). Any other font, another predefined CMap (tranche 3.7b),
//! or a CID font whose program cannot be opened is `Unsupported`: its text
//! is not drawn and the page reports `Text`.

use std::collections::BTreeMap;
use std::sync::Arc;

use pdf_syntax::{Budget, Dictionary, Object, PdfFile, Source};
use skrifa::instance::{LocationRef, Size};
use skrifa::metrics::GlyphMetrics;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::raw::ps::cff::CffFontRef;
use skrifa::raw::ps::type1::Type1Font;
use skrifa::raw::tables::cmap::{CmapSubtable, PlatformId};
use skrifa::{FontRef, GlyphId, GlyphNames, MetadataProvider};

use super::cmap::{CMap, CMapError, Code as CodeKey};
use super::encodings::{
    GLYPH_LIST, MAC_EXPERT, MAC_ROMAN, STANDARD, SYMBOL, SYMBOL_WIDTHS, WIN_ANSI, ZAPF_DINGBATS,
    ZAPF_DINGBATS_LIST, ZAPF_DINGBATS_WIDTHS,
};
use super::predefined;
use super::unicode::{TextMap, name_text};
use super::{resolve, resolve_dictionary, standard, stream_bytes};
use crate::Error;
use crate::geometry::{Point, Segment};

/// Most entries read from a `/W` array of a CID font.
const MAX_W_ENTRIES: usize = 65_536;
/// Most entries read from a `/Differences` array.
const MAX_DIFFERENCES: usize = 4096;
/// CIDs are at most 65 535 (annex C): entries of `/CIDToGIDMap` beyond are unused.
const MAX_CIDS: usize = 65_536;
/// Deepest chain of embedded CMaps followed through `usecmap` or `/UseCMap`.
pub(crate) const MAX_USECMAP_DEPTH: usize = 4;
/// Most name comparisons a Type 1 program may cost to read: `read-fonts`
/// looks up each name of its `/Encoding` among all its glyphs, one by one,
/// without a budget or cancellation; past this bound (well under a second)
/// the program is not read, and a replacement draws the font.
const MAX_TYPE1_LOOKUPS: u64 = 100_000_000;
/// Bytes of `read-fonts`' index per glyph (two ranges) and per subroutine.
const TYPE1_ENTRY_BYTES: u64 = 40;
/// Least bytes of one charstring or subroutine entry (`/a 0 RD `).
const TYPE1_MIN_ENTRY: u64 = 8;

/// A font ready to show text.
#[derive(Debug)]
pub(crate) struct Font {
    program: Arc<[u8]>,
    format: Format,
    /// The program read once, for a Type 1 font (`/FontFile`).
    type1: Option<Type1>,
    units_per_em: f64,
    mapping: Mapping,
    /// Drawn with a replacement font (`standard`): reported.
    substituted: bool,
    /// The Unicode text of its codes (ADR-0006, tranche 3.3).
    text: TextMap,
}

/// How the program bytes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// TrueType or OpenType file, read by `skrifa`.
    OpenType,
    /// Bare CFF (Compact Font Format), read by `read-fonts`.
    Cff,
    /// Type 1 (`/FontFile`), read by `read-fonts` (ADR-0005, amended for
    /// tranche 3.8): cleartext part, then the `eexec` part, binary or hex.
    Type1,
}

/// A Type 1 program as read by `read-fonts`: decrypted, its charstrings and
/// subroutines ready to draw.
#[derive(Clone)]
struct Type1(Arc<Type1Font>);

impl std::fmt::Debug for Type1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Type1")
            .field("glyphs", &self.0.num_glyphs())
            .finish_non_exhaustive()
    }
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
    /// A Type 0 font: codes through its encoding to CIDs.
    Cid {
        codes: CidCodes,
        /// `None` for `/Identity`.
        cid_to_gid: Option<Vec<u16>>,
        widths: CidWidths,
        /// `/W2` and `/DW2` for a vertical font (`Identity-V`, `/WMode 1`).
        vertical: Option<VerticalMetrics>,
    },
}

/// How the codes of a Type 0 font's strings become CIDs.
#[derive(Debug)]
enum CidCodes {
    /// `Identity-H` or `Identity-V`: two bytes per code, the code is the CID.
    Identity,
    /// An embedded or predefined CMap: codes split by its codespace, CIDs
    /// from its `cidchar`/`cidrange`, else its notdef ranges, else CID 0
    /// (9.7.6.3).
    CMap(Arc<CMap>),
}

/// `/W2` and `/DW2` of a vertical CID font (9.7.4.3), in glyph space.
#[derive(Debug)]
struct VerticalMetrics {
    /// Position vector y and vertical displacement of glyphs not listed.
    default: (f64, f64),
    /// Non-overlapping runs sorted by first CID: first, last, metrics
    /// (displacement, position x, position y), one for a whole range or one
    /// per CID.
    runs: Vec<(u32, u32, Vec<[f64; 3]>)>,
}

impl VerticalMetrics {
    /// Displacement and position vector of `cid`, whose horizontal width is
    /// `width` (the default position x is half of it).
    fn metric(&self, cid: u32, width: f64) -> Vertical {
        let index = self.runs.partition_point(|(_, last, _)| *last < cid);
        let listed = match self.runs.get(index) {
            Some((first, _, metrics)) if *first <= cid => match metrics.as_slice() {
                [single] => Some(*single),
                list => usize::try_from(cid - first)
                    .ok()
                    .and_then(|i| list.get(i))
                    .copied(),
            },
            _ => None,
        };
        match listed {
            Some([displacement, x, y]) => Vertical {
                displacement,
                position: (x, y),
            },
            None => Vertical {
                displacement: self.default.1,
                position: (width / 2.0, self.default.0),
            },
        }
    }
}

/// How a glyph of a vertical font is placed and advances (9.7.4.3), in
/// glyph space (thousandths of text space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Vertical {
    /// Vertical displacement (usually negative: down).
    pub displacement: f64,
    /// Position vector: from the glyph's horizontal origin to its vertical
    /// one, subtracted to place the glyph.
    pub position: (f64, f64),
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
    /// The code as shown, for its Unicode text.
    pub key: CodeKey,
    pub glyph: Option<u16>,
    /// The code names a glyph the program does not have.
    pub missing: bool,
    /// Horizontal displacement in glyph space (thousandths of text space).
    pub width: f64,
    /// A single-byte code 32: word spacing applies (9.3.3).
    pub is_space: bool,
    /// Placement and displacement in a vertical font.
    pub vertical: Option<Vertical>,
    /// The CID the encoding maps the code to, for a Type 0 font: `None`
    /// for a code that only a notdef range covers, whose glyph is drawn but
    /// whose text is not the notdef CID's.
    pub cid: Option<u32>,
}

impl Font {
    /// Bytes of the program, counted when the font is cached; for a Type 1
    /// program, its decrypted copy and the index of its glyphs besides.
    pub fn program_len(&self) -> usize {
        match &self.type1 {
            Some(Type1(font)) => {
                let index = u64::from(font.num_glyphs()).saturating_mul(TYPE1_ENTRY_BYTES);
                self.program
                    .len()
                    .saturating_mul(2)
                    .saturating_add(usize::try_from(index).unwrap_or(usize::MAX))
            }
            None => self.program.len(),
        }
    }

    /// Bytes of the encoding CMap tables the font holds alone, counted when
    /// the font is cached: none for `Identity` or a shared predefined
    /// CMap, all of them for an embedded one (with the parents it copied).
    pub fn tables_len(&self) -> u64 {
        match &self.mapping {
            Mapping::Cid {
                codes: CidCodes::CMap(cmap),
                ..
            } if !predefined::is_shared(cmap) => cmap.tables_bytes(),
            _ => 0,
        }
    }

    pub fn units_per_em(&self) -> f64 {
        self.units_per_em
    }

    /// Drawn with a replacement font: the glyphs only resemble the
    /// original ones.
    pub fn substituted(&self) -> bool {
        self.substituted
    }

    /// Has a program to draw its glyphs; a font loaded for its text only
    /// (`load_text_only`) has none.
    pub fn drawable(&self) -> bool {
        !self.program.is_empty()
    }

    /// Writes vertically: `Identity-V` or a CMap with `/WMode 1`.
    pub fn vertical(&self) -> bool {
        matches!(
            &self.mapping,
            Mapping::Cid {
                vertical: Some(_),
                ..
            }
        )
    }

    /// How the Unicode text of its codes is found.
    pub fn text(&self) -> &TextMap {
        &self.text
    }

    /// The codes of `string`: one byte each for a simple font, two bytes
    /// (big-endian) for `Identity-H` and `Identity-V` (a trailing odd byte is
    /// ignored), as the codespace splits them for an embedded CMap.
    pub fn codes<'s>(&'s self, string: &'s [u8]) -> Box<dyn Iterator<Item = Code> + 's> {
        match &self.mapping {
            Mapping::Simple {
                glyphs,
                missing,
                widths,
            } => Box::new(string.iter().map(|byte| Code {
                key: CodeKey {
                    value: u32::from(*byte),
                    len: 1,
                },
                glyph: glyphs.get(usize::from(*byte)).copied().flatten(),
                missing: missing.get(usize::from(*byte)).copied().unwrap_or(false),
                width: widths.get(usize::from(*byte)).copied().unwrap_or(0.0),
                is_space: *byte == b' ',
                vertical: None,
                cid: None,
            })),
            Mapping::Cid {
                codes,
                cid_to_gid,
                widths,
                vertical,
            } => {
                let code = move |key: CodeKey, cid: u32, mapped: bool| {
                    let glyph = match cid_to_gid {
                        None => u16::try_from(cid).ok(),
                        Some(map) => usize::try_from(cid)
                            .ok()
                            .and_then(|cid| map.get(cid))
                            .copied(),
                    };
                    let width = widths.width(cid);
                    Code {
                        key,
                        glyph: glyph.filter(|gid| *gid != 0 || cid == 0),
                        missing: false,
                        width,
                        is_space: key.len == 1 && key.value == 32,
                        vertical: vertical.as_ref().map(|metrics| metrics.metric(cid, width)),
                        cid: mapped.then_some(cid),
                    }
                };
                match codes {
                    CidCodes::Identity => {
                        Box::new(string.as_chunks::<2>().0.iter().map(move |pair| {
                            let cid = u32::from(u16::from_be_bytes(*pair));
                            code(CodeKey { value: cid, len: 2 }, cid, true)
                        }))
                    }
                    CidCodes::CMap(cmap) => {
                        let mut rest = string;
                        Box::new(std::iter::from_fn(move || {
                            let key = cmap.next_code(rest)?;
                            rest = rest.get(usize::from(key.len)..).unwrap_or_default();
                            Some(match cmap.cid(key) {
                                Some(cid) => code(key, cid, true),
                                None => code(key, cmap.notdef(key).unwrap_or(0), false),
                            })
                        }))
                    }
                }
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
            Format::Type1 => {
                // Unscaled: font units, the `FontMatrix` applied.
                let Some(Type1(font)) = &self.type1 else {
                    return Ok(None);
                };
                if glyph.to_u32() >= font.num_glyphs() {
                    return Ok(None);
                }
                font.draw(glyph, None, &mut pen).is_ok()
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

/// A font for its text only, when \`load\` cannot read it for drawing
/// (ADR-0006, tranche 3.4b): no program, its codes, widths and text from
/// the dictionary alone. A simple font's codes are one byte, named by its
/// \`/Encoding\` over the standard encoding (none for a symbolic font
/// without one); a Type 0 font's codes follow \`Identity-H\`/\`-V\` or an
/// embedded CMap, its widths \`/W\` and \`/W2\`. \`Ok(None)\` for another kind,
/// another predefined CMap, or a damaged dictionary. Drawing it draws
/// nothing (\`drawable\` is false).
pub(crate) fn load_text_only<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    match font.get(b"Subtype").and_then(Object::as_name) {
        Some(b"TrueType" | b"Type1" | b"MMType1") => {
            let descriptor = match font.get(b"FontDescriptor") {
                Some(object) => match resolve_dictionary(file, object, budget)? {
                    Some(descriptor) => descriptor,
                    None => return Ok(None),
                },
                None => Dictionary::new(),
            };
            let flags = descriptor
                .get(b"Flags")
                .and_then(Object::as_integer)
                .unwrap_or(0);
            let encoding = match font.get(b"Encoding") {
                Some(object) => resolve(file, object, budget)?,
                None => None,
            };
            let default = if flags & 4 != 0 && encoding.is_none() {
                None
            } else {
                Some(&STANDARD)
            };
            let names = encoding_names(file, encoding.as_ref(), default, budget)?;
            let glyphs = Box::new([None; 256]);
            let widths = simple_widths(file, font, &descriptor, None, &glyphs, 1000.0, budget)?;
            Ok(Some(Font {
                program: Arc::from(Vec::new()),
                format: Format::OpenType,
                type1: None,
                units_per_em: 1000.0,
                mapping: Mapping::Simple {
                    glyphs,
                    missing: Box::new([false; 256]),
                    widths,
                },
                substituted: false,
                text: TextMap::new(font, Some(&names)),
            }))
        }
        Some(b"Type0") => {
            let Some((codes, vertical)) = cid_encoding(file, font, budget)? else {
                return Ok(None);
            };
            let descendant = match font.get(b"DescendantFonts") {
                Some(object) => match resolve(file, object, budget)? {
                    Some(Object::Array(items)) => match items.as_slice() {
                        [descendant] => resolve_dictionary(file, descendant, budget)?,
                        _ => None,
                    },
                    _ => None,
                },
                None => None,
            };
            let Some(descendant) = descendant else {
                return Ok(None);
            };
            let widths = cid_widths(file, &descendant, budget)?;
            let vertical = if vertical {
                Some(vertical_metrics(file, &descendant, budget)?)
            } else {
                None
            };
            let collection = collection_of(file, font, &descendant, budget)?;
            Ok(Some(Font {
                program: Arc::from(Vec::new()),
                format: Format::OpenType,
                type1: None,
                units_per_em: 1000.0,
                mapping: Mapping::Cid {
                    codes,
                    // No program: no glyph either way.
                    cid_to_gid: Some(Vec::new()),
                    widths,
                    vertical,
                },
                substituted: false,
                text: TextMap::new(font, None).with_collection(collection),
            }))
        }
        _ => Ok(None),
    }
}

/// Glyph of each code of a simple font, and the codes whose glyph name
/// names no glyph of the program.
type SimpleGlyphs = (Box<[Option<u16>; 256]>, Box<[bool; 256]>);

/// A font program, how to read it, its units per em, and the Type 1
/// program read.
type Program = (Arc<[u8]>, Format, f64, Option<Type1>);

/// The embedded program of a descriptor, if it can be opened: `/FontFile2`
/// (TrueType), `/FontFile3` with `/Subtype /OpenType`, `/Type1C` or
/// `/CIDFontType0C`, or `/FontFile` (Type 1, read once here and kept in
/// the program's `Type1`).
fn program<S: Source>(
    file: &PdfFile<S>,
    descriptor: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Program>, Error> {
    let (object, format) = if let Some(object) = descriptor.get(b"FontFile2") {
        (object, Format::OpenType)
    } else if let Some(object) = descriptor.get(b"FontFile") {
        (object, Format::Type1)
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
    let mut type1 = None;
    let units_per_em = match format {
        Format::OpenType => match FontRef::new(&bytes).and_then(|font| font.head()) {
            Ok(head) => f64::from(head.units_per_em()),
            Err(_) => return Ok(None),
        },
        Format::Cff => match CffFontRef::new_cff(&bytes, 0, None) {
            Ok(cff) => f64::from(cff.upem()),
            Err(_) => return Ok(None),
        },
        Format::Type1 => {
            // Bounded before reading (`type1_cost`): the work of the reader,
            // and its memory, charged as at most an index entry per eight
            // bytes besides the decrypted copy.
            let Some(cost) = type1_cost(&bytes) else {
                return Ok(None);
            };
            budget.charge_decoded_bytes(cost)?;
            match Type1Font::new(&bytes) {
                Ok(font) => {
                    let upem = f64::from(font.upem());
                    type1 = Some(Type1(Arc::new(font)));
                    upem
                }
                Err(_) => return Ok(None),
            }
        }
    };
    // 16 to 16 384 units per em (OpenType `head`); anything else is damaged.
    if !(16.0..=16_384.0).contains(&units_per_em) {
        return Ok(None);
    }
    Ok(Some((Arc::from(bytes), format, units_per_em, type1)))
}

/// What reading the Type 1 program `bytes` may cost in memory, or `None`
/// when reading it could take too long: its `/Encoding` names (at most the
/// `/` before `eexec`) times its glyphs (at most one per
/// `TYPE1_MIN_ENTRY` bytes after it), each looked up linearly, must stay
/// within `MAX_TYPE1_LOOKUPS`. Without `eexec` it is not a program.
fn type1_cost(bytes: &[u8]) -> Option<u64> {
    let eexec = bytes.windows(5).position(|window| window == b"eexec")?;
    let names = bytes
        .get(..eexec)?
        .iter()
        .filter(|byte| **byte == b'/')
        .count() as u64;
    let glyphs = (bytes.len() - eexec) as u64 / TYPE1_MIN_ENTRY;
    if names.saturating_mul(glyphs) > MAX_TYPE1_LOOKUPS {
        return None;
    }
    Some((bytes.len() as u64).saturating_add(glyphs.saturating_mul(TYPE1_ENTRY_BYTES)))
}

/// A simple font: one byte per code. Without a program that can be read
/// (none embedded, a Type 1 one, or a damaged one), a non-symbolic font is
/// drawn with a replacement font (`standard`).
fn simple<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    let descriptor = match font.get(b"FontDescriptor") {
        Some(object) => match resolve_dictionary(file, object, budget)? {
            Some(descriptor) => descriptor,
            None => return Ok(None),
        },
        None => Dictionary::new(),
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
    let Some((program, format, units_per_em, type1)) = program(file, &descriptor, budget)? else {
        return substitute(file, font, &descriptor, encoding.as_ref(), budget);
    };
    let (glyphs, missing, font_ref, names) = match format {
        Format::OpenType => {
            let default = if symbolic { None } else { Some(&STANDARD) };
            let names = encoding_names(file, encoding.as_ref(), default, budget)?;
            let Ok(font_ref) = FontRef::new(&program) else {
                return Ok(None);
            };
            let (glyphs, missing) = simple_glyphs(&font_ref, &names, encoding.is_some(), symbolic);
            (glyphs, missing, Some(font_ref), names)
        }
        Format::Cff => {
            // Without a base encoding, the font's own (built-in) one applies.
            let mut names = encoding_names(file, encoding.as_ref(), None, budget)?;
            let Ok(cff) = CffFontRef::new_cff(&program, 0, None) else {
                return Ok(None);
            };
            // Codes left to the built-in encoding take their charset name.
            let (glyphs, missing) = cff_glyphs(&cff, &mut names, budget)?;
            (glyphs, missing, None, names)
        }
        Format::Type1 => {
            // As for CFF: without a base encoding, the built-in one applies.
            let mut names = encoding_names(file, encoding.as_ref(), None, budget)?;
            let Some(Type1(program)) = &type1 else {
                return Ok(None);
            };
            let (glyphs, missing) = type1_glyphs(program, &mut names, budget)?;
            (glyphs, missing, None, names)
        }
    };
    let mut widths = simple_widths(
        file,
        font,
        &descriptor,
        font_ref.as_ref(),
        &glyphs,
        units_per_em,
        budget,
    )?;
    // Without /Widths, a Type 1 program gives its own advances (in font
    // units, its FontMatrix applied), as a TrueType one does.
    if font.get(b"Widths").is_none()
        && let Some(Type1(program)) = &type1
    {
        for (slot, glyph) in widths.iter_mut().zip(glyphs.iter()) {
            let advance = glyph.and_then(|gid| {
                program
                    .advance(GlyphId::new(u32::from(gid)), None)
                    .ok()
                    .flatten()
            });
            if let Some(advance) = advance {
                *slot = f64::from(advance) * 1000.0 / units_per_em;
            }
        }
    }
    Ok(Some(Font {
        program: Arc::clone(&program),
        format,
        type1,
        units_per_em,
        mapping: Mapping::Simple {
            glyphs,
            missing,
            widths,
        },
        substituted: false,
        text: TextMap::new(font, Some(&names)),
    }))
}

/// Glyph of each code of a simple Type 1 font, as `cff_glyphs` does: by the
/// glyph name of the PDF's encoding, else by the font's built-in encoding,
/// whose name then becomes the code's (for its text).
fn type1_glyphs(
    font: &Type1Font,
    names: &mut [Option<Vec<u8>>],
    budget: &mut Budget,
) -> Result<SimpleGlyphs, Error> {
    let mut by_name: BTreeMap<&[u8], u16> = BTreeMap::new();
    for (gid, name) in font.glyph_names() {
        budget.charge_object()?;
        if let Ok(gid) = u16::try_from(gid.to_u32()) {
            by_name.entry(name.as_bytes()).or_insert(gid);
        }
    }
    let builtin = font.encoding();
    let mut glyphs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    for ((code, slot), lost) in (0_u8..=255).zip(glyphs.iter_mut()).zip(missing.iter_mut()) {
        let Some(name) = names.get_mut(usize::from(code)) else {
            continue;
        };
        // A name the PDF gives must find its glyph; one the built-in
        // encoding gives may not (a subset), as for CFF.
        let named_by_pdf = name.is_some();
        if name.is_none() {
            // The built-in encoding names the code.
            *name = builtin
                .as_ref()
                .and_then(|encoding| encoding.glyph_name(code))
                .filter(|name| *name != ".notdef")
                .map(|name| name.as_bytes().to_vec());
        }
        let glyph = name
            .as_deref()
            .and_then(|name| by_name.get(name).copied())
            .filter(|gid| *gid != 0);
        *slot = glyph;
        *lost = named_by_pdf
            && glyph.is_none()
            && name.as_deref().is_some_and(|name| name != b".notdef");
    }
    Ok((glyphs, missing))
}

/// A simple font drawn with a replacement font (ADR-0005, step 5.3): its
/// codes go through glyph names (the default base encoding is Standard) to
/// Unicode and the replacement's `cmap`; its widths are the PDF's, or else
/// those of the replacement, which match the font it replaces. `Symbol` and
/// `ZapfDingbats` have a replacement of their own (`symbols`); another
/// symbolic font (a dingbat font) is not replaced.
fn substitute<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    descriptor: &Dictionary,
    encoding: Option<&Object>,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    let name = font
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .unwrap_or_default();
    let base = match name.get(6) {
        Some(b'+') => name.get(7..).unwrap_or_default(),
        _ => name,
    };
    let flags = descriptor
        .get(b"Flags")
        .and_then(Object::as_integer)
        .unwrap_or(0);
    if let Some(set) = symbol_set(base) {
        return symbols(file, font, descriptor, encoding, set, budget);
    }
    if flags & 4 != 0 && flags & 32 == 0 {
        return Ok(None);
    }
    let weight = descriptor.get(b"FontWeight").and_then(number);
    let angle = descriptor
        .get(b"ItalicAngle")
        .and_then(number)
        .unwrap_or(0.0);
    let (family, bold, italic) = standard::choose(base, flags, weight, angle);
    let Some(bytes) = standard::program(family, bold, italic) else {
        return Ok(None);
    };
    let Ok(font_ref) = FontRef::new(bytes) else {
        return Ok(None);
    };
    let units_per_em = match font_ref.head() {
        Ok(head) => f64::from(head.units_per_em()),
        Err(_) => return Ok(None),
    };
    let names = encoding_names(file, encoding, Some(&STANDARD), budget)?;
    let (glyphs, missing) = simple_glyphs(&font_ref, &names, true, false);
    let widths = simple_widths(
        file,
        font,
        descriptor,
        Some(&font_ref),
        &glyphs,
        units_per_em,
        budget,
    )?;
    Ok(Some(Font {
        program: Arc::from(bytes),
        format: Format::OpenType,
        type1: None,
        units_per_em,
        mapping: Mapping::Simple {
            glyphs,
            missing,
            widths,
        },
        substituted: true,
        text: TextMap::new(font, Some(&names)),
    }))
}

/// The Core 14 symbolic fonts, which have their own built-in encoding and
/// glyph set (ISO 32000-2, D.5 and D.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolSet {
    Symbol,
    Dingbats,
}

/// Which symbolic set a font named `base` (subset prefix removed) uses:
/// `Symbol` and its variants (`SymbolMT`, `Symbol,Bold`), `ZapfDingbats`,
/// `ZapfDingbatsITC` and `Dingbats`.
fn symbol_set(base: &[u8]) -> Option<SymbolSet> {
    if base.starts_with(b"Symbol") {
        Some(SymbolSet::Symbol)
    } else if base.starts_with(b"ZapfDingbats") || base == b"Dingbats" {
        Some(SymbolSet::Dingbats)
    } else {
        None
    }
}

/// `Symbol` or `ZapfDingbats` drawn with their replacement (ADR-0005, step
/// 5.3b): glyphs of Noto fonts merged into one program. Codes go through
/// glyph names (the default base encoding is the font's built-in one) to
/// Unicode, through the ZapfDingbats glyph list for dingbat names, then the
/// replacement's `cmap`. Widths are the PDF's, or else those of Adobe's
/// Core 14 metrics, by glyph name.
fn symbols<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    descriptor: &Dictionary,
    encoding: Option<&Object>,
    set: SymbolSet,
    budget: &mut Budget,
) -> Result<Option<Font>, Error> {
    let Some(bytes) = standard::symbols() else {
        return Ok(None);
    };
    let Ok(font_ref) = FontRef::new(bytes) else {
        return Ok(None);
    };
    let units_per_em = match font_ref.head() {
        Ok(head) => f64::from(head.units_per_em()),
        Err(_) => return Ok(None),
    };
    let (builtin, afm) = match set {
        SymbolSet::Symbol => (&SYMBOL, &SYMBOL_WIDTHS[..]),
        SymbolSet::Dingbats => (&ZAPF_DINGBATS, &ZAPF_DINGBATS_WIDTHS[..]),
    };
    let names = encoding_names(file, encoding, Some(builtin), budget)?;
    let charmap = font_ref.charmap();
    let mut glyphs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    let mut texts: Vec<Option<Box<str>>> = vec![None; 256];
    for (((slot, lost), name), text) in glyphs
        .iter_mut()
        .zip(missing.iter_mut())
        .zip(&names)
        .zip(texts.iter_mut())
    {
        let Some(name) = name.as_deref() else {
            continue;
        };
        let code_point = match set {
            SymbolSet::Dingbats => sorted_lookup(&ZAPF_DINGBATS_LIST, name),
            SymbolSet::Symbol => None,
        }
        .or_else(|| unicode(name));
        *text = match code_point.and_then(char::from_u32) {
            Some(c) => Some(c.to_string().into_boxed_str()),
            None => name_text(name).map(String::into_boxed_str),
        };
        let glyph = code_point
            .and_then(char::from_u32)
            .and_then(|c| charmap.map(c))
            .and_then(|gid| u16::try_from(gid.to_u32()).ok())
            .filter(|gid| *gid != 0);
        *slot = glyph;
        *lost = glyph.is_none() && name != b".notdef";
    }
    let mut widths = simple_widths(
        file,
        font,
        descriptor,
        Some(&font_ref),
        &glyphs,
        units_per_em,
        budget,
    )?;
    if font.get(b"Widths").is_none() {
        for (width, name) in widths.iter_mut().zip(&names) {
            if let Some(afm) = name.as_deref().and_then(|name| sorted_lookup(afm, name)) {
                *width = f64::from(afm);
            }
        }
    }
    Ok(Some(Font {
        program: Arc::from(bytes),
        format: Format::OpenType,
        type1: None,
        units_per_em,
        mapping: Mapping::Simple {
            glyphs,
            missing,
            widths,
        },
        substituted: true,
        text: TextMap::with_texts(font, texts),
    }))
}

/// The value of `name` in a table sorted by name.
fn sorted_lookup<T: Copy>(table: &[(&str, T)], name: &[u8]) -> Option<T> {
    let name = std::str::from_utf8(name).ok()?;
    table
        .binary_search_by(|(entry, _)| (*entry).cmp(name))
        .ok()
        .and_then(|index| table.get(index))
        .map(|(_, value)| *value)
}

/// Glyph of each code of a simple CFF font: the name the encoding gives,
/// looked up among the CFF charset names; a code without a name uses the
/// CFF's built-in encoding, and takes the charset name of its glyph in
/// `names` (for its text). A name the font lacks is reported (`missing`).
fn cff_glyphs(
    cff: &CffFontRef<'_>,
    names: &mut [Option<Vec<u8>>],
    budget: &mut Budget,
) -> Result<SimpleGlyphs, Error> {
    let mut by_name: BTreeMap<&[u8], u16> = BTreeMap::new();
    let mut by_glyph: BTreeMap<u16, &[u8]> = BTreeMap::new();
    if let Some(charset) = cff.charset() {
        for (gid, sid) in charset.iter() {
            budget.charge_object()?;
            if let (Some(name), Ok(gid)) = (cff.string(sid), u16::try_from(gid.to_u32())) {
                by_name.entry(name).or_insert(gid);
                by_glyph.entry(gid).or_insert(name);
            }
        }
    }
    let builtin = cff.encoding();
    let mut glyphs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    for ((code, slot), lost) in (0_u8..=255).zip(glyphs.iter_mut()).zip(missing.iter_mut()) {
        let Some(name) = names.get_mut(usize::from(code)) else {
            continue;
        };
        let glyph = match name.as_deref() {
            Some(name) => by_name.get(name).copied(),
            None => builtin
                .as_ref()
                .and_then(|encoding| encoding.map(code))
                .and_then(|gid| u16::try_from(gid.to_u32()).ok()),
        }
        .filter(|gid| *gid != 0);
        *slot = glyph;
        *lost = glyph.is_none() && name.as_deref().is_some_and(|name| name != b".notdef");
        if name.is_none()
            && let Some(charset_name) = glyph.and_then(|gid| by_glyph.get(&gid))
        {
            *name = Some(charset_name.to_vec());
        }
    }
    Ok((glyphs, missing))
}

/// Glyph name of each code: `/BaseEncoding` (or `default`) with
/// `/Differences` applied. Codes WinAnsi or MacRoman leave undefined take
/// their Standard name (ISO 32000-2, 9.6.5.4).
pub(crate) fn encoding_names<S: Source>(
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
pub(crate) fn simple_widths<S: Source>(
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
    let Some((codes, vertical)) = cid_encoding(file, font, budget)? else {
        return Ok(None);
    };
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
    // A CID-keyed Type 1 program (`/FontFile` under a CIDFont) is not
    // read: refused before reading it.
    if descriptor.get(b"FontFile2").is_none() && descriptor.get(b"FontFile").is_some() {
        return Ok(None);
    }
    let Some((program, format, units_per_em, _)) = program(file, &descriptor, budget)? else {
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
        // Refused above.
        (Format::Type1, _) => return Ok(None),
    };
    let widths = cid_widths(file, &descendant, budget)?;
    let vertical = if vertical {
        Some(vertical_metrics(file, &descendant, budget)?)
    } else {
        None
    };
    let collection = collection_of(file, font, &descendant, budget)?;
    Ok(Some(Font {
        program,
        format,
        type1: None,
        units_per_em,
        mapping: Mapping::Cid {
            codes,
            cid_to_gid,
            widths,
            vertical,
        },
        substituted: false,
        text: TextMap::new(font, None).with_collection(collection),
    }))
}

/// The encoding of a Type 0 font (9.7.5) and whether it writes vertically:
/// `Identity-H`, `Identity-V`, one of Adobe's predefined CMaps this build
/// embeds (`predefined`, tranche 3.7b), or an embedded CMap stream
/// (`/WMode 1` in its data or dictionary for vertical). `Ok(None)` for
/// another predefined CMap, a damaged CMap or a `usecmap` chain that cannot
/// be followed; limits and cancellation are errors.
fn cid_encoding<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<(CidCodes, bool)>, Error> {
    match font.get(b"Encoding") {
        Some(Object::Name(name)) => Ok(match name.as_slice() {
            b"Identity-H" => Some((CidCodes::Identity, false)),
            b"Identity-V" => Some((CidCodes::Identity, true)),
            other => predefined::encoding(other).map(|cmap| {
                let vertical = cmap.is_vertical();
                (CidCodes::CMap(cmap), vertical)
            }),
        }),
        Some(object @ Object::Reference(_)) => {
            let Some((cmap, vertical)) = embedded_cmap(file, object, 0, budget)? else {
                return Ok(None);
            };
            Ok(Some((CidCodes::CMap(Arc::new(cmap)), vertical)))
        }
        _ => Ok(None),
    }
}

/// The embedded CMap `object` refers to, with its parents merged under it:
/// another embedded CMap (`/UseCMap` reference), or by name (`usecmap` or
/// `/UseCMap`) `Identity-H`/`-V` or a predefined CMap this build embeds.
/// `depth` counts the CMaps above it.
fn embedded_cmap<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    depth: usize,
    budget: &mut Budget,
) -> Result<Option<(CMap, bool)>, Error> {
    if depth >= MAX_USECMAP_DEPTH {
        return Ok(None);
    }
    let Some(dictionary) = resolve_dictionary(file, object, budget)? else {
        return Ok(None);
    };
    let Some(data) = stream_bytes(file, object, budget)? else {
        return Ok(None);
    };
    let mut cmap = match CMap::parse(&data, budget) {
        Ok(cmap) => cmap,
        Err(CMapError::Syntax(
            pdf_syntax::Error::Malformed { .. }
            | pdf_syntax::Error::Truncated { .. }
            | pdf_syntax::Error::Unsupported(_),
        ))
        | Err(CMapError::Unterminated { .. }) => return Ok(None),
        Err(CMapError::Syntax(other)) => return Err(Error::Syntax(other)),
    };
    let mut vertical =
        cmap.is_vertical() || dictionary.get(b"WMode").and_then(Object::as_integer) == Some(1);
    let parent = match (dictionary.get(b"UseCMap"), cmap.parent()) {
        (Some(object @ Object::Reference(_)), _) => {
            let Some((parent, parent_vertical)) = embedded_cmap(file, object, depth + 1, budget)?
            else {
                return Ok(None);
            };
            vertical |= parent_vertical;
            Some(Arc::new(parent))
        }
        (Some(Object::Name(name)), _) => match named(name, budget)? {
            Some((parent, parent_vertical)) => {
                vertical |= parent_vertical;
                Some(parent)
            }
            None => return Ok(None),
        },
        (Some(_), _) => return Ok(None),
        (None, Some(name)) => {
            let name = name.to_vec();
            match named(&name, budget)? {
                Some((parent, parent_vertical)) => {
                    vertical |= parent_vertical;
                    Some(parent)
                }
                None => return Ok(None),
            }
        }
        (None, None) => None,
    };
    if let Some(parent) = parent {
        match cmap.inherit(&parent, budget) {
            Ok(()) => {}
            Err(CMapError::Syntax(other)) => return Err(Error::Syntax(other)),
            Err(CMapError::Unterminated { .. }) => return Ok(None),
        }
    }
    Ok(Some((cmap, vertical)))
}

/// A CMap named as a `usecmap` parent: `Identity-H`, `Identity-V` or a
/// predefined CMap this build embeds; `None` for another.
fn named(name: &[u8], budget: &mut Budget) -> Result<Option<(Arc<CMap>, bool)>, Error> {
    let vertical = match name {
        b"Identity-H" => false,
        b"Identity-V" => true,
        other => {
            return Ok(predefined::encoding(other).map(|cmap| {
                let vertical = cmap.is_vertical();
                (cmap, vertical)
            }));
        }
    };
    let data: &[u8] =
        b"begincodespacerange <0000> <FFFF> endcodespacerange begincidrange <0000> <FFFF> 0 endcidrange";
    match CMap::parse(data, budget) {
        Ok(cmap) => Ok(Some((Arc::new(cmap), vertical))),
        Err(CMapError::Syntax(other)) => Err(Error::Syntax(other)),
        Err(CMapError::Unterminated { .. }) => Ok(None),
    }
}

/// The UCS2 CMap of the CID collection of a Type 0 font, for the text of
/// its CIDs (ISO 32000-2, 9.10.2): the collection of its predefined
/// encoding, else the `/CIDSystemInfo` of its descendant when it is
/// Adobe-Japan1, -GB1, -CNS1 or -Korea1. `None` otherwise, or when this
/// build does not embed them.
fn collection_of<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    descendant: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Arc<CMap>>, Error> {
    if let Some(Object::Name(name)) = font.get(b"Encoding")
        && let Some(ordering) = predefined::encoding_collection(name)
    {
        return Ok(predefined::collection(ordering));
    }
    let Some(info) = descendant.get(b"CIDSystemInfo") else {
        return Ok(None);
    };
    let Some(info) = resolve_dictionary(file, info, budget)? else {
        return Ok(None);
    };
    // Strings, maybe indirect; a name is tolerated, as some producers
    // write one.
    let mut text = |key: &[u8]| -> Result<Option<Vec<u8>>, Error> {
        Ok(match info.get(key) {
            Some(object) => match resolve(file, object, budget)? {
                Some(Object::String(bytes) | Object::Name(bytes)) => Some(bytes),
                _ => None,
            },
            None => None,
        })
    };
    if text(b"Registry")?.as_deref() != Some(b"Adobe".as_slice()) {
        return Ok(None);
    }
    Ok(text(b"Ordering")?.and_then(|ordering| predefined::collection(&ordering)))
}

/// `/DW2` (`[880 -1000]` by default) and `/W2` (9.7.4.3): `c [w1y vx vy …]`
/// or `cfirst clast w1y vx vy`. Malformed entries end the array; ranges are
/// sorted and later overlapping ones dropped.
fn vertical_metrics<S: Source>(
    file: &PdfFile<S>,
    descendant: &Dictionary,
    budget: &mut Budget,
) -> Result<VerticalMetrics, Error> {
    let default = match descendant.get(b"DW2") {
        Some(object) => match resolve(file, object, budget)? {
            Some(Object::Array(items)) => match items.as_slice() {
                [y, displacement] => number(y).zip(number(displacement)),
                _ => None,
            },
            _ => None,
        },
        None => None,
    }
    .unwrap_or((880.0, -1000.0));
    let listed = match descendant.get(b"W2") {
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
                Some(Object::Array(values)) => {
                    let values: Vec<f64> = values.iter().map_while(number).collect();
                    let metrics: Vec<[f64; 3]> = values.as_chunks::<3>().0.to_vec();
                    let Some(count) = u32::try_from(metrics.len()).ok().filter(|c| *c > 0) else {
                        continue;
                    };
                    let last = first.saturating_add(count - 1);
                    runs.entry(first).or_insert((last, metrics));
                }
                Some(last) => {
                    let (Some(last), Some(displacement), Some(x), Some(y)) = (
                        number(last).and_then(|l| u32::try_from(l as i64).ok()),
                        items.next().and_then(number),
                        items.next().and_then(number),
                        items.next().and_then(number),
                    ) else {
                        break;
                    };
                    if last >= first {
                        runs.entry(first)
                            .or_insert((last, vec![[displacement, x, y]]));
                    }
                }
                None => break,
            }
        }
    }
    let mut sorted: Vec<(u32, u32, Vec<[f64; 3]>)> = Vec::with_capacity(runs.len());
    for (first, (last, metrics)) in runs {
        if sorted
            .last()
            .is_none_or(|(_, previous, _)| *previous < first)
        {
            sorted.push((first, last, metrics));
        }
    }
    Ok(VerticalMetrics {
        default,
        runs: sorted,
    })
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
    fn type1_programs_are_bounded_before_reading() {
        // Encoding names (slashes before eexec) times glyphs (eight bytes
        // each after it), at the bound and one past it.
        let program = |names: usize, after: usize| {
            let mut bytes = "/".repeat(names).into_bytes();
            bytes.extend_from_slice(b"eexec");
            bytes.resize(names + 5 + after, b' ');
            bytes
        };
        let glyphs = usize::try_from(MAX_TYPE1_LOOKUPS / 1000).unwrap();
        assert!(type1_cost(&program(1000, glyphs * 8 - 5)).is_some());
        assert!(type1_cost(&program(1000, glyphs * 8 + 3)).is_none());
        // The memory charged: the bytes, and an index entry per glyph.
        assert_eq!(
            type1_cost(&program(1, 80)),
            Some(86 + 10 * TYPE1_ENTRY_BYTES)
        );
        assert!(type1_cost(b"no encrypted part").is_none());
    }

    #[test]
    fn symbolic_sets_are_recognised_by_name() {
        assert_eq!(symbol_set(b"Symbol"), Some(SymbolSet::Symbol));
        assert_eq!(symbol_set(b"SymbolMT"), Some(SymbolSet::Symbol));
        assert_eq!(symbol_set(b"Symbol,Bold"), Some(SymbolSet::Symbol));
        assert_eq!(symbol_set(b"ZapfDingbats"), Some(SymbolSet::Dingbats));
        assert_eq!(symbol_set(b"ZapfDingbatsITC"), Some(SymbolSet::Dingbats));
        assert_eq!(symbol_set(b"Dingbats"), Some(SymbolSet::Dingbats));
        assert_eq!(symbol_set(b"Wingdings"), None);
        assert_eq!(symbol_set(b"Helvetica"), None);
    }

    /// Every glyph of the built-in encodings of Symbol and ZapfDingbats has
    /// Adobe's width and a glyph in the replacement font.
    #[cfg(feature = "standard-fonts")]
    #[test]
    fn the_symbol_replacement_covers_both_encodings() {
        let program = standard::symbols().expect("embedded");
        let font = FontRef::new(program).expect("font");
        let charmap = font.charmap();
        for (set, encoding, widths) in [
            (SymbolSet::Symbol, &SYMBOL, &SYMBOL_WIDTHS[..]),
            (
                SymbolSet::Dingbats,
                &ZAPF_DINGBATS,
                &ZAPF_DINGBATS_WIDTHS[..],
            ),
        ] {
            let mut glyphs = 0;
            for name in encoding.iter().filter(|name| !name.is_empty()) {
                let bytes = name.as_bytes();
                assert!(
                    sorted_lookup(widths, bytes).is_some(),
                    "{set:?} {name}: width"
                );
                let code = match set {
                    SymbolSet::Dingbats => sorted_lookup(&ZAPF_DINGBATS_LIST, bytes),
                    SymbolSet::Symbol => None,
                }
                .or_else(|| unicode(bytes))
                .and_then(char::from_u32)
                .expect("code point");
                let glyph = charmap.map(code).expect("glyph");
                assert_ne!(glyph.to_u32(), 0, "{set:?} {name}");
                glyphs += 1;
            }
            assert!(glyphs > 180, "{set:?}: {glyphs}");
        }
    }

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
