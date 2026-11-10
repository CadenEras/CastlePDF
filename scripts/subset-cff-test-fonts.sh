#!/usr/bin/env bash
# Builds the CFF test fonts (SIL OFL 1.1) used by scripts/make-fixtures.mjs:
# - source-sans-subset.otf: subset of Source Sans 3 Regular (OpenType, CFF
#   outlines, name-keyed), embedded as /FontFile3 /OpenType;
# - source-sans-subset.cff: its bare `CFF ` table, embedded as /Type1C;
# - noto-cjk-subset.cff: bare CID-keyed `CFF ` table of a subset of Noto Sans
#   CJK SC Regular, embedded as /CIDFontType0C;
# - *.json: glyph id, CID or name and advance of each kept character.
#
# Source Sans declares the Reserved Font Name "Source": the subset, a
# Modified Version under the OFL, is renamed "CastleTest Sans" (name table
# and CFF names). Noto Sans CJK declares no Reserved Font Name.
#
# Requires curl and fonttools 4.60.1 (pyftsubset and python with fontTools).
#   PYTHON=/path/to/python PYFTSUBSET=/path/to/pyftsubset scripts/subset-cff-test-fonts.sh
# The output is committed; CI does not run this script.
set -euo pipefail
SANS_URL=https://raw.githubusercontent.com/adobe-fonts/source-sans/7c691c2772570a0d3a9e1bffe8a9d6074257d985/OTF/SourceSans3-Regular.otf
SANS_SHA256=08df266400933d3178d081a45f94a08814c3e55b4b7dd2e0ff69cb1329f13ab6
CJK_URL=https://raw.githubusercontent.com/notofonts/noto-cjk/165c01b46ea533872e002e0785ff17e44f6d97d8/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf
CJK_SHA256=2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b
SANS_TEXT=' !,.0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'
CJK_TEXT=' 0123456789CDFIKST字体中文'
PYTHON="${PYTHON:-python3}"
PYFTSUBSET="${PYFTSUBSET:-pyftsubset}"
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/tests/fonts"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/sans.otf" "$SANS_URL"
curl -fsSL -o "$work/cjk.otf" "$CJK_URL"
echo "$SANS_SHA256  $work/sans.otf" | sha256sum -c -
echo "$CJK_SHA256  $work/cjk.otf" | sha256sum -c -
common=(--no-hinting --layout-features='' --name-IDs='*' --name-legacy
  --drop-tables+=DSIG,GDEF,GPOS,GSUB,BASE,STAT,VORG,vhea,vmtx --notdef-outline)
"$PYFTSUBSET" "$work/sans.otf" --text="$SANS_TEXT" "${common[@]}" \
  --output-file="$out/source-sans-subset.otf"
"$PYFTSUBSET" "$work/cjk.otf" --text="$CJK_TEXT" "${common[@]}" \
  --output-file="$work/cjk-subset.otf"
"$PYTHON" - "$out" "$work/cjk-subset.otf" "$SANS_TEXT" "$CJK_TEXT" <<'PY'
import json, sys
from fontTools.ttLib import TTFont
out, cjk_path, sans_text, cjk_text = sys.argv[1:5]

def extract(font_path, cff_path):
    font = TTFont(font_path)
    with open(cff_path, "wb") as f:
        f.write(font.reader["CFF "])
    return font

def rename(path, family, postscript):
    font = TTFont(path)
    name = font["name"]
    for record in list(name.names):
        if record.nameID in (1, 16):
            record.string = family
        elif record.nameID in (3, 4, 18):
            record.string = f"{family} Regular"
        elif record.nameID == 6:
            record.string = postscript
        elif record.nameID in (17,):
            record.string = "Regular"
    cff = font["CFF "].cff
    cff.fontNames = [postscript]
    top = cff.topDictIndex[0]
    top.FullName = f"{family} Regular"
    top.FamilyName = family
    font.save(path)

rename(f"{out}/source-sans-subset.otf", "CastleTest Sans", "CastleTestSans-Regular")
sans = extract(f"{out}/source-sans-subset.otf", f"{out}/source-sans-subset.cff")
cmap, order, hmtx = sans.getBestCmap(), sans.getGlyphOrder(), sans["hmtx"]
glyphs = {ch: [order.index(cmap[ord(ch)]), hmtx[cmap[ord(ch)]][0], cmap[ord(ch)]] for ch in sans_text}
json.dump({"unitsPerEm": sans["head"].unitsPerEm, "glyphs": glyphs},
          open(f"{out}/source-sans-subset.json", "w"), indent=1, sort_keys=True, ensure_ascii=False)

cjk = extract(cjk_path, f"{out}/noto-cjk-subset.cff")
top = cjk["CFF "].cff.topDictIndex[0]
assert hasattr(top, "ROS"), "the CJK subset must stay CID-keyed"
cmap, order, hmtx = cjk.getBestCmap(), cjk.getGlyphOrder(), cjk["hmtx"]
def cid(name):
    assert name.startswith("cid"), name
    return int(name[3:])
glyphs = {ch: [order.index(cmap[ord(ch)]), cid(cmap[ord(ch)]), hmtx[cmap[ord(ch)]][0]] for ch in cjk_text}
json.dump({"unitsPerEm": cjk["head"].unitsPerEm, "glyphs": glyphs},
          open(f"{out}/noto-cjk-subset.json", "w"), indent=1, sort_keys=True, ensure_ascii=False)
PY
sha256sum "$out"/source-sans-subset.* "$out"/noto-cjk-subset.*
