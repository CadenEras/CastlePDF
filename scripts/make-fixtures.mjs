#!/usr/bin/env node
// Generates the synthetic public fixtures deterministically.
// `node scripts/make-fixtures.mjs` writes them; `--check` only verifies that
// the committed files are byte-identical to what this script produces.
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { committed as committedCMap } from './make-cmaps.mjs';

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

// A /ToUnicode CMap (ISO 32000-2, 9.10.3) over the codespace ranges
// `codespace` with the mapping blocks `body`.
const toUnicodeCMap = (codespace, body) => [
  '/CIDInit /ProcSet findresource begin', '12 dict begin', 'begincmap',
  '/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def',
  '/CMapName /Adobe-Identity-UCS def', '/CMapType 2 def',
  `1 begincodespacerange\n${codespace}\nendcodespacerange`, body, 'endcmap',
  'CMapName currentdict /CMap defineresource pop', 'end', 'end',
].join('\n');
const hex4 = n => n.toString(16).toUpperCase().padStart(4, '0');

// Text extraction (ADR-0006, tranche 3.3), in Arimo (TrueType) and
// Helvetica. Page 1, every glyph's text known: a simple font whose
// /ToUnicode maps a code to a ligature (F1), a Type 0 Identity-H font with
// a /ToUnicode, one code to a surrogate pair (F2), a /ToUnicode written with
// two-byte codes for a one-byte font (F3), and glyph names from /Differences:
// AGL, uniXXXX and an underscore ligature (F4). Page 2: a Type 0 font
// without /ToUnicode (unknown text, F5), a damaged /ToUnicode (glyph names
// instead, F6) and invisible text (Tr 3).
function textPage() {
  const first = 32;
  const last = 122;
  const widths = [];
  for (let code = first; code <= last; code += 1) {
    const ch = String.fromCharCode(code);
    widths.push(arimoGlyphs.glyphs[ch] ? arimoWidth(ch) : 0);
  }
  const gidOf = ch => arimoGlyphs.glyphs[ch][0];
  const cidHex = text => [...text].map(ch => hex4(gidOf(ch))).join('');
  const cidLine = 'Identity text';
  const unknownLine = 'Unknown';
  const usedGids = [...new Set([...cidLine, ...unknownLine].map(gidOf))].sort((a, b) => a - b);
  const w = usedGids.map(g => {
    const ch = Object.keys(arimoGlyphs.glyphs).find(c => arimoGlyphs.glyphs[c][0] === g);
    return `${g} [${arimoWidth(ch)}]`;
  }).join(' ');
  const cidEntries = [...new Set(cidLine)]
    .map(ch => `<${hex4(gidOf(ch))}> <${hex4(ch.codePointAt(0))}>`);
  cidEntries.push('<00FF> <D835DC00>');
  const page1 = [
    'BT /F1 16 Tf 10 170 Td (ToUnicode $ne) Tj ET',
    `BT /F2 16 Tf 10 140 Td <${cidHex(cidLine)}00FF> Tj ET`,
    'BT /F3 16 Tf 10 110 Td (Other length) Tj ET',
    "BT /F4 16 Tf 10 80 Td (Caf$ % &on ') Tj ET",
  ].join('\n');
  const page2 = [
    `BT /F5 16 Tf 10 170 Td <${cidHex(unknownLine)}> Tj ET`,
    'BT /F6 16 Tf 10 140 Td (Fallback) Tj ET',
    'BT /F1 16 Tf 3 Tr 10 110 Td (Hidden) Tj ET',
  ].join('\n');
  const simple = toUnicode => `<< /Type /Font /Subtype /TrueType /BaseFont /CSTLEE+Arimo /FirstChar ${first} `
    + `/LastChar ${last} /Widths [${widths.join(' ')}] /Encoding /WinAnsiEncoding `
    + `/FontDescriptor 8 0 R /ToUnicode ${toUnicode} 0 R >>`;
  const type0 = extra => '<< /Type /Font /Subtype /Type0 /BaseFont /CSTLEE+Arimo /Encoding /Identity-H '
    + `/DescendantFonts [10 0 R]${extra} >>`;
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] '
      + '/Resources << /Font << /F1 7 0 R /F2 9 0 R /F3 11 0 R /F4 12 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] '
      + '/Resources << /Font << /F1 7 0 R /F5 13 0 R /F6 14 0 R >> >> /Contents 6 0 R >>',
    stream(page1),
    stream(page2),
    simple(16),
    '<< /Type /FontDescriptor /FontName /CSTLEE+Arimo /Flags 32 /FontBBox [-544 -303 2700 1060] '
      + '/ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 /FontFile2 15 0 R >>',
    type0(' /ToUnicode 17 0 R'),
    '<< /Type /Font /Subtype /CIDFontType2 /BaseFont /CSTLEE+Arimo '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> '
      + `/FontDescriptor 8 0 R /CIDToGIDMap /Identity /DW 1000 /W [${w}] >>`,
    simple(18),
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica '
      + '/Encoding << /Type /Encoding /Differences [36 /eacute /Euro /uni00E7 /f_i] >> >>',
    type0(''),
    simple(19),
    stream(arimo.toString('latin1'), ` /Length1 ${arimo.length}`),
    stream(toUnicodeCMap('<00> <FF>',
      '1 beginbfrange\n<20> <7A> <0020>\nendbfrange\n1 beginbfchar\n<24> <00660069>\nendbfchar')),
    stream(toUnicodeCMap('<0000> <FFFF>',
      `${cidEntries.length} beginbfchar\n${cidEntries.join('\n')}\nendbfchar`)),
    stream(toUnicodeCMap('<0000> <FFFF>', '1 beginbfrange\n<0020> <007A> <0020>\nendbfrange')),
    // Damaged: a block never closed.
    stream(toUnicodeCMap('<00> <FF>', '1 beginbfchar\n<46> <0058>')),
  ], '1.4');
}

// An embedded encoding CMap (ISO 32000-2, 9.7.5.3) named `name`, with the
// codespace ranges `codespace` and the mapping blocks `body` (and
// `before`, such as a `usecmap`, ahead of them).
const encodingCMap = (name, codespace, body, before = '') => [
  '/CIDInit /ProcSet findresource begin', '12 dict begin', 'begincmap',
  '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def',
  `/CMapName /${name} def`, '/CMapType 1 def', before,
  `1 begincodespacerange\n${codespace}\nendcodespacerange`, body, 'endcmap',
  'CMapName currentdict /CMap defineresource pop', 'end', 'end',
].join('\n');

// Words, lines and /ActualText (ADR-0006, tranche 3.5a), in Helvetica: an
// /ActualText inline ("fi" for the code of "f") and from a /Properties
// resource ("Reading"), nested (the outer one wins), and without glyphs
// (an image of text, "Logo"); words apart by a TJ gap without a space, a
// small kerning that does not split a word; words placed one by one on a
// line; a line rotated by 90 degrees.
function layoutPage() {
  const content = [
    'BT /F1 14 Tf 20 260 Td /Span << /ActualText (fi) >> BDC (f) Tj EMC (ne print) Tj ET',
    'BT /F1 14 Tf 20 230 Td /Span /P0 BDC (Reeding) Tj EMC ( order) Tj ET',
    'BT /F1 14 Tf 20 200 Td /Span << /ActualText (Outer) >> BDC /Span << /ActualText (Inner) >> BDC (xx) Tj EMC (yy) Tj EMC ET',
    '/Span << /ActualText <FEFF004C006F0067006F> >> BDC q 40 0 0 20 200 195 cm 0.5 g 0 0 1 1 re f Q EMC',
    'BT /F1 14 Tf 20 170 Td [(Gap) -400 (Words) -20 (Kern)] TJ ET',
    'BT /F1 14 Tf 20 140 Td (One) Tj ET BT /F1 14 Tf 60 140 Td (by) Tj ET BT /F1 14 Tf 82 140 Td (one) Tj ET',
    'BT /F1 14 Tf 0 1 -1 0 270 40 Tm (Rotated text) Tj ET',
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> '
      + '/Properties << /P0 6 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>',
    '<< /ActualText (Reading) >>',
  ], '1.5');
}

// Reading order (ADR-0006, tranche 3.5b), in Helvetica 12: a title over
// two columns; two columns of three lines that the content shows
// alternately, a left line then a right one; one line shown in two pieces,
// right then left; a footer over both columns.
function columnsPage() {
  const line = (x, y, text) => `BT /F1 12 Tf ${x} ${y} Td (${text}) Tj ET`;
  const content = [
    'BT /F1 18 Tf 60 360 Td (A title over both columns) Tj ET',
    line(40, 320, 'Left one'), line(220, 320, 'Right one'),
    line(40, 305, 'Left two'), line(220, 305, 'Right two'),
    line(40, 290, 'Left three'), line(220, 290, 'Right three'),
    line(100, 250, 'second half'), line(40, 250, 'First half,'),
    line(40, 40, 'Footer under both columns'),
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Resources << /Font << /F1 5 0 R >> >> '
      + '/Contents 4 0 R >>',
    stream(content),
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>',
  ], '1.4');
}

// Type 0 encodings beyond Identity-H (ADR-0006, tranche 3.4a), in Arimo
// whose CIDs are glyph ids. Page 1: Identity-V, vertical text with the
// default /DW2 (F1); an embedded CMap mixing one-byte codes and a two-byte
// code (F2); an embedded CMap over F2's by /UseCMap, one code overridden
// (F4). Page 2: an embedded CMap over Identity-H by `usecmap`, one CID
// overridden (F3), which pdftoppm draws as notdef boxes without
// poppler-data: page 1 alone is compared with it. Each font has a
// /ToUnicode for the text.
function cmapsPage() {
  const gid = ch => arimoGlyphs.glyphs[ch][0];
  const widthOf = g => {
    const ch = Object.keys(arimoGlyphs.glyphs).find(c => arimoGlyphs.glyphs[c][0] === g);
    return arimoWidth(ch);
  };
  const gidHex = text => [...text].map(ch => hex4(gid(ch))).join('');
  const latin = '0123456789 !,.ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';
  const allGids = [...new Set([...latin].map(gid))].sort((a, b) => a - b);
  const w = allGids.map(g => `${g} [${widthOf(g)}]`).join(' ');
  const byteHex = text => [...text].map(ch => ch.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0')).join('');
  // One-byte codes of the Latin characters to their glyph ids, and <8140> to Z.
  const latinCids = [...latin].map(ch => `<${byteHex(ch)}> ${gid(ch)}`);
  const f2cmap = encodingCMap('Castle-OneByte', '<00> <7F>\n<8140> <8140>',
    `${latinCids.length} begincidchar\n${latinCids.join('\n')}\n<8140> ${gid('Z')}\nendcidchar`);
  const f3cmap = encodingCMap('Castle-UseIdentity', '<0000> <FFFF>',
    `1 begincidchar\n<${hex4(gid(' '))}> ${gid('!')}\nendcidchar`, '/Identity-H usecmap');
  const f4cmap = encodingCMap('Castle-UseParent', '<00> <7F>',
    `1 begincidchar\n<45> ${gid('X')}\nendcidchar`);
  // The /ToUnicode of glyph-id codes; `shown` gives the text of a glyph
  // whose CID is overridden.
  const gidToUnicode = (text, shown = {}) => {
    const entries = [...new Set(text)]
      .map(ch => `<${hex4(gid(ch))}> <${hex4((shown[ch] ?? ch).codePointAt(0))}>`);
    return toUnicodeCMap('<0000> <FFFF>', `${entries.length} beginbfchar\n${entries.join('\n')}\nendbfchar`);
  };
  const vertical = 'Vertical';
  const usecmap = 'Use CMap';
  const content = [
    `BT /F1 20 Tf 270 230 Td <${gidHex(vertical)}> Tj ET`,
    `BT /F2 18 Tf 10 220 Td <${byteHex('Embedded CMap ')}8140> Tj ET`,
    `BT /F4 18 Tf 10 180 Td <${byteHex('Parent Entry')}> Tj ET`,
  ].join('\n');
  const page2 = `BT /F3 18 Tf 10 220 Td <${gidHex(usecmap)}> Tj ET`;
  const type0 = (encoding, toUnicode) => '<< /Type /Font /Subtype /Type0 /BaseFont /CSTLFF+Arimo '
    + `/Encoding ${encoding} /DescendantFonts [8 0 R] /ToUnicode ${toUnicode} 0 R >>`;
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 19 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 260] '
      + '/Resources << /Font << /F1 5 0 R /F2 6 0 R /F4 11 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    type0('/Identity-V', 13),
    type0('12 0 R', 14),
    type0('15 0 R', 16),
    '<< /Type /Font /Subtype /CIDFontType2 /BaseFont /CSTLFF+Arimo '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> '
      + `/FontDescriptor 9 0 R /CIDToGIDMap /Identity /DW 1000 /W [${w}] >>`,
    '<< /Type /FontDescriptor /FontName /CSTLFF+Arimo /Flags 32 /FontBBox [-544 -303 2700 1060] '
      + '/ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 /FontFile2 10 0 R >>',
    stream(arimo.toString('latin1'), ` /Length1 ${arimo.length}`),
    type0('17 0 R', 18),
    stream(f2cmap, ' /Type /CMap /CMapName /Castle-OneByte '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>'),
    stream(gidToUnicode(vertical)),
    stream(toUnicodeCMap('<00> <7F>\n<8140> <8140>',
      '1 beginbfrange\n<20> <7A> <0020>\nendbfrange\n1 beginbfchar\n<8140> <005A>\nendbfchar')),
    stream(f3cmap, ' /Type /CMap /CMapName /Castle-UseIdentity '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>'),
    stream(gidToUnicode(usecmap, { ' ': '!' })),
    stream(f4cmap, ' /Type /CMap /CMapName /Castle-UseParent /UseCMap 12 0 R '
      + '/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>'),
    stream(toUnicodeCMap('<00> <7F>',
      '1 beginbfrange\n<20> <7A> <0020>\nendbfrange\n1 beginbfchar\n<45> <0058>\nendbfchar')),
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 260] '
      + '/Resources << /Font << /F3 7 0 R >> >> /Contents 20 0 R >>',
    stream(page2),
  ], '1.4');
}

// The CID of `code` (hex, its bytes) in Adobe's predefined CMap `name` as
// committed (crates/pdf-render/cmaps), its `usecmap` parent included.
function predefinedCid(name, code) {
  const text = committedCMap(name).toString('latin1');
  const value = parseInt(code, 16);
  for (const [, kind, body] of text.matchAll(/begincid(range|char)([\s\S]*?)endcid\1/g)) {
    const pattern = kind === 'range'
      ? /<([0-9a-fA-F]+)>\s*<([0-9a-fA-F]+)>\s*(\d+)/g
      : /<([0-9a-fA-F]+)>\s*(\d+)/g;
    for (const m of body.matchAll(pattern)) {
      if (m[1].length !== code.length) continue;
      const low = parseInt(m[1], 16);
      const high = kind === 'range' ? parseInt(m[2], 16) : low;
      const cid = Number(kind === 'range' ? m[3] : m[2]);
      if (low <= value && value <= high) return cid + value - low;
    }
  }
  const parent = /\/(\S+)\s+usecmap/.exec(text);
  if (parent) return predefinedCid(parent[1], code);
  throw new Error(`no CID for <${code}> in ${name}`);
}

// Adobe's predefined CMaps and CJK collections (ADR-0006, tranche 3.7b),
// without /ToUnicode: each font is Arimo as a CIDFontType2 whose
// /CIDToGIDMap draws the CIDs of its collection with Latin glyphs (a CJK
// character with a stand-in letter), so that pdftoppm, with poppler-data,
// draws the same. Japan1: UniJIS-UCS2-H (F1), UniJIS-UCS2-V vertical (F2),
// 90ms-RKSJ-H with one-byte Latin and two-byte kana (F3), UniJIS-UTF16-H
// (F4), and Identity-H whose descendant names Adobe-Japan1, its codes being
// CIDs (F5); then UniGB-UCS2-H (F6), UniCNS-UCS2-H (F7), UniKS-UCS2-H (F8).
// The text comes from each collection's UCS2 CMap.
function cjkPage() {
  const stand = { 'あ': 'Q', 'い': 'W', '中': 'Q', '文': 'W', '한': 'Q', '글': 'W' };
  const glyphOf = ch => arimoGlyphs.glyphs[stand[ch] ?? ch];
  const sjis = { 'あ': '82A0', 'い': '82A2' };
  const byte = ch => ch.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0');
  const lines = [
    ['F1', 'UniJIS-UCS2-H', 'Japan1', 'Nihon あい'],
    ['F2', 'UniJIS-UCS2-V', 'Japan1', 'あい'],
    ['F3', '90ms-RKSJ-H', 'Japan1', 'Nihon あい'],
    ['F4', 'UniJIS-UTF16-H', 'Japan1', 'Nihon あい'],
    ['F5', 'Identity-H', 'Japan1', 'Nihon あい'],
    ['F6', 'UniGB-UCS2-H', 'GB1', 'GB 中文'],
    ['F7', 'UniCNS-UCS2-H', 'CNS1', 'CNS 中文'],
    ['F8', 'UniKS-UCS2-H', 'Korea1', 'KS 한글'],
  ].map(([font, encoding, collection, text]) => {
    // The codes shown, and each character's CID.
    const chars = [...text];
    let codes;
    let cids;
    if (encoding === '90ms-RKSJ-H') {
      codes = chars.map(ch => sjis[ch] ?? byte(ch));
      cids = codes.map(code => predefinedCid(encoding, code));
    } else if (encoding === 'Identity-H') {
      cids = chars.map(ch => predefinedCid('UniJIS-UCS2-H', hex4(ch.codePointAt(0))));
      codes = cids.map(hex4);
    } else {
      codes = chars.map(ch => hex4(ch.codePointAt(0)));
      cids = codes.map(code => predefinedCid(encoding, code));
    }
    return { font, encoding, collection, chars, shown: codes.join(''), cids };
  });
  // One CIDFontType2 per collection: CID to glyph, and widths.
  const collections = ['Japan1', 'GB1', 'CNS1', 'Korea1'];
  const descendants = collections.map(collection => {
    const glyphs = new Map();
    for (const line of lines.filter(l => l.collection === collection)) {
      line.chars.forEach((ch, at) => glyphs.set(line.cids[at], glyphOf(ch)));
    }
    const last = Math.max(...glyphs.keys());
    const map = Buffer.alloc(2 * (last + 1));
    for (const [cid, [gid]] of glyphs) map.writeUInt16BE(gid, 2 * cid);
    const w = [...glyphs].sort((a, b) => a[0] - b[0])
      .map(([cid, [, advance]]) => `${cid} [${Math.round(advance * 1000 / arimoGlyphs.unitsPerEm)}]`)
      .join(' ');
    return { collection, map, w };
  });
  let y = 270;
  const content = lines.map(({ font, shown }) => {
    if (font === 'F2') {
      return `BT /${font} 20 Tf 370 270 Td <${shown}> Tj ET`;
    }
    const line = `BT /${font} 18 Tf 10 ${y} Td <${shown}> Tj ET`;
    y -= 34;
    return line;
  }).join('\n');
  // Objects: 1 catalog, 2 pages, 3 page, 4 content, 5 descriptor, 6 Arimo,
  // 7-14 the fonts, 15-18 the descendants, 19-22 their CIDToGIDMaps.
  const fonts = lines.map(({ font }, index) => `/${font} ${7 + index} 0 R`).join(' ');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] '
      + `/Resources << /Font << ${fonts} >> >> /Contents 4 0 R >>`,
    stream(content),
    '<< /Type /FontDescriptor /FontName /CSTLGG+Arimo /Flags 32 /FontBBox [-544 -303 2700 1060] '
      + '/ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 /FontFile2 6 0 R >>',
    stream(arimo.toString('latin1'), ` /Length1 ${arimo.length}`),
    ...lines.map(({ encoding, collection }) => '<< /Type /Font /Subtype /Type0 /BaseFont /CSTLGG+Arimo '
      + `/Encoding /${encoding} /DescendantFonts [${15 + collections.indexOf(collection)} 0 R] >>`),
    ...descendants.map(({ collection, w }, index) => '<< /Type /Font /Subtype /CIDFontType2 '
      + '/BaseFont /CSTLGG+Arimo '
      + `/CIDSystemInfo << /Registry (Adobe) /Ordering (${collection}) /Supplement 0 >> `
      + `/FontDescriptor 5 0 R /CIDToGIDMap ${19 + index} 0 R /DW 1000 /W [${w}] >>`),
    ...descendants.map(({ map }) => stream(map.toString('latin1'))),
  ], '1.4');
}

// A synthetic Type 1 font (Adobe Type 1 Font Format), project-owned: the
// charstring operators used below, encrypted as the format says (charstring
// key 4330, then eexec key 55665, four leading bytes each).
const T1 = { hsbw: [13], rmoveto: [21], rlineto: [5], rrcurveto: [8], closepath: [9],
  callsubr: [10], return: [11], endchar: [14], seac: [12, 6] };
function t1Charstring(...items) {
  const bytes = [];
  for (const item of items) {
    if (typeof item === 'string') {
      bytes.push(...T1[item]);
    } else if (item >= -107 && item <= 107) {
      bytes.push(item + 139);
    } else if (item >= 108 && item <= 1131) {
      bytes.push(((item - 108) >> 8) + 247, (item - 108) & 255);
    } else if (item >= -1131 && item <= -108) {
      bytes.push(((-item - 108) >> 8) + 251, (-item - 108) & 255);
    } else {
      bytes.push(255, (item >>> 24) & 255, (item >>> 16) & 255, (item >>> 8) & 255, item & 255);
    }
  }
  return Buffer.from(bytes);
}
function t1Encrypt(plain, key) {
  let r = key;
  const out = Buffer.alloc(plain.length + 4);
  Buffer.concat([Buffer.alloc(4), plain]).forEach((p, i) => {
    const c = p ^ (r >> 8);
    out[i] = c;
    r = ((c + r) * 52845 + 22719) & 0xffff;
  });
  return out;
}
// The three parts of the program (/Length1, /Length2, /Length3): cleartext,
// eexec part (binary, or hexadecimal with `hex`), trailer. Glyphs: A a
// triangle, B a box with a curved side, O four curves, C drawn by a
// subroutine, Aacute a `seac` composite of A and acute, space empty.
function type1Program(hex, matrix = '0.001 0 0 0.001 0 0') {
  const rd = (name, data) => Buffer.concat([
    Buffer.from(`/${name} ${data.length + 4} RD `, 'latin1'), t1Encrypt(data, 4330), Buffer.from(' ND\n')]);
  const glyphs = [
    ['.notdef', t1Charstring(0, 500, 'hsbw', 'endchar')],
    ['space', t1Charstring(0, 250, 'hsbw', 'endchar')],
    ['A', t1Charstring(0, 600, 'hsbw', 50, 0, 'rmoveto', 250, 650, 'rlineto', 250, -650, 'rlineto',
      'closepath', 'endchar')],
    ['B', t1Charstring(0, 620, 'hsbw', 60, 0, 'rmoveto', 300, 0, 'rlineto', 180, 0, 0, 650, -180, 0,
      'rrcurveto', -300, 0, 'rlineto', 'closepath', 'endchar')],
    ['O', t1Charstring(0, 700, 'hsbw', 350, 0, 'rmoveto', 170, 0, 130, 150, 0, 175, 'rrcurveto',
      0, 175, -130, 150, -170, 0, 'rrcurveto', -170, 0, -130, -150, 0, -175, 'rrcurveto',
      0, -175, 130, -150, 170, 0, 'rrcurveto', 'closepath', 'endchar')],
    ['C', t1Charstring(0, 560, 'hsbw', 0, 'callsubr', 'endchar')],
    ['acute', t1Charstring(0, 300, 'hsbw', 100, 700, 'rmoveto', 100, 120, 'rlineto', 60, -40, 'rlineto',
      'closepath', 'endchar')],
    // asb adx ady bchar achar seac: A (65) with acute (194) shifted by 150.
    ['Aacute', t1Charstring(0, 600, 'hsbw', 0, 150, 0, 65, 194, 'seac')],
  ];
  const subr = t1Charstring(60, 0, 'rmoveto', 440, 0, 'rlineto', 0, 120, 'rlineto', -320, 0, 'rlineto',
    0, 410, 'rlineto', 320, 0, 'rlineto', 0, 120, 'rlineto', -440, 0, 'rlineto', 'closepath', 'return');
  const cleartext = Buffer.from([
    '%!PS-AdobeFont-1.0: CastleType1 001.000',
    '11 dict begin',
    '/FontInfo 2 dict dup begin /FullName (Castle Type1) readonly def /FamilyName (Castle) readonly def end readonly def',
    '/FontName /CastleType1 def', '/PaintType 0 def', '/FontType 1 def',
    `/FontMatrix [${matrix}] readonly def`, '/FontBBox {0 0 700 830} readonly def',
    '/Encoding 256 array', '0 1 255 {1 index exch /.notdef put} for',
    'dup 32 /space put', 'dup 65 /A put', 'dup 66 /B put', 'dup 67 /C put', 'dup 79 /O put',
    'dup 193 /Aacute put', 'dup 194 /acute put',
    'readonly def', 'currentdict end', 'currentfile eexec', '',
  ].join('\n'), 'latin1');
  const privatePart = Buffer.concat([
    Buffer.from('dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n'
      + '/ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/lenIV 4 def\n'
      + '/BlueValues [] ND\n/MinFeature{16 16}ND\n/password 5839 def\n/Subrs 1 array\n', 'latin1'),
    Buffer.from(`dup 0 ${subr.length + 4} RD `, 'latin1'), t1Encrypt(subr, 4330), Buffer.from(' NP\nND\n'),
    Buffer.from(`2 index /CharStrings ${glyphs.length} dict dup begin\n`, 'latin1'),
    ...glyphs.map(([name, data]) => rd(name, data)),
    Buffer.from('end\nend\nreadonly put\nnoaccess put\ndup/FontName get exch definefont pop\n'
      + 'mark currentfile closefile\n', 'latin1'),
  ]);
  const encrypted = t1Encrypt(privatePart, 55665);
  const eexec = hex
    ? Buffer.from((encrypted.toString('hex').match(/.{1,64}/g) ?? []).join('\n') + '\n', 'latin1')
    : encrypted;
  const trailer = Buffer.from(`${'0'.repeat(64)}\n`.repeat(8) + 'cleartomark\n', 'latin1');
  return { cleartext, eexec, trailer };
}

// Embedded Type 1 fonts (ADR-0005 amended, tranche 3.8): the synthetic font
// with its built-in encoding (F1, symbolic), with a PDF encoding whose
// /Differences put its glyphs on lowercase codes (F2), and the same program
// with a hexadecimal eexec part (F3), and with a skewed /FontMatrix of half
// the usual scale, 2000 units per em (F4).
function type1EmbeddedPage() {
  const widths = { 32: 250, 65: 600, 66: 620, 67: 560, 79: 700, 193: 600, 194: 300 };
  const widthList = (first, last, map) => Array.from({ length: last - first + 1 },
    (_, i) => map[first + i] ?? 0).join(' ');
  const program = (hex, matrix) => {
    const { cleartext, eexec, trailer } = type1Program(hex, matrix);
    const data = Buffer.concat([cleartext, eexec, trailer]).toString('latin1');
    return stream(data, ` /Length1 ${cleartext.length} /Length2 ${eexec.length} /Length3 ${trailer.length}`);
  };
  const descriptor = (flags, file) => '<< /Type /FontDescriptor /FontName /CastleType1 '
    + `/Flags ${flags} /FontBBox [0 0 700 830] /ItalicAngle 0 /Ascent 830 /Descent 0 `
    + `/CapHeight 650 /StemV 80 /FontFile ${file} 0 R >>`;
  const lower = { 32: 250, 97: 600, 98: 620, 99: 560, 111: 700 };
  const content = [
    'BT /F1 40 Tf 10 130 Td (ABC O \\301) Tj ET',
    'BT /F2 40 Tf 10 75 Td (abc o) Tj ET',
    'BT /F3 40 Tf 10 20 Td (BOA \\301C) Tj ET',
    'BT /F4 60 Tf 220 120 Td (ABO) Tj ET',
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 360 180] '
      + '/Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R /F4 13 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    `<< /Type /Font /Subtype /Type1 /BaseFont /CastleType1 /FirstChar 32 /LastChar 194 /Widths [${widthList(32, 194, widths)}] /FontDescriptor 8 0 R >>`,
    `<< /Type /Font /Subtype /Type1 /BaseFont /CastleType1 /FirstChar 32 /LastChar 111 /Widths [${widthList(32, 111, lower)}] `
      + '/Encoding << /Type /Encoding /Differences [97 /A /B /C 111 /O] >> /FontDescriptor 9 0 R >>',
    `<< /Type /Font /Subtype /Type1 /BaseFont /CastleType1 /FirstChar 32 /LastChar 194 /Widths [${widthList(32, 194, widths)}] /FontDescriptor 10 0 R >>`,
    descriptor(4, 11),
    descriptor(32, 11),
    descriptor(4, 12),
    program(false),
    program(true),
    '<< /Type /Font /Subtype /Type1 /BaseFont /CastleType1 /FirstChar 65 /LastChar 79 '
      + `/Widths [${widthList(65, 79, { 65: 300, 66: 310, 79: 350 })}] /FontDescriptor 14 0 R >>`,
    descriptor(4, 15),
    program(false, '0.0005 0 0.0001 0.0005 0 0'),
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

// Form XObjects (ISO 32000-2, 8.10): one 200 x 200 page.
// - /R (top left): a form rotated 30 degrees by its /Matrix, which fills far
//   beyond its 50 x 50 /BBox in red, with a blue diagonal stroke: only the
//   rotated box is painted.
// - /N (top right): a form with its own resources, drawing the image /N (a
//   4 x 1 Indexed image: yellow, cyan, magenta, black) then the form /M,
//   which draws a green square; the page's /N is the form itself.
// - /N again at the bottom left, scaled by the page's cm.
// - /G (bottom right): a transparency group of two overlapping opaque red
//   squares, painted at fill alpha 0.5: the overlap is not darker.
function formsPage() {
  const page = [
    'q 1 0 0 1 40 120 cm /R Do Q',
    'q 1 0 0 1 110 110 cm /N Do Q',
    'q 0.6 0 0 0.6 10 10 cm /N Do Q',
    '/H gs /G Do',
  ].join('\n');
  const form = (entries, content) => stream(content,
    ` /Type /XObject /Subtype /Form ${entries}`);
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject '
      + '<< /R 5 0 R /N 6 0 R /G 9 0 R >> /ExtGState << /H << /ca 0.5 >> >> >> /Contents 4 0 R >>',
    stream(page),
    form('/BBox [0 0 50 50] /Matrix [0.8660254 0.5 -0.5 0.8660254 0 0]',
      '1 0 0 rg -100 -100 300 300 re f 0 0 1 RG 3 w 0 0 m 50 50 l S'),
    form('/BBox [0 0 80 80] /Resources << /XObject << /N 7 0 R /M 8 0 R >> >>',
      'q 80 0 0 40 0 40 cm /N Do Q /M Do'),
    imageStream(4, 1, ' /ColorSpace [/Indexed /DeviceRGB 3 <FFFF0000FFFFFF00FF000000>] '
      + '/BitsPerComponent 2', [0b00011011]),
    form('/BBox [0 0 80 40]', '0 0.6 0 rg 20 5 40 30 re f'),
    form('/BBox [0 0 200 200] /Group << /S /Transparency >>',
      '1 0 0 rg 110 10 50 50 re f 140 40 50 50 re f'),
  ], '1.4');
}

// A Type 3 font (ISO 32000-2, 9.6.4) on one 200 x 200 page. Glyph A
// (width 1000) is uncoloured (d1): a triangle with a square hole
// (even-odd), painted in the fill colour. Glyph B (width 600) is coloured
// (d0): a red bar and the 4 x 1 Indexed image of the font's resources.
// Font matrix 0.001. "ABBA" in blue at 30 points from (10, 150); "AB" in
// green at 50 points, rotated 30 degrees, from (40, 30); "BA" at 20 points
// with Tc 5 and a TJ adjustment from (110, 120).
function type3Page() {
  const content = [
    '0 0 1 rg BT /T3 30 Tf 10 150 Td (ABBA) Tj ET',
    '0 0.6 0 rg BT /T3 50 Tf 0.8660254 0.5 -0.5 0.8660254 40 30 Tm (AB) Tj ET',
    '0 0 1 rg BT /T3 20 Tf 5 Tc 110 120 Td [(B) -300 (A)] TJ ET',
  ].join('\n');
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /Font << /T3 5 0 R >> >> /Contents 4 0 R >>',
    stream(content),
    '<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] '
      + '/FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /tri 6 0 R /bar 7 0 R >> '
      + '/Encoding << /Type /Encoding /Differences [65 /tri /bar] >> '
      + '/FirstChar 65 /LastChar 66 /Widths [1000 600] '
      + '/Resources << /XObject << /I 8 0 R >> >> >>',
    stream('1000 0 0 0 1000 1000 d1 0 0 m 1000 0 l 500 1000 l h 350 150 300 300 re f*'),
    stream('600 0 d0 1 0 0 rg 0 0 200 1000 re f q 350 0 0 800 220 100 cm /I Do Q'),
    imageStream(4, 1, ' /ColorSpace [/Indexed /DeviceRGB 3 <FFFF0000FFFFFF00FF000000>] '
      + '/BitsPerComponent 2', [0b00011011]),
  ], '1.4');
}

// Encoders of the simple filters (ISO 32000-2, 7.4.2 to 7.4.5), for the
// filters fixture. Each takes and returns a latin1 string.
const toHex = text => Buffer.from(text, 'latin1').toString('hex').toUpperCase().replace(/(.{64})/g, '$1\n') + '>';
function toAscii85(text) {
  const bytes = Buffer.from(text, 'latin1');
  let out = '';
  for (let i = 0; i < bytes.length; i += 4) {
    const chunk = [0, 1, 2, 3].map(k => bytes[i + k] ?? 0);
    const count = Math.min(4, bytes.length - i);
    let value = ((chunk[0] << 24) >>> 0) + (chunk[1] << 16) + (chunk[2] << 8) + chunk[3];
    if (value === 0 && count === 4) {
      out += 'z';
      continue;
    }
    const digits = [];
    for (let k = 0; k < 5; k += 1) {
      digits.unshift(String.fromCharCode(33 + (value % 85)));
      value = Math.floor(value / 85);
    }
    out += digits.slice(0, count + 1).join('');
  }
  return out + '~>';
}
function toRunLength(text) {
  const bytes = Buffer.from(text, 'latin1');
  const out = [];
  let i = 0;
  while (i < bytes.length) {
    let run = 1;
    while (i + run < bytes.length && bytes[i + run] === bytes[i] && run < 128) run += 1;
    if (run > 1) {
      out.push(257 - run, bytes[i]);
      i += run;
    } else {
      let end = i + 1;
      while (end < bytes.length && end - i < 128 && bytes[end] !== bytes[end - 1]) end += 1;
      out.push(end - i - 1, ...bytes.subarray(i, end));
      i = end;
    }
  }
  out.push(128);
  return Buffer.from(out).toString('latin1');
}
// LZW with EarlyChange 1: the code width follows the decoder's table.
function toLzw(text) {
  const bytes = Buffer.from(text, 'latin1');
  const out = [];
  let buffer = 0;
  let bits = 0;
  let width = 9;
  const put = code => {
    buffer = (buffer * (1 << width)) + code;
    bits += width;
    while (bits >= 8) {
      out.push(Math.floor(buffer / 2 ** (bits - 8)) & 0xff);
      bits -= 8;
      buffer %= 2 ** bits;
    }
  };
  let table = new Map();
  for (let b = 0; b < 256; b += 1) table.set(String.fromCharCode(b), b);
  let next = 258;
  let seen = 258;
  let started = false;
  const code = c => {
    put(c);
    if (started && seen < 4096) seen += 1;
    started = true;
    const limit = seen + 1;
    width = limit < 512 ? 9 : limit < 1024 ? 10 : limit < 2048 ? 11 : 12;
  };
  put(256);
  let current = '';
  for (const b of bytes) {
    const extended = current + String.fromCharCode(b);
    if (table.has(extended)) {
      current = extended;
      continue;
    }
    code(table.get(current));
    if (next < 4096) table.set(extended, next++);
    current = String.fromCharCode(b);
  }
  if (current !== '') code(table.get(current));
  put(257);
  if (bits > 0) out.push((buffer * (1 << (8 - bits))) & 0xff);
  return Buffer.from(out).toString('latin1');
}

// The simple filters on one 200 x 200 page: its content in ASCIIHex, a
// 4 x 4 RGB image in LZW (quarters red, green, blue, gray), a 16 x 1 gray
// ramp in RunLength (runs of 4), and a 2 x 2 RGB image in ASCII85 then
// Flate. Each image covers a quarter of the page.
function filtersPage() {
  const content = ['q 90 0 0 90 5 105 cm /L Do Q', 'q 90 0 0 90 105 105 cm /R Do Q',
    'q 90 0 0 90 5 5 cm /A Do Q', '0 0 1 rg 120 20 60 60 re f'].join('\n');
  const rgb = [];
  for (let y = 0; y < 4; y += 1) {
    for (let x = 0; x < 4; x += 1) {
      const quarter = (y < 2 ? 0 : 2) + (x < 2 ? 0 : 1);
      rgb.push(...[[255, 0, 0], [0, 255, 0], [0, 0, 255], [128, 128, 128]][quarter]);
    }
  }
  const ramp = [];
  for (let x = 0; x < 16; x += 1) ramp.push(Math.floor(x / 4) * 85);
  const latin = bytes => Buffer.from(bytes).toString('latin1');
  const image = (width, height, extra, filter, data) => stream(data,
    ` /Type /XObject /Subtype /Image /Width ${width} /Height ${height} /BitsPerComponent 8${extra} /Filter ${filter}`);
  return buildPdf([
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] '
      + '/Resources << /XObject << /L 5 0 R /R 6 0 R /A 7 0 R >> >> /Contents 4 0 R >>',
    stream(toHex(content), ' /Filter /ASCIIHexDecode'),
    image(4, 4, ' /ColorSpace /DeviceRGB', '/LZWDecode', toLzw(latin(rgb))),
    image(16, 1, ' /ColorSpace /DeviceGray', '/RunLengthDecode', toRunLength(latin(ramp))),
    image(2, 2, ' /ColorSpace /DeviceRGB', '[/ASCII85Decode /FlateDecode]',
      toAscii85(zlibFixedHuffman(latin([255, 255, 0, 0, 255, 255, 255, 0, 255, 0, 0, 0])))),
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
  // Form XObjects: matrix and bounding box, own resources, nesting, reuse,
  // transparency group; see formsPage.
  'forms.pdf': formsPage(),
  // A Type 3 font: uncoloured and coloured glyphs, font resources; see
  // type3Page.
  'type3.pdf': type3Page(),
  // Text extraction: /ToUnicode, glyph names, unknown text; see textPage.
  'text.pdf': textPage(),
  // Type 0 encodings: Identity-V, embedded CMaps, usecmap; see cmapsPage.
  'cmaps.pdf': cmapsPage(),
  // Adobe's predefined CJK CMaps and collections; see cjkPage.
  'cjk.pdf': cjkPage(),
  // Embedded Type 1 fonts: built-in encoding, /Differences, hex eexec,
  // subroutine and seac; see type1EmbeddedPage.
  'type1.pdf': type1EmbeddedPage(),
  // Words, lines and /ActualText; see layoutPage.
  'layout.pdf': layoutPage(),
  // Reading order: title, two columns, a line in pieces; see columnsPage.
  'columns.pdf': columnsPage(),
  // The simple filters: ASCIIHex content, LZW, RunLength and ASCII85 +
  // Flate images; see filtersPage.
  'filters.pdf': filtersPage(),
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
