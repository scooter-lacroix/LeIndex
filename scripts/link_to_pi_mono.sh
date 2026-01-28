#!/usr/bin/env bash
# Link LeIndex to pi-mono for local development and indexing support

set -euo pipefail

# Configuration
PI_MONO_DIR="/home/stan/pi-mono"
LEINDEX_PI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../pi" && pwd)"

if [ ! -d "$PI_MONO_DIR" ]; then
    echo "Error: pi-mono directory not found at $PI_MONO_DIR"
    exit 1
fi

# 1. Create project-local link in pi-mono
PI_MONO_EXT_DIR="$PI_MONO_DIR/.pi/extensions"
mkdir -p "$PI_MONO_EXT_DIR"

echo "Linking LeIndex extension to pi-mono project-local extensions..."
ln -sfn "$LEINDEX_PI_DIR" "$PI_MONO_EXT_DIR/leindex"

# 2. Also register the skill globally for convenience
GLOBAL_SKILLS_DIR="$HOME/.pi/agent/skills"
mkdir -p "$GLOBAL_SKILLS_DIR"

echo "Linking LeIndex skill to global skills..."
ln -sfn "$LEINDEX_PI_DIR/skills/leindex" "$GLOBAL_SKILLS_DIR/leindex"

echo ""
echo "Success! LeIndex is now integrated with pi-mono."
echo "When you run 'pi' inside $PI_MONO_DIR, the LeIndex tools and command will be available."
echo "The 'leindex' skill is also available globally."
