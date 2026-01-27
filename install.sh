#!/usr/bin/env bash
#############################################
# LeIndex Universal Installer
# Version: 5.0.0 - Rust Edition
# Platform: Linux/Unix
#
# One-line installer:
#   curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
#
# Or with wget:
#   wget -qO- https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
#
# Non-interactive mode:
#   curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash -s -- --yes
#############################################

set -euo pipefail

# ============================================================================
# CONFIGURATION
# ============================================================================
readonly SCRIPT_VERSION="5.0.0"
readonly PROJECT_NAME="LeIndex"
readonly PROJECT_SLUG="leindex"
readonly MIN_RUST_MAJOR=1
readonly MIN_RUST_MINOR=75
readonly REPO_URL="https://github.com/scooter-lacroix/leindex"
NONINTERACTIVE=false

# Installation paths
LEINDEX_HOME="${LEINDEX_HOME:-$HOME/.leindex}"
CONFIG_DIR="${LEINDEX_HOME}/config"
DATA_DIR="${LEINDEX_HOME}/data"
LOG_DIR="${LEINDEX_HOME}/logs"
# Install to XDG standard location which is in everyone's PATH
INSTALL_BIN_DIR="${HOME}/.local/bin"

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

    if command -v curl &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    elif command -v wget &> /dev/null; then
        wget -qO- https://sh.rustup.rs | sh -s -- -y
    else
        log_error "Neither curl nor wget found. Please install Rust manually:"
        echo "  Visit: https://rustup.rs/"
        exit 1
    fi

    # Source cargo environment
    source "$HOME/.cargo/env"

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

    # Check if we're in the LeIndex repository (local development)
    # First check for direct package with name = "leindex"
    if [[ -f "Cargo.toml" ]] && grep -q 'name = "'"$PROJECT_SLUG"'"' Cargo.toml 2>/dev/null; then
        log_info "Building from local LeIndex repository..."
        install_method="local"
        repo_dir="$(pwd)"
    # Check for workspace project (LeIndex uses a workspace structure)
    elif [[ -f "Cargo.toml" ]] && grep -q "\[workspace\]" Cargo.toml 2>/dev/null; then
        log_info "Building from local LeIndex workspace repository..."
        install_method="workspace"
        repo_dir="$(pwd)"
    # Check if we have a leindex crate in crates/ directory
    elif [[ -d "crates" ]] && [[ -f "crates/lepasserelle/Cargo.toml" ]] && grep -q "name = \"lepasserelle\"" crates/lepasserelle/Cargo.toml 2>/dev/null; then
        log_info "Building from local LeIndex repository..."
        install_method="local"
        repo_dir="$(pwd)"
    # Check if current directory is a Rust project with Cargo.toml
    elif [[ -f "Cargo.toml" ]] && cargo check --quiet 2>/dev/null; then
        log_info "Building from current directory..."
        install_method="source"
        repo_dir="$(pwd)"
    else
        # Not in a LeIndex repository - offer to clone from remote
        local should_clone=false
        if [[ "$NONINTERACTIVE" == true ]]; then
            log_info "Non-interactive mode: Cloning from remote..."
            should_clone=true
        else
            log_warn "Not in a LeIndex repository"
            echo ""
            echo "LeIndex can be installed from:"
            echo "  ${CYAN}1.${NC} A local clone (if you've already cloned the repository)"
            echo "  ${CYAN}2.${NC} Remote repository (requires git)"
            echo ""
            read -p "Install from remote? [y/N] " -n 1 -r
            echo ""
            if [[ $REPLY =~ ^[Yy]$ ]]; then
                should_clone=true
            fi
        fi

        if [[ "$should_clone" == true ]]; then
            log_info "Cloning LeIndex from $REPO_URL..."

            # Create temporary directory for clone
            local tmp_dir=$(mktemp -d)
            cd "$tmp_dir" || exit 1

            if git clone --depth 1 "$REPO_URL" leindex 2>&1 | tee -a "$INSTALL_LOG"; then
                cd leindex || exit 1
                repo_dir="$(pwd)"
                should_cleanup=true

                # Verify this is the Rust version
                if [[ ! -f "Cargo.toml" ]]; then
                    log_error "The cloned repository does not contain Rust code (no Cargo.toml found)"
                    log_error "You may have cloned the legacy Python version"
                    log_info "Please ensure the remote repository has been updated to the Rust version"
                    log_info "Or install from a local clone: git clone <rust-repo-url> && cd leindex && ./install.sh"
                    cd /
                    rm -rf "$tmp_dir"
                    exit 1
                fi

                log_success "Repository cloned to: $repo_dir"
            else
                log_error "Failed to clone repository"
                cd /
                rm -rf "$tmp_dir"
                exit 1
            fi
        else
            log_error "Installation cancelled"
            echo ""
            echo "To install LeIndex, first clone the repository:"
            echo "  ${CYAN}git clone $REPO_URL${NC}"
            echo "  ${CYAN}cd leindex${NC}"
            echo "  ${CYAN}./install.sh${NC}"
            echo ""
            echo "Make sure you're cloning the Rust version, not the legacy Python version."
            exit 0
        fi
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
        mkdir -p "$INSTALL_BIN_DIR"
        cp "$binary" "$INSTALL_BIN_DIR/"
        chmod +x "$INSTALL_BIN_DIR/$PROJECT_SLUG"
        log_success "Binary installed to: $INSTALL_BIN_DIR/$PROJECT_SLUG"
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

    local binary="$INSTALL_BIN_DIR/$PROJECT_SLUG"

    if [[ ! -f "$binary" ]]; then
        log_error "Binary not found: $binary"
        return 1
    fi

    if $binary --version &> /dev/null; then
        local version=$($binary --version 2>&1 || echo "unknown")
        log_success "Installation verified: $version"
        return 0
    else
        log_error "Installation verification failed"
        return 1
    fi
}

setup_directories() {
    print_step 4 4 "Setting up Directories"

    # Create install bin directory and other directories
    mkdir -p "$INSTALL_BIN_DIR"

    for dir in "$CONFIG_DIR" "$DATA_DIR" "$LOG_DIR"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            log_success "Created: $dir"
        fi
    done
}

update_path() {
    print_header "Update PATH"

    # ~/.local/bin should already be in PATH via standard XDG conventions
    # Most Linux distributions include it in default PATH
    log_info "Binary installed to ~/.local/bin (standard XDG location)"
    log_info "This location is typically already in your PATH"

    # Verify ~/.local/bin is in PATH
    if echo ":$PATH:" | grep -q ":$HOME/.local/bin:"; then
        log_success "~/.local/bin is already in PATH"
    else
        print_warning "~/.local/bin is not in your PATH"
        echo ""
        echo "Add the following to your shell configuration (~/.bashrc or ~/.zshrc):"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        echo ""
        echo "Then restart your shell or run:"
        echo "  source ~/.bashrc  # or ~/.zshrc"
    fi
}

# ============================================================================
# AI TOOL DETECTION
# ============================================================================

detect_ai_tools() {
    print_header "Detecting AI Tools"

    local detected_tools=()
    local detected_ides=()
    local detected_clis=()

    # === IDE Detection ===

    # Cursor
    if command -v cursor &> /dev/null || [[ -d "$HOME/.cursor" ]]; then
        detected_ides+=("Cursor")
    fi

    # VS Code
    if command -v code &> /dev/null || [[ -d "$HOME/.vscode" ]] || [[ -d "$HOME/.config/Code" ]]; then
        detected_ides+=("VS Code")
    fi

    # VSCodium
    if command -v codium &> /dev/null || [[ -d "$HOME/.config/VSCodium" ]]; then
        detected_ides+=("VSCodium")
    fi

    # Zed
    if command -v zed &> /dev/null || [[ -d "$HOME/.config/zed" ]]; then
        detected_ides+=("Zed")
    fi

    # Antigravity (uses VS Code config)
    if [[ -d "$HOME/.antigravity" ]]; then
        detected_ides+=("Antigravity")
    fi

    # === CLI Tool Detection ===

    # Claude Code
    if command -v claude &> /dev/null || [[ -n "${CLAUDE_CONFIG_DIR:-}" ]] || [[ -d "$HOME/.config/claude-code" ]]; then
        detected_clis+=("Claude Code")
    fi

    # Codex CLI
    if command -v codex &> /dev/null; then
        detected_clis+=("Codex CLI")
    fi

    # Amp Code
    if command -v amp &> /dev/null; then
        detected_clis+=("Amp Code")
    fi

    # Gemini CLI
    if command -v gemini &> /dev/null || command -v gemini-cli &> /dev/null; then
        detected_clis+=("Gemini CLI")
    fi

    # Opencode
    if command -v opencode &> /dev/null || [[ -d "$HOME/.config/opencode" ]]; then
        detected_clis+=("Opencode")
    fi

    # Droid
    if command -v droid &> /dev/null; then
        detected_clis+=("Droid")
    fi

    # Pi-mono
    if command -v pi &> /dev/null || command -v pi-mono &> /dev/null; then
        detected_clis+=("Pi-mono")
    fi

    # Goose
    if command -v goose &> /dev/null; then
        detected_clis+=("Goose")
    fi

    # Maestro
    if command -v maestro &> /dev/null; then
        detected_clis+=("Maestro")
    fi

    # Combine all detected tools
    detected_tools=("${detected_ides[@]}" "${detected_clis[@]}")

    if [[ ${#detected_tools[@]} -gt 0 ]]; then
        log_success "Detected AI tools:"
        echo ""

        if [[ ${#detected_ides[@]} -gt 0 ]]; then
            echo "  ${CYAN}IDEs:${NC}"
            for tool in "${detected_ides[@]}"; do
                print_bullet "$tool"
            done
            echo ""
        fi

        if [[ ${#detected_clis[@]} -gt 0 ]]; then
            echo "  ${CYAN}CLI Tools:${NC}"
            for tool in "${detected_clis[@]}"; do
                print_bullet "$tool"
            done
            echo ""
        fi

        log_info "LeIndex MCP server can be configured for these tools"
        echo ""
        show_mcp_config "${detected_ides[@]}" "${detected_clis[@]}"
    else
        print_warning "No AI tools detected"
        log_info "LeIndex will be installed as a standalone tool"
    fi
}

show_mcp_config() {
    local ides=("$@")
    local clis=("${ides[@]:${#ides[@]}}")

    print_header "MCP Server Configuration"

    echo "LeIndex runs as an HTTP-based MCP server on port 47268"
    echo ""
    echo "Start the server:"
    echo "  ${CYAN}leindex serve${NC}"
    echo ""
    echo "Or customize port:"
    echo "  ${CYAN}LEINDEX_PORT=3000 leindex serve${NC}"
    echo ""

    # Claude Code / Cursor / VS Code configuration
    if printf '%s\n' "${ides[@]}" | grep -qE "(Cursor|VS Code|VSCodium|Claude)"; then
        echo "For ${CYAN}Claude Code / Cursor / VS Code${NC}:"
        echo ""
        echo "Edit ~/.config/claude-code/mcp.json (or Cursor's settings.json):"
        echo ""
        echo "  {"
        echo "    \"mcpServers\": {"
        echo "      \"leindex\": {"
        echo "        \"type\": \"http\","
        echo "        \"url\": \"http://127.0.0.1:47268/mcp\""
        echo "      }"
        echo "    }"
        echo "  }"
        echo ""
    fi

    # Zed configuration
    if printf '%s\n' "${ides[@]}" | grep -q "Zed"; then
        echo "For ${CYAN}Zed${NC}:"
        echo ""
        echo "Edit ~/.config/zed/settings.json:"
        echo ""
        echo "  {"
        echo "    \"lsp\": {"
        echo "      \"leindex\": {"
        echo "        \"type\": \"http\","
        echo "        \"url\": \"http://127.0.0.1:47268/mcp\""
        echo "      }"
        echo "    }"
        echo "  }"
        echo ""
    fi

    # CLI tools configuration
    if [[ ${#clis[@]} -gt 0 ]]; then
        echo "For ${CYAN}CLI Tools${NC} (that support HTTP MCP servers):"
        echo ""
        echo "Configure with URL: ${CYAN}http://127.0.0.1:47268/mcp${NC}"
        echo ""
    fi

    echo "Note: The LeIndex server must be running for MCP integration to work."
    echo ""
    echo "Start it manually, or set up as a background service:"
    echo "  ${CYAN}nohup leindex serve > ~/.leindex/logs/server.log 2>&1 &${NC}"
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

main() {
    # Parse arguments
    for arg in "$@"; do
        case $arg in
            --yes|-y)
                NONINTERACTIVE=true
                ;;
            --help|-h)
                echo "LeIndex Installer v$SCRIPT_VERSION"
                echo ""
                echo "Usage: $0 [--yes|--help]"
                echo ""
                echo "Options:"
                echo "  --yes, -y    Non-interactive mode (auto-confirm all prompts)"
                echo "  --help, -h   Show this help message"
                echo ""
                exit 0
                ;;
        esac
    done

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
        if [[ "$NONINTERACTIVE" == true ]]; then
            log_info "Non-interactive mode: Installing Rust automatically..."
            install_rust
        else
            read -p "Would you like to install Rust now? [y/N] " -n 1 -r
            echo ""
            if [[ $REPLY =~ ^[Yy]$ ]]; then
                install_rust
            else
                log_error "Rust is required to build LeIndex"
                exit 1
            fi
        fi
    fi

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

    # Detect AI tools
    detect_ai_tools

    # Success message
    print_header "Installation Complete!"

    log_success "LeIndex has been installed successfully!"
    echo ""
    echo "  ${BOLD}Binary:${NC}       $INSTALL_BIN_DIR/$PROJECT_SLUG"
    echo "  ${BOLD}Config:${NC}       $CONFIG_DIR"
    echo "  ${BOLD}Data:${NC}         $DATA_DIR"
    echo "  ${BOLD}Install log:${NC}  $INSTALL_LOG"
    echo ""
    echo "To get started:"
    echo "  ${CYAN}1.${NC} Verify installation: ${YELLOW}$PROJECT_SLUG --version${NC}"
    echo "  ${CYAN}2.${NC} Index a project: ${YELLOW}$PROJECT_SLUG index /path/to/project${NC}"
    echo "  ${CYAN}3.${NC} Run diagnostics: ${YELLOW}$PROJECT_SLUG diagnostics${NC}"
    echo "  ${CYAN}4.${NC} Start MCP server: ${YELLOW}$PROJECT_SLUG serve${NC}"
    echo ""
    echo "For MCP server configuration, see the documentation."
    echo ""
    printf "${GREEN}Happy indexing!${NC} 🚀\n"
}

# Run main function
main "$@"
