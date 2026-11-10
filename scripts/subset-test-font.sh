#!/usr/bin/env bash
# Builds tests/fonts/arimo-subset.ttf, a small subset of Arimo Regular (SIL
# OFL 1.1) used by the font fixtures, and arimo-subset.json (glyph id and
# advance width of each kept character) for scripts/make-fixtures.mjs.
#
# Requires curl and fonttools 4.60.1 (pyftsubset and python with fontTools).
#   PYTHON=/path/to/python PYFTSUBSET=/path/to/pyftsubset scripts/subset-test-font.sh
# The output is committed; CI does not run this script.
set -euo pipefail
COMMIT=4a6255f269916ae7ad3fc2706b0935e7621396b8
URL="https://raw.githubusercontent.com/googlefonts/arimo/$COMMIT/fonts/ttf/Arimo-Regular.ttf"
SHA256=41b22bc8f0b51f932825d37bc55b5eb6ba67dfe599a626e4aff2b43b624f9f8c
TEXT=' !,.0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'
PYTHON="${PYTHON:-python3}"
PYFTSUBSET="${PYFTSUBSET:-pyftsubset}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/Arimo-Regular.ttf" "$URL"
echo "$SHA256  $work/Arimo-Regular.ttf" | sha256sum -c -
"$PYFTSUBSET" "$work/Arimo-Regular.ttf" \
  --text="$TEXT" --no-hinting --layout-features='' --name-IDs='*' --name-legacy \
  --drop-tables+=DSIG,GDEF,GPOS,GSUB,STAT,gasp,kern --notdef-outline \
  --output-file="$root/tests/fonts/arimo-subset.ttf"
"$PYTHON" - "$root/tests/fonts/arimo-subset.ttf" "$TEXT" > "$root/tests/fonts/arimo-subset.json" <<'PY'
import json, sys
from fontTools.ttLib import TTFont
font = TTFont(sys.argv[1])
cmap = font.getBestCmap()
order = font.getGlyphOrder()
hmtx = font["hmtx"]
glyphs = {}
for ch in sys.argv[2]:
    name = cmap[ord(ch)]
    glyphs[ch] = [order.index(name), hmtx[name][0]]
json.dump({"unitsPerEm": font["head"].unitsPerEm, "glyphs": glyphs}, sys.stdout, indent=1, sort_keys=True)
print()
PY
sha256sum "$root/tests/fonts/arimo-subset.ttf"
