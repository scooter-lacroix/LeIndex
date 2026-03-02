#!/bin/bash
# Publish LeIndex workspace crates to crates.io in dependency order
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

# Version to publish
VERSION="0.1.0"

echo -e "${GREEN}=== LeIndex Crate Publishing Script ===${NC}"
echo "Publishing version: $VERSION"
echo ""

# Dependency order (base crates first)
CRATES=(
    "leparse"
    "legraphe"
    "lestockage"
    "lerecherche"
    "lephase"
    "lepasserelle"
    "leglobal"
    "leserve"
    "leedit"
    "levalidation"
)

# Function to update Cargo.toml for publishing
prepare_crate() {
    local crate=$1
    local crate_path="crates/$crate/Cargo.toml"
    
    echo -e "${YELLOW}Preparing $crate...${NC}"
    
    # Backup original
    cp "$crate_path" "$crate_path.backup"
    
    # Replace path dependencies with versioned dependencies for internal crates
    sed -i "s/leparse = { path = \"\.\.\/leparse\" }/leparse = { path = \"\.\.\/leparse\", version = \"$VERSION\" }/g" "$crate_path"
    sed -i "s/legraphe = { path = \"\.\.\/legraphe\" }/legraphe = { path = \"\.\.\/legraphe\", version = \"$VERSION\" }/g" "$crate_path"
    sed -i "s/lestockage = { path = \"\.\.\/lestockage\" }/lestockage = { path = \"\.\.\/lestockage\", version = \"$VERSION\" }/g" "$crate_path"
    sed -i "s/lerecherche = { path = \"\.\.\/lerecherche\" }/lerecherche = { path = \"\.\.\/lerecherche\", version = \"$VERSION\" }/g" "$crate_path"
    sed -i "s/lephase = { path = \"\.\.\/lephase\" }/lephase = { path = \"\.\.\/lephase\", version = \"$VERSION\" }/g" "$crate_path"
    
    echo -e "${GREEN}✓ $crate prepared${NC}"
}

# Function to restore original Cargo.toml
restore_crate() {
    local crate=$1
    local crate_path="crates/$crate/Cargo.toml"
    
    if [ -f "$crate_path.backup" ]; then
        mv "$crate_path.backup" "$crate_path"
        echo -e "${GREEN}✓ $crate restored${NC}"
    fi
}

# Function to publish a crate
publish_crate() {
    local crate=$1
    local crate_path="crates/$crate"
    
    echo -e "${YELLOW}Publishing $crate...${NC}"
    
    cd "$crate_path"
    
    if [ -n "$DRY_RUN" ]; then
        echo "Would run: cargo publish $DRY_RUN"
        cargo publish $DRY_RUN 2>&1 || true
    else
        cargo publish --allow-dirty 2>&1 || {
            echo -e "${RED}Failed to publish $crate${NC}"
            cd ../..
            return 1
        }
    fi
    
    cd ../..
    
    if [ -z "$DRY_RUN" ]; then
        echo -e "${GREEN}✓ $crate published${NC}"
        echo "Waiting for crates.io to update..."
        sleep 30  # Wait for crates.io index to update
    fi
}

# Main execution
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
    
    echo ""
    echo -e "${YELLOW}Preparing all crates...${NC}"
    for crate in "${CRATES[@]}"; do
        prepare_crate "$crate"
    done
    
    echo ""
    echo -e "${GREEN}Publishing crates in dependency order...${NC}"
    
    for crate in "${CRATES[@]}"; do
        echo ""
        echo "=========================================="
        echo -e "${YELLOW}Publishing $crate${NC}"
        echo "=========================================="
        publish_crate "$crate"
    done
    
    echo ""
    echo -e "${GREEN}=== Publishing Complete ===${NC}"
    echo ""
    
    # Restore original Cargo.toml files
    echo -e "${YELLOW}Restoring original Cargo.toml files...${NC}"
    for crate in "${CRATES[@]}"; do
        restore_crate "$crate"
    done
    
    echo ""
    echo -e "${GREEN}All crates published successfully!${NC}"
    echo ""
    echo "Users can now run: cargo install leindex"
}

# Cleanup function on exit
cleanup() {
    if [ -z "$DRY_RUN" ]; then
        echo ""
        echo -e "${YELLOW}Cleaning up...${NC}"
        for crate in "${CRATES[@]}"; do
            restore_crate "$crate" 2>/dev/null || true
        done
    fi
}
trap cleanup EXIT

# Run main
main
