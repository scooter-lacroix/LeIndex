#!/usr/bin/env bash
#############################################
# LeIndex Universal Installer
# Version: 5.1.0 - Rust Edition (Multi-Shell & Multi-Distro)
# Platform: Linux/Unix (Bash, Zsh, Fish, Arch, Debian, Ubuntu, Fedora, etc.)
#
# One-line installer:
#   curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
#
# For fish shell users:
#   curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
#
# Cargo install alternative:
#   cargo install leindex
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
readonly SCRIPT_VERSION="5.1.0"
readonly PROJECT_NAME="LeIndex"
readonly PROJECT_SLUG="leindex"
readonly MIN_RUST_MAJOR=1
readonly MIN_RUST_MINOR=75
readonly REPO_URL="https://github.com/scooter-lacroix/leindex"
NONINTERACTIVE=false
PRESERVE_BINARY=false
PRESERVE_CONFIG=false
PRESERVE_DATA=false
PRESERVE_LOGS=false
KEEP_ALL=false
SELECTIVE_PURGE=false

# System detection variables
USER_SHELL="bash"
SHELL_RC=""
SHELL_PROFILE=""
DISTRO_ID="unknown"
DISTRO_NAME="Unknown"
DISTRO_VERSION="unknown"
PKG_MANAGER="unknown"
IS_ARCH=false
IS_DEBIAN=false
IS_FEDORA=false

# Installation paths
LEINDEX_HOME="${LEINDEX_HOME:-$HOME/.leindex}"
CONFIG_DIR="${LEINDEX_HOME}/config"
DATA_DIR="${LEINDEX_HOME}/data"
LOG_DIR="${LEINDEX_HOME}/logs"
TEMP_BACKUP_DIR="${HOME}/.leindex.tmp"
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
# SYSTEM DETECTION
# ============================================================================

detect_shell() {
    USER_SHELL=$(basename "$SHELL" 2>/dev/null || echo "bash")
    
    log_info "Detected shell: $USER_SHELL"
    
    case "$USER_SHELL" in
        bash)
            SHELL_RC="$HOME/.bashrc"
            SHELL_PROFILE="$HOME/.bash_profile"
            ;;
        zsh)
            SHELL_RC="$HOME/.zshrc"
            SHELL_PROFILE="$HOME/.zprofile"
            ;;
        fish)
            SHELL_RC="$HOME/.config/fish/config.fish"
            SHELL_PROFILE="$HOME/.config/fish/config.fish"
            mkdir -p "$HOME/.config/fish"
            ;;
        *)
            SHELL_RC="$HOME/.profile"
            SHELL_PROFILE="$HOME/.profile"
            ;;
    esac
}

detect_distribution() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        DISTRO_ID="${ID:-unknown}"
        DISTRO_NAME="${NAME:-Unknown}"
        DISTRO_VERSION="${VERSION_ID:-unknown}"
    elif [[ -f /etc/arch-release ]]; then
        DISTRO_ID="arch"
        DISTRO_NAME="Arch Linux"
        DISTRO_VERSION="rolling"
    else
        DISTRO_ID="unknown"
        DISTRO_NAME="Unknown"
        DISTRO_VERSION="unknown"
    fi
    
    log_info "Detected distribution: $DISTRO_NAME ($DISTRO_ID) $DISTRO_VERSION"
    
    if command -v pacman &> /dev/null; then
        PKG_MANAGER="pacman"
        IS_ARCH=true
        log_info "Package manager: pacman (Arch Linux)"
    elif command -v apt &> /dev/null || command -v apt-get &> /dev/null; then
        PKG_MANAGER="apt"
        IS_DEBIAN=true
        log_info "Package manager: apt (Debian/Ubuntu)"
    elif command -v dnf &> /dev/null; then
        PKG_MANAGER="dnf"
        IS_FEDORA=true
        log_info "Package manager: dnf (Fedora)"
    elif command -v yum &> /dev/null; then
        PKG_MANAGER="yum"
        IS_FEDORA=true
        log_info "Package manager: yum (RHEL/CentOS)"
    elif command -v zypper &> /dev/null; then
        PKG_MANAGER="zypper"
        log_info "Package manager: zypper (openSUSE)"
    else
        PKG_MANAGER="unknown"
        log_warn "No recognized package manager found"
    fi
}

install_dependencies() {
    print_header "Checking Dependencies"
    
    local missing_deps=()
    
    if ! command -v curl &> /dev/null && ! command -v wget &> /dev/null; then
        missing_deps+=("curl or wget")
    fi
    
    if ! command -v git &> /dev/null; then
        missing_deps+=("git")
    fi
    
    if [[ ${#missing_deps[@]} -eq 0 ]]; then
        log_success "All required dependencies are installed"
        return 0
    fi
    
    log_warn "Missing dependencies: ${missing_deps[*]}"
    
    local should_install=false
    if [[ "$NONINTERACTIVE" == true ]]; then
        log_info "Non-interactive mode: Installing dependencies automatically..."
        should_install=true
    else
        echo ""
        read -p "Would you like to install missing dependencies? [Y/n] " -n 1 -r
        echo ""
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            should_install=true
        fi
    fi
    
    if [[ "$should_install" != true ]]; then
        log_error "Dependencies are required to continue"
        exit 1
    fi
    
    case "$PKG_MANAGER" in
        pacman)
            log_info "Installing dependencies with pacman..."
            local pacman_packages=()
            for dep in "${missing_deps[@]}"; do
                case "$dep" in
                    "curl or wget") pacman_packages+=("curl") ;;
                    "git") pacman_packages+=("git") ;;
                esac
            done
            if [[ ${#pacman_packages[@]} -gt 0 ]]; then
                sudo pacman -Sy --noconfirm "${pacman_packages[@]}" 2>&1 | tee -a "$INSTALL_LOG"
            fi
            ;;
        apt)
            log_info "Installing dependencies with apt..."
            sudo apt-get update 2>&1 | tee -a "$INSTALL_LOG"
            local apt_packages=()
            for dep in "${missing_deps[@]}"; do
                case "$dep" in
                    "curl or wget") apt_packages+=("curl") ;;
                    "git") apt_packages+=("git") ;;
                esac
            done
            if [[ ${#apt_packages[@]} -gt 0 ]]; then
                sudo apt-get install -y "${apt_packages[@]}" 2>&1 | tee -a "$INSTALL_LOG"
            fi
            ;;
        dnf)
            log_info "Installing dependencies with dnf..."
            local dnf_packages=()
            for dep in "${missing_deps[@]}"; do
                case "$dep" in
                    "curl or wget") dnf_packages+=("curl") ;;
                    "git") dnf_packages+=("git") ;;
                esac
            done
            if [[ ${#dnf_packages[@]} -gt 0 ]]; then
                sudo dnf install -y "${dnf_packages[@]}" 2>&1 | tee -a "$INSTALL_LOG"
            fi
            ;;
        yum)
            log_info "Installing dependencies with yum..."
            local yum_packages=()
            for dep in "${missing_deps[@]}"; do
                case "$dep" in
                    "curl or wget") yum_packages+=("curl") ;;
                    "git") yum_packages+=("git") ;;
                esac
            done
            if [[ ${#yum_packages[@]} -gt 0 ]]; then
                sudo yum install -y "${yum_packages[@]}" 2>&1 | tee -a "$INSTALL_LOG"
            fi
            ;;
        zypper)
            log_info "Installing dependencies with zypper..."
            local zypper_packages=()
            for dep in "${missing_deps[@]}"; do
                case "$dep" in
                    "curl or wget") zypper_packages+=("curl") ;;
                    "git") zypper_packages+=("git") ;;
                esac
            done
            if [[ ${#zypper_packages[@]} -gt 0 ]]; then
                sudo zypper install -y "${zypper_packages[@]}" 2>&1 | tee -a "$INSTALL_LOG"
            fi
            ;;
        *)
            log_error "Cannot install dependencies: unknown package manager"
            echo ""
            echo "Please install the following manually:"
            for dep in "${missing_deps[@]}"; do
                echo "  - $dep"
            done
            exit 1
            ;;
    esac
    
    log_success "Dependencies installed successfully"
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

    if [[ -f "$HOME/.cargo/env" ]]; then
        source "$HOME/.cargo/env"
    fi

    if detect_rust; then
        log_success "Rust installed successfully"
        add_rust_to_shell_config
    else
        log_error "Rust installation failed"
        exit 1
    fi
}

add_rust_to_shell_config() {
    log_info "Configuring Rust environment for $USER_SHELL..."
    
    case "$USER_SHELL" in
        fish)
            if [[ -f "$SHELL_RC" ]]; then
                if ! grep -q "set -gx PATH.*cargo/bin" "$SHELL_RC" 2>/dev/null; then
                    echo "" >> "$SHELL_RC"
                    echo "# Rust cargo environment" >> "$SHELL_RC"
                    echo "set -gx PATH \$HOME/.cargo/bin \$PATH" >> "$SHELL_RC"
                    log_success "Added Rust to $SHELL_RC"
                fi
            fi
            ;;
        *)
            if [[ -f "$SHELL_RC" ]]; then
                if ! grep -q "cargo/env" "$SHELL_RC" 2>/dev/null; then
                    echo "" >> "$SHELL_RC"
                    echo "# Rust cargo environment" >> "$SHELL_RC"
                    echo ". \"\$HOME/.cargo/env\"" >> "$SHELL_RC"
                    log_success "Added Rust to $SHELL_RC"
                fi
            fi
            ;;
    esac
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

    log_info "Binary installed to /usr/local/bin (system-wide location)"
    log_info "This location is in the standard PATH for all Unix-like systems"

    if echo ":$PATH:" | grep -q ":/usr/local/bin:"; then
        log_success "/usr/local/bin is in PATH"
    else
        print_warning "/usr/local/bin is not in your PATH"
        echo ""
        echo "Add the following to your shell configuration ($SHELL_RC):"
        
        case "$USER_SHELL" in
            fish)
                echo "  ${CYAN}set -gx PATH /usr/local/bin \$PATH${NC}"
                ;;
            *)
                echo "  ${CYAN}export PATH=\"/usr/local/bin:\$PATH\"${NC}"
                ;;
        esac
        
        echo ""
        echo "Then restart your shell or run:"
        
        case "$USER_SHELL" in
            fish)
                echo "  ${CYAN}source ~/.config/fish/config.fish${NC}"
                ;;
            bash)
                echo "  ${CYAN}source ~/.bashrc${NC}"
                ;;
            zsh)
                echo "  ${CYAN}source ~/.zshrc${NC}"
                ;;
            *)
                echo "  ${CYAN}source ~/.profile${NC}"
                ;;
        esac
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
            local mcp_config_dir=""
            if [[ -d "$HOME/.cursor" ]]; then
                mcp_config_dir="$HOME/.cursor"
            elif [[ -d "$HOME/.config/Code/User" ]]; then
                mcp_config_dir="$HOME/.config/Code/User"
            elif [[ -d "$HOME/.config/VSCodium/User" ]]; then
                mcp_config_dir="$HOME/.config/VSCodium/User"
            else
                print_warning "No VS Code/Cursor config directory found for $tool"
                return 2
            fi
            local mcp_config_file="$mcp_config_dir/mcp.json"
            backup_file="${mcp_config_file}.backup"
            configure_json_mcp "$mcp_config_file" "$backup_file"
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
            config_file="$HOME/.gemini/antigravity/mcp_config.json"
            if [[ ! -d "$HOME/.gemini/antigravity" ]]; then
                mkdir -p "$HOME/.gemini/antigravity"
            fi
            backup_file="${config_file}.backup"
            configure_json_mcp "$config_file" "$backup_file"
            return $?
            ;;
        "LM Studio")
            config_file="$HOME/.lmstudio/mcp.json"
            backup_file="${config_file}.backup"
            configure_json_mcp "$config_file" "$backup_file"
            return $?
            ;;
        *)
            return 2
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

    if [[ ! -f "$config_file" ]] || [[ ! -s "$config_file" ]]; then
        mkdir -p "$(dirname "$config_file")"
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
        log_info "Created MCP config: $config_file"
        return 0
    fi

    backup_config_file "$config_file" "$backup_file"

    if command -v python3 &> /dev/null; then
        python3 -c "
import json
import sys

config_file = '$config_file'
try:
    with open(config_file, 'r') as f:
        content = f.read().strip()
        if content:
            config = json.loads(content)
        else:
            config = {}
except:
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

config['mcpServers']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
"
        return $?
    elif command -v jq &> /dev/null; then
        jq '.mcpServers.leindex = {"command": "leindex", "args": ["mcp"]}' "$config_file" > "${config_file}.tmp" && mv "${config_file}.tmp" "$config_file"
        return $?
    else
        return 1
    fi
}

# Configure VS Code/Cursor MCP (mcp.json format)
configure_vscode_mcp() {
    local config_file="$1"
    local backup_file="$2"

    local mcp_config_dir="$(dirname "$config_file")"
    local mcp_config_file="$mcp_config_dir/mcp.json"

    mkdir -p "$mcp_config_dir"

    if [[ ! -f "$mcp_config_file" ]]; then
        cat > "$mcp_config_file" << 'EOF'
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"]
    }
  }
}
EOF
        log_info "Created MCP config: $mcp_config_file"
        return 0
    fi

    backup_config_file "$mcp_config_file" "$backup_file"
    configure_json_mcp "$mcp_config_file" "${mcp_config_file}.backup"
    return $?
}

# Configure Zed MCP
configure_zed_mcp() {
    local config_file="$1"
    local backup_file="$2"

    local config_dir="$(dirname "$config_file")"
    mkdir -p "$config_dir"

    if [[ ! -f "$config_file" ]] || [[ ! -s "$config_file" ]]; then
        cat > "$config_file" << 'EOF'
{
  "mcp": {
    "leindex": {
      "command": ["leindex", "mcp"],
      "type": "local"
    }
  }
}
EOF
        log_info "Created Zed MCP config: $config_file"
        return 0
    fi

    backup_config_file "$config_file" "$backup_file"

    if command -v python3 &> /dev/null; then
        python3 -c "
import json
import sys

config_file = '$config_file'
try:
    with open(config_file, 'r') as f:
        content = f.read().strip()
        if content:
            config = json.loads(content)
        else:
            config = {}
except:
    config = {}

if 'mcp' not in config:
    config['mcp'] = {}

config['mcp']['leindex'] = {
    'command': ['leindex', 'mcp'],
    'type': 'local'
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
"
        return $?
    else
        return 1
    fi
}

# Configure Opencode MCP
configure_opencode_mcp() {
    local config_file="$1"
    local backup_file="$2"

    local config_dir="$(dirname "$config_file")"
    mkdir -p "$config_dir"

    if [[ ! -f "$config_file" ]] || [[ ! -s "$config_file" ]]; then
        cat > "$config_file" << 'EOF'
{
  "mcp": {
    "leindex": {
      "command": ["leindex", "mcp"],
      "type": "local"
    }
  }
}
EOF
        log_info "Created Opencode MCP config: $config_file"
        return 0
    fi

    backup_config_file "$config_file" "$backup_file"

    if command -v python3 &> /dev/null; then
        python3 -c "
import json
import sys

config_file = '$config_file'
try:
    with open(config_file, 'r') as f:
        content = f.read().strip()
        if content:
            config = json.loads(content)
        else:
            config = {}
except:
    config = {}

if 'mcp' not in config:
    config['mcp'] = {}

config['mcp']['leindex'] = {
    'command': ['leindex', 'mcp'],
    'type': 'local'
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')
"
        return $?
    else
        return 1
    fi
}

# Configure LM Studio MCP
configure_lmstudio_mcp() {
    local config_file="$1"
    local backup_file="$2"

    local config_dir="$(dirname "$config_file")"
    mkdir -p "$config_dir"

    if [[ ! -f "$config_file" ]] || [[ ! -s "$config_file" ]]; then
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
        log_info "Created LM Studio MCP config: $config_file"
        return 0
    fi

    backup_config_file "$config_file" "$backup_file"
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
# SELECTIVE PURGE SYSTEM
# ============================================================================

show_selective_purge_menu() {
    print_header "Selective Purge Menu"

    echo "Select what to preserve:"
    echo ""
    echo "  ${CYAN}1)${NC} ${BOLD}Binary only${NC}        Remove config/data/logs, keep binary"
    echo "  ${CYAN}2)${NC} ${BOLD}Config only${NC}        Remove binary/data/logs, keep config"
    echo "  ${CYAN}3)${NC} ${BOLD}Data only${NC}          Remove binary/config/logs, keep data"
    echo "  ${CYAN}4)${NC} ${BOLD}Logs only${NC}          Remove binary/config/data, keep logs"
    echo "  ${CYAN}5)${NC} ${BOLD}Custom selection${NC}    Interactive menu for each component"
    echo "  ${CYAN}6)${NC} ${BOLD}Purge all${NC}          Remove everything (default behavior)"
    echo "  ${CYAN}7)${NC} ${BOLD}Keep all${NC}          Preserve everything"
    echo "  ${CYAN}0)${NC} ${BOLD}Cancel${NC}            Cancel installation"
    echo ""
    read -p "Enter your choice [0-7]: " -n 1 -r
    echo ""

    case $REPLY in
        1)
            # Keep binary only
            PRESERVE_BINARY=true
            SELECTIVE_PURGE=true
            log_info "Selected: Keep binary only"
            ;;
        2)
            # Keep config only
            PRESERVE_CONFIG=true
            SELECTIVE_PURGE=true
            log_info "Selected: Keep config only"
            ;;
        3)
            # Keep data only
            PRESERVE_DATA=true
            SELECTIVE_PURGE=true
            log_info "Selected: Keep data only"
            ;;
        4)
            # Keep logs only
            PRESERVE_LOGS=true
            SELECTIVE_PURGE=true
            log_info "Selected: Keep logs only"
            ;;
        5)
            # Custom selection
            show_custom_selection_menu
            ;;
        6)
            # Purge all
            SELECTIVE_PURGE=false
            log_info "Selected: Purge all"
            ;;
        7)
            # Keep all
            KEEP_ALL=true
            log_info "Selected: Keep all"
            ;;
        0)
            log_info "Installation cancelled by user"
            exit 0
            ;;
        *)
            log_error "Invalid choice"
            echo ""
            read -p "Try again? [Y/n] " -n 1 -r
            echo ""
            if [[ $REPLY =~ ^[Nn]$ ]]; then
                log_info "Installation cancelled"
                exit 0
            fi
            show_selective_purge_menu
            ;;
    esac
}

show_custom_selection_menu() {
    print_header "Custom Component Selection"

    echo "Select components to ${GREEN}PRESERVE${NC}:"
    echo ""

    # Binary
    read -p "Preserve ${BOLD}binary${NC}? [y/N] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        PRESERVE_BINARY=true
        print_bullet "Binary will be preserved"
    else
        print_bullet "Binary will be removed"
    fi

    # Config
    read -p "Preserve ${BOLD}config${NC}? [y/N] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        PRESERVE_CONFIG=true
        print_bullet "Config will be preserved"
    else
        print_bullet "Config will be removed"
    fi

    # Data
    read -p "Preserve ${BOLD}data${NC}? [y/N] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        PRESERVE_DATA=true
        print_bullet "Data will be preserved"
    else
        print_bullet "Data will be removed"
    fi

    # Logs
    read -p "Preserve ${BOLD}logs${NC}? [y/N] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        PRESERVE_LOGS=true
        print_bullet "Logs will be preserved"
    else
        print_bullet "Logs will be removed"
    fi

    # Check if at least one component is being preserved
    if [[ "$PRESERVE_BINARY" == false ]] && [[ "$PRESERVE_CONFIG" == false ]] && \
       [[ "$PRESERVE_DATA" == false ]] && [[ "$PRESERVE_LOGS" == false ]]; then
        echo ""
        log_warn "No components selected for preservation"
        read -p "Proceed with full purge? [y/N] " -n 1 -r
        echo ""
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            SELECTIVE_PURGE=false
        else
            log_info "Restarting selection..."
            echo ""
            show_custom_selection_menu
        fi
    else
        SELECTIVE_PURGE=true
        echo ""
        log_success "Custom selection complete"
    fi
}

backup_data_to_temp() {
    local backup_success=false

    print_header "Backing Up Data"

    # Create temp directory
    if [[ -d "$TEMP_BACKUP_DIR" ]]; then
        log_warn "Temp backup directory already exists, removing..."
        rm -rf "$TEMP_BACKUP_DIR"
    fi

    mkdir -p "$TEMP_BACKUP_DIR"
    log_info "Created temporary backup directory: $TEMP_BACKUP_DIR"
    echo ""

    # Backup config if preserving
    if [[ "$PRESERVE_CONFIG" == true ]] && [[ -d "$CONFIG_DIR" ]]; then
        log_info "Backing up config..."
        if cp -r "$CONFIG_DIR" "$TEMP_BACKUP_DIR/config" 2>/dev/null; then
            log_success "Config backed up"
            backup_success=true
        else
            log_error "Failed to backup config"
        fi
    fi

    # Backup data if preserving
    if [[ "$PRESERVE_DATA" == true ]] && [[ -d "$DATA_DIR" ]]; then
        log_info "Backing up data..."
        if cp -r "$DATA_DIR" "$TEMP_BACKUP_DIR/data" 2>/dev/null; then
            log_success "Data backed up"
            backup_success=true
        else
            log_error "Failed to backup data"
        fi
    fi

    # Backup logs if preserving
    if [[ "$PRESERVE_LOGS" == true ]] && [[ -d "$LOG_DIR" ]]; then
        log_info "Backing up logs..."
        if cp -r "$LOG_DIR" "$TEMP_BACKUP_DIR/logs" 2>/dev/null; then
            log_success "Logs backed up"
            backup_success=true
        else
            log_error "Failed to backup logs"
        fi
    fi

    # Backup binary if preserving
    if [[ "$PRESERVE_BINARY" == true ]] && [[ -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" ]]; then
        log_info "Backing up binary..."
        mkdir -p "$TEMP_BACKUP_DIR/binary"
        if cp "$INSTALL_BIN_DIR/$PROJECT_SLUG" "$TEMP_BACKUP_DIR/binary/" 2>/dev/null; then
            log_success "Binary backed up"
            backup_success=true
        else
            log_error "Failed to backup binary"
        fi
    fi

    echo ""
    if [[ "$backup_success" == true ]]; then
        log_success "Backup complete"
        return 0
    else
        log_error "Backup failed"
        return 1
    fi
}

validate_backup_integrity() {
    print_header "Validating Backup Integrity"

    local validation_failed=false

    # Validate config if backed up
    if [[ -d "$TEMP_BACKUP_DIR/config" ]]; then
        log_info "Validating config backup..."
        local config_files=$(find "$TEMP_BACKUP_DIR/config" -type f 2>/dev/null | wc -l)
        if [[ $config_files -gt 0 ]]; then
            log_success "Config backup valid ($config_files files)"
        else
            log_error "Config backup validation failed"
            validation_failed=true
        fi
    fi

    # Validate data if backed up
    if [[ -d "$TEMP_BACKUP_DIR/data" ]]; then
        log_info "Validating data backup..."
        local data_files=$(find "$TEMP_BACKUP_DIR/data" -type f 2>/dev/null | wc -l)
        if [[ $data_files -gt 0 ]]; then
            log_success "Data backup valid ($data_files files)"
        else
            log_error "Data backup validation failed"
            validation_failed=true
        fi
    fi

    # Validate logs if backed up
    if [[ -d "$TEMP_BACKUP_DIR/logs" ]]; then
        log_info "Validating logs backup..."
        local log_files=$(find "$TEMP_BACKUP_DIR/logs" -type f 2>/dev/null | wc -l)
        if [[ $log_files -gt 0 ]]; then
            log_success "Logs backup valid ($log_files files)"
        else
            log_error "Logs backup validation failed"
            validation_failed=true
        fi
    fi

    # Validate binary if backed up
    if [[ -f "$TEMP_BACKUP_DIR/binary/$PROJECT_SLUG" ]]; then
        log_info "Validating binary backup..."
        if [[ -x "$TEMP_BACKUP_DIR/binary/$PROJECT_SLUG" ]] || chmod +x "$TEMP_BACKUP_DIR/binary/$PROJECT_SLUG" 2>/dev/null; then
            log_success "Binary backup valid"
        else
            log_error "Binary backup validation failed"
            validation_failed=true
        fi
    fi

    echo ""
    if [[ "$validation_failed" == true ]]; then
        log_error "Backup validation failed"
        return 1
    else
        log_success "All backups validated successfully"
        return 0
    fi
}

restore_backup_data() {
    print_header "Restoring Preserved Data"

    local restore_success=false

    # Restore config
    if [[ -d "$TEMP_BACKUP_DIR/config" ]]; then
        log_info "Restoring config..."
        if cp -r "$TEMP_BACKUP_DIR/config"/* "$CONFIG_DIR/" 2>/dev/null; then
            log_success "Config restored"
            restore_success=true
        else
            log_error "Failed to restore config"
        fi
    fi

    # Restore data
    if [[ -d "$TEMP_BACKUP_DIR/data" ]]; then
        log_info "Restoring data..."
        if cp -r "$TEMP_BACKUP_DIR/data"/* "$DATA_DIR/" 2>/dev/null; then
            log_success "Data restored"
            restore_success=true
        else
            log_error "Failed to restore data"
        fi
    fi

    # Restore logs
    if [[ -d "$TEMP_BACKUP_DIR/logs" ]]; then
        log_info "Restoring logs..."
        if cp -r "$TEMP_BACKUP_DIR/logs"/* "$LOG_DIR/" 2>/dev/null; then
            log_success "Logs restored"
            restore_success=true
        else
            log_error "Failed to restore logs"
        fi
    fi

    # Restore binary
    if [[ -f "$TEMP_BACKUP_DIR/binary/$PROJECT_SLUG" ]]; then
        log_info "Restoring binary..."
        if sudo cp "$TEMP_BACKUP_DIR/binary/$PROJECT_SLUG" "$INSTALL_BIN_DIR/" && \
           sudo chmod +x "$INSTALL_BIN_DIR/$PROJECT_SLUG" 2>/dev/null; then
            log_success "Binary restored"
            restore_success=true
        else
            log_error "Failed to restore binary"
        fi
    fi

    echo ""
    if [[ "$restore_success" == true ]]; then
        log_success "Data restoration complete"

        # Clean up temp directory
        log_info "Cleaning up temporary backup..."
        rm -rf "$TEMP_BACKUP_DIR"
        log_success "Temporary backup removed"
        return 0
    else
        log_warn "Some data could not be restored"
        log_info "Temporary backup preserved at: $TEMP_BACKUP_DIR"
        return 1
    fi
}

handle_validation_failure() {
    print_header "Validation Failure"

    log_error "Backup validation failed"
    echo ""
    echo "Options:"
    echo "  ${CYAN}1)${NC} Retry backup and validation"
    echo "  ${CYAN}2)${NC} Abort installation"
    echo "  ${CYAN}3)${NC} Continue anyway (not recommended)"
    echo ""
    read -p "Choose an option [1-3]: " -n 1 -r
    echo ""

    case $REPLY in
        1)
            log_info "Retrying backup..."
            rm -rf "$TEMP_BACKUP_DIR"
            if backup_data_to_temp && validate_backup_integrity; then
                return 0
            else
                handle_validation_failure
            fi
            ;;
        2)
            log_info "Installation aborted by user"
            # Clean up temp directory
            rm -rf "$TEMP_BACKUP_DIR"
            exit 1
            ;;
        3)
            log_warn "Continuing despite validation failure..."
            return 0
            ;;
        *)
            log_error "Invalid choice"
            handle_validation_failure
            ;;
    esac
}

selective_purge() {
    print_header "Selective Purge"

    local has_existing=false

    # Check what exists
    local has_binary=false
    local has_config=false
    local has_data=false
    local has_logs=false

    if [[ -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" ]]; then
        has_binary=true
        has_existing=true
    fi

    if [[ -d "$CONFIG_DIR" ]]; then
        has_config=true
        has_existing=true
    fi

    if [[ -d "$DATA_DIR" ]]; then
        has_data=true
        has_existing=true
    fi

    if [[ -d "$LOG_DIR" ]]; then
        has_logs=true
        has_existing=true
    fi

    if [[ "$has_existing" == false ]]; then
        log_info "No existing installation found"
        return 0
    fi

    # Show what was found
    echo "Found existing components:"
    [[ "$has_binary" == true ]] && print_bullet "Binary: $INSTALL_BIN_DIR/$PROJECT_SLUG"
    [[ "$has_config" == true ]] && print_bullet "Config: $CONFIG_DIR"
    [[ "$has_data" == true ]] && print_bullet "Data: $DATA_DIR"
    [[ "$has_logs" == true ]] && print_bullet "Logs: $LOG_DIR"
    echo ""

    # If keep all flag is set, skip purge
    if [[ "$KEEP_ALL" == true ]]; then
        log_info "Keep all flag set - preserving all existing data"
        return 0
    fi

    # If selective purge is not enabled and we're not in non-interactive mode, show menu
    if [[ "$SELECTIVE_PURGE" == false ]] && [[ "$NONINTERACTIVE" != true ]]; then
        show_selective_purge_menu
    fi

    # In non-interactive mode without specific flags, default to keep all
    if [[ "$NONINTERACTIVE" == true ]] && [[ "$SELECTIVE_PURGE" == false ]] && [[ "$KEEP_ALL" == false ]]; then
        log_info "Non-interactive mode: Defaulting to keep all"
        KEEP_ALL=true
        return 0
    fi

    # If keeping all, return early
    if [[ "$KEEP_ALL" == true ]]; then
        log_info "Preserving all existing data"
        return 0
    fi

    # If selective purge, backup data first
    if [[ "$SELECTIVE_PURGE" == true ]]; then
        if backup_data_to_temp; then
            if ! validate_backup_integrity; then
                handle_validation_failure
            fi
        else
            log_error "Backup failed - aborting selective purge"
            exit 1
        fi
    fi

    # Stop running server
    if pgrep -f "$PROJECT_SLUG serve" > /dev/null; then
        log_info "Stopping LeIndex server..."
        if pkill -f "$PROJECT_SLUG serve" 2>/dev/null; then
            sleep 1
            log_success "Server stopped"
        else
            log_warn "Failed to stop server"
        fi
    fi

    # Remove binary if not preserving
    if [[ "$has_binary" == true ]] && [[ "$PRESERVE_BINARY" == false ]]; then
        log_info "Removing binary..."
        if sudo rm -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" 2>/dev/null || \
           rm -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" 2>/dev/null; then
            log_success "Binary removed"
        else
            log_warn "Failed to remove binary"
        fi
    fi

    # Remove config if not preserving
    if [[ "$has_config" == true ]] && [[ "$PRESERVE_CONFIG" == false ]]; then
        log_info "Removing config..."
        if rm -rf "$CONFIG_DIR" 2>/dev/null; then
            log_success "Config removed"
        else
            log_warn "Failed to remove config"
        fi
    fi

    # Remove data if not preserving
    if [[ "$has_data" == true ]] && [[ "$PRESERVE_DATA" == false ]]; then
        log_info "Removing data..."
        if rm -rf "$DATA_DIR" 2>/dev/null; then
            log_success "Data removed"
        else
            log_warn "Failed to remove data"
        fi
    fi

    # Remove logs if not preserving
    if [[ "$has_logs" == true ]] && [[ "$PRESERVE_LOGS" == false ]]; then
        log_info "Removing logs..."
        # Preserve install log if it exists
        if [[ -f "$INSTALL_LOG" ]]; then
            local install_log_name=$(basename "$INSTALL_LOG")
            cp "$INSTALL_LOG" "/tmp/$install_log_name" 2>/dev/null
        fi
        if rm -rf "$LOG_DIR" 2>/dev/null; then
            # Recreate log directory for new installation
            mkdir -p "$LOG_DIR"
            if [[ -f "/tmp/$install_log_name" ]]; then
                mv "/tmp/$install_log_name" "$INSTALL_LOG"
            fi
            log_success "Logs removed"
        else
            log_warn "Failed to remove logs"
        fi
    fi

    log_success "Selective purge complete"
    echo ""
}

# ============================================================================
# CLEANUP FUNCTIONS
# ============================================================================

purge_existing_installation() {
    print_header "Purging Existing Installation"

    local has_existing=false

    # Check if binary exists
    if [[ -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" ]]; then
        has_existing=true
        log_info "Found existing binary: $INSTALL_BIN_DIR/$PROJECT_SLUG"
    fi

    # Check if LeIndex home directory exists
    if [[ -d "$LEINDEX_HOME" ]]; then
        has_existing=true
        log_info "Found existing data directory: $LEINDEX_HOME"
    fi

    # Check if server is running
    if pgrep -f "$PROJECT_SLUG serve" > /dev/null; then
        has_existing=true
        log_info "LeIndex server is running"
    fi

    if [[ "$has_existing" == false ]]; then
        log_info "No existing installation found"
        return 0
    fi

    # Confirm before purging (only in interactive mode)
    if [[ "$NONINTERACTIVE" != true ]]; then
        echo ""
        print_warning "This will remove:"
        echo "  - Binary: $INSTALL_BIN_DIR/$PROJECT_SLUG"
        echo "  - Data directory: $LEINDEX_HOME"
        echo "  - Config directory: $CONFIG_DIR"
        echo "  - Stop running server (if any)"
        echo ""
        read -p "Continue with purge? [y/N] " -n 1 -r
        echo ""
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            log_info "Purge cancelled"
            return 0
        fi
    else
        log_info "Non-interactive mode: Purging existing installation..."
    fi

    # Stop running server
    if pgrep -f "$PROJECT_SLUG serve" > /dev/null; then
        log_info "Stopping LeIndex server..."
        if pkill -f "$PROJECT_SLUG serve" 2>/dev/null; then
            sleep 1
            log_success "Server stopped"
        else
            log_warn "Failed to stop server (may require manual intervention)"
        fi
    fi

    # Remove binary
    if [[ -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" ]]; then
        log_info "Removing binary..."
        if sudo rm -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" 2>/dev/null; then
            log_success "Binary removed"
        elif rm -f "$INSTALL_BIN_DIR/$PROJECT_SLUG" 2>/dev/null; then
            log_success "Binary removed"
        else
            log_warn "Failed to remove binary (may require manual removal)"
        fi
    fi

    # Remove config directory (note: DATA_DIR is inside LEINDEX_HOME, so we remove the whole home)
    if [[ -d "$LEINDEX_HOME" ]]; then
        log_info "Removing LeIndex home directory..."
        if rm -rf "$LEINDEX_HOME" 2>/dev/null; then
            # Recreate log directory immediately after removal so logging continues to work
            mkdir -p "$LOG_DIR"
            touch "$INSTALL_LOG"
            log_success "Data directory removed"
        else
            log_warn "Failed to remove data directory (may require manual removal)"
        fi
    fi

    log_success "Purge complete"
    echo ""
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

main() {
    for arg in "$@"; do
        case $arg in
            --yes|-y)
                NONINTERACTIVE=true
                ;;
            --preserve-binary)
                PRESERVE_BINARY=true
                SELECTIVE_PURGE=true
                ;;
            --preserve-config)
                PRESERVE_CONFIG=true
                SELECTIVE_PURGE=true
                ;;
            --preserve-data)
                PRESERVE_DATA=true
                SELECTIVE_PURGE=true
                ;;
            --preserve-logs)
                PRESERVE_LOGS=true
                SELECTIVE_PURGE=true
                ;;
            --preserve-all)
                PRESERVE_BINARY=true
                PRESERVE_CONFIG=true
                PRESERVE_DATA=true
                PRESERVE_LOGS=true
                KEEP_ALL=true
                ;;
            --help|-h)
                echo "LeIndex Installer v$SCRIPT_VERSION"
                echo ""
                echo "Usage: $0 [--yes|--help] [--preserve-binary|--preserve-config|--preserve-data|--preserve-logs|--preserve-all]"
                echo ""
                echo "Options:"
                echo "  --yes, -y           Non-interactive mode (auto-confirm all prompts)"
                echo "  --preserve-binary     Preserve binary during reinstall"
                echo "  --preserve-config     Preserve config during reinstall"
                echo "  --preserve-data       Preserve data during reinstall"
                echo "  --preserve-logs       Preserve logs during reinstall"
                echo "  --preserve-all       Preserve all components during reinstall"
                echo "  --help, -h          Show this help message"
                echo ""
                echo "Examples:"
                echo "  $0 --yes --preserve-config    # Install non-interactively, keeping config"
                echo "  $0 --preserve-all             # Reinstall binary only, keep everything else"
                echo ""
                exit 0
                ;;
        esac
    done

    detect_shell
    detect_distribution

    print_header "LeIndex Rust Installer"

    echo "  ${BOLD}Project:${NC}       $PROJECT_NAME"
    echo "  ${BOLD}Version:${NC}       $SCRIPT_VERSION"
    echo "  ${BOLD}Repository:${NC}    $REPO_URL"
    echo "  ${BOLD}Shell:${NC}         $USER_SHELL"
    echo "  ${BOLD}Distribution:${NC}  $DISTRO_NAME ($DISTRO_ID)"
    echo "  ${BOLD}Package Mgr:${NC}   $PKG_MANAGER"
    echo ""

    install_dependencies

    purge_existing_installation

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

    install_leindex

    if ! verify_installation; then
        log_error "Installation verification failed"
        exit 1
    fi

    setup_directories

    update_path

    detect_ai_tools

    if [[ "$NONINTERACTIVE" != true ]]; then
        offer_start_server
    fi

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
    echo "  ${CYAN}5.${NC} ${BOLD}Start frontend dashboard:${NC} ${YELLOW}$PROJECT_SLUG dashboard${NC}"
    echo ""
    echo "  ${BOLD}Frontend Dashboard:${NC}"
    echo "  - Dev server: ${YELLOW}$PROJECT_SLUG dashboard${NC} or ${YELLOW}cd dashboard && bun run dev${NC}"
    echo "  - Production build: ${YELLOW}$PROJECT_SLUG dashboard --prod${NC} or ${YELLOW}cd dashboard && bun run build${NC}"
    echo "  - Access dashboard at: ${CYAN}http://localhost:5173${NC}"
    echo "  - Custom port: ${YELLOW}$PROJECT_SLUG dashboard --port 3000${NC}"
    echo ""
    echo "For MCP server configuration, see the documentation."
    echo ""
    printf "${GREEN}Happy indexing!${NC}\n"
}

# Run main function
main "$@"
