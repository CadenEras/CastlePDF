#!/usr/bin/env node
// Generates the synthetic public fixtures deterministically.
// `node scripts/make-fixtures.mjs` writes them; `--check` only verifies that
// the committed files are byte-identical to what this script produces.
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = path.join(root, 'tests', 'corpus', 'public');
const damagedDir = path.join(root, 'tests', 'corpus', 'damaged');

// Builds a classic-xref PDF from object bodies (object n = bodies[n - 1]).
// Offsets are computed on the exact bytes, so the xref table is valid.
export function buildPdf(bodies, version = '1.4') {
  const chunks = [Buffer.from(`%PDF-${version}\n%\xE2\xE3\xCF\xD3\n`, 'latin1')];
  let length = chunks[0].length;
  const offsets = [];
  bodies.forEach((body, index) => {
    offsets.push(length);
    const chunk = Buffer.from(`${index + 1} 0 obj\n${body}\nendobj\n`, 'latin1');
    chunks.push(chunk);
    length += chunk.length;
  });
  const xrefOffset = length;
  const entries = offsets.map(o => `${String(o).padStart(10, '0')} 00000 n \n`).join('');
  chunks.push(Buffer.from(
    `xref\n0 ${bodies.length + 1}\n0000000000 65535 f \n${entries}`
    + `trailer\n<< /Size ${bodies.length + 1} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF\n`,
    'latin1'));
  return Buffer.concat(chunks);
}

// Appends an incremental update (ISO 32000-2, 7.5.6): the given objects
// (number -> body), one xref subsection per object, and a trailer whose /Prev
// points at the previous section.
export function appendUpdate(base, objects, size) {
  const prev = Number(/startxref\n(\d+)\n%%EOF\n$/.exec(base.toString('latin1'))[1]);
  const chunks = [base];
  let length = base.length;
  let entries = '';
  for (const [number, body] of Object.entries(objects)) {
    const chunk = Buffer.from(`${number} 0 obj\n${body}\nendobj\n`, 'latin1');
    entries += `${number} 1\n${String(length).padStart(10, '0')} 00000 n \n`;
    chunks.push(chunk);
    length += chunk.length;
  }
  chunks.push(Buffer.from(
    `xref\n${entries}trailer\n<< /Size ${size} /Root 1 0 R /Prev ${prev} >>\n`
    + `startxref\n${length}\n%%EOF\n`, 'latin1'));
  return Buffer.concat(chunks);
}

function stream(content, extra = '') {
  return `<< /Length ${Buffer.byteLength(content, 'latin1')}${extra} >>\nstream\n${content}\nendstream`;
}

// Deterministic zlib (RFC 1950/1951) with one fixed-Huffman block of literals
// only: real Huffman-coded Deflate whose bytes never depend on the Node or
// zlib version that runs this script. Returns a latin1 string.
export function zlibFixedHuffman(text) {
  const input = Buffer.from(text, 'latin1');
  const out = [0x78, 0x01];
  let bitBuffer = 0;
  let bitCount = 0;
  const writeBits = (value, count) => {
    for (let i = 0; i < count; i += 1) {
      bitBuffer |= ((value >> i) & 1) << bitCount;
      bitCount += 1;
      if (bitCount === 8) {
        out.push(bitBuffer);
        bitBuffer = 0;
        bitCount = 0;
      }
    }
  };
  // Huffman codes are packed most significant bit first.
  const writeCode = (code, length) => {
    for (let i = length - 1; i >= 0; i -= 1) writeBits((code >> i) & 1, 1);
  };
  const literal = symbol => {
    if (symbol < 144) writeCode(0x30 + symbol, 8);
    else if (symbol < 256) writeCode(0x190 + symbol - 144, 9);
    else writeCode(symbol - 256, 7);
  };
  writeBits(1, 1); // BFINAL
  writeBits(1, 2); // BTYPE = 01, fixed Huffman codes
  for (const byte of input) literal(byte);
  literal(256); // end of block
  if (bitCount > 0) out.push(bitBuffer);
  let a = 1;
  let b = 0;
  for (const byte of input) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  const adler = ((b << 16) | a) >>> 0;
  out.push((adler >>> 24) & 0xff, (adler >>> 16) & 0xff, (adler >>> 8) & 0xff, adler & 0xff);
  return Buffer.from(out).toString('latin1');
}

// PNG "Up" prediction (filter type 2) of rows of `columns` bytes.
function pngUp(bytes, columns) {
  const out = [];
  for (let row = 0; row * columns < bytes.length; row += 1) {
    out.push(2);
    for (let i = 0; i < columns; i += 1) {
      const above = row === 0 ? 0 : bytes[(row - 1) * columns + i];
      out.push((bytes[row * columns + i] - above) & 0xff);
    }
  }
  return Buffer.from(out).toString('latin1');
}

// Builds a PDF 1.5 file with a cross-reference stream (ISO 32000-2, 7.5.8).
// `objects` maps every object number to `{ body }` (written directly) or
// `{ compressed: true, body }` (placed in the object stream `objstm`). The
// object stream and the cross-reference stream are Flate-encoded; the
// cross-reference data also uses the PNG Up predictor, as producers often do.
// With `hybrid`, a classic table lists the direct objects and a /XRefStm
// stream lists only the compressed ones (7.5.8.4).
export function buildXrefStreamPdf(objects, objstm, { hybrid = false } = {}) {
  const chunks = [Buffer.from('%PDF-1.5\n%\xE2\xE3\xCF\xD3\n', 'latin1')];
  let length = chunks[0].length;
  const entries = new Map([[0, [0, 0, 65535]]]);
  const write = (number, body) => {
    entries.set(number, [1, length, 0]);
    const chunk = Buffer.from(`${number} 0 obj\n${body}\nendobj\n`, 'latin1');
    chunks.push(chunk);
    length += chunk.length;
  };
  const numbers = Object.keys(objects).map(Number).sort((a, b) => a - b);
  let header = '';
  let body = '';
  let index = 0;
  for (const number of numbers) {
    const object = objects[number];
    if (object.compressed) {
      header += `${number} ${body.length} `;
      body += `${object.body}\n`;
      entries.set(number, [2, objstm, index]);
      index += 1;
    } else {
      write(number, object.body);
    }
  }
  write(objstm, stream(zlibFixedHuffman(header + body),
    ` /Type /ObjStm /N ${index} /First ${header.length} /Filter /FlateDecode`));
  const xrefNumber = Math.max(...entries.keys()) + 1;
  const size = xrefNumber + 1;
  const xrefOffset = length;
  entries.set(xrefNumber, [1, xrefOffset, 0]);
  // W [1 3 1]: type, then 3-byte field 2, then 1-byte field 3.
  const listed = hybrid
    ? [...entries.keys()].filter(n => entries.get(n)[0] === 2).sort((a, b) => a - b)
    : Array.from({ length: size }, (_, n) => n);
  const records = [];
  for (const number of listed) {
    const [type, field2, field3] = entries.get(number) ?? [0, 0, 0];
    records.push(type, (field2 >> 16) & 0xff, (field2 >> 8) & 0xff, field2 & 0xff, field3 & 0xff);
  }
  const index2 = hybrid ? ` /Index [${listed.map(n => `${n} 1`).join(' ')}]` : '';
  const trailerKeys = hybrid ? '' : ' /Root 1 0 R';
  write(xrefNumber, stream(zlibFixedHuffman(pngUp(Buffer.from(records), 5)),
    ` /Type /XRef /Size ${size} /W [1 3 1]${index2}${trailerKeys}`
    + ' /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>'));
  if (!hybrid) {
    chunks.push(Buffer.from(`startxref\n${xrefOffset}\n%%EOF\n`, 'latin1'));
    return Buffer.concat(chunks);
  }
  // Classic table: every uncompressed object, in contiguous runs; compressed
  // objects are absent. (Listing them as free is also common, but qpdf 12
  // then lets the free entry hide the stream entry while qpdf 10 and
  // CastlePDF do not; the unit tests cover that shape, the oracle this one.)
  const direct = [...entries.keys()].filter(n => entries.get(n)[0] !== 2).sort((a, b) => a - b);
  let table = 'xref\n';
  for (let i = 0; i < direct.length;) {
    let j = i;
    while (j + 1 < direct.length && direct[j + 1] === direct[j] + 1) j += 1;
    table += `${direct[i]} ${j - i + 1}\n`;
    for (let k = i; k <= j; k += 1) {
      const [type, field2, field3] = entries.get(direct[k]);
      const kind = type === 0 ? 'f' : 'n';
      table += `${String(field2).padStart(10, '0')} ${String(field3).padStart(5, '0')} ${kind} \n`;
    }
    i = j + 1;
  }
  const tableOffset = length;
  chunks.push(Buffer.from(`${table}trailer\n<< /Size ${size} /Root 1 0 R /XRefStm ${xrefOffset} >>\n`
    + `startxref\n${tableOffset}\n%%EOF\n`, 'latin1'));
  return Buffer.concat(chunks);
}

// The one-page document of `onePage`, with catalog, pages and font marked for
// an object stream; the page and its content stream stay uncompressed.
function compressedPage(text) {
  const [catalog, pages, page, content, font] = onePage(text);
  return {
    1: { compressed: true, body: catalog },
    2: { compressed: true, body: pages },
    3: { body: page },
    4: { body: content },
    5: { compressed: true, body: font },
  };
}

function onePage(text) {
  return [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] '
      + '/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>',
    stream(`BT /F1 24 Tf 72 770 Td (${text}) Tj ET`),
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
}

// Subset of Arimo Regular (SIL OFL 1.1, tests/fonts), built by
// scripts/subset-test-font.sh, with the glyph id and advance of each kept
// character.
const arimo = readFileSync(path.join(root, 'tests', 'fonts', 'arimo-subset.ttf'));
const arimoGlyphs = JSON.parse(readFileSync(path.join(root, 'tests', 'fonts', 'arimo-subset.json'), 'utf8'));
const arimoWidth = ch => Math.round(arimoGlyphs.glyphs[ch][1] * 1000 / arimoGlyphs.unitsPerEm);

// One page in Arimo embedded as TrueType (FontFile2): a simple font with
// WinAnsiEncoding and /Widths (F1), and a Type 0 font with Identity-H and a
// CIDFontType2 descendant whose CIDs are glyph ids (F2).
function trueTypePage() {
  const first = 32;
  const last = 122;
  const widths = [];
  for (let code = first; code <= last; code += 1) {
    const ch = String.fromCharCode(code);
    widths.push(arimoGlyphs.glyphs[ch] ? arimoWidth(ch) : 0);
  }
  const cidLine = 'Identity H 2026';
  const gids = [...cidLine].map(ch => arimoGlyphs.glyphs[ch][0]);
  const hex = gids.map(g => g.toString(16).padStart(4, '0')).join('');
  const w = [...new Set(gids)].sort((a, b) => a - b)
    .map(g => {
      const ch = Object.keys(arimoGlyphs.glyphs).find(c => arimoGlyphs.glyphs[c][0] === g);
      return `${g} [${arimoWidth(ch)}]`;
    }).join(' ');
  const content = [
    'BT /F1 24 Tf 10 120 Td (CastlePDF TrueType) Tj ET',
    'BT /F1 18 Tf 2 Tc 10 Tw 80 Tz 10 85 Td [(Spacing) -500 (and scale)] TJ ET',
    `BT /F2 24 Tf 10 50 Td <${hex}> Tj ET`,
    '0 0 1 rg BT /F1 20 Tf 1 Tr 1 w 10 15 Td (Stroked) Tj ET',
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 150] '
      + '/Resources << /Font << /F1 5 0 R /F2 8 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    `<< /Type /Font /Subtype /TrueType /BaseFont /CSTLAA+Arimo /FirstChar ${first} `
      + `/LastChar ${last} /Widths [${widths.join(' ')}] /Encoding /WinAnsiEncoding `
      + '/FontDescriptor 6 0 R >>',
    '<< /Type /FontDescriptor /FontName /CSTLAA+Arimo /Flags 32 /FontBBox [-544 -303 2700 1060] '
      + '/ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 /FontFile2 7 0 R >>',
    stream(arimo.toString('latin1'), ` /Length1 ${arimo.length}`),
    '<< /Type /Font /Subtype /Type0 /BaseFont /CSTLAA+Arimo /Encoding /Identity-H '
      + '/DescendantFonts [9 0 R] >>',
    '<< /Type /Font /Subtype /CIDFontType2 /BaseFont /CSTLAA+Arimo '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> '
      + `/FontDescriptor 6 0 R /CIDToGIDMap /Identity /DW 1000 /W [${w}] >>`,
  ], '1.4');
}

// CFF test fonts (SIL OFL 1.1, tests/fonts), built by
// scripts/subset-cff-test-fonts.sh: Source Sans 3 (name-keyed CFF, also as
// an OpenType file) and Noto Sans CJK SC (CID-keyed CFF).
const fontFile = name => readFileSync(path.join(root, 'tests', 'fonts', name));
const sans = JSON.parse(fontFile('source-sans-subset.json'));
const cjk = JSON.parse(fontFile('noto-cjk-subset.json'));

// One page in CFF fonts: /Type1C with WinAnsi (F1) and with its built-in
// encoding (F2), a CID-keyed /CIDFontType0C under Type 0 Identity-H whose
// codes are CIDs (F3), and an OpenType file with CFF outlines (F4, /Type1 as
// ISO 32000-2, 9.9 requires for CFF outlines).
function cffPage() {
  const first = 32;
  const last = 122;
  const widths = [];
  for (let code = first; code <= last; code += 1) {
    const glyph = sans.glyphs[String.fromCharCode(code)];
    widths.push(glyph ? Math.round(glyph[1] * 1000 / sans.unitsPerEm) : 0);
  }
  const cidLine = 'CID 中文字体 2026';
  const cids = [...cidLine].map(ch => cjk.glyphs[ch][1]);
  const hex = cids.map(c => c.toString(16).padStart(4, '0')).join('');
  const w = [...new Set(cids)].sort((a, b) => a - b).map(c => {
    const ch = Object.keys(cjk.glyphs).find(k => cjk.glyphs[k][1] === c);
    return `${c} [${Math.round(cjk.glyphs[ch][2] * 1000 / cjk.unitsPerEm)}]`;
  }).join(' ');
  const content = [
    'BT /F1 22 Tf 10 125 Td (CastlePDF CFF Type1C) Tj ET',
    'BT /F2 22 Tf 10 95 Td (Builtin encoding) Tj ET',
    `BT /F3 22 Tf 10 60 Td <${hex}> Tj ET`,
    'BT /F4 22 Tf 10 25 Td (OpenType CFF) Tj ET',
  ].join('\n');
  const simple = (descriptor, extra) =>
    `<< /Type /Font /Subtype /Type1 /BaseFont /CSTLBB+CastleTestSans-Regular /FirstChar ${first} `
    + `/LastChar ${last} /Widths [${widths.join(' ')}]${extra} /FontDescriptor ${descriptor} 0 R >>`;
  const descriptor = (name, flags, file, number) =>
    `<< /Type /FontDescriptor /FontName /${name} /Flags ${flags} /FontBBox [-500 -300 1300 1000] `
    + `/ItalicAngle 0 /Ascent 900 /Descent -250 /CapHeight 660 /StemV 80 /${file} ${number} 0 R >>`;
  const program = (bytes, subtype) => stream(bytes.toString('latin1'), ` /Subtype /${subtype}`);
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 150] '
      + '/Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 9 0 R /F4 13 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    simple(7, ' /Encoding /WinAnsiEncoding'),
    simple(7, ''),
    descriptor('CSTLBB+CastleTestSans-Regular', 32, 'FontFile3', 8),
    program(fontFile('source-sans-subset.cff'), 'Type1C'),
    '<< /Type /Font /Subtype /Type0 /BaseFont /CSTLCC+NotoSansCJKsc-Regular /Encoding /Identity-H '
      + '/DescendantFonts [10 0 R] >>',
    '<< /Type /Font /Subtype /CIDFontType0 /BaseFont /CSTLCC+NotoSansCJKsc-Regular '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> '
      + `/FontDescriptor 11 0 R /DW 1000 /W [${w}] >>`,
    descriptor('CSTLCC+NotoSansCJKsc-Regular', 4, 'FontFile3', 12),
    program(fontFile('noto-cjk-subset.cff'), 'CIDFontType0C'),
    `<< /Type /Font /Subtype /Type1 /BaseFont /CSTLDD+CastleTestSans-Regular /FirstChar ${first} `
      + `/LastChar ${last} /Widths [${widths.join(' ')}] /Encoding /WinAnsiEncoding `
      + '/FontDescriptor 14 0 R >>',
    descriptor('CSTLDD+CastleTestSans-Regular', 32, 'FontFile3', 15),
    program(fontFile('source-sans-subset.otf'), 'OpenType'),
  ], '1.6');
}

// An image XObject: dictionary entries `extra` and raw sample bytes,
// Flate-compressed when `flate`.
function imageStream(width, height, extra, bytes, flate = false) {
  const data = Buffer.from(bytes).toString('latin1');
  const body = flate ? zlibFixedHuffman(data) : data;
  return stream(body, ` /Type /XObject /Subtype /Image /Width ${width} /Height ${height}${extra}`
    + (flate ? ' /Filter /FlateDecode' : ''));
}

// Two pages of image XObjects (ADR-0005, step 5.4a), each 200 x 200 with
// four 80 x 80 images at (10, 110), (110, 110), (10, 10) and (110, 10).
// Page 1: a 4 x 4 RGB image (Flate; rows red, green, blue, gray ramp), an
// 8 x 8 1-bit gray checkerboard (white first), a 4 x 1 Indexed image (2 bits:
// yellow, cyan, magenta, black) and an 8 x 8 stencil mask (a diagonal
// band) painted blue. Page 2, on a red background: a 2 x 1 green image with
// an /SMask (opaque left, transparent right), a 4 x 1 gray image with a
// colour key masking value 0 (black), a 2 x 1 16-bit RGB image (white,
// blue) and a 2 x 1 gray image with /Decode [1 0] (black, white).
function imagesPage() {
  const rgb = [];
  const rows = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
  for (const color of rows) for (let x = 0; x < 4; x += 1) rgb.push(...color);
  for (let x = 0; x < 4; x += 1) rgb.push(x * 85, x * 85, x * 85);
  const checker = [];
  for (let y = 0; y < 8; y += 1) checker.push(y % 2 === 0 ? 0b10101010 : 0b01010101);
  const band = [];
  for (let y = 0; y < 8; y += 1) band.push(~(0b11000000 >> y) & 0xff);
  const draw = (name, x, y) => `q 80 0 0 80 ${x} ${y} cm /${name} Do Q`;
  const page1 = [draw('A', 10, 110), draw('B', 110, 110), draw('C', 10, 10),
    '0 0 1 rg', draw('D', 110, 10)].join('\n');
  const page2 = ['1 0 0 rg 0 0 200 200 re f', draw('E', 10, 110), draw('F', 110, 110),
    draw('G', 10, 10), draw('H', 110, 10)].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /XObject << /A 7 0 R /B 8 0 R /C 9 0 R /D 10 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /XObject << /E 11 0 R /F 13 0 R /G 14 0 R /H 15 0 R >> >> /Contents 6 0 R >>',
    stream(page1),
    stream(page2),
    imageStream(4, 4, ' /ColorSpace /DeviceRGB /BitsPerComponent 8', rgb, true),
    imageStream(8, 8, ' /ColorSpace /DeviceGray /BitsPerComponent 1', checker),
    imageStream(4, 1, ' /ColorSpace [/Indexed /DeviceRGB 3 <FFFF0000FFFFFF00FF000000>] '
      + '/BitsPerComponent 2', [0b00011011]),
    imageStream(8, 8, ' /ImageMask true', band),
    imageStream(2, 1, ' /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 12 0 R',
      [0, 160, 0, 0, 160, 0]),
    imageStream(2, 1, ' /ColorSpace /DeviceGray /BitsPerComponent 8', [255, 0]),
    imageStream(4, 1, ' /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [0 0]',
      [0, 255, 0, 255]),
    imageStream(2, 1, ' /ColorSpace /DeviceRGB /BitsPerComponent 16',
      [255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 255, 255]),
    imageStream(2, 1, ' /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]', [0, 255]),
  ], '1.4');
}

// JPEG (DCTDecode) images: synthetic content encoded by Pillow 11.3.0
// (tests/images, scripts/make-test-jpegs.sh). The Adobe CMYK one stores
// inverted samples, compensated by /Decode as producers do. Page 1: RGB and
// gray; page 2: CMYK, apart because CMYK to RGB differs from pdftoppm by
// design (no colour management, as for paths).
function jpegPage() {
  const jpeg = (name, extra) => stream(
    readFileSync(path.join(root, 'tests', 'images', name)).toString('latin1'),
    ` /Type /XObject /Subtype /Image /Width 32 /Height 16 /BitsPerComponent 8${extra} /Filter /DCTDecode`);
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /XObject << /R 5 0 R /G 6 0 R >> >> /Contents 4 0 R >>',
    stream(['q 160 0 0 80 20 110 cm /R Do Q', 'q 160 0 0 40 20 60 cm /G Do Q'].join('\n')),
    jpeg('rgb.jpg', ' /ColorSpace /DeviceRGB'),
    jpeg('gray.jpg', ' /ColorSpace /DeviceGray'),
    jpeg('cmyk.jpg', ' /ColorSpace /DeviceCMYK /Decode [1 0 1 0 1 0 1 0]'),
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /XObject << /C 7 0 R >> >> /Contents 9 0 R >>',
    stream('q 160 0 0 80 20 110 cm /C Do Q'),
  ], '1.4');
}

// Inline images (ADR-0005, step 5.4b), with the abbreviations of ISO
// 32000-2, 8.9.7: one 200 x 200 page with four 80 x 80 images at (10, 110),
// (110, 110), (10, 10) and (110, 10). A 2 x 2 RGB image (red, green / blue,
// white); a 4 x 1 Indexed image (2 bits: yellow, cyan, magenta, black)
// whose colour space is a resource and whose data is Flate-compressed; an
// 8 x 8 stencil mask (diagonal band) painted blue with /D [1 0]; the gray
// JPEG of tests/images with /L. No data contains a delimited `EI`, so readers
// without /L (PDF 2.0) find the same end.
function inlinePage() {
  const latin = bytes => Buffer.from(bytes).toString('latin1');
  const band = [];
  for (let y = 0; y < 8; y += 1) band.push(0b11000000 >> y);
  const jpeg = readFileSync(path.join(root, 'tests', 'images', 'gray.jpg')).toString('latin1');
  const image = (x, y, entries, data) => `q 80 0 0 80 ${x} ${y} cm BI ${entries} ID ${data} EI Q`;
  const content = [
    image(10, 110, '/W 2 /H 2 /BPC 8 /CS /RGB',
      latin([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])),
    image(110, 110, '/W 4 /H 1 /BPC 2 /CS /Sp /F /Fl', zlibFixedHuffman(latin([0b00011011]))),
    '0 0 1 rg',
    image(10, 10, '/IM true /W 8 /H 8 /D [1 0]', latin(band)),
    image(110, 10, `/W 32 /H 16 /BPC 8 /CS /G /F [/DCT] /L ${Buffer.byteLength(jpeg, 'latin1')}`, jpeg),
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /ColorSpace '
      + '<< /Sp [/Indexed /DeviceRGB 3 <FFFF0000FFFFFF00FF000000>] >> >> /Contents 4 0 R >>',
    stream(content),
  ], '1.4');
}

export const fixtures = {
  // One A4 page, one line of text in a standard 14 font, uncompressed.
  'minimal.pdf': buildPdf(onePage('CastlePDF minimal fixture')),
  // Three pages in a nested tree: MediaBox and Resources inherited from the
  // root, Rotate from an intermediate node, one page with its own MediaBox.
  'page-tree.pdf': buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 /MediaBox [0 0 595 842] '
      + '/Resources << /Font << /F1 7 0 R >> >> >>',
    '<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 /Rotate 90 >>',
    '<< /Type /Page /Parent 3 0 R /Contents 8 0 R >>',
    '<< /Type /Page /Parent 3 0 R /MediaBox [0 0 842 595] /Contents 9 0 R >>',
    '<< /Type /Page /Parent 2 0 R /Contents 10 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    stream('BT /F1 24 Tf 72 770 Td (Page 1) Tj ET'),
    stream('BT /F1 24 Tf 72 520 Td (Page 2) Tj ET'),
    stream('BT /F1 24 Tf 72 770 Td (Page 3) Tj ET'),
  ]),
  // One page whose content stream is Flate-compressed; a /Filter given by an
  // indirect reference; a data stream with a PNG predictor (4 columns).
  'flate.pdf': buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] '
      + '/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>',
    stream(zlibFixedHuffman('BT /F1 24 Tf 72 770 Td (CastlePDF Flate fixture) Tj ET'),
      ' /Filter 6 0 R'),
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    '/FlateDecode',
    stream(zlibFixedHuffman(pngUp(Buffer.from([1, 2, 3, 4, 2, 4, 6, 8, 3, 6, 9, 12]), 4)),
      ' /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>'),
  ]),
  // PDF 1.5 layout: catalog, pages and font compressed in object stream 6;
  // cross-reference stream (Flate + PNG predictor) instead of a table.
  'objstm.pdf': buildXrefStreamPdf(compressedPage('CastlePDF object stream fixture'), 6),
  // The same objects in a hybrid file: classic table plus /XRefStm stream.
  'hybrid.pdf': buildXrefStreamPdf(compressedPage('CastlePDF hybrid fixture'), 6,
    { hybrid: true }),
  // Vector graphics for the renderer, with known probe areas (user space,
  // y up). Page 1, 200 x 200: grey background; red fill (10..90, 10..70);
  // blue 4-unit stroke of (110..190, 10..70) with round joins; green square
  // rotated 45 degrees around (50, 120); yellow fill clipped to a circle of
  // radius 30 around (150, 130); in the top strip (y 172..192): a DeviceCMYK
  // cyan box, a red box at 50 % fill alpha (ExtGState), an even-odd ring and
  // a dashed line [6 4] from x 130 to 190 at y 182. Its content is
  // Flate-compressed and split in two streams in the middle of an operation.
  // Page 2, 200 x 100 rotated 90 degrees: red over x 0..50.
  'shapes.pdf': buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 6 0 R 9 0 R] /Count 3 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /ExtGState << /GS1 8 0 R >> >> /Contents [4 0 R 5 0 R] >>',
    stream(zlibFixedHuffman('0.9 g 0 0 200 200 re f\n1 0 0 rg 10 10 80 60'),
      ' /Filter /FlateDecode'),
    stream(zlibFixedHuffman([
      're f',
      'q 0 0 1 RG 4 w 1 j 110 10 80 60 re S Q',
      'q 1 0 0 1 50 120 cm 0.7071068 0.7071068 -0.7071068 0.7071068 0 0 cm',
      '0 0.6 0 rg -20 -20 40 40 re f Q',
      'q 180 130 m 180 146.57 166.57 160 150 160 c 133.43 160 120 146.57 120 130 c',
      '120 113.43 133.43 100 150 100 c 166.57 100 180 113.43 180 130 c h W n',
      '1 1 0 rg 110 90 80 80 re f Q',
      'q /GS1 gs 1 0 0 rg 50 172 30 20 re f Q',
      '0 g 90 172 30 20 re 98 178 14 8 re f*',
      'q 0 G 2 w [6 4] 0 d 130 182 m 190 182 l S Q',
    ].join('\n')), ' /Filter /FlateDecode'),
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate 90 /Resources << >> '
      + '/Contents 7 0 R >>',
    stream('1 0 0 rg 0 0 50 100 re f'),
    '<< /Type /ExtGState /ca 0.5 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Resources << >> /Contents 10 0 R >>',
    stream('1 0 0 0 k 0 10 10 10 re f 0 1 0 0 k 10 10 10 10 re f '
      + '0 0 1 0 k 0 0 10 10 re f 0 0 0 1 k 10 0 10 10 re f'),
  ]),
  // Embedded TrueType text: simple font (F1), spacing, TJ and stroked text,
  // and a Type 0 Identity-H font (F2); see trueTypePage.
  'truetype.pdf': trueTypePage(),
  // Embedded CFF text: Type1C (WinAnsi and built-in encoding), CID-keyed
  // CIDFontType0C, OpenType with CFF outlines; see cffPage.
  'cff.pdf': cffPage(),
  // Image XObjects: colour spaces, bit depths, masks and /Decode; see
  // imagesPage.
  'images.pdf': imagesPage(),
  // JPEG images: YCbCr, gray and Adobe CMYK; see jpegPage.
  'jpeg.pdf': jpegPage(),
  // Inline images: abbreviations, resource colour space, Flate, stencil,
  // JPEG; see inlinePage.
  'inline-images.pdf': inlinePage(),
  // The same page, then an incremental update replacing its content stream.
  'incremental.pdf': appendUpdate(
    buildPdf(onePage('CastlePDF original revision')),
    { 4: stream('BT /F1 24 Tf 72 770 Td (CastlePDF updated revision) Tj ET') },
    6),
};

// Damage helpers for the reconstruction fixtures (ADR-0004).
const latin1 = buffer => buffer.toString('latin1');

// Cuts the file at its last `startxref`.
function withoutStartxref(buffer) {
  return buffer.subarray(0, latin1(buffer).lastIndexOf('startxref'));
}

// Cuts the file at the first occurrence of `marker`.
function cutAt(buffer, marker) {
  const at = latin1(buffer).indexOf(marker);
  if (at < 0) throw new Error(`${marker} not found`);
  return buffer.subarray(0, at);
}

// Replaces the first occurrence of `from` (same bytes otherwise).
function replaceOnce(buffer, from, to) {
  const text = latin1(buffer);
  if (!text.includes(from)) throw new Error(`${from} not found`);
  return Buffer.from(text.replace(from, to), 'latin1');
}

// Sets the value of the last `startxref`.
function setStartxref(buffer, offset) {
  const text = latin1(buffer);
  const at = text.lastIndexOf('startxref\n');
  return Buffer.from(text.slice(0, at)
    + text.slice(at).replace(/startxref\n\d+/, `startxref\n${offset}`), 'latin1');
}

const startxrefOf = buffer => Number(/startxref\n(\d+)\n%%EOF\n$/.exec(latin1(buffer))[1]);

// Files whose cross-reference data is damaged and that a reader repairs by
// scanning (tests/corpus/damaged, ADR-0004); `manifest.json` there gives
// the expected result of each.
export const damaged = (() => {
  const minimal = fixtures['minimal.pdf'];
  // The header followed by 3 more bytes: every object is 3 bytes past the
  // offset its entry gives, the table too (startxref is corrected).
  const shifted = setStartxref(
    replaceOnce(minimal, '%PDF-1.4\n', '%PDF-1.4\n%x\n'), startxrefOf(minimal) + 3);
  const [catalog, pages, page, , font] = onePage('');
  const decoy = 'BT /F1 24 Tf 72 770 Td (9 0 obj) Tj ET';
  return {
    // No `startxref` and no `%%EOF`: trailer found by the scan.
    'no-startxref.pdf': withoutStartxref(minimal),
    // `startxref` one byte before `xref`.
    'wrong-startxref.pdf': setStartxref(minimal, startxrefOf(minimal) - 1),
    // Entries 3 bytes short: the file opens, objects are found by a scan.
    'shifted-objects.pdf': shifted,
    // Cut inside the font object: no table, no trailer, root = catalog.
    'truncated.pdf': cutAt(minimal, '/BaseFont'),
    // Cut before the table: every object intact, no trailer.
    'no-trailer.pdf': cutAt(minimal, 'xref\n'),
    // incremental.pdf without its last startxref: object 4 is present twice,
    // the later (updated) one wins.
    'duplicate-object.pdf': withoutStartxref(fixtures['incremental.pdf']),
    // Content stream with `9 0 obj` in its data (direct /Length); object 6
    // holds `10 0 obj` with an indirect /Length (skipped up to endstream).
    'header-in-stream.pdf': withoutStartxref(buildPdf([
      catalog, pages, page, stream(decoy), font,
      '<< /Length 7 0 R >>\nstream\n10 0 obj (decoy) endobj\nendstream',
      String(Buffer.byteLength('10 0 obj (decoy) endobj', 'latin1')),
    ])),
    // objstm.pdf with startxref pointing at the header: the object stream
    // and the cross-reference stream dictionary are found by the scan.
    'objstm-unindexed.pdf': setStartxref(fixtures['objstm.pdf'], 0),
    // A trailer with /Encrypt and no startxref: refused, never repaired.
    'encrypted.pdf': withoutStartxref(replaceOnce(
      buildPdf([...onePage('CastlePDF encrypted'), '<< /Filter /Standard /V 1 /R 2 >>']),
      '/Root 1 0 R', '/Root 1 0 R /Encrypt 6 0 R')),
    // The same kind of file cut before its table: the /Encrypt key is lost
    // with the trailer, the encryption dictionary (object 6) still refuses it.
    'encrypted-truncated.pdf': cutAt(
      buildPdf([...onePage('CastlePDF encrypted'),
        '<< /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>']),
      'xref\n'),
  };
})();

export const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const check = process.argv.includes('--check');
  let failed = false;
  mkdirSync(outDir, { recursive: true });
  mkdirSync(damagedDir, { recursive: true });
  const all = [
    ...Object.entries(fixtures).map(([name, bytes]) => [outDir, name, bytes]),
    ...Object.entries(damaged).map(([name, bytes]) => [damagedDir, name, bytes]),
  ];
  for (const [dir, name, bytes] of all) {
    const file = path.join(dir, name);
    if (check) {
      let current = null;
      try { current = readFileSync(file); } catch { /* reported below */ }
      if (!current?.equals(bytes)) {
        console.error(`${name}: differs from generator output`);
        failed = true;
        continue;
      }
    } else {
      writeFileSync(file, bytes);
    }
    console.log(`${name}  ${bytes.length} bytes  sha256 ${sha256(bytes)}`);
  }
  process.exit(failed ? 1 : 0);
}
