#!/usr/bin/env bash
# LeIndex Pi-Mono Integration Installer
# Installs LeIndex extensions and skills globally for the pi agent

set -euo pipefail

# Configuration
LEINDEX_PI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../pi" && pwd)"
GLOBAL_PI_DIR="$HOME/.pi/agent"
GLOBAL_EXT_DIR="$GLOBAL_PI_DIR/extensions"
GLOBAL_SKILLS_DIR="$GLOBAL_PI_DIR/skills"

echo "Installing LeIndex integration globally for pi..."

# 1. Ensure directories exist
mkdir -p "$GLOBAL_EXT_DIR"
mkdir -p "$GLOBAL_SKILLS_DIR"

# 2. Link the extension
# Auto-discovery works for ~/.pi/agent/extensions/*/index.ts
echo "Linking extension to $GLOBAL_EXT_DIR/leindex..."
ln -sfn "$LEINDEX_PI_DIR" "$GLOBAL_EXT_DIR/leindex"

# 3. Link the skill
# Auto-discovery works for ~/.pi/agent/skills/leindex/SKILL.md
echo "Linking skill to $GLOBAL_SKILLS_DIR/leindex..."
ln -sfn "$LEINDEX_PI_DIR/skills/leindex" "$GLOBAL_SKILLS_DIR/leindex"

echo ""
echo "Success! LeIndex is now integrated with pi globally."
echo "You can now run 'pi' from any directory and have access to:"
echo "  - Tools: leindex_index, leindex_search, leindex_analyze, etc."
echo "  - Command: /leindex"
echo "  - Skill: leindex"
