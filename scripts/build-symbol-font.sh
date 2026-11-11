#!/usr/bin/env bash
# Builds crates/pdf-render/fonts/CastleSymbols-Regular.ttf, which replaces
# Symbol and ZapfDingbats when a PDF does not embed them (ADR-0005, step
# 5.3b), from Noto Sans Math, Noto Sans, Noto Sans Symbols and Noto Sans
# Symbols 2 (SIL OFL 1.1, no reserved font name) at pinned commits, and the
# glyph names of the two encodings (pdf.js encodings.js, Apache-2.0; Adobe
# glyph lists, BSD-3-Clause). See scripts/build-symbol-font.py.
#
# Requires curl, Python 3 and fontTools 4.60.1.
#   PYTHON=/path/to/python scripts/build-symbol-font.sh
# The output is committed; CI does not run this script.
set -euo pipefail
PYTHON="${PYTHON:-python3}"
noto=4cf78353cd1668040ba8efa13029cd910fcdf9e2
pdfjs=81525fd446779f0a29003cf06e3ba33751954826
agl=0276763a3cdf204a1badc283d90f5bc7ac13130d
declare -A SOURCES=(
  [NotoSansMath-Regular.ttf]="https://raw.githubusercontent.com/notofonts/notofonts.github.io/$noto/fonts/NotoSansMath/hinted/ttf/NotoSansMath-Regular.ttf"
  [NotoSans-Regular.ttf]="https://raw.githubusercontent.com/notofonts/notofonts.github.io/$noto/fonts/NotoSans/hinted/ttf/NotoSans-Regular.ttf"
  [NotoSansSymbols-Regular.ttf]="https://raw.githubusercontent.com/notofonts/notofonts.github.io/$noto/fonts/NotoSansSymbols/hinted/ttf/NotoSansSymbols-Regular.ttf"
  [NotoSansSymbols2-Regular.ttf]="https://raw.githubusercontent.com/notofonts/notofonts.github.io/$noto/fonts/NotoSansSymbols2/hinted/ttf/NotoSansSymbols2-Regular.ttf"
  [encodings.js]="https://raw.githubusercontent.com/mozilla/pdf.js/$pdfjs/src/core/encodings.js"
  [glyphlist.txt]="https://raw.githubusercontent.com/adobe-type-tools/agl-aglfn/$agl/glyphlist.txt"
  [zapfdingbats.txt]="https://raw.githubusercontent.com/adobe-type-tools/agl-aglfn/$agl/zapfdingbats.txt"
  [OFL-symbols.txt]="https://raw.githubusercontent.com/notofonts/symbols/c42cbaee99cb13013814864f8081d8c18656ec02/OFL.txt"
)
declare -A SHA256=(
  [NotoSansMath-Regular.ttf]=d51afd5739c7ba6c44fcab35a88160e25dfb69a2d4ad0bd99533f8d894af1f96
  [NotoSans-Regular.ttf]=478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823
  [NotoSansSymbols-Regular.ttf]=d0e98e9a2c046594c5021437273943be7e79e0fd980fde125279e22302212595
  [NotoSansSymbols2-Regular.ttf]=c4a0a80f0041ce4be81e2478faad22776d23edb98ae3f0d19bd37044820ecf9d
  [encodings.js]=eee7b0b49fbf0c27fd1765abeea43621c23da3d1a8a592ad0e5ddfe6743658e3
  [glyphlist.txt]=a3b2f61ced9f3644cc0d4ecde5c59df34ca286c689d9484a43a710a81c466789
  [zapfdingbats.txt]=f6394e3cb8a447e84a1dad75d4baaf2aa7f45dc104faf369f4720e1a774ef2dc
  [OFL-symbols.txt]=b118dd41337806a5d4797052c77caf3bd096aed783e5eb21b4d11154351e1ac0
)
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/crates/pdf-render/fonts"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for file in "${!SOURCES[@]}"; do
  curl -fsSL -o "$work/$file" "${SOURCES[$file]}"
  echo "${SHA256[$file]}  $work/$file" | sha256sum -c -
done
"$PYTHON" -I "$root/scripts/build-symbol-font.py" "$work" "$work/CastleSymbols-Regular.ttf"
cp "$work/CastleSymbols-Regular.ttf" "$out/"
# The three Noto repositories share this licence text but for the URL of
# their copyright line; the font's copyright record names all three.
cp "$work/OFL-symbols.txt" "$out/OFL-Noto.txt"
cd "$out" && sha256sum CastleSymbols-Regular.ttf && du -h CastleSymbols-Regular.ttf
