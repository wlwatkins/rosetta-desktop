#!/usr/bin/env bash
# One-time, build-time only. Produces models/ ; nothing here ships with the app.
#   tools/setup_model.sh              -> models/ (int8, ~365MB)
#   tools/setup_model.sh --keep-fp32  -> also models_fp32/ for `rosetta-desktop bench`
set -euo pipefail
cd "$(dirname "$0")/.."
uv venv --python 3.12 .venv-export
uv pip install --python .venv-export torch --index-url https://download.pytorch.org/whl/cpu
uv pip install --python .venv-export \
    "transformers>=4.44" "optimum[onnxruntime]>=1.23" onnx sentencepiece protobuf numpy
PYTHONIOENCODING=utf-8 .venv-export/Scripts/python.exe tools/export_model.py "$@"
