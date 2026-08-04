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
        echo -e "${YELLOW}Would run: cargo publish --allow-dirty $DRY_RUN${NC}"
        # No `|| true` here: a validation failure must propagate as a
        # non-zero status instead of reporting completion. `set -e` turns
        # the cargo failure into the script's exit status.
        cargo publish --allow-dirty $DRY_RUN 2>&1
    else
        # If the exact, non-yanked version is already on crates.io, skip.
        # The registry search service can lag after the API and sparse index
        # are already current, so use the exact version endpoint instead.
        CRATES_API_URL="https://crates.io/api/v1/crates/leindex/${VERSION}"
        CHECK_BODY="$(mktemp)"
        trap 'rm -f "$CHECK_BODY"' EXIT
        HTTP_STATUS=""
        # Query the exact-version endpoint with bounded connect/request
        # timeouts, retrying transient failures. Only an explicit HTTP 404
        # means "not published yet" and licenses us to publish; timeouts,
        # network/TLS failures, other HTTP statuses, and invalid JSON abort
        # with an error rather than publishing into an unknown state.
        for attempt in 1 2 3; do
            if HTTP_STATUS="$(curl -sS -o "$CHECK_BODY" -w '%{http_code}' \
                --connect-timeout 10 --max-time 30 \
                -H "User-Agent: LeIndex publish helper" "$CRATES_API_URL" 2>/dev/null)"; then
                if [ "$HTTP_STATUS" = "404" ] || [ "$HTTP_STATUS" = "200" ]; then
                    break
                fi
                echo -e "${YELLOW}Warning: crates.io API returned HTTP ${HTTP_STATUS} (attempt ${attempt}/3); retrying...${NC}"
            else
                echo -e "${YELLOW}Warning: crates.io API unreachable (attempt ${attempt}/3); retrying...${NC}"
                HTTP_STATUS=""
            fi
            [ "$attempt" -lt 3 ] && sleep 3
        done
        case "$HTTP_STATUS" in
            404)
                echo -e "${YELLOW}Publishing leindex ${VERSION}...${NC}"
                cargo publish --allow-dirty 2>&1 || {
                    echo -e "${RED}Failed to publish leindex${NC}"
                    exit 1
                }
                echo -e "${GREEN}✓ leindex ${VERSION} published${NC}"
                echo "Waiting for crates.io index to update..."
                sleep 30
                ;;
            200)
                if VERSION="$VERSION" python3 -c 'import json, os, sys; v=json.load(sys.stdin).get("version", {}); sys.exit(0 if v.get("num") == os.environ["VERSION"] and not v.get("yanked", False) else 1)' < "$CHECK_BODY"; then
                    echo -e "${GREEN}✓ leindex ${VERSION} already published — skipping${NC}"
                else
                    echo -e "${RED}Error: leindex ${VERSION} exists on crates.io but is yanked or the version does not match${NC}"
                    exit 1
                fi
                ;;
            *)
                echo -e "${RED}Error: could not verify publication status of leindex ${VERSION} (final HTTP status: ${HTTP_STATUS:-unreachable})${NC}"
                exit 1
                ;;
        esac
    fi

    echo ""
    echo -e "${GREEN}leindex ${VERSION} publish complete!${NC}"
    echo ""
    echo "Users can now run: cargo install leindex --features onnx"
}

# Run main
main
