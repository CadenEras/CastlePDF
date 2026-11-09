// Synthetic corpus: committed files match the generator and the manifest.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { buildPdf, damaged, fixtures, sha256 } from '../make-fixtures.mjs';

const corpus = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..',
  'tests', 'corpus');
// Valid fixtures, and the damaged ones of ADR-0004, each with its manifest.
const sets = [['public', fixtures], ['damaged', damaged]].map(([name, generated]) => {
  const dir = path.join(corpus, name);
  const manifest = JSON.parse(readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
  return { name, dir, generated, manifest };
});

for (const { name, dir, generated, manifest } of sets) {
  test(`${name}: every generated fixture is committed unchanged and listed`, () => {
    const listed = new Map(manifest.fixtures.map(f => [f.file, f]));
    for (const [file, bytes] of Object.entries(generated)) {
      assert.ok(readFileSync(path.join(dir, file)).equals(bytes), `${file} differs`);
      assert.equal(listed.get(file)?.sha256, sha256(bytes), `${file} manifest hash`);
    }
    assert.equal(listed.size, Object.keys(generated).length);
  });

  test(`${name}: manifest entries carry the required metadata`, () => {
    for (const entry of manifest.fixtures) {
      for (const key of ['id', 'file', 'sha256', 'origin', 'license', 'features', 'expected']) {
        assert.ok(entry[key], `${entry.file ?? '?'}: missing ${key}`);
      }
    }
  });
}

test('damaged fixtures are damaged copies, never valid files', () => {
  for (const [file, bytes] of Object.entries(damaged)) {
    assert.ok(!Object.values(fixtures).some(valid => valid.equals(bytes)), file);
  }
});

test('xref offsets point at their objects', () => {
  const pdf = buildPdf(['<< /Type /Catalog >>', '<< /A 1 >>']).toString('latin1');
  const start = Number(/startxref\n(\d+)/.exec(pdf)[1]);
  assert.equal(pdf.slice(start, start + 4), 'xref');
  const offsets = [...pdf.matchAll(/(\d{10}) 00000 n /g)].map(m => Number(m[1]));
  offsets.forEach((offset, i) => assert.ok(pdf.startsWith(`${i + 1} 0 obj`, offset)));
});

test('the deterministic zlib encoder is real Deflate', async () => {
  const { inflateSync } = await import('node:zlib');
  const { zlibFixedHuffman } = await import('../make-fixtures.mjs');
  const all = String.fromCharCode(...Array.from({ length: 256 }, (_, i) => i));
  for (const text of ['', 'a', 'BT /F1 24 Tf (x) Tj ET', all, all.repeat(40)]) {
    const encoded = Buffer.from(zlibFixedHuffman(text), 'latin1');
    assert.equal(inflateSync(encoded).toString('latin1'), text);
  }
});
