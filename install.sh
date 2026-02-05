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
# Install to standard system-wide location (requires sudo)
# This ensures leindex is accessible to all AI tools regardless of PATH
INSTALL_BIN_DIR="/usr/local/bin"

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
        log_info "Installing to system directory: $INSTALL_BIN_DIR"
        echo ""

        # /usr/local/bin should exist on all Unix systems, but check first
        if [[ ! -d "$INSTALL_BIN_DIR" ]]; then
            log_info "Creating $INSTALL_BIN_DIR (requires sudo)..."
            if ! sudo mkdir -p "$INSTALL_BIN_DIR" 2>/dev/null; then
                log_error "Failed to create $INSTALL_BIN_DIR (sudo required)"
                log_error "Please run: sudo mkdir -p $INSTALL_BIN_DIR"
                exit 1
            fi
        fi

        # Copy binary with sudo
        if sudo cp "$binary" "$INSTALL_BIN_DIR/" && sudo chmod +x "$INSTALL_BIN_DIR/$PROJECT_SLUG"; then
            log_success "Binary installed to: $INSTALL_BIN_DIR/$PROJECT_SLUG"
        else
            log_error "Failed to install binary (sudo required)"
            exit 1
        fi
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

    if ! $binary --version &> /dev/null; then
        log_error "Installation verification failed"
        return 1
    fi

    local version=$($binary --version 2>&1 || echo "unknown")
    log_success "Binary check passed: $version"

    # Ensure additive lephase and MCP command surfaces are present.
    if ! $binary phase --help &> /dev/null; then
        log_error "Installed binary does not expose 'phase' command"
        return 1
    fi
    print_bullet "Phase command detected"

    if ! $binary mcp --help &> /dev/null; then
        log_error "Installed binary does not expose 'mcp' command"
        return 1
    fi
    print_bullet "MCP command detected"

    # Smoke-test phase analysis against a tiny temporary project.
    local tmp_project
    tmp_project=$(mktemp -d)
    mkdir -p "$tmp_project/src"
    printf "pub fn installer_smoke()->i32{1}\n" > "$tmp_project/src/lib.rs"

    if $binary phase --phase 1 --path "$tmp_project" --max-files 10 --max-chars 800 > /dev/null 2>&1; then
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

    # Create LeIndex data directories (bin directory is created during install with sudo)
    for dir in "$CONFIG_DIR" "$DATA_DIR" "$LOG_DIR"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            log_success "Created: $dir"
        fi
    done
}

update_path() {
    print_header "Installation Location"

    # Binary is installed to /usr/local/bin which is always in PATH
    log_info "Binary installed to /usr/local/bin (system-wide location)"
    log_info "This location is in the standard PATH for all Unix-like systems"

    # Verify /usr/local/bin is in PATH
    if echo ":$PATH:" | grep -q ":/usr/local/bin:"; then
        log_success "/usr/local/bin is in PATH"
    else
        print_warning "/usr/local/bin is not in your PATH"
        echo ""
        echo "This is unusual. Add the following to your shell configuration:"
        echo "  export PATH=\"/usr/local/bin:\$PATH\""
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

    # LM Studio
    if [[ -f "$HOME/.lmstudio/mcp.json" ]] || [[ -d "$HOME/.lmstudio" ]]; then
        detected_clis+=("LM Studio")
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

        # In non-interactive mode, show config and exit
        if [[ "$NONINTERACTIVE" == true ]]; then
            log_info "Non-interactive mode: Skipping MCP configuration"
            echo ""
            show_mcp_config_instructions
            return 0
        fi

        # Ask user if they want to configure MCP for detected tools
        echo ""
        read -p "Would you like to configure LeIndex MCP server for these tools? [y/N] " -n 1 -r
        echo ""

        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            log_info "Skipping MCP configuration"
            echo ""
            show_mcp_config_instructions
            return 0
        fi

        # Configure selected tools
        configure_mcp_servers "${detected_tools[@]}"
    else
        print_warning "No AI tools detected"
        log_info "LeIndex will be installed as a standalone tool"
    fi
}

configure_mcp_servers() {
    local tools=("$@")

    print_header "Select Tools to Configure"

    echo "Select which tools to configure (enter numbers separated by spaces, or 'all'):"
    echo ""

    local i=1
    for tool in "${tools[@]}"; do
        echo "  ${CYAN}$i)${NC} $tool"
        ((i++))
    done
    echo ""
    echo "  ${CYAN}all)${NC} Configure all detected tools"

    read -p "Selection: " selection

    # Parse selection
    local selected_tools=()
    if [[ "$selection" == "all" ]]; then
        selected_tools=("${tools[@]}")
    else
        for num in $selection; do
            if [[ "$num" =~ ^[0-9]+$ ]] && [[ $num -ge 1 ]] && [[ $num -le ${#tools[@]} ]]; then
                selected_tools+=("${tools[$((num-1))]}")
            fi
        done
    fi

    if [[ ${#selected_tools[@]} -eq 0 ]]; then
        log_warn "No valid tools selected"
        return 0
    fi

    echo ""
    log_info "Configuring LeIndex MCP server for: ${selected_tools[*]}"
    echo ""

    # Configure each selected tool
    local success_count=0
    local failed_count=0
    local skipped_count=0

    for tool in "${selected_tools[@]}"; do
        echo -n "  Configuring $tool... "
        if configure_tool_mcp "$tool"; then
            echo "${GREEN}✓${NC} Done"
            ((success_count++))
        elif [[ $? -eq 2 ]]; then
            echo "${YELLOW}⊘${NC} Skipped (no config file found)"
            ((skipped_count++))
        else
            echo "${RED}✗${NC} Failed"
            ((failed_count++))
        fi
    done

    echo ""
    echo "Configuration Summary:"
    echo "  ${GREEN}✓${NC} $success_count tool(s) configured successfully"
    if [[ $skipped_count -gt 0 ]]; then
        echo "  ${YELLOW}⊘${NC} $skipped_count tool(s) skipped (no config file)"
    fi
    if [[ $failed_count -gt 0 ]]; then
        echo "  ${RED}✗${NC} $failed_count tool(s) failed to configure"
    fi
    echo ""

    if [[ $success_count -gt 0 ]]; then
        log_success "MCP configuration complete!"
        echo ""
        echo "Next steps:"
        echo "  1. Restart your AI tool(s) to load the new configuration"
        echo "  2. Start the LeIndex server: ${CYAN}leindex serve${NC}"
        echo "  3. Or run it in the background: ${CYAN}nohup leindex serve > ~/.leindex/logs/server.log 2>&1 &${NC}"
    fi
}

configure_tool_mcp() {
    local tool="$1"
    local config_file=""
    local backup_file=""

    case "$tool" in
        "Claude Code")
            config_file="$HOME/.config/claude-code/mcp.json"
            backup_file="$HOME/.config/claude-code/mcp.json.backup"
            configure_json_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "Cursor"|"VS Code"|"VSCodium")
            # Cursor and VS Code family
            config_file="$HOME/.cursor/settings.json"
            if [[ ! -f "$config_file" ]]; then
                config_file="$HOME/.config/Code/User/settings.json"
            fi
            if [[ ! -f "$config_file" ]]; then
                config_file="$HOME/.config/VSCodium/User/settings.json"
            fi
            backup_file="${config_file}.backup"
            configure_vscode_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "Zed")
            config_file="$HOME/.config/zed/settings.json"
            backup_file="${config_file}.backup"
            configure_zed_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "Opencode")
            config_file="$HOME/.config/opencode/opencode.json"
            backup_file="${config_file}.backup"
            configure_opencode_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "Antigravity")
            # Antigravity uses Cursor/VS Code config
            config_file="$HOME/.cursor/settings.json"
            backup_file="${config_file}.backup"
            configure_vscode_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "LM Studio")
            config_file="$HOME/.lmstudio/mcp.json"
            backup_file="${config_file}.backup"
            configure_lmstudio_mcp "$config_file" "$backup_file"
            return $?
            ;;
        *)
            # CLI tools that might have config files
            return 2  # Skipped
            ;;
    esac
}

# Backup a file before modification
backup_config_file() {
    local file="$1"
    local backup="$2"

    if [[ -f "$file" ]]; then
        cp "$file" "$backup"
        return 0
    fi
    return 1
}

# Configure Claude Code MCP (mcp.json format with stdio command)
configure_json_mcp() {
    local config_file="$1"
    local backup_file="$2"

    # Check if config exists
    if [[ ! -f "$config_file" ]]; then
        # Create parent directory if needed
        mkdir -p "$(dirname "$config_file")"
        # Create new config with leindex as subprocess (stdio mode)
        cat > "$config_file" << 'EOF'
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"]
    }
  }
}
EOF
        return 0
    fi

    # Backup existing config
    backup_config_file "$config_file" "$backup_file"

    # Use Python or jq to add leindex to mcpServers
    if command -v python3 &> /dev/null; then
        python3 <<PYTHON
import json
import sys

try:
    with open('$config_file', 'r') as f:
        config = json.load(f)
except:
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Use stdio command mode (subprocess) - AI tools will start/stop automatically
config['mcpServers']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

with open('$config_file', 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
PYTHON
        return $?
    elif command -v jq &> /dev/null; then
        # Fallback to jq if python3 is not available
        jq '.mcpServers.leindex = {"command": "leindex", "args": ["mcp"]}' "$config_file" > "${config_file}.tmp" && mv "${config_file}.tmp" "$config_file"
        return $?
    else
        return 1
    fi
}

# Configure VS Code/Cursor MCP (settings.json with mcpServers)
configure_vscode_mcp() {
    local config_file="$1"
    local backup_file="$2"

    # Check if config exists
    if [[ ! -f "$config_file" ]]; then
        return 2  # Skipped
    fi

    # Backup existing config
    backup_config_file "$config_file" "$backup_file"

    # VS Code uses the same mcp.json format as Claude Code
    # but the file is in a different location
    local mcp_config_dir="$(dirname "$config_file")"
    local mcp_config_file="$mcp_config_dir/mcp.json"

    if [[ ! -f "$mcp_config_file" ]]; then
        # Create mcp.json in the same directory
        cat > "$mcp_config_file" << 'EOF'
{
  "mcpServers": {
    "leindex": {
      "type": "http",
      "url": "http://127.0.0.1:47268/mcp"
    }
  }
}
EOF
        return 0
    fi

    # Update existing mcp.json
    configure_json_mcp "$mcp_config_file" "${mcp_config_file}.backup"
    return $?
}

# Configure Zed MCP (LSP format)
configure_zed_mcp() {
    local config_file="$1"
    local backup_file="$2"

    # Check if config exists
    if [[ ! -f "$config_file" ]]; then
        return 2  # Skipped
    fi

    # Backup existing config
    backup_config_file "$config_file" "$backup_file"

    # Zed uses a different config format
    if command -v python3 &> /dev/null; then
        python3 <<PYTHON
import json
import sys

try:
    with open('$config_file', 'r') as f:
        config = json.load(f)
except:
    config = {}

if 'lsp' not in config:
    config['lsp'] = {}

config['lsp']['leindex'] = {
    'type': 'http',
    'url': 'http://127.0.0.1:47268/mcp'
}

with open('$config_file', 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
PYTHON
        return $?
    elif command -v jq &> /dev/null; then
        jq '.lsp.leindex = {"type": "http", "url": "http://127.0.0.1:47268/mcp"}' "$config_file" > "${config_file}.tmp" && mv "${config_file}.tmp" "$config_file"
        return $?
    else
        return 1
    fi
}

# Configure Opencode MCP
configure_opencode_mcp() {
    local config_file="$1"
    local backup_file="$2"

    # Check if config exists
    if [[ ! -f "$config_file" ]]; then
        return 2  # Skipped
    fi

    # Backup existing config
    backup_config_file "$config_file" "$backup_file"

    # Opencode MCP format: command array, type: local
    if command -v python3 &> /dev/null; then
        python3 <<PYTHON
import json
import sys

try:
    with open('$config_file', 'r') as f:
        config = json.load(f)
except:
    config = {}

if 'mcp' not in config:
    config['mcp'] = {}

config['mcp']['leindex'] = {
    'command': ['leindex', 'mcp'],
    'type': 'local'
}

with open('$config_file', 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
PYTHON
        return $?
    else
        return 1
    fi
}

# Configure LM Studio MCP (uses mcp.json format like Cursor)
configure_lmstudio_mcp() {
    local config_file="$1"
    local backup_file="$2"

    # LM Studio uses the same mcp.json format as Cursor/Claude Code
    configure_json_mcp "$config_file" "$backup_file"
    return $?
}

show_mcp_config_instructions() {
    echo "═══════════════════════════════════════════════════"
    echo "  MCP Server Configuration"
    echo "═══════════════════════════════════════════════════"
    echo ""
    echo "LeIndex runs as a subprocess (stdio mode) for automatic AI tool integration"
    echo ""
    echo "The AI tool will automatically start and stop LeIndex as needed."
    echo ""
    echo "For manual configuration, add to your tool's config:"
    echo ""
    echo "  Claude Code (~/.config/claude-code/mcp.json):"
    echo '    {"mcpServers": {"leindex": {"command": "leindex", "args": ["mcp"]}}}'
    echo ""
    echo "  Cursor (~/.cursor/mcp.json):"
    echo '    {"mcpServers": {"leindex": {"command": "leindex", "args": ["mcp"]}}}'
    echo ""
    echo "  VS Code (~/.config/Code/User/mcp.json):"
    echo '    {"mcpServers": {"leindex": {"command": "leindex", "args": ["mcp"]}}}'
    echo ""
    echo "  Zed (~/.config/zed/settings.json):"
    echo '    {"lsp": {"leindex": {"command": "leindex", "args": ["mcp"]}}}'
    echo ""
    echo "  Opencode (~/.config/opencode/opencode.json):"
    echo '    {"mcp": {"leindex": {"command": ["leindex", "mcp"], "type": "local"}}}'
    echo ""
    echo "  LM Studio (~/.lmstudio/mcp.json):"
    echo '    {"mcpServers": {"leindex": {"command": "leindex", "args": ["mcp"]}}}'
    echo ""
    echo "Note: When configured this way, AI tools automatically start LeIndex"
    echo "      when needed and stop it when the tool closes."
    echo ""
    echo "To run LeIndex manually (for testing):"
    echo "  ${CYAN}leindex mcp${NC}  # stdio mode (reads from stdin, writes to stdout)"
    echo "  ${CYAN}leindex serve${NC}  # HTTP server mode on port 47268"
    echo ""
}

offer_start_server() {
    print_header "Server Status & Testing"

    echo "The LeIndex MCP server is configured to run as a subprocess (stdio mode)."
    echo "Your AI tools will automatically start and stop it as needed."
    echo ""
    echo "You can test the configuration manually:"
    echo ""
    echo "Options:"
    echo "  ${CYAN}1)${NC} Test server in stdio mode (verify JSON-RPC communication)"
    echo "  ${CYAN}2)${NC} Start HTTP server for testing (legacy mode, not recommended)"
    echo "  ${CYAN}3)${NC} Skip (AI tools will manage server automatically)"
    echo ""
    read -p "Choose an option [1-3]: " -n 1 -r
    echo ""

    case $REPLY in
        1)
            log_info "Testing LeIndex MCP stdio mode..."
            echo ""
            echo "Enter a JSON-RPC request (one line) to test, or press Ctrl+C to exit."
            echo "Example: {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"
            echo ""
            echo "Starting stdio mode..."
            # Start server in stdio mode for testing
            exec "$INSTALL_BIN_DIR/$PROJECT_SLUG" mcp
            ;;
        2)
            log_info "Starting LeIndex MCP HTTP server (legacy mode)..."
            echo ""

            # Check if server is already running
            if pgrep -f "$PROJECT_SLUG serve" > /dev/null; then
                log_warn "LeIndex HTTP server is already running!"
                echo ""
                echo "Server PID: $(pgrep -f '$PROJECT_SLUG serve')"
                echo ""
                return 0
            fi

            # Create log directory
            mkdir -p "$LOG_DIR"

            # Start HTTP server in background
            nohup "$INSTALL_BIN_DIR/$PROJECT_SLUG" serve > "$LOG_DIR/server.log" 2>&1 &
            local server_pid=$!

            # Wait a bit and check if server started successfully
            sleep 2

            if pgrep -f "$PROJECT_SLUG serve" > /dev/null; then
                log_success "LeIndex server started successfully!"
                echo ""
                echo "  ${BOLD}Server PID:${NC}     $server_pid"
                echo "  ${BOLD}Log file:${NC}       $LOG_DIR/server.log"
                echo "  ${BOLD}Server URL:${NC}     http://127.0.0.1:47268/mcp"
                echo ""
                echo "To stop the server:"
                echo "  ${CYAN}kill $server_pid${NC}"
                echo "  ${CYAN}pkill -f '$PROJECT_SLUG serve'${NC}"
                echo ""
                echo "To restart the server:"
                echo "  ${CYAN}$PROJECT_SLUG serve${NC}"
                echo ""
                echo "To view logs:"
                echo "  ${CYAN}tail -f $LOG_DIR/server.log${NC}"
                echo ""
            else
                log_error "Failed to start server. Check logs: $LOG_DIR/server.log"
                return 1
            fi
            ;;
        3|*)
            log_info "Server will be managed by your AI tools automatically"
            echo ""
            echo "When you use your AI tool's code search or analysis features,"
            echo "it will automatically start the LeIndex server in stdio mode."
            echo ""
            echo "To test stdio mode manually:"
            echo "  ${CYAN}$PROJECT_SLUG mcp${NC}"
            echo ""
            echo "To start HTTP server (not recommended):"
            echo "  ${CYAN}$PROJECT_SLUG serve${NC}"
            echo ""
            ;;
    esac
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

    # Offer to start LeIndex server
    if [[ "$NONINTERACTIVE" != true ]]; then
        offer_start_server
    fi

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
