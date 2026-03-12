#!/usr/bin/env bash
#############################################
# LeIndex Universal Installer
# Version: 5.1.0 - Rust Edition
# Platform: macOS
#
# Installer:
#   curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install_macos.sh -o install-leindex-macos.sh
#   bash install-leindex-macos.sh
#
# Cargo install alternative:
#   cargo install leindex
#############################################

set -euo pipefail

# ============================================================================
# CONFIGURATION
# ============================================================================
readonly SCRIPT_VERSION="5.1.0"
readonly PROJECT_NAME="LeIndex"
readonly PROJECT_SLUG="leindex"
readonly MIN_RUST_MAJOR=1
readonly MIN_RUST_MINOR=75
readonly REPO_URL="https://github.com/scooter-lacroix/LeIndex"

# Installation paths
LEINDEX_HOME="${LEINDEX_HOME:-$HOME/.leindex}"
CONFIG_DIR="${LEINDEX_HOME}/config"
DATA_DIR="${LEINDEX_HOME}/data"
LOG_DIR="${LEINDEX_HOME}/logs"
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
CARGO_ENV_FILE="${CARGO_HOME_DIR}/env"
INSTALL_BIN_DIR="${CARGO_HOME_DIR}/bin"
INSTALL_BIN_PATH="${INSTALL_BIN_DIR}/${PROJECT_SLUG}"
LEGACY_LOCAL_BIN_PATH="${HOME}/.local/bin/${PROJECT_SLUG}"
LEGACY_LEINDEX_HOME_BIN_PATH="${LEINDEX_HOME}/bin/${PROJECT_SLUG}"
STAR_MARKER_PATH="${LEINDEX_HOME}/.github-starred"

# ============================================================================
# COLOR OUTPUT
# ============================================================================
readonly RED=$'\033[0;31m'
readonly GREEN=$'\033[0;32m'
readonly BLUE=$'\033[0;34m'
readonly YELLOW=$'\033[1;33m'
readonly CYAN=$'\033[0;36m'
readonly MAGENTA=$'\033[0;35m'
readonly BOLD=$'\033[1m'
readonly DIM=$'\033[2m'
readonly NC=$'\033[0m'

# ============================================================================
# LOGGING SYSTEM
# ============================================================================

# Create log directory
mkdir -p "$LOG_DIR"
INSTALL_LOG="$LOG_DIR/install-$(date +%Y%m%d-%H%M%S).log"

echo "=== LeIndex Installation Log ===" > "$INSTALL_LOG"
echo "Date: $(date)" >> "$INSTALL_LOG"
echo "Script Version: $SCRIPT_VERSION" >> "$INSTALL_LOG"
echo "================================" >> "$INSTALL_LOG"
echo "" >> "$INSTALL_LOG"

log_info() {
    local msg="$*"
    echo "[INFO] $msg" >> "$INSTALL_LOG"
    printf "${CYAN}[INFO]${NC} %s\n" "$msg"
}

log_error() {
    local msg="$*"
    echo "[ERROR] $msg" >> "$INSTALL_LOG"
    printf "${RED}[ERROR]${NC} %s\n" "$msg"
}

log_warn() {
    local msg="$*"
    echo "[WARN] $msg" >> "$INSTALL_LOG"
    printf "${YELLOW}[WARN]${NC} %s\n" "$msg"
}

log_success() {
    local msg="$*"
    echo "[SUCCESS] $msg" >> "$INSTALL_LOG"
    printf "${GREEN}[SUCCESS]${NC} %s\n" "$msg"
}

source_cargo_env_if_present() {
    if [[ -f "$CARGO_ENV_FILE" ]]; then
        # shellcheck disable=SC1090
        source "$CARGO_ENV_FILE"
    fi
}

ensure_cargo_home_ready() {
    source_cargo_env_if_present

    if ! command -v cargo &> /dev/null; then
        log_warn "Cargo is not available and $INSTALL_BIN_DIR does not exist yet."
        echo ""
        read -p "Install Rust/Cargo and create $INSTALL_BIN_DIR now? [y/N] " -n 1 -r
        echo ""
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            install_rust
        else
            log_error "Cargo is required to install LeIndex into $INSTALL_BIN_DIR"
            exit 1
        fi
    fi

    mkdir -p "$INSTALL_BIN_DIR"
}

cleanup_legacy_binary_locations() {
    if [[ -f "$LEGACY_LOCAL_BIN_PATH" ]] && [[ "$LEGACY_LOCAL_BIN_PATH" != "$INSTALL_BIN_PATH" ]]; then
        if rm -f "$LEGACY_LOCAL_BIN_PATH" 2>/dev/null; then
            log_success "Removed legacy install: $LEGACY_LOCAL_BIN_PATH"
        else
            log_warn "Could not remove legacy install: $LEGACY_LOCAL_BIN_PATH"
        fi
    fi

    if [[ -f "$LEGACY_LEINDEX_HOME_BIN_PATH" ]] && [[ "$LEGACY_LEINDEX_HOME_BIN_PATH" != "$INSTALL_BIN_PATH" ]]; then
        if rm -f "$LEGACY_LEINDEX_HOME_BIN_PATH" 2>/dev/null; then
            log_success "Removed legacy install: $LEGACY_LEINDEX_HOME_BIN_PATH"
        else
            log_warn "Could not remove legacy install: $LEGACY_LEINDEX_HOME_BIN_PATH"
        fi
    fi
}

report_path_resolution() {
    local resolved_path
    resolved_path="$(command -v "$PROJECT_SLUG" 2>/dev/null || true)"

    if [[ -z "$resolved_path" ]]; then
        print_warning "$PROJECT_SLUG is not currently resolvable on PATH"
        echo "  Remediation: source \"$CARGO_ENV_FILE\" or add $INSTALL_BIN_DIR to PATH"
        return 0
    fi

    if [[ "$resolved_path" == "$INSTALL_BIN_PATH" ]]; then
        log_success "$PROJECT_SLUG resolves to the installed cargo binary"
        return 0
    fi

    print_warning "$PROJECT_SLUG currently resolves to $resolved_path instead of $INSTALL_BIN_PATH"
    echo "  Remediation: remove the older binary or put $INSTALL_BIN_DIR earlier in PATH"
    echo "  Check duplicates with: which -a $PROJECT_SLUG"
}

maybe_star_repo() {
    mkdir -p "$LEINDEX_HOME"

    echo ""
    log_info "Thank you for installing LeIndex."

    if [[ -f "$STAR_MARKER_PATH" ]]; then
        log_success "GitHub star already recorded for this installation."
        return 0
    fi

    if command -v gh &> /dev/null && gh auth status >/dev/null 2>&1; then
        if gh api -X PUT \
            -H "Accept: application/vnd.github+json" \
            "user/starred/scooter-lacroix/LeIndex" >/dev/null 2>&1; then
            : > "$STAR_MARKER_PATH"
            log_success "Starred scooter-lacroix/LeIndex on GitHub"
            return 0
        fi
    fi

    log_warn "Could not star the repository automatically. You can star it here: $REPO_URL"
}

print_header() {
    local title="$1"
    echo ""
    printf "${BOLD}${CYAN}═══════════════════════════════════════════════════${NC}\n"
    printf "${BOLD}${CYAN}  %s${NC}\n" "$title"
    printf "${BOLD}${CYAN}═══════════════════════════════════════════════════${NC}\n"
    echo ""
}

print_step() {
    local current=$1
    local total=$2
    local description="$3"
    printf "${BLUE}[Step %d/%d]${NC} %s\n" "$current" "$total" "$description"
}

print_bullet() {
    printf "  ${GREEN}✓${NC} %s\n" "$*"
}

print_warning() {
    printf "${YELLOW}⚠${NC}  %s\n" "$*"
}

# ============================================================================
# RUST DETECTION
# ============================================================================

detect_rust() {
    if command -v rustc &> /dev/null; then
        RUSTC_VERSION=$(rustc --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        RUST_MAJOR=$(echo "$RUSTC_VERSION" | cut -d. -f1)
        RUST_MINOR=$(echo "$RUSTC_VERSION" | cut -d. -f2)

        if [[ $RUST_MAJOR -gt $MIN_RUST_MAJOR ]] || \
           [[ $RUST_MAJOR -eq $MIN_RUST_MAJOR && $RUST_MINOR -ge $MIN_RUST_MINOR ]]; then
            log_success "Rust $RUSTC_VERSION detected"
            return 0
        else
            log_error "Rust $RUSTC_VERSION is too old. Minimum required: $MIN_RUST_MAJOR.$MIN_RUST_MINOR"
            return 1
        fi
    else
        log_error "Rust not found. Please install Rust first."
        return 1
    fi
}

install_rust() {
    print_header "Installing Rust Toolchain"

    log_info "Downloading rustup installer..."

    local rustup_script
    rustup_script="$(mktemp)"

    if command -v curl &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$rustup_script"
    elif command -v wget &> /dev/null; then
        wget -qO "$rustup_script" https://sh.rustup.rs
    else
        log_error "Neither curl nor wget found. Please install Rust manually:"
        echo "  Visit: https://rustup.rs/"
        exit 1
    fi

    sh "$rustup_script" -y
    rm -f "$rustup_script"

    # Source cargo environment
    source_cargo_env_if_present

    if detect_rust; then
        log_success "Rust installed successfully"
    else
        log_error "Rust installation failed"
        exit 1
    fi
}

# ============================================================================
# INSTALLATION
# ============================================================================

install_leindex() {
    print_step 2 4 "Building LeIndex"

    local install_method=""
    local repo_dir=""
    local should_cleanup=false

    # Check if we're in the repo directory
    if [[ -f "Cargo.toml" ]] && grep -q "$PROJECT_SLUG" Cargo.toml 2>/dev/null; then
        log_info "Building from current directory..."
        install_method="source"
        repo_dir="$(pwd)"
    else
        # Not in repo - clone it first
        log_info "Cloning LeIndex repository..."

        # Create a temporary directory for cloning
        local tmp_dir
        tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t 'leindex-install')
        repo_dir="$tmp_dir/leindex"

        # Clone the repository
        if command -v git &> /dev/null; then
            if git clone --depth 1 "$REPO_URL" "$repo_dir" 2>&1 | tee -a "$INSTALL_LOG"; then
                log_success "Repository cloned to: $repo_dir"
                should_cleanup=true
            else
                log_error "Failed to clone repository"
                rm -rf "$tmp_dir"
                exit 1
            fi
        else
            log_error "git not found. Please install git first:"
            echo "  Visit: https://git-scm.com/"
            rm -rf "$tmp_dir"
            exit 1
        fi

        cd "$repo_dir" || {
            log_error "Failed to enter repository directory"
            rm -rf "$tmp_dir"
            exit 1
        }
    fi

    # Build from source
    log_info "Building LeIndex..."
    if cargo build --release --bins 2>&1 | tee -a "$INSTALL_LOG"; then
        log_success "Build completed successfully"
    else
        log_error "Build failed"
        if [[ "$should_cleanup" == true ]]; then
            cd /
            rm -rf "$repo_dir"
        fi
        exit 1
    fi

    # Install binary
    local binary="target/release/$PROJECT_SLUG"
    if [[ -f "$binary" ]]; then
        ensure_cargo_home_ready
        cp "$binary" "$INSTALL_BIN_PATH"
        chmod +x "$INSTALL_BIN_PATH"
        log_success "Binary installed to: $INSTALL_BIN_PATH"
        cleanup_legacy_binary_locations
    else
        log_error "Binary not found after build"
        if [[ "$should_cleanup" == true ]]; then
            cd /
            rm -rf "$repo_dir"
        fi
        exit 1
    fi

    # Clean up temporary clone if we created it
    if [[ "$should_cleanup" == true ]]; then
        log_info "Cleaning up temporary files..."
        cd /
        rm -rf "$repo_dir"
        log_success "Cleanup complete"
    fi
}

verify_installation() {
    print_step 3 4 "Verifying Installation"

    local binary="$INSTALL_BIN_PATH"

    if [[ ! -f "$binary" ]]; then
        log_error "Binary not found: $binary"
        return 1
    fi

    if ! $binary --version &> /dev/null; then
        log_error "Installation verification failed"
        return 1
    fi

    local version expected_version
    version=$($binary --version 2>&1 || echo "unknown")
    expected_version="$(grep -E '^version = ' Cargo.toml | head -1 | cut -d '\"' -f2)"
    log_success "Binary check passed: $version"

    if [[ -n "$expected_version" ]] && [[ "$version" != *"$expected_version"* ]]; then
        log_error "Installed binary version mismatch. Expected $expected_version, got: $version"
        return 1
    fi

    if ! LEINDEX_SKIP_POST_INSTALL_HOOK=1 $binary phase --help &> /dev/null; then
        log_error "Installed binary does not expose 'phase' command"
        return 1
    fi
    print_bullet "Phase command detected"

    if ! LEINDEX_SKIP_POST_INSTALL_HOOK=1 $binary mcp --help &> /dev/null; then
        log_error "Installed binary does not expose 'mcp' command"
        return 1
    fi
    print_bullet "MCP command detected"

    local tmp_project
    tmp_project=$(mktemp -d 2>/dev/null || mktemp -d -t 'leindex-smoke')
    mkdir -p "$tmp_project/src"
    printf "pub fn installer_smoke()->i32{1}\n" > "$tmp_project/src/lib.rs"

    if LEINDEX_SKIP_POST_INSTALL_HOOK=1 $binary phase --phase 1 --path "$tmp_project" --max-files 10 --max-chars 800 > /dev/null 2>&1; then
        print_bullet "Phase-analysis smoke test passed"
    else
        log_error "Phase-analysis smoke test failed"
        rm -rf "$tmp_project"
        return 1
    fi

    rm -rf "$tmp_project"
    log_success "Installation verified with feature smoke checks"
    return 0
}

setup_directories() {
    print_step 4 4 "Setting up Directories"

    for dir in "$CONFIG_DIR" "$DATA_DIR" "$LOG_DIR"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            log_success "Created: $dir"
        fi
    done
}

update_path() {
    print_header "Update PATH"

    local shell_config=""
    local bin_export="export PATH=\"$INSTALL_BIN_DIR:\$PATH\""

    # Detect shell and config file
    if [[ -n "${ZSH_VERSION:-}" ]]; then
        shell_config="$HOME/.zshrc"
    elif [[ -n "${BASH_VERSION:-}" ]]; then
        shell_config="$HOME/.bashrc"
    else
        shell_config="$HOME/.profile"
    fi

    # Check if PATH is already configured
    if grep -q "$INSTALL_BIN_DIR" "$shell_config" 2>/dev/null; then
        log_info "PATH already configured in $shell_config"
        return 0
    fi

    echo "" >> "$shell_config"
    echo "# LeIndex" >> "$shell_config"
    echo "$bin_export" >> "$shell_config"

    log_success "Added to PATH in $shell_config"
    print_warning "You may need to restart your shell or run:"
    echo "  source $shell_config"
}

# ============================================================================
# AI TOOL DETECTION
# ============================================================================

detect_ai_tools() {
    print_header "Detecting AI Tools"

    local detected_clis=()

    # Check for Claude Code
    if command -v claude &> /dev/null || [[ -n "${CLAUDE_CONFIG_DIR:-}" ]]; then
        detected_clis+=("claude")
    fi

    # Check for Cursor
    if command -v cursor &> /dev/null || [[ -d "$HOME/.cursor" ]]; then
        detected_clis+=("cursor")
    fi

    # Check for Windsurf
    if command -v windsurf &> /dev/null || [[ -d "$HOME/.windsurf" ]]; then
        detected_clis+=("windsurf")
    fi

    if [[ ${#detected_clis[@]} -gt 0 ]]; then
        log_success "Detected AI tools:"
        for tool in "${detected_clis[@]}"; do
            print_bullet "$tool"
        done
        echo ""
        log_info "LeIndex MCP server can be configured for these tools"
    else
        print_warning "No AI tools detected"
        log_info "LeIndex will be installed as a standalone tool"
    fi
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

main() {
    print_header "LeIndex Rust Installer"

    echo "  ${BOLD}Project:${NC}     $PROJECT_NAME"
    echo "  ${BOLD}Version:${NC}     $SCRIPT_VERSION"
    echo "  ${BOLD}Repository:${NC}  $REPO_URL"
    echo ""

    # Step 1: Check Rust
    print_step 1 4 "Checking Rust Toolchain"

    if ! detect_rust; then
        echo ""
        log_warn "Rust is not installed or is too old"
        echo ""
        read -p "Would you like to install Rust now? [y/N] " -n 1 -r
        echo ""
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            install_rust
        else
            log_error "Rust is required to build LeIndex"
            exit 1
        fi
    fi

    ensure_cargo_home_ready

    # Step 2: Build LeIndex
    install_leindex

    # Step 3: Verify
    if ! verify_installation; then
        log_error "Installation verification failed"
        exit 1
    fi

    # Step 4: Setup directories
    setup_directories

    # Update PATH
    update_path
    report_path_resolution

    # Detect AI tools
    detect_ai_tools

    maybe_star_repo

    # Success message
    print_header "Installation Complete!"

    log_success "LeIndex has been installed successfully!"
    echo ""
    echo "  ${BOLD}Binary:${NC}       $INSTALL_BIN_PATH"
    echo "  ${BOLD}Config:${NC}       $CONFIG_DIR"
    echo "  ${BOLD}Data:${NC}         $DATA_DIR"
    echo "  ${BOLD}Install log:${NC}  $INSTALL_LOG"
    echo ""
    echo "To get started:"
    echo "  ${CYAN}1.${NC} Restart your shell so ${YELLOW}$INSTALL_BIN_DIR${NC} is on PATH"
    echo "  ${CYAN}2.${NC} Verify installation: ${YELLOW}$PROJECT_SLUG --version${NC}"
    echo "  ${CYAN}3.${NC} Index a project: ${YELLOW}$PROJECT_SLUG index /path/to/project${NC}"
    echo "  ${CYAN}4.${NC} Run diagnostics: ${YELLOW}$PROJECT_SLUG diagnostics${NC}"
    echo ""
    echo "For MCP server configuration, see the documentation."
    echo ""
    printf "${GREEN}Happy indexing!${NC} 🚀\n"
}

# Run main function
main "$@"
