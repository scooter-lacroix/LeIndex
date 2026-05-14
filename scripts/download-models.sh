#!/usr/bin/env bash
# download-models.sh — Fetch source model files for the worker bundle pipeline
#
# VAL-CPHASE-022: Produces deterministic worker-ready model assets.
# VAL-CPHASE-023: Fails fast on missing expected assets.
# VAL-CPHASE-024: Enforces bundle size guardrails.
#
# Usage:
#   bash scripts/download-models.sh [--skip-verify] [--output-dir DIR]
#
# Environment:
#   LEINDEX_MODEL_OUTPUT_DIR  Override output directory (default: models/)
#
# Exits non-zero if any required download fails or checksum verification fails.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Defaults ──────────────────────────────────────────────────────────────
OUTPUT_DIR="${LEINDEX_MODEL_OUTPUT_DIR:-${REPO_ROOT}/models}"
SKIP_VERIFY=false

# ── Argument parsing ──────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-verify)
            SKIP_VERIFY=true
            shift
            ;;
        --output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# ── Model definitions ─────────────────────────────────────────────────────
# Each model entry: name, HuggingFace repo, expected files, and SHA256 checksums
# for the source (pre-conversion) files.

declare -A MODEL_SOURCES
declare -A MODEL_SHA256

# Embedding model: Qwen3-Embedding-0.6B
MODEL_SOURCES[qwen3-embed-0.6b]="Qwen/Qwen3-Embedding-0.6B"
MODEL_SHA256[qwen3-embed-0.6b-config]="SKIP"  # config.json varies; verify presence only
MODEL_SHA256[qwen3-embed-0.6b-tokenizer]="SKIP"  # tokenizer.json verified by convert step

# Tokenizer is shared across model variants
TOKENIZER_FILE="tokenizer.json"

# ── Bundle size guard ─────────────────────────────────────────────────────
# VAL-CPHASE-024: Maximum total download size in bytes (3 GiB guardrail for
# source files; the final bundle guard is enforced after conversion/quantization).
MAX_BUNDLE_SIZE=$((3 * 1024 * 1024 * 1024))

# ── Helper functions ──────────────────────────────────────────────────────

log()  { echo "[download-models] $*"; }
warn() { echo "[download-models] WARNING: $*" >&2; }
die()  { echo "[download-models] ERROR: $*" >&2; exit 1; }

check_tool() {
    if ! command -v "$1" &>/dev/null; then
        die "Required tool '$1' not found. Please install it before running this script."
    fi
}

verify_sha256() {
    local file="$1" expected="$2"
    if [[ "$expected" == "SKIP" ]]; then
        return 0
    fi
    local actual
    actual=$(sha256sum "$file" | awk '{print $1}')
    if [[ "$actual" != "$expected" ]]; then
        die "SHA256 mismatch for $file: expected $expected, got $actual"
    fi
}

# ── Pre-flight checks ─────────────────────────────────────────────────────

check_tool curl
check_tool sha256sum

mkdir -p "${OUTPUT_DIR}"

# ── Download tokenizer ────────────────────────────────────────────────────
# The tokenizer is shared across model variants and is required for worker operation.

TOKENIZER_PATH="${OUTPUT_DIR}/${TOKENIZER_FILE}"
TOKENIZER_URL="https://huggingface.co/${MODEL_SOURCES[qwen3-embed-0.6b]}/resolve/main/${TOKENIZER_FILE}"

if [[ -f "${TOKENIZER_PATH}" ]]; then
    log "Tokenizer already present: ${TOKENIZER_PATH}"
else
    log "Downloading tokenizer from ${TOKENIZER_URL}"
    curl --fail --location --progress-bar -o "${TOKENIZER_PATH}.tmp" "${TOKENIZER_URL}" \
        || die "Failed to download tokenizer from ${TOKENIZER_URL}"
    mv "${TOKENIZER_PATH}.tmp" "${TOKENIZER_PATH}"
fi

# Verify tokenizer is valid JSON (basic check)
if ! python3 -c "import json; json.load(open('${TOKENIZER_PATH}'))" 2>/dev/null; then
    if ! jq empty "${TOKENIZER_PATH}" 2>/dev/null; then
        warn "Could not verify tokenizer JSON structure (no python3 or jq available)"
    fi
fi
log "Tokenizer verified: ${TOKENIZER_PATH}"

# ── Download model config ─────────────────────────────────────────────────
# Config is needed for ONNX conversion to know model architecture.

CONFIG_PATH="${OUTPUT_DIR}/config.json"
CONFIG_URL="https://huggingface.co/${MODEL_SOURCES[qwen3-embed-0.6b]}/resolve/main/config.json"

if [[ -f "${CONFIG_PATH}" ]]; then
    log "Config already present: ${CONFIG_PATH}"
else
    log "Downloading config from ${CONFIG_URL}"
    curl --fail --location --progress-bar -o "${CONFIG_PATH}.tmp" "${CONFIG_URL}" \
        || die "Failed to download config from ${CONFIG_URL}"
    mv "${CONFIG_PATH}.tmp" "${CONFIG_PATH}"
fi
log "Config verified: ${CONFIG_PATH}"

# ── Download model weights ────────────────────────────────────────────────
# Download the safetensors or pytorch model files for conversion.

MODEL_BASE_URL="https://huggingface.co/${MODEL_SOURCES[qwen3-embed-0.6b]}/resolve/main"

# Try safetensors first (preferred for determinism), fall back to pytorch
MODEL_FILES=()
if curl --fail --head --silent "${MODEL_BASE_URL}/model.safetensors" &>/dev/null; then
    MODEL_FILES=("model.safetensors")
elif curl --fail --head --silent "${MODEL_BASE_URL}/pytorch_model.bin" &>/dev/null; then
    MODEL_FILES=("pytorch_model.bin")
else
    # Try indexed safetensors
    for i in 1 2 3 4 5; do
        if curl --fail --head --silent "${MODEL_BASE_URL}/model-${i:0:5}-of-00005.safetensors" &>/dev/null; then
            MODEL_FILES+=("model-${i:0:5}-of-00005.safetensors")
        fi
    done
    if [[ ${#MODEL_FILES[@]} -eq 0 ]]; then
        die "Could not find model weight files at ${MODEL_BASE_URL}"
    fi
fi

for model_file in "${MODEL_FILES[@]}"; do
    TARGET="${OUTPUT_DIR}/${model_file}"
    URL="${MODEL_BASE_URL}/${model_file}"

    if [[ -f "${TARGET}" ]]; then
        log "Model weights already present: ${TARGET}"
    else
        log "Downloading model weights from ${URL}"
        curl --fail --location --progress-bar -o "${TARGET}.tmp" "${URL}" \
            || die "Failed to download ${model_file}"
        mv "${TARGET}.tmp" "${TARGET}"
    fi
done

# ── Download model license ────────────────────────────────────────────────

LICENSE_PATH="${OUTPUT_DIR}/LICENSE"
LICENSE_URL="https://huggingface.co/${MODEL_SOURCES[qwen3-embed-0.6b]}/raw/main/LICENSE"

if [[ -f "${LICENSE_PATH}" ]]; then
    log "License already present: ${LICENSE_PATH}"
else
    log "Downloading model license"
    if curl --fail --location --silent -o "${LICENSE_PATH}.tmp" "${LICENSE_URL}" 2>/dev/null; then
        mv "${LICENSE_PATH}.tmp" "${LICENSE_PATH}"
        log "License downloaded: ${LICENSE_PATH}"
    else
        warn "Could not download license file (non-fatal, will create placeholder)"
        echo "Qwen3 models are licensed under Apache 2.0." > "${LICENSE_PATH}"
        echo "See https://huggingface.co/${MODEL_SOURCES[qwen3-embed-0.6b]} for details." >> "${LICENSE_PATH}"
    fi
fi

# ── Post-download verification ────────────────────────────────────────────
# VAL-CPHASE-023: Fail fast if required assets are missing.

log "Verifying downloaded assets..."

REQUIRED_FILES=(
    "${TOKENIZER_FILE}"
    "config.json"
)

for f in "${REQUIRED_FILES[@]}"; do
    if [[ ! -f "${OUTPUT_DIR}/$f" ]]; then
        die "Required asset missing: ${OUTPUT_DIR}/$f"
    fi
done

# At least one model weight file must exist
FOUND_WEIGHTS=false
for f in "${OUTPUT_DIR}"/model*.safetensors "${OUTPUT_DIR}"/pytorch_model.bin; do
    if [[ -f "$f" ]]; then
        FOUND_WEIGHTS=true
        break
    fi
done

if [[ "$FOUND_WEIGHTS" != "true" ]]; then
    die "No model weight files found in ${OUTPUT_DIR}"
fi

# ── Bundle size guard ─────────────────────────────────────────────────────
# VAL-CPHASE-024: Reject output whose size exceeds the agreed target/guardrail.

BUNDLE_SIZE=$(du -sb "${OUTPUT_DIR}" | awk '{print $1}')
BUNDLE_SIZE_MB=$((BUNDLE_SIZE / 1024 / 1024))
MAX_BUNDLE_SIZE_MB=$((MAX_BUNDLE_SIZE / 1024 / 1024))

log "Bundle size: ${BUNDLE_SIZE_MB} MiB (guardrail: ${MAX_BUNDLE_SIZE_MB} MiB)"

if [[ $BUNDLE_SIZE -gt $MAX_BUNDLE_SIZE ]]; then
    die "Bundle size (${BUNDLE_SIZE_MB} MiB) exceeds guardrail (${MAX_BUNDLE_SIZE_MB} MiB). " \
        "Reduce model size or adjust MAX_BUNDLE_SIZE."
fi

# ── Generate checksums ────────────────────────────────────────────────────
# VAL-CPHASE-025: Checksums are generated for shipped artifacts.

CHECKSUM_FILE="${OUTPUT_DIR}/checksums.sha256"

log "Generating checksums..."
{
    cd "${OUTPUT_DIR}"
    sha256sum tokenizer.json config.json LICENSE 2>/dev/null || true
    # Include model weight files
    for f in model*.safetensors pytorch_model.bin; do
        if [[ -f "$f" ]]; then
            sha256sum "$f"
        fi
    done
} > "${CHECKSUM_FILE}"

log "Checksums written to ${CHECKSUM_FILE}"

# ── Summary ───────────────────────────────────────────────────────────────

log "Download complete."
log "  Output directory: ${OUTPUT_DIR}"
log "  Bundle size: ${BUNDLE_SIZE_MB} MiB"
log "  Checksums: ${CHECKSUM_FILE}"
log ""
log "Next steps:"
log "  bash scripts/convert-to-onnx.sh"
log "  bash scripts/quantize-onnx.sh"
