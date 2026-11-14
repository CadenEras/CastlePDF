#!/usr/bin/env node
// Adobe's predefined CMaps embedded by pdf-render (ADR-0006, tranche 3.7b):
// the encoding CMaps met on the corpus (and their vertical forms), and the
// UCS2 CMap of each CJK collection, each file deflated as it is.
//
//   node scripts/make-cmaps.mjs <cmap-resources> <mapping-resources-pdf>
//   node scripts/make-cmaps.mjs --check <cmap-resources> <mapping-resources-pdf>
//
// The two arguments are the directories of Adobe's repositories at the
// commits named in crates/pdf-render/cmaps/README.md. The first form writes
// crates/pdf-render/cmaps/*.z; `--check` only verifies that each committed
// file inflates to the bytes of its source, whatever zlib wrote it.
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateSync, inflateSync } from 'node:zlib';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = path.join(root, 'crates', 'pdf-render', 'cmaps');

// Name, repository (0: cmap-resources, 1: mapping-resources-pdf), path.
export const CMAPS = [
  ['UniJIS-UCS2-H', 0, 'Adobe-Japan1-7/CMap/UniJIS-UCS2-H'],
  ['UniJIS-UCS2-V', 0, 'Adobe-Japan1-7/CMap/UniJIS-UCS2-V'],
  ['UniJIS-UTF16-H', 0, 'Adobe-Japan1-7/CMap/UniJIS-UTF16-H'],
  ['UniJIS-UTF16-V', 0, 'Adobe-Japan1-7/CMap/UniJIS-UTF16-V'],
  ['90ms-RKSJ-H', 0, 'Adobe-Japan1-7/CMap/90ms-RKSJ-H'],
  ['90ms-RKSJ-V', 0, 'Adobe-Japan1-7/CMap/90ms-RKSJ-V'],
  ['UniGB-UCS2-H', 0, 'Adobe-GB1-6/CMap/UniGB-UCS2-H'],
  ['UniGB-UCS2-V', 0, 'Adobe-GB1-6/CMap/UniGB-UCS2-V'],
  ['UniCNS-UCS2-H', 0, 'Adobe-CNS1-7/CMap/UniCNS-UCS2-H'],
  ['UniCNS-UCS2-V', 0, 'Adobe-CNS1-7/CMap/UniCNS-UCS2-V'],
  ['UniKS-UCS2-H', 0, 'Adobe-Korea1-2/CMap/UniKS-UCS2-H'],
  ['UniKS-UCS2-V', 0, 'Adobe-Korea1-2/CMap/UniKS-UCS2-V'],
  ['Adobe-Japan1-UCS2', 1, 'pdf2unicode/Adobe-Japan1-UCS2'],
  ['Adobe-GB1-UCS2', 1, 'pdf2unicode/Adobe-GB1-UCS2'],
  ['Adobe-CNS1-UCS2', 1, 'pdf2unicode/Adobe-CNS1-UCS2'],
  ['Adobe-Korea1-UCS2', 1, 'pdf2unicode/Adobe-Korea1-UCS2'],
];

/** The CMap `name` as committed, inflated (for the fixtures). */
export function committed(name) {
  return inflateSync(readFileSync(path.join(outDir, `${name}.z`)));
}

function main(args) {
  const check = args[0] === '--check';
  const [cmapResources, mappingResources] = check ? args.slice(1) : args;
  if (!cmapResources || !mappingResources) {
    console.error('usage: make-cmaps.mjs [--check] <cmap-resources> <mapping-resources-pdf>');
    process.exit(2);
  }
  const repositories = [cmapResources, mappingResources];
  let failed = false;
  let total = 0;
  for (const [name, repository, source] of CMAPS) {
    const original = readFileSync(path.join(repositories[repository], source));
    const target = path.join(outDir, `${name}.z`);
    if (check) {
      if (!committed(name).equals(original)) {
        console.error(`differs from its source: ${name}`);
        failed = true;
      }
    } else {
      writeFileSync(target, deflateSync(original, { level: 9 }));
    }
    total += readFileSync(target).length;
  }
  console.log(`${CMAPS.length} CMaps, ${total} bytes deflated`);
  if (failed) {
    process.exit(1);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2));
}
