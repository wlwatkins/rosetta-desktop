"""
One-time, BUILD-TIME ONLY conversion of Helsinki-NLP/opus-mt-tc-big-he-en into
the artifacts the Rust binary loads. Nothing here ships: the app itself has no
Python dependency.

    python tools/export_model.py                # -> models/  (~365MB, int8)
    python tools/export_model.py --keep-fp32    # also -> models_fp32/ for `bench`
    python tools/export_model.py --keep-raw     # leave the ~3GB intermediates

Outputs into models/:
    encoder_model.onnx          int8-quantized encoder
    decoder_model_merged.onnx   int8-quantized decoder, with the KV-cache branch
    tokenizer_source.json       HF `tokenizers` json for encoding Hebrew
    vocab_target.json           id -> piece table, for detokenizing English
    model_meta.json             ids and dims the Rust decode loop needs
"""

import json
import shutil
import sys
from pathlib import Path

MODEL_ID = "Helsinki-NLP/opus-mt-tc-big-he-en"
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "models"
FP32 = ROOT / "models_fp32"
RAW = OUT / "_raw_onnx"

# Files the fp32 benchmark variant needs, alongside the shared json assets.
FP32_WEIGHTS = ("encoder_model.onnx", "decoder_model_merged.onnx")
SHARED_JSON = ("tokenizer_source.json", "vocab_target.json", "model_meta.json")


def export_onnx() -> None:
    from optimum.exporters.onnx import main_export

    print(f"[1/5] exporting {MODEL_ID} -> onnx (downloads ~1.2GB)", flush=True)
    main_export(
        MODEL_ID,
        output=RAW,
        task="text2text-generation-with-past",
        opset=17,
        device="cpu",
    )


def quantize() -> None:
    from onnxruntime.quantization import QuantType, quantize_dynamic

    OUT.mkdir(parents=True, exist_ok=True)
    for name in FP32_WEIGHTS:
        src = RAW / name
        if not src.exists():
            raise SystemExit(
                f"expected {src}; export produced {sorted(p.name for p in RAW.glob('*.onnx'))}"
            )
        dst = OUT / name
        # The merged decoder hides its matmuls inside `If` subgraphs, which the
        # quantizer skips unless told to descend into them -- without this the
        # decoder comes out at its full 900MB.
        extra = {"EnableSubgraph": True} if "merged" in name else None
        print(f"[2/5] quantizing {name} -> int8", flush=True)
        quantize_dynamic(src, dst, weight_type=QuantType.QInt8, extra_options=extra)
        print(f"      {src.stat().st_size / 1e6:.0f}MB -> {dst.stat().st_size / 1e6:.0f}MB")


def build_tokenizer() -> None:
    """
    Marian pairs a 32k SentencePiece unigram model with a 60k shared vocab, so the
    piece -> id mapping comes from vocab.json rather than the .spm's own indices.
    We rebuild a `tokenizers` Unigram whose vocab list is ordered by vocab.json id,
    which makes the Rust side a plain `Tokenizer::from_file` with no C++ deps.

    transformers' own convert_slow_tokenizer cannot do this for Marian (it looks
    for a `vocab_file` attribute the tokenizer does not have), hence the manual
    construction.
    """
    import sentencepiece.sentencepiece_model_pb2 as spb
    from huggingface_hub import hf_hub_download
    from tokenizers import Tokenizer, decoders, normalizers, pre_tokenizers, processors
    from tokenizers.models import Unigram

    print("[3/5] building tokenizer", flush=True)
    spm = spb.ModelProto()
    spm.ParseFromString(Path(hf_hub_download(MODEL_ID, "source.spm")).read_bytes())
    scores = {p.piece: p.score for p in spm.pieces}

    vocab = json.loads(
        Path(hf_hub_download(MODEL_ID, "vocab.json")).read_text(encoding="utf-8")
    )
    size = max(vocab.values()) + 1

    # Pieces that exist only in the English half of the shared vocab must never
    # win a Viterbi segmentation of Hebrew input; park them below the real scores.
    floor = min(scores.values()) - 10.0
    table = [("", floor)] * size
    for piece, idx in vocab.items():
        table[idx] = (piece, scores.get(piece, floor))

    tok = Tokenizer(Unigram(table, unk_id=vocab["<unk>"], byte_fallback=False))
    tok.normalizer = normalizers.Sequence(
        [
            normalizers.Precompiled(spm.normalizer_spec.precompiled_charsmap),
            normalizers.Replace(" {2,}", " "),
        ]
    )
    tok.pre_tokenizer = pre_tokenizers.Metaspace(replacement="▁", prepend_scheme="always")
    tok.decoder = decoders.Metaspace(replacement="▁", prepend_scheme="always")
    tok.post_processor = processors.TemplateProcessing(
        single="$A </s>",
        pair="$A </s> $B </s>",
        special_tokens=[("</s>", vocab["</s>"])],
    )

    OUT.mkdir(parents=True, exist_ok=True)
    tok.save(str(OUT / "tokenizer_source.json"))

    ids = [""] * size
    for piece, idx in vocab.items():
        ids[idx] = piece
    (OUT / "vocab_target.json").write_text(json.dumps(ids, ensure_ascii=False), encoding="utf-8")

    probe = "שלום עולם"
    print(f"      {probe!r} -> {tok.encode(probe).tokens}")


def write_meta() -> None:
    from transformers import AutoConfig, AutoTokenizer

    print("[4/5] writing model_meta.json", flush=True)
    cfg = AutoConfig.from_pretrained(MODEL_ID)
    tok = AutoTokenizer.from_pretrained(MODEL_ID)
    payload = {
        "model_id": MODEL_ID,
        "d_model": cfg.d_model,
        "decoder_layers": cfg.decoder_layers,
        "decoder_attention_heads": cfg.decoder_attention_heads,
        "head_dim": cfg.d_model // cfg.decoder_attention_heads,
        "vocab_size": cfg.vocab_size,
        "decoder_start_token_id": cfg.decoder_start_token_id,
        "eos_token_id": cfg.eos_token_id,
        "pad_token_id": cfg.pad_token_id,
        "unk_token_id": tok.unk_token_id,
        "max_length": 512,
    }
    (OUT / "model_meta.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")


def stage_fp32() -> None:
    """Keep an unquantized copy so `rosetta-desktop bench` can compare the two."""
    print("[5/5] staging fp32 copy for bench", flush=True)
    FP32.mkdir(parents=True, exist_ok=True)
    for name in FP32_WEIGHTS:
        shutil.copy2(RAW / name, FP32 / name)
    for name in SHARED_JSON:
        shutil.copy2(OUT / name, FP32 / name)


if __name__ == "__main__":
    export_onnx()
    quantize()
    build_tokenizer()
    write_meta()

    if "--keep-fp32" in sys.argv:
        stage_fp32()
    if RAW.exists() and "--keep-raw" not in sys.argv:
        print("cleaning intermediates", flush=True)
        shutil.rmtree(RAW)

    print("\ndone ->", OUT)
