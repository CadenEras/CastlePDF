#!/usr/bin/env bash
# Builds the fonts that replace fonts a PDF names but does not embed
# (ADR-0005, step 5.3): Arimo (Helvetica, Arial), Tinos (Times) and Cousine
# (Courier), four styles each, SIL OFL 1.1, from pinned commits of
# github.com/googlefonts. Each is reduced to the characters of the
# Standard, WinAnsi and MacRoman encodings (245 code points), without
# hinting, and written to crates/pdf-render/fonts with its licence.
#
# Requires curl and fonttools 4.60.1 (pyftsubset).
#   PYFTSUBSET=/path/to/pyftsubset scripts/build-standard-fonts.sh
# The output is committed; CI does not run this script.
set -euo pipefail
PYFTSUBSET="${PYFTSUBSET:-pyftsubset}"
declare -A COMMITS=(
  [arimo]=4a6255f269916ae7ad3fc2706b0935e7621396b8
  [tinos]=3b4482a99b80ea5fc75f187b1be3120a3f5905b3
  [cousine]=0c999beec9ab736acbf2cc15b1c87857c1a76352
)
declare -A SHA256=(
  [Arimo-Regular.ttf]=41b22bc8f0b51f932825d37bc55b5eb6ba67dfe599a626e4aff2b43b624f9f8c
  [Arimo-Bold.ttf]=d7a8b187cf8444d4cfee102e8eae9e3043682fd5106d5d33ed677fe268a0e2ba
  [Arimo-Italic.ttf]=ffdf690e4357a392e4edcc3df0b3adb0302920f75e54f2e6eec355afbe7edcba
  [Arimo-BoldItalic.ttf]=77c967d49d4ef4a63c39fbcd1e27892102e503e35a20983cb058430c4f268b34
  [Tinos-Regular.ttf]=60a0e8ef0c04dd5dd69ffe91025fa2ae5836cbd35600a82ba031977557e2cb61
  [Tinos-Bold.ttf]=393269dbab8899f938db19783eca5eac92eb431f7ae0ab45b8349ca895f1a06b
  [Tinos-Italic.ttf]=5942266ed398b155d7dc23e36833e7ec6be988f2439bdbeb8ef1bede808eaa91
  [Tinos-BoldItalic.ttf]=a5de79f0fe863ea0954757acb3d47b3ccd0a930ce3dd5b97230cd3866790a06e
  [Cousine-Regular.ttf]=5a57f0184000371cb22fe3fcea4c500354cab1a69efaf7beaf3e6eca6ecfefea
  [Cousine-Bold.ttf]=331215ec6445f41e98d8971251bb6237e722907f4101faae990583accfe79545
  [Cousine-Italic.ttf]=afb869eee8643d09915c3286edb61dad66278ceb1511c79b474ce21548421bcd
  [Cousine-BoldItalic.ttf]=7f804549c941ceaac0a9635389ed09c4bbc86e4bfe37589191dacd1b5b0e26a5
)
# Code points of the glyph names of StandardEncoding, WinAnsiEncoding and
# MacRomanEncoding (ISO 32000-2, annex D) through the Adobe Glyph List,
# plus no-break space and soft hyphen.
UNICODES='U+0020-007E,U+00A0-00FF,U+0131,U+0141-0142,U+0152-0153,U+0160-0161,U+0178,U+017D-017E,U+0192,U+02C6-02C7,U+02D8-02DD,U+03C0,U+2013-2014,U+2018-201A,U+201C-201E,U+2020-2022,U+2026,U+2030,U+2039-203A,U+2044,U+20AC,U+2122,U+2126,U+2202,U+2206,U+220F,U+2211,U+221A,U+221E,U+222B,U+2248,U+2260,U+2264-2265,U+25CA,U+F8FF,U+FB01-FB02'
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/crates/pdf-render/fonts"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
for family in arimo tinos cousine; do
  name="${family^}"
  for style in Regular Bold Italic BoldItalic; do
    file="$name-$style.ttf"
    curl -fsSL -o "$work/$file" \
      "https://raw.githubusercontent.com/googlefonts/$family/${COMMITS[$family]}/fonts/ttf/$file"
    echo "${SHA256[$file]}  $work/$file" | sha256sum -c -
    # Name records are kept: they carry the copyright and licence notices.
    "$PYFTSUBSET" "$work/$file" --unicodes="$UNICODES" --no-hinting --layout-features='' \
      --name-IDs='*' --name-legacy --name-languages='*' \
      --drop-tables+=DSIG,GDEF,GPOS,GSUB,STAT,gasp,kern --notdef-outline \
      --output-file="$out/$file"
  done
  curl -fsSL -o "$out/OFL-$name.txt" \
    "https://raw.githubusercontent.com/googlefonts/$family/${COMMITS[$family]}/OFL.txt"
done
cd "$out" && sha256sum ./*.ttf && du -ch ./*.ttf | tail -1
