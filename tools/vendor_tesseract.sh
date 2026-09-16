#!/usr/bin/env bash
# One-time setup of vendor/: the Tesseract runtime the app loads at startup.
#
# Windows' built-in OCR engine has no Hebrew, so Tesseract is vendored next to
# the executable and loaded with libloading -- which is why the build needs no
# C++ toolchain.
set -euo pipefail
cd "$(dirname "$0")/.."

TESS_DIR="/c/Program Files/Tesseract-OCR"

if [ ! -f "$TESS_DIR/libtesseract-5.dll" ]; then
    echo "Tesseract not found; installing via winget..."
    winget install --id UB-Mannheim.TesseractOCR \
        --accept-package-agreements --accept-source-agreements --disable-interactivity
fi
if [ ! -f "$TESS_DIR/libtesseract-5.dll" ]; then
    echo "error: still no $TESS_DIR/libtesseract-5.dll" >&2
    exit 1
fi

mkdir -p vendor/bin vendor/tessdata
cp "$TESS_DIR"/*.dll vendor/bin/
echo "copied $(ls vendor/bin/*.dll | wc -l) dll"

# tessdata_best is slower than the default set but noticeably more accurate on
# the small, antialiased text the overlay is pointed at.
for lang in heb eng; do
    out="vendor/tessdata/$lang.traineddata"
    if [ ! -f "$out" ]; then
        echo "downloading $lang.traineddata"
        curl -fsSL -o "$out" \
            "https://github.com/tesseract-ocr/tessdata_best/raw/main/$lang.traineddata"
    fi
done

# The installer ships the whole pango/cairo/icu stack for its PDF tools; none of
# it is reachable from the OCR API we call.
python tools/prune_vendor.py --apply
rm -rf vendor/_unused

echo
echo "vendor/ ready: $(ls vendor/bin/*.dll | wc -l) dll, $(du -sh vendor | cut -f1)"
