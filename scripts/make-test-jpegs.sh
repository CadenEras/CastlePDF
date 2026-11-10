#!/usr/bin/env bash
# Builds the JPEG test images (tests/images/*.jpg) used by
# scripts/make-fixtures.mjs: synthetic content (project-owned), encoded by
# Pillow 11.3.0 (its bundled libjpeg-turbo) in three colour models:
# - rgb.jpg: 32 x 16, YCbCr, left half red, right half blue, top row band
#   green (4 px);
# - gray.jpg: 32 x 16, a horizontal ramp from black to white;
# - cmyk.jpg: 32 x 16, Adobe CMYK (inverted samples, APP14), left half cyan,
#   right half yellow.
#
# Requires a python with Pillow 11.3.0.
#   PYTHON=/path/to/python scripts/make-test-jpegs.sh
# The output is committed; CI does not run this script.
set -euo pipefail
PYTHON="${PYTHON:-python3}"
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/tests/images"
mkdir -p "$out"
"$PYTHON" - "$out" <<'PY'
import sys
import PIL
from PIL import Image
assert PIL.__version__ == "11.3.0", PIL.__version__
out = sys.argv[1]

rgb = Image.new("RGB", (32, 16))
for y in range(16):
    for x in range(32):
        if y < 4:
            rgb.putpixel((x, y), (0, 160, 0))
        else:
            rgb.putpixel((x, y), (220, 0, 0) if x < 16 else (0, 0, 220))
rgb.save(f"{out}/rgb.jpg", quality=95, subsampling=0, optimize=False)

gray = Image.new("L", (32, 16))
for y in range(16):
    for x in range(32):
        gray.putpixel((x, y), round(x * 255 / 31))
gray.save(f"{out}/gray.jpg", quality=95, optimize=False)

cmyk = Image.new("CMYK", (32, 16))
for y in range(16):
    for x in range(32):
        cmyk.putpixel((x, y), (255, 0, 0, 0) if x < 16 else (0, 0, 255, 0))
cmyk.save(f"{out}/cmyk.jpg", quality=95, optimize=False)
PY
sha256sum "$out"/*.jpg
