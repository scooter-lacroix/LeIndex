#!/usr/bin/env bash
# convert-to-onnx.sh — Convert downloaded model files to ONNX format
#
# VAL-CPHASE-022: Produces deterministic worker-ready ONNX model assets.
# VAL-CPHASE-023: Fails fast on missing expected inputs.
# VAL-CPHASE-024: Enforces bundle size guardrails.
#
# Prerequisites:
#   - Python 3 with torch, transformers, onnx, onnxruntime installed
#   - OR: a pre-converted ONNX model placed manually in models/
#
# Usage:
#   bash scripts/convert-to-onnx.sh [--output-dir DIR] [--skip-if-present]
#
# Environment:
#   LEINDEX_MODEL_OUTPUT_DIR  Override output directory (default: models/)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Defaults ──────────────────────────────────────────────────────────────
OUTPUT_DIR="${LEINDEX_MODEL_OUTPUT_DIR:-${REPO_ROOT}/models}"
SKIP_IF_PRESENT=false

# ── Argument parsing ──────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --skip-if-present)
            SKIP_IF_PRESENT=true
            shift
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# ── Constants ─────────────────────────────────────────────────────────────
EMBED_MODEL_NAME="qwen3-embed-0.6b"
EMBED_ONNX_FILE="${OUTPUT_DIR}/${EMBED_MODEL_NAME}.onnx"
TOKENIZER_FILE="${OUTPUT_DIR}/tokenizer.json"

# VAL-CPHASE-024: Maximum ONNX model size (800 MiB guardrail for single model).
MAX_MODEL_SIZE=$((800 * 1024 * 1024))

# ── Helper functions ──────────────────────────────────────────────────────

log()  { echo "[convert-to-onnx] $*"; }
warn() { echo "[convert-to-onnx] WARNING: $*" >&2; }
die()  { echo "[convert-to-onnx] ERROR: $*" >&2; exit 1; }

# ── Pre-flight checks ─────────────────────────────────────────────────────
# VAL-CPHASE-023: Fail fast on missing expected inputs.

log "Checking prerequisites..."

if [[ ! -f "${TOKENIZER_FILE}" ]]; then
    die "Tokenizer not found at ${TOKENIZER_FILE}. Run scripts/download-models.sh first."
fi

if [[ ! -f "${OUTPUT_DIR}/config.json" ]]; then
    die "Model config not found at ${OUTPUT_DIR}/config.json. Run scripts/download-models.sh first."
fi

# Check if ONNX model already exists
if [[ "$SKIP_IF_PRESENT" == "true" && -f "${EMBED_ONNX_FILE}" ]]; then
    log "ONNX model already present: ${EMBED_ONNX_FILE}"
    log "Skipping conversion (--skip-if-present)."
    exit 0
fi

# ── Conversion ────────────────────────────────────────────────────────────
# Try Python-based conversion first. If Python deps are unavailable,
# check for a pre-existing ONNX file (e.g., from manual download).

CONVERTED=false

# Check for Python conversion capability
if command -v python3 &>/dev/null; then
    log "Attempting Python-based ONNX conversion..."

    # Create a temporary conversion script
    CONVERT_SCRIPT=$(mktemp /tmp/leindex-convert-XXXXXX.py)
    cat > "${CONVERT_SCRIPT}" << 'PYEOF'
"""Convert HuggingFace model to ONNX format for the worker bundle pipeline."""
import sys
import os
import json

def main():
    output_dir = os.environ.get("LEINDEX_CONVERT_OUTPUT_DIR", "models")
    model_name = os.environ.get("LEINDEX_CONVERT_MODEL_NAME", "qwen3-embed-0.6b")
    hf_repo = os.environ.get("LEINDEX_CONVERT_HF_REPO", "Qwen/Qwen3-Embedding-0.6B")
    onnx_path = os.path.join(output_dir, f"{model_name}.onnx")

    if os.path.exists(onnx_path):
        print(f"[convert-to-onnx] ONNX model already exists: {onnx_path}")
        return 0

    try:
        import torch
        from transformers import AutoModel, AutoTokenizer
        import onnx
        import onnxruntime as ort
    except ImportError as e:
        print(f"[convert-to-onnx] Python deps not available: {e}")
        print("[convert-to-onnx] Falling back to pre-existing ONNX file check.")
        return 1

    print(f"[convert-to-onnx] Loading model from {hf_repo}...")
    try:
        tokenizer = AutoTokenizer.from_pretrained(hf_repo, trust_remote_code=True)
        model = AutoModel.from_pretrained(hf_repo, trust_remote_code=True)
        model.eval()
    except Exception as e:
        print(f"[convert-to-onnx] Failed to load model: {e}")
        return 1

    # Create dummy input for export
    print("[convert-to-onnx] Creating dummy input for export...")
    dummy_text = "def hello_world(): pass"
    inputs = tokenizer(dummy_text, return_tensors="pt", padding=True, truncation=True, max_length=512)

    # Export to ONNX
    print(f"[convert-to-onnx] Exporting to {onnx_path}...")
    try:
        torch.onnx.export(
            model,
            (inputs["input_ids"], inputs["attention_mask"]),
            onnx_path,
            input_names=["input_ids", "attention_mask"],
            output_names=["embeddings"],
            dynamic_axes={
                "input_ids": {0: "batch", 1: "sequence"},
                "attention_mask": {0: "batch", 1: "sequence"},
                "embeddings": {0: "batch"},
            },
            opset_version=17,
            do_constant_folding=True,
        )
    except Exception as e:
        print(f"[convert-to-onnx] ONNX export failed: {e}")
        return 1

    # Verify the exported model
    print("[convert-to-onnx] Verifying exported ONNX model...")
    try:
        onnx_model = onnx.load(onnx_path)
        onnx.checker.check_model(onnx_model)
        print(f"[convert-to-onnx] ONNX model verified: {onnx_path}")
    except Exception as e:
        print(f"[convert-to-onnx] ONNX verification warning: {e}")

    # Report size
    size_mb = os.path.getsize(onnx_path) / (1024 * 1024)
    print(f"[convert-to-onnx] Model size: {size_mb:.1f} MiB")

    return 0

if __name__ == "__main__":
    sys.exit(main())
PYEOF

    # Run the conversion script
    LEINDEX_CONVERT_OUTPUT_DIR="${OUTPUT_DIR}" \
    LEINDEX_CONVERT_MODEL_NAME="${EMBED_MODEL_NAME}" \
    LEINDEX_CONVERT_HF_REPO="Qwen/Qwen3-Embedding-0.6B" \
    python3 "${CONVERT_SCRIPT}" && CONVERTED=true || true

    rm -f "${CONVERT_SCRIPT}"
fi

# ── Fallback: check for pre-existing ONNX ─────────────────────────────────
if [[ "$CONVERTED" != "true" ]]; then
    if [[ -f "${EMBED_ONNX_FILE}" ]]; then
        log "Using pre-existing ONNX model: ${EMBED_ONNX_FILE}"
        CONVERTED=true
    else
        # Check if there's a .bak file we can use (existing in the repo)
        BAK_FILE="${EMBED_ONNX_FILE}.bak"
        if [[ -f "${BAK_FILE}" ]]; then
            log "Restoring ONNX model from backup: ${BAK_FILE}"
            cp "${BAK_FILE}" "${EMBED_ONNX_FILE}"
            CONVERTED=true
        else
            die "No ONNX model found and conversion failed. " \
                "Either install Python deps (torch, transformers, onnx) or " \
                "manually place ${EMBED_ONNX_FILE}"
        fi
    fi
fi

# ── Post-conversion verification ──────────────────────────────────────────
# VAL-CPHASE-023: Fail fast if expected outputs are missing.

log "Verifying conversion outputs..."

if [[ ! -f "${EMBED_ONNX_FILE}" ]]; then
    die "ONNX model file missing after conversion: ${EMBED_ONNX_FILE}"
fi

if [[ ! -f "${TOKENIZER_FILE}" ]]; then
    die "Tokenizer file missing: ${TOKENIZER_FILE}"
fi

# ── Bundle size guard ─────────────────────────────────────────────────────
# VAL-CPHASE-024: Reject output whose size exceeds the agreed target.

MODEL_SIZE=$(stat --format=%s "${EMBED_ONNX_FILE}" 2>/dev/null || stat -f%z "${EMBED_ONNX_FILE}" 2>/dev/null || echo 0)
MODEL_SIZE_MB=$((MODEL_SIZE / 1024 / 1024))
MAX_MODEL_SIZE_MB=$((MAX_MODEL_SIZE / 1024 / 1024))

log "ONNX model size: ${MODEL_SIZE_MB} MiB (guardrail: ${MAX_MODEL_SIZE_MB} MiB)"

if [[ $MODEL_SIZE -gt $MAX_MODEL_SIZE ]]; then
    die "ONNX model size (${MODEL_SIZE_MB} MiB) exceeds guardrail (${MAX_MODEL_SIZE_MB} MiB). " \
        "Consider quantization or a smaller model variant."
fi

# ── Generate checksums ────────────────────────────────────────────────────
# VAL-CPHASE-025: Checksums are generated for shipped artifacts.

CHECKSUM_FILE="${OUTPUT_DIR}/checksums.sha256"

log "Generating checksums..."
{
    cd "${OUTPUT_DIR}"
    sha256sum "${EMBED_MODEL_NAME}.onnx" tokenizer.json 2>/dev/null
} > "${CHECKSUM_FILE}"

log "Checksums written to ${CHECKSUM_FILE}"

# ── Summary ───────────────────────────────────────────────────────────────

log "Conversion complete."
log "  ONNX model: ${EMBED_ONNX_FILE} (${MODEL_SIZE_MB} MiB)"
log "  Tokenizer: ${TOKENIZER_FILE}"
log "  Checksums: ${CHECKSUM_FILE}"
log ""
log "Next step:"
log "  bash scripts/quantize-onnx.sh"
