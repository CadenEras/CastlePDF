"""Builds CastleSymbols-Regular.ttf, the font that replaces Symbol and
ZapfDingbats when a PDF names them without embedding them (ADR-0005, step
5.3b).

Each glyph of the two fonts (their built-in encodings, through the Adobe
Glyph List and the ZapfDingbats glyph list) is taken from the first Noto
font that has its code point, in an order chosen per font: Noto Sans Math
first for Symbol (operators and Greek in a mathematical style, like Adobe's
Symbol), Noto Sans Symbols 2 first for ZapfDingbats. The subsets are merged
into one font, so that the renderer keeps one program per font. Code points
that the glyph lists put in Adobe's private use area (bracket pieces, serif
and sans copyright signs...) are mapped in the merged `cmap` to the glyph of
their standard Unicode equivalent.

Usage, with fontTools 4.60.1 (run by scripts/build-symbol-font.sh):
    python build-symbol-font.py <sources directory> <output .ttf>
The sources directory holds the four Noto fonts, encodings.js,
glyphlist.txt and zapfdingbats.txt (see the shell script for their pinned
origins). Prints the coverage and fails if a glyph is not covered.
"""

import re
import sys
from pathlib import Path

from fontTools import subset
from fontTools.merge import Merger
from fontTools.ttLib import TTFont

FAMILY = "CastlePDF Symbols"
POSTSCRIPT = "CastlePDFSymbols-Regular"

# Adobe private use code points of the glyph lists -> standard equivalent.
ALIASES = {
    0xF6D9: 0x00A9,  # copyrightserif
    0xF6DA: 0x00AE,  # registerserif
    0xF6DB: 0x2122,  # trademarkserif
    0xF8E5: 0x203E,  # radicalex
    0xF8E6: 0x23D0,  # arrowvertex
    0xF8E7: 0x23AF,  # arrowhorizex
    0xF8E8: 0x00AE,  # registersans
    0xF8E9: 0x00A9,  # copyrightsans
    0xF8EA: 0x2122,  # trademarksans
    0xF8EB: 0x239B,  # parenlefttp
    0xF8EC: 0x239C,  # parenleftex
    0xF8ED: 0x239D,  # parenleftbt
    0xF8EE: 0x23A1,  # bracketlefttp
    0xF8EF: 0x23A2,  # bracketleftex
    0xF8F0: 0x23A3,  # bracketleftbt
    0xF8F1: 0x23A7,  # bracelefttp
    0xF8F2: 0x23A8,  # braceleftmid
    0xF8F3: 0x23A9,  # braceleftbt
    0xF8F4: 0x23AA,  # braceex
    0xF8F5: 0x23AE,  # integralex
    0xF8F6: 0x239E,  # parenrighttp
    0xF8F7: 0x239F,  # parenrightex
    0xF8F8: 0x23A0,  # parenrightbt
    0xF8F9: 0x23A4,  # bracketrighttp
    0xF8FA: 0x23A5,  # bracketrightex
    0xF8FB: 0x23A6,  # bracketrightbt
    0xF8FC: 0x23AB,  # bracerighttp
    0xF8FD: 0x23AC,  # bracerightmid
    0xF8FE: 0x23AD,  # bracerightbt
}

FONTS = ["NotoSansMath", "NotoSans", "NotoSansSymbols", "NotoSansSymbols2"]
ORDER = {
    "Symbol": ["NotoSansMath", "NotoSans", "NotoSansSymbols", "NotoSansSymbols2"],
    "ZapfDingbats": ["NotoSansSymbols2", "NotoSansSymbols", "NotoSansMath", "NotoSans"],
}


def encoding(js, name):
    body = re.search(r"const " + name + r" = \[(.*?)\];", js, re.S).group(1)
    return [n for n in re.findall(r'"([^"]*)"', body) if n]


def glyph_list(path):
    out = {}
    for line in path.read_text().splitlines():
        if line and not line.startswith("#"):
            name, codes = line.split(";")
            out.setdefault(name, int(codes.split()[0], 16))
    return out


def main(sources, output):
    js = (sources / "encodings.js").read_text()
    agl = glyph_list(sources / "glyphlist.txt")
    zapf = glyph_list(sources / "zapfdingbats.txt")
    cmaps = {f: TTFont(sources / f"{f}-Regular.ttf").getBestCmap() for f in FONTS}

    # Code points each source font contributes; code points to alias.
    chosen = {f: set() for f in FONTS}
    aliases = {}
    report = {}
    for font, table, names in [
        ("Symbol", "SymbolSetEncoding", agl),
        ("ZapfDingbats", "ZapfDingbatsEncoding", zapf),
    ]:
        counts = {f: 0 for f in FONTS}
        for name in encoding(js, table):
            code = names.get(name, agl.get(name))
            if code is None:
                sys.exit(f"{font}: {name} has no code point")
            target = ALIASES.get(code, code)
            # One glyph per code point: one taken for the other font stays.
            taken = next((f for f in FONTS if target in chosen[f]), None)
            source = taken or next((f for f in ORDER[font] if target in cmaps[f]), None)
            if source is None:
                sys.exit(f"{font}: {name} U+{target:04X} is in no source font")
            chosen[source].add(target)
            counts[source] += 1
            if target != code:
                aliases[code] = target
        report[font] = counts

    parts = []
    work = output.parent / "symbols-work"
    work.mkdir(exist_ok=True)
    for name in FONTS:
        if not chosen[name]:
            continue
        options = subset.Options()
        options.hinting = False
        options.layout_features = []
        options.name_IDs = ["*"]
        options.name_languages = ["*"]
        options.name_legacy = True
        options.notdef_outline = True
        options.drop_tables += ["DSIG", "GDEF", "GPOS", "GSUB", "MATH", "STAT", "gasp", "kern", "vhea", "vmtx"]
        font = TTFont(sources / f"{name}-Regular.ttf")
        subsetter = subset.Subsetter(options)
        subsetter.populate(unicodes=sorted(chosen[name] | {0x20}))
        subsetter.subset(font)
        font.recalcTimestamp = False
        path = work / f"{name}.ttf"
        font.save(path)
        parts.append(path)

    merged = Merger().merge([str(p) for p in parts])
    # Private use code points share the glyph of their equivalent.
    for table in merged["cmap"].tables:
        if table.isUnicode():
            for code, target in aliases.items():
                if target in table.cmap:
                    table.cmap[code] = table.cmap[target]
    names = merged["name"]
    for record in list(names.names):
        if record.nameID in (1, 3, 4, 6, 16, 17):
            names.removeNames(nameID=record.nameID)
    names.setName(FAMILY, 1, 3, 1, 0x409)
    names.setName("Regular", 2, 3, 1, 0x409)
    names.setName(f"{FAMILY} Regular; built from Noto fonts", 3, 3, 1, 0x409)
    names.setName(f"{FAMILY} Regular", 4, 3, 1, 0x409)
    names.setName(POSTSCRIPT, 6, 3, 1, 0x409)
    names.setName(
        "Copyright 2022 The Noto Project Authors (https://github.com/notofonts/symbols, "
        "https://github.com/notofonts/math, https://github.com/notofonts/latin-greek-cyrillic). "
        "Subset and merged for CastlePDF.",
        0,
        3,
        1,
        0x409,
    )
    # Reproducible: the dates of the first source, not the time of merging
    # or saving.
    first = TTFont(sources / f"{FONTS[0]}-Regular.ttf")["head"]
    merged.recalcTimestamp = False
    merged["head"].created = first.created
    merged["head"].modified = first.modified
    merged.save(output)

    check = TTFont(output).getBestCmap()
    needed = set().union(*chosen.values()) | set(aliases)
    lost = sorted(code for code in needed if code not in check)
    if lost:
        sys.exit("not in the merged font: " + ", ".join(f"U+{c:04X}" for c in lost))
    print("glyphs per source font:", report)
    print(f"code points: {len(needed)}, of which aliases: {len(aliases)}")
    print(f"glyphs: {len(TTFont(output).getGlyphOrder())}")


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
