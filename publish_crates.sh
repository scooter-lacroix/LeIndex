#!/bin/bash
# Publish the LeIndex crate to crates.io.
#
# Post-embed-merge surface: a single `leindex` crate carries BOTH the main
# binary and the ONNX worker (`[[bin]] leindex-embed`), so `cargo install
# leindex --features onnx` installs both. There are no separate workspace
# crates to publish in dependency order anymore.
#
# Usage: ./publish_crates.sh [--dry-run]

set -e

DRY_RUN=""
if [ "$1" == "--dry-run" ]; then
    DRY_RUN="--dry-run"
    echo "Running in DRY-RUN mode (no actual publishing)"
fi

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Version comes from Cargo.toml (single source of truth, never hardcoded).
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')

echo -e "${GREEN}=== LeIndex Crate Publishing Script ===${NC}"
echo "Publishing version: $VERSION"
echo ""

main() {
    # Verify authentication
    if [ -z "$DRY_RUN" ]; then
        echo "Verifying crates.io authentication..."
        cargo whoami 2>/dev/null || {
            echo -e "${RED}Error: Not authenticated with crates.io${NC}"
            echo "Run: cargo login"
            exit 1
        }
    fi

    if [ -n "$DRY_RUN" ]; then
        echo -e "${YELLOW}Would run: cargo publish $DRY_RUN${NC}"
        cargo publish $DRY_RUN 2>&1 || true
    else
        # If the version is already on crates.io, skip (mirrors release.yml).
        if cargo search leindex 2>/dev/null | grep -q "^leindex = \"${VERSION}\""; then
            echo -e "${GREEN}✓ leindex ${VERSION} already published — skipping${NC}"
        else
            echo -e "${YELLOW}Publishing leindex ${VERSION}...${NC}"
            cargo publish --allow-dirty 2>&1 || {
                echo -e "${RED}Failed to publish leindex${NC}"
                exit 1
            }
            echo -e "${GREEN}✓ leindex ${VERSION} published${NC}"
            echo "Waiting for crates.io index to update..."
            sleep 30
        fi
    fi

    echo ""
    echo -e "${GREEN}All crates published successfully!${NC}"
    echo ""
    echo "Users can now run: cargo install leindex --features onnx"
}

# Run main
main
