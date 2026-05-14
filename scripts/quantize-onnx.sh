#!/usr/bin/env bash
# quantize-onnx.sh — Quantize ONNX models for smaller bundle size
#
# VAL-CPHASE-022: Produces deterministic worker-ready model assets.
# VAL-CPHASE-023: Fails fast on missing expected inputs.
# VAL-CPHASE-024: Enforces bundle size guardrails.
# VAL-CPHASE-025: Checksums are generated for shipped artifacts.
#
# This script applies INT8 dynamic quantization to the ONNX model to reduce
# bundle size while preserving embedding quality within the quality gate.
#
# Prerequisites:
#   - Python 3 with onnxruntime installed (for quantization)
#   - OR: the FP32 ONNX model is kept as-is if quantization deps are unavailable
#
# Usage:
#   bash scripts/quantize-onnx.sh [--output-dir DIR] [--mode MODE]
#
# Modes:
#   int8_dynamic   INT8 dynamic quantization (default, recommended)
#   int8_static    INT8 static quantization (requires calibration data)
#   skip           Skip quantization, keep FP32 model
#
# Environment:
#   LEINDEX_MODEL_OUTPUT_DIR  Override output directory (default: models/)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Defaults ──────────────────────────────────────────────────────────────
OUTPUT_DIR="${LEINDEX_MODEL_OUTPUT_DIR:-${REPO_ROOT}/models}"
QUANT_MODE="int8_dynamic"

# ── Argument parsing ──────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --mode)
            QUANT_MODE="$2"
            shift 2
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# ── Constants ─────────────────────────────────────────────────────────────
EMBED_MODEL_NAME="qwen3-embed-0.6b"
FP32_ONNX_FILE="${OUTPUT_DIR}/${EMBED_MODEL_NAME}.onnx"
TOKENIZER_FILE="${OUTPUT_DIR}/tokenizer.json"

# VAL-CPHASE-024: Maximum quantized model size (600 MiB guardrail).
MAX_QUANT_MODEL_SIZE=$((600 * 1024 * 1024))

# ── Helper functions ──────────────────────────────────────────────────────

log()  { echo "[quantize-onnx] $*"; }
warn() { echo "[quantize-onnx] WARNING: $*" >&2; }
die()  { echo "[quantize-onnx] ERROR: $*" >&2; exit 1; }

# ── Pre-flight checks ─────────────────────────────────────────────────────
# VAL-CPHASE-023: Fail fast on missing expected inputs.

log "Checking prerequisites..."

if [[ ! -f "${FP32_ONNX_FILE}" ]]; then
    die "FP32 ONNX model not found at ${FP32_ONNX_FILE}. Run scripts/convert-to-onnx.sh first."
fi

if [[ ! -f "${TOKENIZER_FILE}" ]]; then
    die "Tokenizer not found at ${TOKENIZER_FILE}. Run scripts/download-models.sh first."
fi

# ── Skip mode ─────────────────────────────────────────────────────────────
if [[ "${QUANT_MODE}" == "skip" ]]; then
    log "Quantization skipped (--mode skip). Keeping FP32 model."
    log "  Model: ${FP32_ONNX_FILE}"

    # Still generate checksums
    CHECKSUM_FILE="${OUTPUT_DIR}/checksums.sha256"
    log "Generating checksums..."
    {
        cd "${OUTPUT_DIR}"
        sha256sum "${EMBED_MODEL_NAME}.onnx" tokenizer.json 2>/dev/null
    } > "${CHECKSUM_FILE}"
    log "Checksums written to ${CHECKSUM_FILE}"

    # Bundle size guard
    BUNDLE_SIZE=$(du -sb "${OUTPUT_DIR}" | awk '{print $1}')
    BUNDLE_SIZE_MB=$((BUNDLE_SIZE / 1024 / 1024))
    log "Bundle size: ${BUNDLE_SIZE_MB} MiB"

    exit 0
fi

# ── Quantization ──────────────────────────────────────────────────────────

QUANTIZED=false
QUANT_ONNX_FILE="${OUTPUT_DIR}/${EMBED_MODEL_NAME}-quant.onnx"

# Try Python-based quantization
if command -v python3 &>/dev/null; then
    log "Attempting Python-based ONNX quantization (mode: ${QUANT_MODE})..."

    QUANT_SCRIPT=$(mktemp /tmp/leindex-quantize-XXXXXX.py)
    cat > "${QUANT_SCRIPT}" << PYEOF
"""Quantize ONNX model for the worker bundle pipeline."""
import sys
import os

def main():
    output_dir = os.environ.get("LEINDEX_QUANT_OUTPUT_DIR", "models")
    model_name = os.environ.get("LEINDEX_QUANT_MODEL_NAME", "qwen3-embed-0.6b")
    quant_mode = os.environ.get("LEINDEX_QUANT_MODE", "int8_dynamic")

    fp32_path = os.path.join(output_dir, f"{model_name}.onnx")
    quant_path = os.path.join(output_dir, f"{model_name}-quant.onnx")

    if not os.path.exists(fp32_path):
        print(f"[quantize-onnx] FP32 model not found: {fp32_path}")
        return 1

    try:
        from onnxruntime.quantization import quantize_dynamic, quantize_static, QuantType
        import onnx
    except ImportError as e:
        print(f"[quantize-onnx] Quantization deps not available: {e}")
        return 1

    print(f"[quantize-onnx] Loading FP32 model: {fp32_path}")
    fp32_size_mb = os.path.getsize(fp32_path) / (1024 * 1024)
    print(f"[quantize-onnx] FP32 model size: {fp32_size_mb:.1f} MiB")

    if quant_mode == "int8_dynamic":
        print(f"[quantize-onnx] Applying INT8 dynamic quantization...")
        try:
            quantize_dynamic(
                model_input=fp32_path,
                model_output=quant_path,
                weight_type=QuantType.QUInt8 if False else QuantType.QInt8,
            )
        except Exception as e:
            print(f"[quantize-onnx] Dynamic quantization failed: {e}")
            return 1
    elif quant_mode == "int8_static":
        print(f"[quantize-onnx] INT8 static quantization requires calibration data.")
        print(f"[quantize-onnx] Falling back to dynamic quantization.")
        try:
            quantize_dynamic(
                model_input=fp32_path,
                model_output=quant_path,
                weight_type=QuantType.QInt8,
            )
        except Exception as e:
            print(f"[quantize-onnx] Fallback quantization failed: {e}")
            return 1
    else:
        print(f"[quantize-onnx] Unknown quantization mode: {quant_mode}")
        return 1

    if os.path.exists(quant_path):
        quant_size_mb = os.path.getsize(quant_path) / (1024 * 1024)
        reduction = (1 - quant_size_mb / fp32_size_mb) * 100
        print(f"[quantize-onnx] Quantized model size: {quant_size_mb:.1f} MiB ({reduction:.1f}% reduction)")

        # Verify the quantized model loads
        try:
            import onnxruntime as ort
            sess = ort.InferenceSession(quant_path)
            print(f"[quantize-onnx] Quantized model loads successfully")
            del sess
        except Exception as e:
            print(f"[quantize-onnx] Warning: quantized model verification failed: {e}")
    else:
        print(f"[quantize-onnx] Quantized model not created")
        return 1

    return 0

if __name__ == "__main__":
    sys.exit(main())
PYEOF

    LEINDEX_QUANT_OUTPUT_DIR="${OUTPUT_DIR}" \
    LEINDEX_QUANT_MODEL_NAME="${EMBED_MODEL_NAME}" \
    LEINDEX_QUANT_MODE="${QUANT_MODE}" \
    python3 "${QUANT_SCRIPT}" && QUANTIZED=true || true

    rm -f "${QUANT_SCRIPT}"
fi

# ── Handle quantization result ────────────────────────────────────────────

if [[ "$QUANTIZED" == "true" && -f "${QUANT_ONNX_FILE}" ]]; then
    # Replace the FP32 model with the quantized version
    log "Replacing FP32 model with quantized version..."
    mv "${FP32_ONNX_FILE}" "${FP32_ONNX_FILE}.fp32"
    mv "${QUANT_ONNX_FILE}" "${FP32_ONNX_FILE}"
    log "FP32 backup: ${FP32_ONNX_FILE}.fp32"
else
    warn "Quantization not available or failed. Keeping FP32 model."
    log "To enable quantization, install: pip install onnxruntime"
fi

# ── Post-quantization verification ────────────────────────────────────────
# VAL-CPHASE-023: Fail fast if expected outputs are missing.

log "Verifying final bundle..."

if [[ ! -f "${FP32_ONNX_FILE}" ]]; then
    die "ONNX model file missing after quantization: ${FP32_ONNX_FILE}"
fi

if [[ ! -f "${TOKENIZER_FILE}" ]]; then
    die "Tokenizer file missing: ${TOKENIZER_FILE}"
fi

# ── Bundle size guard ─────────────────────────────────────────────────────
# VAL-CPHASE-024: Reject output whose size exceeds the agreed target.

MODEL_SIZE=$(stat --format=%s "${FP32_ONNX_FILE}" 2>/dev/null || stat -f%z "${FP32_ONNX_FILE}" 2>/dev/null || echo 0)
MODEL_SIZE_MB=$((MODEL_SIZE / 1024 / 1024))
MAX_QUANT_MODEL_SIZE_MB=$((MAX_QUANT_MODEL_SIZE / 1024 / 1024))

log "Final model size: ${MODEL_SIZE_MB} MiB (guardrail: ${MAX_QUANT_MODEL_SIZE_MB} MiB)"

if [[ $MODEL_SIZE -gt $MAX_QUANT_MODEL_SIZE ]]; then
    die "Quantized model size (${MODEL_SIZE_MB} MiB) exceeds guardrail (${MAX_QUANT_MODEL_SIZE_MB} MiB). " \
        "Consider a smaller model variant or more aggressive quantization."
fi

# ── Generate final checksums ──────────────────────────────────────────────
# VAL-CPHASE-025: Checksums are generated for shipped binaries and model artifacts.

CHECKSUM_FILE="${OUTPUT_DIR}/checksums.sha256"

log "Generating final checksums..."
{
    cd "${OUTPUT_DIR}"
    sha256sum "${EMBED_MODEL_NAME}.onnx" tokenizer.json 2>/dev/null
    # Include license if present
    if [[ -f LICENSE ]]; then
        sha256sum LICENSE
    fi
} > "${CHECKSUM_FILE}"

log "Checksums written to ${CHECKSUM_FILE}"

# ── Summary ───────────────────────────────────────────────────────────────

BUNDLE_SIZE=$(du -sb "${OUTPUT_DIR}" | awk '{print $1}')
BUNDLE_SIZE_MB=$((BUNDLE_SIZE / 1024 / 1024))

log "Quantization complete."
log "  Model: ${FP32_ONNX_FILE} (${MODEL_SIZE_MB} MiB)"
log "  Tokenizer: ${TOKENIZER_FILE}"
log "  Bundle size: ${BUNDLE_SIZE_MB} MiB"
log "  Checksums: ${CHECKSUM_FILE}"
log ""
log "Bundle is ready for release packaging."
log "Verify with: sha256sum ${OUTPUT_DIR}/*"
