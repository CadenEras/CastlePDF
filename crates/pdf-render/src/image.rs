//! Image XObjects (ISO 32000-2, 8.9): decoded to straight RGBA8 for the
//! rasteriser (ADR-0005, step 5.4a).
//!
//! Supported: data through the existing filters, a final `DCTDecode`
//! decoded by `zune-jpeg` (without its SIMD, `unsafe`, paths); colour spaces
//! DeviceGray, DeviceRGB, DeviceCMYK (the conversion of 10.4.2.4, as for
//! paths), ICCBased reduced to its `/N` device space, CalGray and CalRGB as
//! their device spaces, Indexed over those; 1, 2, 4, 8 and 16 bits per
//! component; `/Decode`; stencil masks (`/ImageMask`); `/SMask` and stencil
//! `/Mask` (resampled to the image if their size differs); colour-key
//! `/Mask`; `/Matte` (soft masks of premultiplied images). Anything else is
//! reported, never drawn as if exact: another filter (JPX, JBIG2, CCITT...)
//! as `Image`, another colour space (Lab, Separation, DeviceN) as
//! `ColorSpace`, a mask that cannot be applied as `SoftMask`.
//!
//! Sizes are checked before any allocation: an image whose width × height
//! exceeds `max_image_pixels` is not decoded but reported (`Image`), the
//! rest of the page being drawn; every decoded byte is charged to the
//! budget.

use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, PdfFile, Source, Stream};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace as JpegSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::{Error, RenderLimits, Unsupported};

/// Largest image side: the rasteriser's pixmaps are indexed by `u16`.
pub(crate) const MAX_IMAGE_SIDE: u32 = 65_535;

/// An image ready to draw: straight (not premultiplied) RGBA8, rows top to
/// bottom.
#[derive(Debug)]
pub(crate) struct Decoded {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    /// Some pixel is not opaque.
    pub transparent: bool,
}

/// Colour space of image samples.
#[derive(Debug, Clone, PartialEq)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// Base space and its colours: `hival + 1` entries of the base's
    /// components, 0 to 255 each.
    Indexed {
        base: Box<Space>,
        lookup: Vec<u8>,
    },
}

impl Space {
    fn components(&self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Indexed { .. } => 1,
        }
    }
}

/// What a decoding yields: the image, or why it is not drawn.
pub(crate) type Outcome = Result<Decoded, Unsupported>;

/// Decodes the image XObject `stream`. \`space\` is its `/ColorSpace`,
/// already resolved if it named a resource; `fill` the fill colour (sRGB, 0
/// to 255) painted through a stencil mask. Limits, cancellation and input
/// errors are errors; a damaged or unsupported image is an `Outcome` error.
pub(crate) fn decode<S: Source>(
    file: &PdfFile<S>,
    stream: &Stream,
    space: Option<&Object>,
    fill: [u8; 3],
    limits: &RenderLimits,
    budget: &mut Budget,
) -> Result<Outcome, Error> {
    let dictionary = &stream.dictionary;
    let mask = dictionary.get(b"ImageMask") == Some(&Object::Boolean(true));
    let Some(samples) = samples(file, stream, limits, budget)? else {
        return Ok(Err(Unsupported::Image));
    };
    if mask {
        return stencil(&samples, dictionary, fill, budget).map(Ok);
    }
    let space = match (&samples.jpeg, space) {
        // A JPEG says itself how many components it has.
        (Some(components), _) => match components {
            1 => Space::Gray,
            3 => Space::Rgb,
            4 => Space::Cmyk,
            _ => return Ok(Err(Unsupported::Image)),
        },
        (None, Some(object)) => match color_space(file, object, budget)? {
            Some(space) => space,
            None => return Ok(Err(Unsupported::ColorSpace)),
        },
        (None, None) => return Ok(Err(Unsupported::ColorSpace)),
    };
    let mut image = match colors(&samples, &space, dictionary, budget)? {
        Ok(image) => image,
        Err(reason) => return Ok(Err(reason)),
    };
    // Soft mask first; a stencil or colour-key /Mask otherwise.
    match dictionary.get(b"SMask") {
        Some(object @ Object::Reference(_)) => {
            match soft_mask(file, object, &mut image, &space, limits, budget)? {
                Ok(()) => {}
                Err(reason) => return Ok(Err(reason)),
            }
        }
        _ => match dictionary.get(b"Mask") {
            Some(Object::Array(ranges)) => color_key(&samples, &space, ranges, &mut image),
            Some(object @ Object::Reference(_)) => {
                match stencil_mask(file, object, &mut image, limits, budget)? {
                    Ok(()) => {}
                    Err(reason) => return Ok(Err(reason)),
                }
            }
            _ => {}
        },
    }
    Ok(Ok(image))
}

/// Raw samples of an image and their layout.
struct Samples {
    width: u32,
    height: u32,
    bits: u32,
    data: Vec<u8>,
    /// Components of a JPEG, which decodes to 8-bit samples itself.
    jpeg: Option<usize>,
}

/// `/Width`, `/Height` and `/BitsPerComponent` checked, then the data:
/// filtered, or decoded as a JPEG when the last filter is `DCTDecode`.
/// `None` for a damaged or unsupported image.
fn samples<S: Source>(
    file: &PdfFile<S>,
    stream: &Stream,
    limits: &RenderLimits,
    budget: &mut Budget,
) -> Result<Option<Samples>, Error> {
    let dictionary = &stream.dictionary;
    let dimension = |key: &[u8]| {
        dictionary
            .get(key)
            .and_then(Object::as_integer)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| (1..=MAX_IMAGE_SIDE).contains(v))
    };
    let (Some(width), Some(height)) = (dimension(b"Width"), dimension(b"Height")) else {
        return Ok(None);
    };
    if !within(width, height, limits) {
        return Ok(None);
    }
    let mask = dictionary.get(b"ImageMask") == Some(&Object::Boolean(true));
    let bits = if mask {
        1
    } else {
        match dictionary
            .get(b"BitsPerComponent")
            .and_then(Object::as_integer)
        {
            Some(bits @ (1 | 2 | 4 | 8 | 16)) => bits as u32,
            // JPEG data carries its own precision.
            _ if last_filter(dictionary) == Some(b"DCTDecode".as_slice()) => 8,
            _ => return Ok(None),
        }
    };
    if last_filter(dictionary) == Some(b"DCTDecode".as_slice()) {
        let Some(encoded) = without_last_filter(file, stream, budget)? else {
            return Ok(None);
        };
        return jpeg(&encoded, limits, budget);
    }
    let data = match file.decoded_stream_data(stream, budget) {
        Ok(data) => data,
        Err(pdf_syntax::Error::Unsupported(_)) => return Ok(None),
        Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
            return Ok(None);
        }
        Err(other) => return Err(Error::Syntax(other)),
    };
    Ok(Some(Samples {
        width,
        height,
        bits,
        data,
        jpeg: None,
    }))
}

/// An image of this size may be decoded (`max_image_pixels`).
fn within(width: u32, height: u32, limits: &RenderLimits) -> bool {
    u64::from(width) * u64::from(height) <= limits.max_image_pixels
}

/// The last filter of a stream, if any.
fn last_filter(dictionary: &Dictionary) -> Option<&[u8]> {
    match dictionary.get(b"Filter") {
        Some(Object::Name(name)) => Some(name),
        Some(Object::Array(names)) => names.last().and_then(Object::as_name),
        _ => None,
    }
}

/// The data of `stream` through every filter but the last (`DCTDecode`).
fn without_last_filter<S: Source>(
    file: &PdfFile<S>,
    stream: &Stream,
    budget: &mut Budget,
) -> Result<Option<Vec<u8>>, Error> {
    let mut dictionary = stream.dictionary.clone();
    match dictionary.get(b"Filter").cloned() {
        Some(Object::Array(mut names)) => {
            names.pop();
            dictionary.insert(b"Filter".to_vec(), Object::Array(names));
            if let Some(Object::Array(mut parameters)) = dictionary.get(b"DecodeParms").cloned() {
                parameters.pop();
                dictionary.insert(b"DecodeParms".to_vec(), Object::Array(parameters));
            }
        }
        _ => {
            dictionary.remove(b"Filter");
            dictionary.remove(b"DecodeParms");
        }
    }
    let stream = Stream {
        dictionary,
        data: stream.data.clone(),
    };
    match file.decoded_stream_data(&stream, budget) {
        Ok(data) => Ok(Some(data)),
        Err(
            pdf_syntax::Error::Unsupported(_)
            | pdf_syntax::Error::Malformed { .. }
            | pdf_syntax::Error::Truncated { .. },
        ) => Ok(None),
        Err(other) => Err(Error::Syntax(other)),
    }
}

/// Decodes a JPEG to 8-bit samples: gray, RGB (from YCbCr or RGB) or CMYK
/// (YCCK converted here). Only these output spaces are asked of
/// `zune-jpeg`, which has no conversion for other pairs.
fn jpeg(data: &[u8], limits: &RenderLimits, budget: &mut Budget) -> Result<Option<Samples>, Error> {
    let side = usize::try_from(MAX_IMAGE_SIDE).unwrap_or(usize::MAX);
    let options = DecoderOptions::default()
        .set_max_width(side)
        .set_max_height(side)
        .set_strict_mode(false);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    if decoder.decode_headers().is_err() {
        return Ok(None);
    }
    let (Some(info), Some(input)) = (decoder.info(), decoder.input_colorspace()) else {
        return Ok(None);
    };
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    if width == 0 || height == 0 {
        return Ok(None);
    }
    if !within(width, height, limits) {
        return Ok(None);
    }
    let (output, components) = match input {
        JpegSpace::Luma => (JpegSpace::Luma, 1),
        JpegSpace::YCbCr | JpegSpace::RGB => (JpegSpace::RGB, 3),
        JpegSpace::CMYK => (JpegSpace::CMYK, 4),
        JpegSpace::YCCK => (JpegSpace::YCCK, 4),
        _ => return Ok(None),
    };
    budget.charge_decoded_bytes(u64::from(width) * u64::from(height) * components as u64)?;
    let options = DecoderOptions::default()
        .set_max_width(side)
        .set_max_height(side)
        .set_strict_mode(false)
        .jpeg_set_out_colorspace(output);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let Ok(mut pixels) = decoder.decode() else {
        return Ok(None);
    };
    let expected = width as usize * height as usize * components;
    if pixels.len() != expected {
        return Ok(None);
    }
    if input == JpegSpace::YCCK {
        ycck_to_cmyk(&mut pixels);
    }
    Ok(Some(Samples {
        width,
        height,
        bits: 8,
        data: pixels,
        jpeg: Some(components),
    }))
}

/// YCCK (Adobe transform 2): Y, Cb, Cr to R, G, B (JFIF), then C, M, Y =
/// 255 - R, G, B; K unchanged.
fn ycck_to_cmyk(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [y, cb, cr, k] = pixel.map(f32::from);
        let r = y + 1.402 * (cr - 128.0);
        let g = y - 0.344_136 * (cb - 128.0) - 0.714_136 * (cr - 128.0);
        let b = y + 1.772 * (cb - 128.0);
        *pixel = [
            255 - to_byte(r),
            255 - to_byte(g),
            255 - to_byte(b),
            to_byte(k),
        ];
    }
}

fn to_byte(value: f32) -> u8 {
    // Clamped, so the cast cannot truncate.
    value.round().clamp(0.0, 255.0) as u8
}

/// The colour space of an image, `None` if not supported.
fn color_space<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Space>, Error> {
    let object = resolve(file, object, budget)?;
    Ok(match &object {
        Some(Object::Name(name)) => device(name),
        Some(Object::Array(items)) => match items.as_slice() {
            [Object::Name(family)] => device(family),
            [Object::Name(family), parameters] => match family.as_slice() {
                b"ICCBased" => {
                    let components = match resolve(file, parameters, budget)? {
                        Some(Object::Dictionary(dictionary)) => {
                            dictionary.get(b"N").and_then(Object::as_integer)
                        }
                        _ => None,
                    };
                    match components {
                        Some(1) => Some(Space::Gray),
                        Some(3) => Some(Space::Rgb),
                        Some(4) => Some(Space::Cmyk),
                        _ => None,
                    }
                }
                b"CalGray" => Some(Space::Gray),
                b"CalRGB" => Some(Space::Rgb),
                _ => None,
            },
            [Object::Name(family), base, hival, lookup] if family.as_slice() == b"Indexed" => {
                indexed(file, base, hival, lookup, budget)?
            }
            _ => None,
        },
        _ => None,
    })
}

fn device(name: &[u8]) -> Option<Space> {
    match name {
        b"DeviceGray" | b"G" => Some(Space::Gray),
        b"DeviceRGB" | b"RGB" => Some(Space::Rgb),
        b"DeviceCMYK" | b"CMYK" => Some(Space::Cmyk),
        _ => None,
    }
}

/// `[/Indexed base hival lookup]` (8.6.6.3): a base that is not itself
/// Indexed, `hival` from 0 to 255, and a lookup string or stream with at
/// least `(hival + 1) × components` bytes.
fn indexed<S: Source>(
    file: &PdfFile<S>,
    base: &Object,
    hival: &Object,
    lookup: &Object,
    budget: &mut Budget,
) -> Result<Option<Space>, Error> {
    // An Indexed base is refused, so this recursion is one level deep.
    let base = match color_space(file, base, budget)? {
        Some(Space::Indexed { .. }) | None => return Ok(None),
        Some(base) => base,
    };
    let Some(hival) = hival
        .as_integer()
        .and_then(|h| usize::try_from(h).ok())
        .filter(|h| *h <= 255)
    else {
        return Ok(None);
    };
    let bytes = match lookup {
        Object::String(bytes) => Some(bytes.clone()),
        Object::Reference(id) => match file.resolve(*id, budget) {
            Ok(IndirectValue::Object(Object::String(bytes))) => Some(bytes),
            Ok(IndirectValue::Stream(stream)) => match file.decoded_stream_data(&stream, budget) {
                Ok(bytes) => Some(bytes),
                Err(
                    pdf_syntax::Error::Unsupported(_)
                    | pdf_syntax::Error::Malformed { .. }
                    | pdf_syntax::Error::Truncated { .. },
                ) => None,
                Err(other) => return Err(Error::Syntax(other)),
            },
            Ok(_) => None,
            Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => None,
            Err(other) => return Err(Error::Syntax(other)),
        },
        _ => None,
    };
    let needed = (hival + 1) * base.components();
    Ok(bytes
        .filter(|bytes| bytes.len() >= needed)
        .map(|mut bytes| {
            bytes.truncate(needed);
            Space::Indexed {
                base: Box::new(base),
                lookup: bytes,
            }
        }))
}

/// `object` resolved once; a stream gives its dictionary.
fn resolve<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Object>, Error> {
    let Object::Reference(id) = object else {
        return Ok(Some(object.clone()));
    };
    match file.resolve(*id, budget) {
        Ok(IndirectValue::Object(object)) => Ok(Some(object)),
        Ok(IndirectValue::Stream(stream)) => Ok(Some(Object::Dictionary(stream.dictionary))),
        Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => Ok(None),
        Err(other) => Err(Error::Syntax(other)),
    }
}

/// Reads `components` samples per pixel, `bits` each, rows padded to a
/// byte; missing data reads as 0 (short streams are common and drawn).
struct Reader<'a> {
    data: &'a [u8],
    bits: u32,
    row_bytes: usize,
}

impl<'a> Reader<'a> {
    fn new(samples: &'a Samples, components: usize) -> Self {
        let row_bits = samples.width as usize * components * samples.bits as usize;
        Self {
            data: &samples.data,
            bits: samples.bits,
            row_bytes: row_bits.div_ceil(8),
        }
    }

    /// Sample `index` of row `y`, from 0 to 2^bits - 1.
    fn sample(&self, y: usize, index: usize) -> u32 {
        let start = y * self.row_bytes;
        let byte = |offset: usize| u32::from(self.data.get(start + offset).copied().unwrap_or(0));
        match self.bits {
            16 => (byte(index * 2) << 8) | byte(index * 2 + 1),
            8 => byte(index),
            bits => {
                let bit = index * bits as usize;
                let shift = 8 - bits - (bit % 8) as u32;
                (byte(bit / 8) >> shift) & ((1 << bits) - 1)
            }
        }
    }
}

/// The `/Decode` ranges of `components` components, or the defaults:
/// `[0 1]` each, `[0 2^bits-1]` for Indexed (8.9.5.2).
fn decode_ranges(
    dictionary: &Dictionary,
    components: usize,
    bits: u32,
    indexed: bool,
) -> Vec<(f32, f32)> {
    let default = if indexed {
        (0.0, ((1u32 << bits) - 1) as f32)
    } else {
        (0.0, 1.0)
    };
    let given: Option<Vec<f32>> = match dictionary.get(b"Decode") {
        Some(Object::Array(items)) if items.len() == components * 2 => items
            .iter()
            .map(|item| match item {
                Object::Integer(v) => Some(*v as f32),
                Object::Real(v) => Some(*v as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    };
    match given {
        Some(values) => values
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[a, b]| (*a, *b))
            .collect(),
        None => vec![default; components],
    }
}

/// Converts the samples of `space` to RGBA (opaque).
fn colors(
    samples: &Samples,
    space: &Space,
    dictionary: &Dictionary,
    budget: &mut Budget,
) -> Result<Outcome, Error> {
    let components = space.components();
    let indexed = matches!(space, Space::Indexed { .. });
    let ranges = decode_ranges(dictionary, components, samples.bits, indexed);
    let max = ((1u64 << samples.bits) - 1) as f32;
    let reader = Reader::new(samples, components);
    let (width, height) = (samples.width as usize, samples.height as usize);
    budget.charge_decoded_bytes((width * height * 4) as u64)?;
    let mut rgba = vec![0u8; width * height * 4];
    let mut values = [0.0f32; 4];
    for (y, row) in rgba.chunks_exact_mut(width * 4).enumerate() {
        if y % 256 == 0 {
            budget.check_cancelled()?;
        }
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            for (c, (value, (low, high))) in values.iter_mut().zip(&ranges).enumerate() {
                let sample = reader.sample(y, x * components + c) as f32;
                *value = low + sample * (high - low) / max;
            }
            *pixel = rgb(space, &values);
        }
    }
    Ok(Ok(Decoded {
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
        rgba,
        transparent: false,
    }))
}

/// One pixel of `space` (components already through `/Decode`) as opaque
/// RGBA.
fn rgb(space: &Space, values: &[f32; 4]) -> [u8; 4] {
    let unit = |v: f32| to_byte(v.clamp(0.0, 1.0) * 255.0);
    let [c0, c1, c2, c3] = *values;
    match space {
        Space::Gray => {
            let g = unit(c0);
            [g, g, g, 255]
        }
        Space::Rgb => [unit(c0), unit(c1), unit(c2), 255],
        Space::Cmyk => [
            unit(1.0 - (c0 + c3).min(1.0)),
            unit(1.0 - (c1 + c3).min(1.0)),
            unit(1.0 - (c2 + c3).min(1.0)),
            255,
        ],
        Space::Indexed { base, lookup } => {
            let count = base.components();
            let index = (c0.round().max(0.0) as usize).min(lookup.len() / count - 1);
            let mut entry = [0.0f32; 4];
            for (slot, byte) in entry.iter_mut().zip(
                lookup
                    .get(index * count..(index + 1) * count)
                    .unwrap_or_default(),
            ) {
                *slot = f32::from(*byte) / 255.0;
            }
            rgb(base, &entry)
        }
    }
}

/// A stencil mask (8.9.6.2): with the default `/Decode [0 1]`, sample 0
/// paints the fill colour and 1 leaves the page unchanged.
fn stencil(
    samples: &Samples,
    dictionary: &Dictionary,
    fill: [u8; 3],
    budget: &mut Budget,
) -> Result<Decoded, Error> {
    let inverted = matches!(
        dictionary.get(b"Decode"),
        Some(Object::Array(items)) if matches!(items.first(), Some(Object::Integer(1)) | Some(Object::Real(1.0)))
    );
    let (width, height) = (samples.width as usize, samples.height as usize);
    budget.charge_decoded_bytes((width * height * 4) as u64)?;
    let reader = Reader::new(samples, 1);
    let mut rgba = vec![0u8; width * height * 4];
    for (y, row) in rgba.chunks_exact_mut(width * 4).enumerate() {
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let painted = (reader.sample(y, x) == 0) != inverted;
            *pixel = if painted {
                [fill[0], fill[1], fill[2], 255]
            } else {
                [0, 0, 0, 0]
            };
        }
    }
    Ok(Decoded {
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
        rgba,
        transparent: true,
    })
}

/// Opacity of each pixel of `image` from a mask image, resampled (nearest)
/// when its size differs: `opacity(mask sample)` from 0 to 255.
fn apply_mask(image: &mut Decoded, mask: &Samples, opacity: impl Fn(u32) -> u8) {
    let reader = Reader::new(mask, 1);
    let (width, height) = (usize::from(image.width), usize::from(image.height));
    let (mask_width, mask_height) = (mask.width as usize, mask.height as usize);
    for (y, row) in image.rgba.chunks_exact_mut(width * 4).enumerate() {
        let my = y * mask_height / height;
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let mx = x * mask_width / width;
            pixel[3] = opacity(reader.sample(my, mx));
        }
    }
    image.transparent = true;
}

/// `/SMask` (11.6.5.3): a gray image whose samples are the opacity.
/// With `/Matte`, the image colours were premultiplied by the mask against
/// that colour of the image's space: `c' = m + a (c - m)`; they are restored
/// as `c = m + (c' - m) / a` (in RGB, exact for gray and RGB images).
fn soft_mask<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    image: &mut Decoded,
    space: &Space,
    limits: &RenderLimits,
    budget: &mut Budget,
) -> Result<Result<(), Unsupported>, Error> {
    let Some(stream) = mask_stream(file, object, budget)? else {
        return Ok(Err(Unsupported::SoftMask));
    };
    let matte = match stream.dictionary.get(b"Matte") {
        None => None,
        Some(Object::Array(items)) if items.len() == space.components() => {
            let mut values = [0.0f32; 4];
            for (value, item) in values.iter_mut().zip(items) {
                *value = match item {
                    Object::Integer(v) => *v as f32,
                    Object::Real(v) => *v as f32,
                    _ => return Ok(Err(Unsupported::SoftMask)),
                };
            }
            Some(rgb(space, &values))
        }
        Some(_) => return Ok(Err(Unsupported::SoftMask)),
    };
    let Some(mask) = samples(file, &stream, limits, budget)? else {
        return Ok(Err(Unsupported::SoftMask));
    };
    if mask.jpeg.is_some_and(|components| components != 1) {
        return Ok(Err(Unsupported::SoftMask));
    }
    let max = (1u32 << mask.bits) - 1;
    let ranges = decode_ranges(&stream.dictionary, 1, mask.bits, false);
    let (low, high) = ranges.first().copied().unwrap_or((0.0, 1.0));
    apply_mask(image, &mask, |sample| {
        to_byte((low + sample as f32 * (high - low) / max as f32).clamp(0.0, 1.0) * 255.0)
    });
    if let Some(matte) = matte {
        for pixel in image.rgba.as_chunks_mut::<4>().0 {
            let alpha = f32::from(pixel[3]) / 255.0;
            if alpha > 0.0 {
                for (channel, m) in pixel.iter_mut().zip(matte).take(3) {
                    let m = f32::from(m);
                    *channel = to_byte(m + (f32::from(*channel) - m) / alpha);
                }
            }
        }
    }
    Ok(Ok(()))
}

/// A stencil `/Mask` (8.9.6.3): a 1-bit image mask; sample 1 (after
/// `/Decode`) masks the image out.
fn stencil_mask<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    image: &mut Decoded,
    limits: &RenderLimits,
    budget: &mut Budget,
) -> Result<Result<(), Unsupported>, Error> {
    let Some(stream) = mask_stream(file, object, budget)? else {
        return Ok(Err(Unsupported::SoftMask));
    };
    let Some(mask) = samples(file, &stream, limits, budget)? else {
        return Ok(Err(Unsupported::SoftMask));
    };
    let inverted = matches!(
        stream.dictionary.get(b"Decode"),
        Some(Object::Array(items)) if matches!(items.first(), Some(Object::Integer(1)) | Some(Object::Real(1.0)))
    );
    apply_mask(image, &mask, |sample| {
        if (sample == 0) != inverted { 255 } else { 0 }
    });
    Ok(Ok(()))
}

fn mask_stream<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Stream>, Error> {
    let Object::Reference(id) = object else {
        return Ok(None);
    };
    match file.resolve(*id, budget) {
        Ok(IndirectValue::Stream(stream)) => Ok(Some(stream)),
        Ok(_) | Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
            Ok(None)
        }
        Err(other) => Err(Error::Syntax(other)),
    }
}

/// Colour-key masking (8.9.6.4): a pixel whose every sample lies in its
/// `[min max]` range is not painted.
fn color_key(samples: &Samples, space: &Space, ranges: &[Object], image: &mut Decoded) {
    let components = space.components();
    let bounds: Option<Vec<(u32, u32)>> = ranges
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[low, high]| {
            let low = u32::try_from(low.as_integer()?).ok()?;
            let high = u32::try_from(high.as_integer()?).ok()?;
            Some((low, high))
        })
        .collect();
    let Some(bounds) = bounds.filter(|b| b.len() == components) else {
        return;
    };
    let reader = Reader::new(samples, components);
    let width = usize::from(image.width);
    for (y, row) in image.rgba.chunks_exact_mut(width * 4).enumerate() {
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let keyed = bounds.iter().enumerate().all(|(c, (low, high))| {
                (*low..=*high).contains(&reader.sample(y, x * components + c))
            });
            if keyed {
                pixel[3] = 0;
                image.transparent = true;
            }
        }
    }
}
