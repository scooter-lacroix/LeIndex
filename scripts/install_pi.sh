#!/usr/bin/env bash
# LeIndex Pi-Mono Integration Installer

set -euo pipefail

# Get absolute path to the pi directory in LeIndex
PI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../pi" && pwd)"

# Find pi binary
PI_BIN="/home/stan/pi-mono/pi"

if [ ! -f "$PI_BIN" ]; then
    echo "Error: pi binary not found at $PI_BIN"
    exit 1
fi

echo "Installing LeIndex integration into pi..."
"$PI_BIN" install "$PI_DIR"

echo "Success! LeIndex skill and extension are now registered."
echo "You can use the 'leindex' skill in your next pi session."
