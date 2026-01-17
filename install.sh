#!/usr/bin/env bash
#############################################
# LeIndex Universal Installer
# Version: 4.0.0 - Beautiful & Interactive
# Platform: Linux/Unix
#############################################

set -euo pipefail

# ============================================================================
# CONFIGURATION
# ============================================================================
readonly SCRIPT_VERSION="1.5.0"
readonly PROJECT_NAME="LeIndex"
readonly PROJECT_SLUG="leindex"
readonly MIN_PYTHON_MAJOR=3
readonly MIN_PYTHON_MINOR=10
readonly REPO_URL="https://github.com/scooter-lacroix/leindex"
readonly PYPI_PACKAGE="leindex"

# Installation paths
LEINDEX_HOME="${LEINDEX_HOME:-$HOME/.leindex}"
CONFIG_DIR="${LEINDEX_HOME}/config"
DATA_DIR="${LEINDEX_HOME}/data"
LOG_DIR="${LEINDEX_HOME}/logs"

# Backup directory
BACKUP_DIR="/tmp/leindex-install-backup-$(date +%Y%m%d_%H%M%S)"

# ============================================================================
# LOGGING SYSTEM
# ============================================================================

# Check for debug mode
if [[ "${DEBUG:-}" == "1" ]] || [[ "${VERBOSE:-}" == "1" ]]; then
    set -x  # Print every command
fi

# Create log file
mkdir -p "$LOG_DIR"
INSTALL_LOG="$LOG_DIR/install-$(date +%Y%m%d-%H%M%S).log"

# Initialize log file
echo "=== LeIndex Installation Log ===" > "$INSTALL_LOG"
echo "Date: $(date)" >> "$INSTALL_LOG"
echo "Script Version: $SCRIPT_VERSION" >> "$INSTALL_LOG"
echo "DEBUG Mode: ${DEBUG:-0}" >> "$INSTALL_LOG"
echo "================================" >> "$INSTALL_LOG"
echo "" >> "$INSTALL_LOG"

# Logging functions
log_debug() {
    local msg="$*"
    echo "[DEBUG] $msg" >> "$INSTALL_LOG"
    if [[ "${DEBUG:-}" == "1" ]]; then
        printf "${DIM}[DEBUG]${NC} %s\n" "$msg" >&2
    fi
}

log_info() {
    local msg="$*"
    echo "[INFO] $msg" >> "$INSTALL_LOG"
    printf "${CYAN}[INFO]${NC} %s\n" "$msg" >&2
}

log_error() {
    local msg="$*"
    echo "[ERROR] $msg" >> "$INSTALL_LOG"
    printf "${RED}[ERROR]${NC} %s\n" "$msg" >&2
}

log_warn() {
    local msg="$*"
    echo "[WARN] $msg" >> "$INSTALL_LOG"
    printf "${YELLOW}[WARN]${NC} %s\n" "$msg" >&2
}

log_success() {
    local msg="$*"
    echo "[SUCCESS] $msg" >> "$INSTALL_LOG"
    printf "${GREEN}[SUCCESS]${NC} %s\n" "$msg" >&2
}

# Log command execution
log_cmd() {
    local cmd="$*"
    log_debug "Executing: $cmd"
    echo "[CMD] $cmd" >> "$INSTALL_LOG"
}

# ============================================================================
# COLOR OUTPUT - FIXED with proper escape sequences
# ============================================================================

# Use $'...' syntax for proper escape sequence interpretation
readonly RED=$'\033[0;31m'
readonly GREEN=$'\033[0;32m'
readonly BLUE=$'\033[0;34m'
readonly YELLOW=$'\033[1;33m'
readonly CYAN=$'\033[0;36m'
readonly MAGENTA=$'\033[0;35m'
readonly BOLD=$'\033[1m'
readonly DIM=$'\033[2m'
readonly NC=$'\033[0m'

# Check if terminal supports colors
if [[ ! -t 1 ]] || [[ "${TERM:-}" == "dumb" ]]; then
    # Fallback for non-interactive terminals
    readonly RED="" GREEN="" BLUE="" YELLOW="" CYAN="" MAGENTA="" BOLD="" DIM="" NC=""
fi

# ============================================================================
# UTILITY FUNCTIONS
# ============================================================================

# Print styled header with ASCII art
print_header() {
    local width=70
    clear
    printf "${CYAN}╔${NC}"
    printf '═%.0s' $(seq 1 $width)
    printf "${CYAN}╗${NC}\n"

    printf "${CYAN}║${NC}${BOLD}  🚀 %s v2.0 %s${NC}" "$PROJECT_NAME" "Installer v$SCRIPT_VERSION"
    local remaining=$(($width - ${#PROJECT_NAME} - 19))  # 19 = " v2.0 Installer vX.X.X"
    printf ' %.0s' $(seq 1 $remaining)
    printf "${CYAN}║${NC}\n"

    printf "${CYAN}║${NC}  %s" "✨ AI-Powered Code Search & MCP Server"
    remaining=$(($width - 42))
    printf ' %.0s' $(seq 1 $remaining)
    printf "${CYAN}║${NC}\n"

    printf "${CYAN}╚${NC}"
    printf '═%.0s' $(seq 1 $width)
    printf "${CYAN}╝${NC}\n"
    echo ""
}

# Print animated welcome message
print_welcome() {
    printf "${BOLD}${CYAN}Welcome to the future of code search!${NC}\n"
    echo ""
    printf "${DIM}Let's get you set up with LeIndex v2.0 in just a few moments.${NC}\n"
    printf "${DIM}This installer will:${NC}\n"
    echo ""
    printf "  ${GREEN}✓${NC} Detect your Python environment\n"
    printf "  ${GREEN}✓${NC} Find your AI coding tools\n"
    printf "  ${GREEN}✓${NC} Install LeIndex v2.0 with the best package manager\n"
    printf "  ${GREEN}✓${NC} Migrate v1 configuration (if present)\n"
    printf "  ${GREEN}✓${NC} Configure integrations with your favorite tools\n"
    echo ""

    if ask_yes_no "Ready to begin?" "y"; then
        return 0
    else
        print_info "Installation cancelled by user"
        exit 0
    fi
}

# Print section header with style
print_section() {
    echo ""
    printf "${BLUE}┌─${NC} ${BOLD}%s${NC} ${BLUE}─${NC}\n" "$1"
    printf "${BLUE}│${NC}\n"
}

# Print success message with emoji
print_success() {
    printf "${GREEN}✓${NC} %s\n" "$1"
}

# Print warning message with emoji
print_warning() {
    printf "${YELLOW}⚠${NC} %s\n" "$1"
}

# Print error message with emoji
print_error() {
    printf "${RED}✗${NC} %s\n" "$1"
}

# Print info message with emoji
print_info() {
    printf "${CYAN}ℹ${NC} %s\n" "$1"
}

# Print bullet point
print_bullet() {
    printf "  ${CYAN}•${NC} %s\n" "$1"
}

# Print step indicator
print_step() {
    local step="$1"
    local total="$2"
    local description="$3"
    printf "${MAGENTA}[${step}/${total}]${NC} ${BOLD}%s${NC}\n" "$description"
}

# Show progress spinner (runs in background)
show_spinner() {
    local message="$1"
    local pid=$2
    local delay=0.1
    local spinstr='|/-\'

    printf "${CYAN}%s${NC} " "$message"

    while kill -0 $pid 2>/dev/null; do
        local temp=${spinstr#?}
        printf " [%c]  " "$spinstr"
        local spinstr=$temp${spinstr%"$temp"}
        sleep $delay
        printf "\b\b\b\b\b\b\b"
    done

    printf "\b\b\b\b\b\b\b"
}

# Ask yes/no question with better styling
ask_yes_no() {
    local prompt="$1"
    local default="${2:-n}"

    local prompt_suffix
    local default_value

    if [[ "$default" == "y" ]]; then
        prompt_suffix="${GREEN}[Y/n]${NC}"
        default_value="y"
    else
        prompt_suffix="${YELLOW}[y/N]${NC}"
        default_value="n"
    fi

    while true; do
        printf "\n${YELLOW}?${NC} %s %s " "$prompt" "$prompt_suffix"
        read -r answer < /dev/tty
        answer=${answer:-$default_value}

        case "$answer" in
            [Yy]|[Yy][Ee][Ss])
                echo ""
                return 0
                ;;
            [Nn]|[Nn][Oo])
                echo ""
                return 1
                ;;
            *)
                printf "${RED}Please answer yes or no.${NC}"
                ;;
        esac
    done
}

# Global variable for ask_choice result
ASK_CHOICE_RESULT=""

# Ask for a choice from a list
ask_choice() {
    local prompt="$1"
    shift
    local options=("$@")

    echo ""
    printf "${BOLD}${CYAN}%s${NC}\n" "$prompt"
    echo ""

    local i=1
    for option in "${options[@]}"; do
        printf "  ${GREEN}%2d${NC}) %s\n" "$i" "$option"
        ((i++))
    done
    echo ""

    while true; do
        printf "${YELLOW}#${NC} Enter choice [1-%d]: " "${#options[@]}"
        read -r choice < /dev/tty
        echo ""

        if [[ "$choice" =~ ^[0-9]+$ ]] && [ "$choice" -ge 1 ] && [ "$choice" -le "${#options[@]}" ]; then
            ASK_CHOICE_RESULT=$((choice - 1))
            return 0
        else
            print_error "Invalid choice. Please enter a number between 1 and ${#options[@]}"
        fi
    done
}

# Detect operating system
detect_os() {
    case "$(uname -s)" in
        Linux*)     echo "linux" ;;
        Darwin*)    echo "macos" ;;
        CYGWIN*|MINGW*|MSYS*) echo "windows" ;;
        *)          echo "unknown" ;;
    esac
}

# ============================================================================
# ERROR HANDLING & ROLLBACK
# ============================================================================

# Initialize rollback system
init_rollback() {
    mkdir -p "$BACKUP_DIR"
    echo "$BACKUP_DIR" > /tmp/leindex-backup-dir-$$
}

# Backup file for potential rollback
backup_file() {
    local file="$1"

    if [[ -f "$file" ]]; then
        local backup_name="$BACKUP_DIR/$(basename "$file")-$(echo "$file" | tr '/' '_')"
        cp "$file" "$backup_name"
        echo "$backup_name:$file" >> "$BACKUP_DIR/manifest.txt"
        print_warning "Backed up: $file"
    fi
}

# Rollback changes on failure
rollback() {
    local exit_code=$1

    if [[ $exit_code -eq 0 ]]; then
        rm -rf "$BACKUP_DIR" 2>/dev/null || true
        return 0
    fi

    echo ""
    print_error "Installation failed. Rolling back changes..."
    echo ""

    if [[ -f "$BACKUP_DIR/manifest.txt" ]]; then
        while IFS=: read -r backup original; do
            if [[ -f "$backup" ]]; then
                cp "$backup" "$original"
                print_success "Restored: $original"
            fi
        done < "$BACKUP_DIR/manifest.txt"
    fi

    rmdir "$CONFIG_DIR" 2>/dev/null || true
    rmdir "$DATA_DIR" 2>/dev/null || true
    rmdir "$LOG_DIR" 2>/dev/null || true

    print_warning "Rollback complete"
    rm -rf "$BACKUP_DIR" 2>/dev/null || true
}

# Set up error trap - but only for critical failures (exit code 1)
# Tool configuration failures (exit code 2) should not trigger rollback
handle_error() {
    local exit_code=$?
    if [[ $exit_code -eq 1 ]]; then
        rollback $exit_code
    elif [[ $exit_code -ne 0 ]]; then
        echo ""
        print_warning "Some tool configurations failed, but LeIndex is installed"
        print_info "You can configure tools manually later"
        echo ""
        print_warning "Installation log: $INSTALL_LOG"
        print_info "Run with DEBUG=1 for detailed output:"
        print_bullet "DEBUG=1 ./install.sh"
    fi
}

trap 'handle_error' EXIT

# ============================================================================
# ENVIRONMENT DETECTION
# ============================================================================

# Total steps for progress tracking
TOTAL_STEPS=8

# Ensure uv is installed - our preferred package manager
# uv provides 10-100x faster operations than pip
ensure_uv() {
    # Check if uv is already installed
    if command -v uv &> /dev/null; then
        # Check if it's working
        if uv --version &> /dev/null; then
            return 0
        fi
    fi

    log_info "uv not found, installing..."

    # Detect the OS and install uv accordingly
    local os_type=$(detect_os)

    case "$os_type" in
        linux)
            # Try curl first, then wget, then local install
            if command -v curl &> /dev/null; then
                log_info "Installing uv via curl..."
                if curl -LsSf https://astral.sh/uv/install.sh | sh >> "$INSTALL_LOG" 2>&1; then
                    # Source the uv environment if installed to user profile
                    if [[ -f "$HOME/.local/bin/uv" ]]; then
                        export PATH="$HOME/.local/bin:$PATH"
                    fi
                    print_success "uv installed via curl"
                    return 0
                fi
            fi

            if command -v wget &> /dev/null; then
                log_info "Installing uv via wget..."
                if wget -qO- https://astral.sh/uv/install.sh | sh >> "$INSTALL_LOG" 2>&1; then
                    if [[ -f "$HOME/.local/bin/uv" ]]; then
                        export PATH="$HOME/.local/bin:$PATH"
                    fi
                    print_success "uv installed via wget"
                    return 0
                fi
            fi

            # Try pip install as last resort
            if $PYTHON_CMD -m pip --version &> /dev/null; then
                log_info "Installing uv via pip..."
                if $PYTHON_CMD -m pip install uv >> "$INSTALL_LOG" 2>&1; then
                    print_success "uv installed via pip"
                    return 0
                fi
            fi
            ;;
        macos)
            if command -v brew &> /dev/null; then
                log_info "Installing uv via brew..."
                if brew install uv >> "$INSTALL_LOG" 2>&1; then
                    print_success "uv installed via brew"
                    return 0
                fi
            fi

            # Fallback to curl on macOS
            if command -v curl &> /dev/null; then
                log_info "Installing uv via curl..."
                if curl -LsSf https://astral.sh/uv/install.sh | sh >> "$INSTALL_LOG" 2>&1; then
                    if [[ -f "$HOME/.local/bin/uv" ]]; then
                        export PATH="$HOME/.local/bin:$PATH"
                    fi
                    print_success "uv installed via curl"
                    return 0
                fi
            fi
            ;;
        windows)
            # Windows: use PowerShell install
            if powershell -Command "irm https://astral.sh/uv/install.ps1 | iex" >> "$INSTALL_LOG" 2>&1; then
                print_success "uv installed via PowerShell"
                return 0
            fi
            ;;
    esac

    log_warn "Failed to install uv automatically, falling back to pip"
    return 1
}

# Detect and setup Python interpreter
# This function will automatically create a Python 3.13 environment if needed
# Prioritizes uv for Python version management and venv creation
detect_python() {
    print_step 1 $TOTAL_STEPS "Setting up Python Environment"

    # Try to ensure uv is available first
    ensure_uv
    local has_uv=$?

    # Set up venv location
    LEINDEX_VENV="$LEINDEX_HOME/venv"
    VENV_PYTHON="$LEINDEX_VENV/bin/python"

    # If uv is available, use it for Python management
    if [[ $has_uv -eq 0 ]] && command -v uv &> /dev/null; then
        print_info "Using uv for Python environment management"

        # Check if venv already exists
        if [[ -f "$VENV_PYTHON" ]]; then
            PYTHON_VERSION=$($VENV_PYTHON -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
            print_success "Using existing venv: Python $PYTHON_VERSION"
            PYTHON_CMD="$VENV_PYTHON"
            return
        fi

        # Try to find Python 3.13 with uv
        print_info "Searching for Python 3.13..."
        local uv_python
        uv_python=$(uv python find 3.13 2>/dev/null | head -1)

        if [[ -n "$uv_python" && -f "$uv_python" ]]; then
            print_success "Found Python 3.13 via uv"
            PYTHON_CMD="$uv_python"
            PYTHON_VERSION="3.13"
            return
        fi

        # Try to install Python 3.13 with uv
        print_info "Installing Python 3.13 via uv..."
        if uv python install 3.13 >> "$INSTALL_LOG" 2>&1; then
            uv_python=$(uv python find 3.13 2>/dev/null | head -1)
            if [[ -n "$uv_python" && -f "$uv_python" ]]; then
                print_success "Python 3.13 installed via uv"
                PYTHON_CMD="$uv_python"
                PYTHON_VERSION="3.13"
                return
            fi
        fi

        # Fall back to creating venv with uv
        print_info "Creating venv with uv..."
        mkdir -p "$LEINDEX_HOME"
        if uv venv "$LEINDEX_VENV" --python 3.13 >> "$INSTALL_LOG" 2>&1; then
            print_success "Venv created with uv"
            PYTHON_CMD="$VENV_PYTHON"
            PYTHON_VERSION=$($PYTHON_CMD -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
            print_success "Using Python $PYTHON_VERSION from venv"
            return
        fi
    fi

    # Fallback: Try to find system Python 3.10-3.13
    print_info "Searching for system Python..."
    local preferred_python=("python3.13" "python3.12" "python3.11" "python3.10")
    for cmd in "${preferred_python[@]}" "python3" "python"; do
        if command -v "$cmd" &> /dev/null; then
            local ver=$($cmd -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
            local maj=$($cmd -c 'import sys; print(sys.version_info.major)')
            local min=$($cmd -c 'import sys; print(sys.version_info.minor)')

            if [[ $maj -eq 3 && $min -ge 10 && $min -le 13 ]]; then
                print_success "Found Python $ver"
                PYTHON_CMD="$cmd"
                PYTHON_VERSION="$ver"
                return
            fi
        fi
    done

    # If no compatible Python found, create venv using system tools
    print_warning "No Python 3.10-3.13 found, creating venv..."

    # Try to find Python 3.13 for venv creation
    local venv_creator=""
    for cmd in python3.13 python3.12 python3.11 python3.10; do
        if command -v "$cmd" &> /dev/null; then
            local v=$($cmd -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
            local maj=$($cmd -c 'import sys; print(sys.version_info.major)')
            local min=$($cmd -c 'import sys; print(sys.version_info.minor)')
            if [[ $maj -eq 3 && $min -ge 10 && $min -le 13 ]]; then
                venv_creator="$cmd"
                print_info "Found Python $v for venv creation"
                break
            fi
        fi
    done

    if [[ -z "$venv_creator" ]]; then
        print_info "Attempting to install Python 3.13..."
        local os_type=$(detect_os)

        case "$os_type" in
            linux)
                if command -v apt &> /dev/null; then
                    print_info "Installing Python 3.13 via apt..."
                    if sudo apt update && sudo apt install -y python3.13 python3.13-venv python3.13-dev python3.13-distutils 2>> "$INSTALL_LOG"; then
                        venv_creator="python3.13"
                    fi
                elif command -v dnf &> /dev/null; then
                    print_info "Installing Python 3.13 via dnf..."
                    if sudo dnf install -y python3.13 python3.13-pip python3.13-devel 2>> "$INSTALL_LOG"; then
                        venv_creator="python3.13"
                    fi
                elif command -v pacman &> /dev/null; then
                    print_info "Installing Python 3.13 via pacman..."
                    if sudo pacman -S --noconfirm python3.13 2>> "$INSTALL_LOG"; then
                        venv_creator="python3.13"
                    fi
                fi
                ;;
            macos)
                if command -v brew &> /dev/null; then
                    print_info "Installing Python 3.13 via brew..."
                    if brew install python@3.13 2>> "$INSTALL_LOG"; then
                        venv_creator="python3.13"
                    fi
                fi
                ;;
        esac
    fi

    if [[ -z "$venv_creator" ]]; then
        print_error "Failed to find or install Python 3.10-3.13"
        echo ""
        printf "${BOLD}Please install Python 3.13 manually:${NC}\n"
        print_bullet "Ubuntu/Debian: ${CYAN}sudo apt install python3.13 python3.13-venv${NC}"
        print_bullet "macOS: ${CYAN}brew install python@3.13${NC}"
        print_bullet "pyenv: ${CYAN}pyenv install 3.13${NC}"
        print_bullet "uv: ${CYAN}curl -LsSf https://astral.sh/uv/install.sh | sh${NC}"
        exit 1
    fi

    # Create the virtual environment
    print_info "Creating virtual environment at: $LEINDEX_VENV"
    mkdir -p "$LEINDEX_HOME"
    if $venv_creator -m venv "$LEINDEX_VENV" >> "$INSTALL_LOG" 2>&1; then
        print_success "Virtual environment created"
        PYTHON_CMD="$VENV_PYTHON"
        PYTHON_VERSION=$($PYTHON_CMD -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
        print_success "Using Python $PYTHON_VERSION from venv"
    else
        print_error "Failed to create virtual environment"
        print_info "Check log: $INSTALL_LOG"
        exit 1
    fi
}
# Detect package manager
# Priority: uv > venv pip > pipx > system pip
detect_package_manager() {
    print_step 2 $TOTAL_STEPS "Detecting Package Manager"

    # PRIORITY 1: uv (fastest, handles Python versions too)
    if command -v uv &> /dev/null && uv --version &> /dev/null; then
        PKG_MANAGER="uv"
        PKG_INSTALL_CMD="uv pip install"
        print_success "uv detected (⚡ 10-100x faster than pip)"
        return
    fi

    # PRIORITY 2: venv's pip (isolated, uses correct Python)
    if [[ -n "${LEINDEX_VENV:-}" ]] && [[ -f "$LEINDEX_VENV/bin/pip" ]]; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="$PYTHON_CMD -m pip install"
        print_success "Using venv pip (isolated environment)"
        return
    fi

    # PRIORITY 3: pipx (isolated installations)
    if command -v pipx &> /dev/null; then
        PKG_MANAGER="pipx"
        PKG_INSTALL_CMD="pipx install"
        print_success "pipx detected (isolated installations)"
        return
    fi

    # PRIORITY 4: Python module pip (via detected Python)
    if $PYTHON_CMD -m pip --version &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="$PYTHON_CMD -m pip install"
        print_success "pip (via Python module) detected"
        return
    fi

    # PRIORITY 5: System pip3
    if command -v pip3 &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="pip3 install"
        print_success "pip3 detected"
        return
    fi

    # PRIORITY 6: System pip
    if command -v pip &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="pip install"
        print_success "pip detected"
        return
    fi

    # No package manager found - offer installation
    print_error "No package manager found"
    echo ""
    printf "${BOLD}Installing uv (recommended):${NC}\n"
    print_bullet "curl -LsSf https://astral.sh/uv/install.sh | sh"
    echo ""
    printf "${BOLD}Or install pip:${NC}\n"
    print_bullet "$PYTHON_CMD -m ensurepip --upgrade"
    exit 1
}

# Get display name for tool
get_tool_display_name() {
    case "$1" in
        # CLI Tools
        "claude-cli") echo "Claude CLI" ;;
        "claude-code-cli") echo "Claude Code" ;;
        "codex-cli") echo "Codex CLI" ;;
        "amp-code") echo "Amp Code" ;;
        "opencode") echo "OpenCode" ;;
        "qwen-cli") echo "Qwen CLI" ;;
        "kilocode-cli") echo "Kilocode CLI" ;;
        "goose-cli") echo "Goose CLI" ;;
        "iflow-cli") echo "iFlow CLI" ;;
        "droid-cli") echo "Droid CLI" ;;
        "gemini-cli") echo "Gemini CLI" ;;
        "aider") echo "Aider" ;;
        "mistral-cli") echo "Mistral CLI" ;;
        "gpt-cli") echo "GPT CLI" ;;
        "cursor-cli") echo "Cursor CLI" ;;
        "pliny-cli") echo "Pliny CLI" ;;
        "continue-cli") echo "Continue CLI" ;;
        # Editors/IDEs
        "cursor") echo "Cursor IDE" ;;
        "antigravity") echo "Antigravity" ;;
        "zed") echo "Zed Editor" ;;
        "vscode") echo "VS Code" ;;
        "vscodium") echo "VSCodium" ;;
        "jetbrains") echo "JetBrains IDEs" ;;
        "windsurf") echo "Windsurf" ;;
        "continue") echo "Continue" ;;
        "claude-desktop") echo "Claude Desktop" ;;
        *) echo "$1" ;;
    esac
}

# Detect installed AI tools
detect_ai_tools() {
    print_step 3 $TOTAL_STEPS "Detecting AI Coding Tools"

    local detected_editors=()
    local detected_clis=()

    # Common binary locations for explicit checking
    local common_bins=("/usr/local/bin" "/usr/bin" "$HOME/.local/bin" "$HOME/bin" "/opt/homebrew/bin" "/opt/homebrew/sbin" "/opt/bin")

    # Helper function to check for executable in PATH and common bins
    check_cmd() {
        local cmd="$1"
        # Check PATH first
        command -v "$cmd" &> /dev/null && return 0
        # Check common bin directories
        for bin_dir in "${common_bins[@]}"; do
            [[ -x "$bin_dir/$cmd" ]] && return 0
        done
        return 1
    }

    # ============================================================================
    # EDITORS / IDEs - Check config directories
    # ============================================================================

    # Claude Desktop
    [[ -d "$HOME/.config/claude" ]] && detected_editors+=("claude-desktop")

    # Cursor IDE
    [[ -d "$HOME/.cursor" || -d "$HOME/.config/cursor" ]] && detected_editors+=("cursor")

    # Antigravity
    [[ -d "$HOME/.config/antigravity" || -d "$HOME/.antigravity" ]] && detected_editors+=("antigravity")

    # VS Code / VSCodium
    [[ -d "$HOME/.config/Code" ]] && detected_editors+=("vscode")
    [[ -d "$HOME/.config/VSCodium" ]] && detected_editors+=("vscodium")

    # Zed Editor (native / Flatpak)
    [[ -d "$HOME/.config/zed" || -d "$HOME/.var/app/dev.zed.Zed/config/zed" || -d "$HOME/.var/app/com.zed.Zed/config/zed" ]] && detected_editors+=("zed")

    # JetBrains IDEs
    [[ -d "$HOME/.config/JetBrains" ]] && detected_editors+=("jetbrains")

    # Windsurf
    [[ -d "$HOME/.config/windsurf" ]] && detected_editors+=("windsurf")

    # Continue
    [[ -d "$HOME/.config/continue" ]] && detected_editors+=("continue")

    # ============================================================================
    # CLI TOOLS - Check executables (with alternative names)
    # ============================================================================

    # Claude Code (check for config directory)
    [[ -d "$HOME/.claude" ]] && detected_clis+=("claude-code-cli")

    # Claude CLI (standalone)
    check_cmd "claude" && detected_clis+=("claude-cli")

    # Codex CLI
    check_cmd "codex" && detected_clis+=("codex-cli")

    # Amp Code (amp or amp-code)
    check_cmd "amp" && detected_clis+=("amp-code")
    check_cmd "amp-code" && detected_clis+=("amp-code")

    # OpenCode
    check_cmd "opencode" && detected_clis+=("opencode")

    # Qwen CLI (qwen or qwen-cli)
    check_cmd "qwen" && detected_clis+=("qwen-cli")
    check_cmd "qwen-cli" && detected_clis+=("qwen-cli")

    # Kilocode CLI (kilocode or kilocode-cli)
    check_cmd "kilocode" && detected_clis+=("kilocode-cli")
    check_cmd "kilocode-cli" && detected_clis+=("kilocode-cli")

    # Goose CLI
    check_cmd "goose" && detected_clis+=("goose-cli")

    # iFlow CLI
    check_cmd "iflow" && detected_clis+=("iflow-cli")

    # Droid CLI
    check_cmd "droid" && detected_clis+=("droid-cli")

    # Gemini CLI
    check_cmd "gemini" && detected_clis+=("gemini-cli")

    # Aider
    check_cmd "aider" && detected_clis+=("aider")

    # Mistral CLI
    check_cmd "mistral" && detected_clis+=("mistral-cli")

    # GPT CLI (gpt or gpt-cli)
    check_cmd "gpt" && detected_clis+=("gpt-cli")
    check_cmd "gpt-cli" && detected_clis+=("gpt-cli")

    # Cursor CLI
    check_cmd "cursor-cli" && detected_clis+=("cursor-cli")

    # Pliny CLI
    check_cmd "pliny" && detected_clis+=("pliny-cli")

    # Continue CLI
    check_cmd "continue-cli" && detected_clis+=("continue-cli")

    # ============================================================================
    # DISPLAY RESULTS
    # ============================================================================

    local total_count=$((${#detected_editors[@]} + ${#detected_clis[@]}))

    if [[ $total_count -gt 0 ]]; then
        print_success "Great news! Found $total_count AI tool(s) on your system:"

        # Display Editors & IDEs
        if [[ ${#detected_editors[@]} -gt 0 ]]; then
            echo ""
            printf "${BOLD}${CYAN}Editors & IDEs:${NC}\n"
            for tool in "${detected_editors[@]}"; do
                print_bullet "$(get_tool_display_name "$tool")"
            done
        fi

        # Display CLI Tools
        if [[ ${#detected_clis[@]} -gt 0 ]]; then
            echo ""
            printf "${BOLD}${CYAN}CLI Tools:${NC}\n"
            for tool in "${detected_clis[@]}"; do
                print_bullet "$(get_tool_display_name "$tool")"
            done
        fi
    else
        print_warning "No AI tools detected"
        print_info "That's okay! We'll install LeIndex as a standalone MCP server"
    fi
}

# ============================================================================
# INSTALLATION
# ============================================================================

# Install LeIndex package
install_leindex() {
    print_step 4 $TOTAL_STEPS "Installing LeIndex"

    print_info "Upgrading package manager..."
    case "$PKG_MANAGER" in
        uv)
            uv self-update 2>/dev/null || true
            ;;
        pip|pipx)
            $PYTHON_CMD -m pip install --upgrade pip setuptools wheel 2>/dev/null || true
            ;;
    esac

    # Force reinstall to ensure clean installation
    print_info "Removing old $PYPI_PACKAGE installation (if present)..."
    case "$PKG_MANAGER" in
        uv)
            uv pip uninstall "$PYPI_PACKAGE" -y 2>/dev/null || true
            ;;
        pipx)
            pipx uninstall "$PYPI_PACKAGE" 2>/dev/null || true
            ;;
        pip)
            $PYTHON_CMD -m pip uninstall "$PYPI_PACKAGE" -y 2>/dev/null || true
            ;;
    esac

    print_info "Installing fresh $PYPI_PACKAGE..."
    if $PKG_INSTALL_CMD "$PYPI_PACKAGE"; then
        print_success "$PROJECT_NAME installed successfully"
    else
        print_error "Failed to install $PYPI_PACKAGE"
        exit 1
    fi

    if [[ "$PKG_MANAGER" == "uv" ]]; then
        print_success "Installation verified (via uv)"
    elif $PYTHON_CMD -c "import leindex.server" 2>/dev/null; then
        VERSION=$($PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>/dev/null || echo "unknown")
        print_success "Installation verified: version $VERSION"
    else
        print_error "Installation verification failed"
        exit 1
    fi
}

# Setup directory structure
setup_directories() {
    print_step 5 $TOTAL_STEPS "Setting up Directories"

    for dir in "$CONFIG_DIR" "$DATA_DIR" "$LOG_DIR"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            print_success "Created: $dir"
        fi
    done
}

# Run first-time setup for v2.0 configuration
run_first_time_setup() {
    log_info "Running first-time setup for configuration..."

    # Create a Python script to run first_time_setup
    local python_output
    python_output=$($PYTHON_CMD -c "
import sys
sys.path.insert(0, '.')
try:
    from leindex.config.setup import first_time_setup, SetupResult
    result = first_time_setup()
    if result.success:
        print(f'SUCCESS: Config created at {result.config_path}', file=sys.stderr)
        if result.hardware_info:
            print(f'INFO: Hardware detected: {result.hardware_info}', file=sys.stderr)
    else:
        print(f'ERROR: {result.error}', file=sys.stderr)
        sys.exit(1)
except Exception as e:
    print(f'WARNING: First-time setup failed: {e}', file=sys.stderr)
    print('INFO: Will use default configuration', file=sys.stderr)
" 2>&1)

    local exit_code=$?
    echo "$python_output" >> "$INSTALL_LOG"

    if [[ $exit_code -eq 0 ]]; then
        print_success "Configuration initialized"
    else
        print_warning "First-time setup completed with warnings"
        print_info "Default configuration will be used"
    fi
}

# Detect and migrate v1 configuration to v2
detect_and_migrate_config() {
    print_info "Checking for v1 configuration..."

    local old_config="$LEINDEX_HOME/config.yaml"
    local new_config="$HOME/.leindex/mcp_config.yaml"

    if [[ -f "$old_config" ]] && [[ ! -f "$new_config" ]]; then
        print_warning "Found v1 configuration"
        print_info "Migrating to v2 format..."

        # Run migration
        local python_output
        python_output=$($PYTHON_CMD -c "
import sys
import os
sys.path.insert(0, '.')
try:
    from leindex.config.migration import migrate_config
    from leindex.config.global_config import GlobalConfigManager

    old_path = '$old_config'
    new_path = '$new_config'

    if os.path.exists(old_path):
        # Migrate config
        migrated_config = migrate_config(old_path, '1.0', '2.0')

        # Save migrated config
        manager = GlobalConfigManager(new_path)
        from leindex.config.global_config import GlobalConfig
        config = manager._dict_to_dataclass(migrated_config)
        manager.save_config(config)

        print(f'SUCCESS: Migrated config to {new_path}', file=sys.stderr)

        # Backup old config
        backup_path = old_path + '.v1_backup'
        os.rename(old_path, backup_path)
        print(f'INFO: Old config backed up to {backup_path}', file=sys.stderr)
except Exception as e:
    print(f'ERROR: Migration failed: {e}', file=sys.stderr)
    print('INFO: Please migrate manually', file=sys.stderr)
    sys.exit(1)
" 2>&1)

        if [[ $? -eq 0 ]]; then
            print_success "Configuration migrated to v2"
        else
            print_warning "Migration had issues, please check logs"
        fi
    elif [[ -f "$new_config" ]]; then
        print_success "v2 configuration already exists"
    fi
}

# ============================================================================
# TOOL INTEGRATION
# ============================================================================

# Merge JSON configuration for Claude Desktop/Cursor (no disabled/env fields)
merge_json_config() {
    local config_file="$1"
    local server_name="$2"
    local server_command="${3:-leindex}"

    log_info "merge_json_config called with: config_file=$config_file, server_name=$server_name, command=$server_command"
    log_debug "Config file path: $config_file"

    # Run Python script and capture exit status and output
    local python_output
    local python_exit_code

    log_debug "Starting Python script execution..."
    log_cmd "$PYTHON_CMD -c \"<JSON merge script>\""

    python_output=$($PYTHON_CMD -c "
import json
import sys
import os

config_file = \"$config_file\"
server_name = \"$server_name\"
server_command = \"$server_command\"

print(f\"[DEBUG] Config file: {config_file}\", file=sys.stderr)
print(f\"[DEBUG] Server name: {server_name}\", file=sys.stderr)
print(f\"[DEBUG] Server command: {server_command}\", file=sys.stderr)

# Ensure directory exists
config_dir = os.path.dirname(config_file)
if config_dir and not os.path.exists(config_dir):
    try:
        os.makedirs(config_dir, exist_ok=True)
        print(f\"[INFO] Created directory: {config_dir}\", file=sys.stderr)
    except Exception as e:
        print(f\"[ERROR] Error creating directory {config_dir}: {e}\", file=sys.stderr)
        sys.exit(1)

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
                print(f\"[INFO] Loaded existing config from {config_file}\", file=sys.stderr)
            except json.JSONDecodeError as e:
                print(f\"[WARN] Existing config is invalid JSON: {e}\", file=sys.stderr)
                print(f\"[INFO] Backing up invalid config and creating new one.\", file=sys.stderr)
                config = {}
        else:
            print(f\"[INFO] Config file exists but is empty\", file=sys.stderr)
            config = {}
except FileNotFoundError:
    config = {}
    print(f\"[INFO] Config file not found, will create: {config_file}\", file=sys.stderr)
except Exception as e:
    print(f\"[ERROR] Error reading config: {e}\", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    config = {}

if 'mcpServers' not in config:
    print(f\"[INFO] Creating mcpServers section\", file=sys.stderr)
    config['mcpServers'] = {}

# Check if server already exists and explicitly remove it for fresh config
if server_name in config.get('mcpServers', {}):
    existing_config = config['mcpServers'][server_name]
    print(f\"[INFO] Server '{server_name}' already configured. Removing old config for refresh...\", file=sys.stderr)
    print(f\"[DEBUG] Old config: {existing_config}\", file=sys.stderr)
    del config['mcpServers'][server_name]

# Claude Desktop/Cursor do NOT support 'disabled' or 'env' fields
new_server_config = {
    'command': server_command,
    'args': ['mcp']
}
print(f\"[INFO] Writing fresh config for '{server_name}'...\", file=sys.stderr)
print(f\"[DEBUG] New server config: {new_server_config}\", file=sys.stderr)
config['mcpServers'][server_name] = new_server_config

# Validate config can be serialized
try:
    json.dumps(config)
    print(f\"[DEBUG] Config is valid JSON\", file=sys.stderr)
except (TypeError, ValueError) as e:
    print(f\"[ERROR] Invalid configuration structure: {e}\", file=sys.stderr)
    sys.exit(1)

# Write to file with explicit error handling
try:
    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')
    print(f\"[SUCCESS] Successfully wrote: {config_file}\", file=sys.stderr)
    print(f\"[DEBUG] Final config: {json.dumps(config, indent=2)}\", file=sys.stderr)
except Exception as e:
    print(f\"[ERROR] Error writing config file {config_file}: {e}\", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    sys.exit(1)

print(f\"Updated: {config_file}\")
" 2>&1)
    python_exit_code=$?

    log_debug "Python script exit code: $python_exit_code"
    echo "$python_output" >> "$INSTALL_LOG"

    if [[ $python_exit_code -ne 0 ]]; then
        log_error "Python script failed with exit code $python_exit_code"
        log_error "Python output:"
        echo "$python_output" | while IFS= read -r line; do
            log_error "  $line"
        done
        return 1
    fi

    log_debug "Python script output:"
    echo "$python_output" | while IFS= read -r line; do
        log_debug "  $line"
    done

    log_success "Successfully merged JSON config to $config_file"

    # Return Python script's exit status
    return $python_exit_code
}

# Get the correct server command for MCP configuration
# Returns 'leindex' if installed in PATH, otherwise returns the full Python module path
get_server_command() {
    # If using a venv, use the Python interpreter directly
    if [[ -n "${LEINDEX_VENV:-}" ]] && [[ -f "$LEINDEX_VENV/bin/python" ]]; then
        echo "$LEINDEX_VENV/bin/python -m leindex.server"
        return
    fi

    # Check if leindex command is in PATH
    if command -v leindex &>/dev/null; then
        echo "leindex"
        return
    fi

    # Fall back to using the detected Python with module
    echo "$PYTHON_CMD -m leindex.server"
}

# Configure Claude Desktop
configure_claude_desktop() {
    print_section "Configuring Claude Desktop"
    log_info "Starting Claude Desktop configuration"

    # Search for Claude Desktop config (NOT Claude Code!)
    # Claude Desktop uses claude_desktop_config.json
    local claude_configs=(
        "$HOME/.config/claude/claude_desktop_config.json"
        "$HOME/.config/Claude/claude_desktop_config.json"
    )

    local config_file=""
    local config_dir=""

    log_info "Searching for Claude Desktop config..."
    for conf in "${claude_configs[@]}"; do
        log_debug "Checking for config at: $conf"
        if [[ -f "$conf" ]]; then
            config_file="$conf"
            config_dir=$(dirname "$conf")
            log_info "Found existing config at: $config_file"
            print_bullet "Found config at: $config_file"
            break
        fi
    done

    # If no existing config found, create in default location
    if [[ -z "$config_file" ]]; then
        config_dir="$HOME/.config/claude"
        config_file="$config_dir/claude_desktop_config.json"
        log_info "No existing config found. Will create: $config_file"
        print_info "No existing config found. Will create: $config_file"
    fi

    log_debug "Config directory: $config_dir"
    log_debug "Config file: $config_file"

    log_debug "Creating config directory if needed..."
    if mkdir -p "$config_dir" 2>> "$INSTALL_LOG"; then
        log_success "Created/configirmed config directory: $config_dir"
    else
        log_error "Failed to create config directory: $config_dir"
        print_error "Failed to create config directory"
        return 2
    fi

    log_debug "Backing up existing config file..."
    if backup_file "$config_file" 2>> "$INSTALL_LOG"; then
        log_debug "Backup completed"
    else
        log_debug "No existing file to backup (or backup failed)"
    fi

    log_info "Calling merge_json_config for Claude Desktop..."
    local server_cmd
    server_cmd=$(get_server_command)
    if merge_json_config "$config_file" "leindex" "$server_cmd"; then
        log_success "Claude Desktop configured successfully"
        print_success "Claude Desktop configured"
        print_bullet "Config: $config_file"
    else
        log_error "Failed to configure Claude Desktop"
        print_error "Failed to configure Claude Desktop"
        log_info "Check log file for details: $INSTALL_LOG"
        return 2
    fi
}

# Configure Claude Code (different from Claude Desktop!)
configure_claude_cli() {
    print_section "Configuring Claude Code"

    # Claude Code uses ~/.claude/.claude.json as the main config file
    # Fallback to ~/.claude.json then ~/.config/claude-code/mcp.json if the main file doesn't exist
    local config_file=""
    local config_dir=""

    # Primary location: ~/.claude/.claude.json (main Claude Code settings)
    if [[ -f "$HOME/.claude/.claude.json" ]]; then
        config_file="$HOME/.claude/.claude.json"
        config_dir="$HOME/.claude"
        print_bullet "Found main config at: $config_file"
    elif [[ -f "$HOME/.claude.json" ]]; then
        # Secondary location: ~/.claude.json (older location)
        config_file="$HOME/.claude.json"
        config_dir="$HOME"
        print_info "Using secondary config location: $config_file"
    else
        # Fallback location: ~/.config/claude-code/mcp.json
        config_file="$HOME/.config/claude-code/mcp.json"
        config_dir="$HOME/.config/claude-code"
        print_info "No main config found. Using fallback: $config_file"
    fi

    mkdir -p "$config_dir" || { print_warning "Failed to create config directory"; return 2; }
    backup_file "$config_file" 2>/dev/null || true

    # Get the correct server command
    local server_cmd
    server_cmd=$(get_server_command)

    # Claude Code MCP config format with proper merging
    # For ~/.claude.json: MERGE into existing mcpServers object
    # For fallback file: create/update mcpServers structure
    if $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"
server_name = "leindex"
server_command = "$server_cmd"
is_main_config = config_file.endswith(".claude.json")

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
                print(f"Notice: Loaded existing config from {config_file}", file=sys.stderr)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                config = {}
        else:
            print(f"Notice: Config file exists but is empty", file=sys.stderr)
            config = {}
except FileNotFoundError:
    config = {}
    print(f"Notice: Config file not found, will create: {config_file}", file=sys.stderr)
except Exception as e:
    print(f"Warning: Error reading config: {e}", file=sys.stderr)
    config = {}

# Ensure mcpServers key exists
if 'mcpServers' not in config:
    print(f"Notice: Creating mcpServers section", file=sys.stderr)
    config['mcpServers'] = {}

# Check if server already exists and explicitly remove it for fresh config
if server_name in config.get('mcpServers', {}):
    existing_config = config['mcpServers'][server_name]
    print(f"Notice: Server '{server_name}' already configured. Removing old config for refresh...", file=sys.stderr)
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config['mcpServers'][server_name]

# Add/update the LeIndex MCP server with correct args
print(f"Notice: Writing fresh config for '{server_name}'...", file=sys.stderr)
config['mcpServers'][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

# Write back to file (preserving all other settings)
with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF
    then
        print_success "Claude Code configured"
        print_bullet "Config: $config_file"
    else
        print_warning "Failed to configure Claude Code"
        return 2
    fi
}

# Configure Cursor IDE
configure_cursor() {
    print_section "Configuring Cursor IDE"

    # Search for Cursor config in multiple locations (system-agnostic)
    local cursor_configs=(
        "$HOME/.cursor/mcp.json"  # Primary location
        "$HOME/.claude.json"  # Some setups use this for Cursor too
        "$HOME/.config/cursor/mcp.json"
    )

    local config_file=""
    local config_dir=""

    for conf in "${cursor_configs[@]}"; do
        if [[ -f "$conf" ]]; then
            config_file="$conf"
            config_dir=$(dirname "$conf")
            print_bullet "Found config at: $config_file"
            break
        fi
    done

    # If no existing config found, create in default location
    if [[ -z "$config_file" ]]; then
        config_dir="$HOME/.cursor"
        config_file="$config_dir/mcp.json"
        print_info "No existing config found. Will create: $config_file"
    fi

    mkdir -p "$config_dir" || { print_warning "Failed to create config directory"; return 2; }
    backup_file "$config_file" 2>/dev/null || true

    local server_cmd
    server_cmd=$(get_server_command)
    if merge_json_config "$config_file" "leindex" "$server_cmd"; then
        print_success "Cursor configured"
        print_bullet "Config: $config_file"
    else
        print_warning "Failed to configure Cursor"
        return 2
    fi
}

# Merge JSON configuration for VS Code (extension-specific)
merge_vscode_config() {
    local config_file="$1"
    local server_name="$2"
    local server_command="${3:-leindex}"
    local extension_key="${4:-cline.mcpServers}"  # Default to Cline

    # Run Python script and capture exit status
    $PYTHON_CMD -c "
import json
import sys
import os

config_file = \"$config_file\"
server_name = \"$server_name\"
server_command = \"$server_command\"
extension_key = \"$extension_key\"

# Ensure directory exists
config_dir = os.path.dirname(config_file)
if config_dir and not os.path.exists(config_dir):
    try:
        os.makedirs(config_dir, exist_ok=True)
        print(f\"Created directory: {config_dir}\", file=sys.stderr)
    except Exception as e:
        print(f\"Error creating directory {config_dir}: {e}\", file=sys.stderr)
        sys.exit(1)

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f\"Warning: Existing config is invalid JSON: {e}\", file=sys.stderr)
                print(f\"Backing up invalid config and creating new one.\", file=sys.stderr)
                config = {}
        else:
            config = {}
except FileNotFoundError:
    config = {}
    print(f\"Config file not found, will create: {config_file}\", file=sys.stderr)
except Exception as e:
    print(f\"Error reading config: {e}\", file=sys.stderr)
    config = {}

# Ensure extension-specific key exists
if extension_key not in config:
    config[extension_key] = {}

# Check if server already exists and explicitly remove it for fresh config
if extension_key in config and server_name in config[extension_key]:
    existing_config = config[extension_key][server_name]
    print(f\"Notice: Server '{server_name}' already configured in {extension_key}. Removing for refresh...\", file=sys.stderr)
    print(f\"Debug: Old config: {existing_config}\", file=sys.stderr)
    del config[extension_key][server_name]

# Add server to extension-specific config
print(f\"Notice: Writing fresh config for '{server_name}' in {extension_key}...\", file=sys.stderr)
config[extension_key][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

# Validate config can be serialized
try:
    json.dumps(config)
except (TypeError, ValueError) as e:
    print(f\"Error: Invalid configuration structure: {e}\", file=sys.stderr)
    sys.exit(1)

# Write to file with explicit error handling
try:
    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')
    print(f\"Successfully wrote: {config_file}\", file=sys.stderr)
except Exception as e:
    print(f\"Error writing config file {config_file}: {e}\", file=sys.stderr)
    sys.exit(1)

print(f\"Updated: {config_file}\")
"

    # Return Python script's exit status
    return $?
}

# Configure VS Code / VSCodium
# $1 = "auto" to skip prompt and use Cline as default
configure_vscode() {
    print_section "Configuring VS Code Family"

    local extension_key="cline.mcpServers"
    local extension_name="Cline"

    # Only ask for extension choice if not in auto mode
    if [[ "${1:-}" != "auto" ]]; then
        # Ask which MCP extension the user uses
        echo ""
        printf "${BOLD}${CYAN}Which VS Code MCP extension are you using?${NC}\n"
        echo ""

        local options=(
            "Cline (saoudrizwan.claude)"
            "Continue (Continue.continue)"
            "Skip VS Code configuration"
        )

        ASK_CHOICE_RESULT=""
        ask_choice "Select your MCP extension:" "${options[@]}"
        local choice=$ASK_CHOICE_RESULT
        echo ""

        if [[ $choice -eq 2 ]]; then
            print_warning "Skipping VS Code configuration"
            return
        fi

        if [[ $choice -eq 1 ]]; then
            extension_key="continue.mcpServers"
            extension_name="Continue"
        fi
    else
        print_info "Using Cline extension as default (most popular)"
        echo ""
    fi

    local vscode_configs=(
        "$HOME/.config/Code/User/settings.json"
        "$HOME/.config/VSCodium/User/settings.json"
        "$HOME/.vscode/settings.json"
    )

    for config_file in "${vscode_configs[@]}"; do
        local config_dir
        config_dir=$(dirname "$config_file")

        if [[ -d "$(dirname "$config_dir")" ]]; then
            mkdir -p "$config_dir"
            backup_file "$config_file" 2>/dev/null || true

            local server_cmd
            server_cmd=$(get_server_command)
            merge_vscode_config "$config_file" "leindex" "$server_cmd" "$extension_key"
            print_success "VS Code configured ($extension_name): $config_file"
        fi
    done

    print_info "Note: Make sure you have the $extension_name extension installed"
}

# Configure Zed Editor
configure_zed() {
    print_section "Configuring Zed Editor"

    # Get the correct server command
    local leindex_cmd
    leindex_cmd=$(get_server_command)

    local config_files=()
    local native_config_file="$HOME/.config/zed/settings.json"
    local flatpak_config_file="$HOME/.var/app/dev.zed.Zed/config/zed/settings.json"
    local flatpak_alt_config_file="$HOME/.var/app/com.zed.Zed/config/zed/settings.json"

    # Configure any existing Zed installations we can detect (native / Flatpak).
    # Avoid creating new Flatpak directories unless they already exist.
    if [[ -d "$(dirname "$flatpak_config_file")" || -f "$flatpak_config_file" ]]; then
        config_files+=("$flatpak_config_file")
    fi
    if [[ -d "$(dirname "$flatpak_alt_config_file")" || -f "$flatpak_alt_config_file" ]]; then
        config_files+=("$flatpak_alt_config_file")
    fi
    if [[ -d "$(dirname "$native_config_file")" || -f "$native_config_file" ]]; then
        config_files+=("$native_config_file")
    fi

    # Default to native path if nothing exists yet.
    if [[ ${#config_files[@]} -eq 0 ]]; then
        config_files=("$native_config_file")
    fi

    for config_file in "${config_files[@]}"; do
        local config_dir
        config_dir="$(dirname "$config_file")"

        mkdir -p "$config_dir"
        backup_file "$config_file" 2>/dev/null || true

        LEINDEX_CMD="$leindex_cmd" ZED_CONFIG_FILE="$config_file" $PYTHON_CMD << 'PYTHON_EOF'
import json
import os
import sys

config_file = os.environ.get("ZED_CONFIG_FILE")
leindex_cmd = os.environ.get("LEINDEX_CMD", "leindex")

if not config_file:
    print("Error: ZED_CONFIG_FILE not set", file=sys.stderr)
    sys.exit(1)

# Validate / load existing config (JSON only)
try:
    with open(config_file, "r") as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
                if not isinstance(config, dict):
                    print(
                        "Warning: Existing Zed settings is not a JSON object; overwriting.",
                        file=sys.stderr,
                    )
                    config = {}
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                print("Backing up invalid config and creating new one.", file=sys.stderr)
                config = {}
        else:
            config = {}
except FileNotFoundError:
    config = {}

# Zed expects MCP servers under `context_servers` (not `language_models`).
if "context_servers" not in config or not isinstance(config.get("context_servers"), dict):
    config["context_servers"] = {}

# Check if server already exists and explicitly remove it for a fresh config
if "leindex" in config["context_servers"]:
    existing_config = config["context_servers"]["leindex"]
    print(
        "Notice: Server 'leindex' already configured in Zed. Removing for refresh...",
        file=sys.stderr,
    )
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config["context_servers"]["leindex"]

# Clean up legacy location from older installer versions
language_models = config.get("language_models")
if isinstance(language_models, dict):
    mcp_servers = language_models.get("mcp_servers")
    if isinstance(mcp_servers, dict) and "leindex" in mcp_servers:
        print(
            "Notice: Removing legacy 'language_models.mcp_servers.leindex' entry...",
            file=sys.stderr,
        )
        del mcp_servers["leindex"]
        if not mcp_servers:
            language_models.pop("mcp_servers", None)
    if not language_models:
        config.pop("language_models", None)

# Add MCP server configuration
print("Notice: Writing fresh config for 'leindex' in Zed...", file=sys.stderr)
config["context_servers"]["leindex"] = {
    "enabled": True,
    "command": leindex_cmd,
    "args": ["mcp"],
}

# Validate config can be serialized
try:
    json.dumps(config)
except (TypeError, ValueError) as e:
    print(f"Error: Invalid configuration structure: {e}", file=sys.stderr)
    sys.exit(1)

with open(config_file, "w") as f:
    json.dump(config, f, indent=2)
    f.write("\n")

print(f"Updated: {config_file}")
PYTHON_EOF
    done

    print_success "Zed Editor configured"
    for config_file in "${config_files[@]}"; do
        print_bullet "Config: $config_file"
    done
}

# Configure JetBrains IDEs
configure_jetbrains() {
    print_section "Configuring JetBrains IDEs"

    local config_dir="$HOME/.config/JetBrains"

    if [[ -d "$config_dir" ]]; then
        print_info "JetBrains IDEs detected"
        print_info "Manual configuration required for JetBrains:"
        print_bullet "Install the 'MCP Support' plugin from JetBrains Marketplace"
        print_bullet "Configure MCP server: command='leindex', args=['mcp']"
        print_warning "See documentation for JetBrains-specific setup"
    else
        print_info "No JetBrains IDEs detected"
    fi
}

# Configure CLI tools (PATH setup for leindex command)
configure_cli_tools() {
    print_section "Configuring CLI Tools"

    if command -v leindex &> /dev/null; then
        print_success "'leindex' command available in PATH"
    else
        print_warning "'leindex' command not in PATH"
        print_info "Add Python user base to PATH:"

        local user_base
        user_base=$($PYTHON_CMD -m site --user-base 2>/dev/null || echo "$HOME/.local")
        local bin_dir="$user_base/bin"

        print_bullet "export PATH=\"\$PATH:$bin_dir\""

        local shell_config=""
        if [[ -n "${ZSH_VERSION:-}" ]]; then
            shell_config="$HOME/.zshrc"
        elif [[ -n "${BASH_VERSION:-}" ]]; then
            shell_config="$HOME/.bashrc"
        fi

        if [[ -n "$shell_config" ]] && ask_yes_no "Add to $shell_config?" "n"; then
            echo "" >> "$shell_config"
            echo "# LeIndex" >> "$shell_config"
            echo "export PATH=\"\$PATH:$bin_dir\"" >> "$shell_config"
            print_success "Added to $shell_config"
            print_warning "Run 'source $shell_config' or restart your shell"
        fi
    fi

    if command -v leindex-search &> /dev/null; then
        print_success "'leindex-search' command available"
    fi
}

# Generic CLI tool MCP configuration
# Most CLI AI tools use similar JSON config patterns
configure_cli_mcp() {
    local tool_name="$1"
    local config_file="$2"
    local display_name="$3"
    local json_key="${4:-mcpServers}"  # Optional: custom JSON key (e.g., 'amp.mcpServers', 'mcp')

    print_section "Configuring $display_name"

    local config_dir
    config_dir=$(dirname "$config_file")

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"
tool_name = "$tool_name"
json_key = "$json_key"

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure the JSON key exists (supports namespaced keys like 'amp.mcpServers')
if json_key not in config:
    config[json_key] = {}

# Check if server already exists and explicitly remove it for fresh config
if 'leindex' in config.get(json_key, {}):
    existing_config = config[json_key]['leindex']
    print(f"Notice: Server 'leindex' already configured. Removing for refresh...", file=sys.stderr)
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config[json_key]['leindex']

# Add LeIndex server
print(f"Notice: Writing fresh config for 'leindex' with key '{json_key}'...", file=sys.stderr)
config[json_key]['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "$display_name configured"
    print_bullet "Config: $config_file"
}

# Configure tool with TOML format (for tools like Codex CLI)
# Source: https://developers.openai.com/codex/mcp/
# Note: Codex uses [mcp_servers.servername] format (underscore, not camelCase!)
configure_toml_mcp() {
    local tool_name="$1"
    local config_file="$2"
    local display_name="$3"

    print_section "Configuring $display_name"

    local config_dir
    config_dir=$(dirname "$config_file")

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    # Append TOML configuration
    # Codex uses [mcp_servers.servername] format (underscore in key, not camelCase!)
    # Source: https://github.com/openai/codex/issues/2760
    cat >> "$config_file" << EOF

[mcp_servers.$tool_name]
command = "leindex"
args = ["mcp"]
EOF

    print_success "$display_name configured"
    print_bullet "Config: $config_file (TOML format)"
}

# Configure tool with YAML format for Goose extensions
# Source: https://block.github.io/goose/docs/guides/config-files/
# Goose uses 'extensions:' key with 'type: stdio' and 'cmd:' (not 'command:')
configure_yaml_mcp() {
    local tool_name="$1"
    local config_file="$2"
    local display_name="$3"

    print_section "Configuring $display_name"

    local config_dir
    config_dir=$(dirname "$config_file")

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    # Append YAML configuration for Goose extensions
    # Goose uses extensions: key (not mcpServers), with type: stdio and cmd:
    cat >> "$config_file" << EOF

extensions:
  $tool_name:
    type: stdio
    cmd: "leindex"
    args:
      - "mcp"
    description: "LeIndex code search MCP server"
EOF

    print_success "$display_name configured"
    print_bullet "Config: $config_file (YAML extensions format)"
}

# Configure Antigravity IDE
configure_antigravity() {
    print_section "Configuring Antigravity IDE"

    local config_dir="$HOME/.config/antigravity"
    local config_file="$config_dir/mcp_config.json"

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Check if server already exists and explicitly remove it for fresh config
if 'leindex' in config.get('mcpServers', {}):
    existing_config = config['mcpServers']['leindex']
    print(f"Notice: Server 'leindex' already configured. Removing for refresh...", file=sys.stderr)
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config['mcpServers']['leindex']

config['mcpServers']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}
print(f"Notice: Writing fresh config for 'leindex'...", file=sys.stderr)

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "Antigravity configured"
    print_bullet "Config: $config_file"
}

# Configure specific CLI tools
# Documentation sources: GitHub repos, official docs, and MCP setup guides

# Codex CLI - Uses TOML format (not JSON!)
# Source: https://github.com/openai/codex/blob/main/docs/config.md
configure_codex_cli() {
    configure_toml_mcp "leindex" "$HOME/.codex/config.toml" "Codex CLI"
}

# Amp Code - Uses namespaced key 'amp.mcpServers'
# Source: https://docs.netlify.com/build/build-with-aI/netlify-mcp-server/
configure_amp_code() {
    configure_cli_mcp "leindex" "$HOME/.config/amp/settings.json" "Amp Code" "amp.mcpServers"
}

# OpenCode - Uses 'mcp' key (not 'mcpServers')
# Requires 'type: local', command as array, and 'enabled' field
# Source: https://opencode.ai/docs/config/
configure_opencode() {
    print_section "Configuring OpenCode"

    local config_file="$HOME/.config/opencode/opencode.json"
    local config_dir
    config_dir=$(dirname "$config_file")

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    # Get the correct server command (handles venv, PATH, or Python module)
    local server_cmd
    server_cmd=$(get_server_command)

    # OpenCode requires 'type: local', command as array, and enabled field
    $PYTHON_CMD << PYTHON_EOF
import json
import sys
import shlex

config_file = "$config_file"
server_cmd = "$server_cmd"


# Parse server command into array
# If it's just "leindex", use that. Otherwise split by spaces.
if server_cmd.strip() == "leindex":
    command_array = ["leindex", "mcp"]
else:
    # Split the command properly (handles paths with spaces)
    cmd_parts = shlex.split(server_cmd)
    # Add 'mcp' as the last argument
    command_array = cmd_parts + ["mcp"]


try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure 'mcp' key exists
if 'mcp' not in config:
    config['mcp'] = {}

# Check if server already exists and explicitly remove it for fresh config
if 'leindex' in config.get('mcp', {}):
    existing_config = config['mcp']['leindex']
    print(f"Notice: Server 'leindex' already configured. Removing for refresh...", file=sys.stderr)
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config['mcp']['leindex']

# OpenCode requires: type='local', command as array (not string), enabled field
# Args should be merged into command array, not separate field
print(f"Notice: Writing fresh config for 'leindex' with OpenCode format...", file=sys.stderr)
new_config = {
    'type': 'local',
    'command': command_array,
    'enabled': True
}
config['mcp']['leindex'] = new_config


# Validate config can be serialized
try:
    json.dumps(config)
except (TypeError, ValueError) as e:
    print(f"Error: Invalid configuration structure: {e}", file=sys.stderr)
    sys.exit(1)

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')


print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "OpenCode configured"
    print_bullet "Config: $config_file"
}

# Qwen CLI - Standard JSON, corrected filename
# Source: https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/
configure_qwen_cli() {
    configure_cli_mcp "leindex" "$HOME/.qwen/settings.json" "Qwen CLI"
}

# Kilocode CLI - Corrected path (uses .config subdirectory)
# Source: https://www.reddit.com/r/kilocode/comments/1n0xqmx/
configure_kilocode_cli() {
    configure_cli_mcp "leindex" "$HOME/.config/kilocode/mcp_settings.json" "Kilocode CLI"
}

# Goose CLI - Uses YAML format (not JSON!)
# Source: https://github.com/block/goose/discussions/1286
configure_goose_cli() {
    configure_yaml_mcp "leindex" "$HOME/.config/goose/config.yaml" "Goose CLI"
}

# iFlow CLI - Corrected filename (settings.json, not mcp_config.json)
# Source: https://platform.iflow.cn/en/cli/examples/mcp
configure_iflow_cli() {
    configure_cli_mcp "leindex" "$HOME/.iflow/settings.json" "iFlow CLI"
}

# Droid CLI (Factory AI) - Requires 'type: stdio' field!
# Source: https://docs.factory.ai/cli/configuration/mcp
configure_droid_cli() {
    print_section "Configuring Droid CLI (Factory AI)"

    local config_file="$HOME/.factory/mcp.json"
    local config_dir
    config_dir=$(dirname "$config_file")

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    # Droid requires 'type: stdio' field for stdio MCP servers
    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure mcpServers exists
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Remove existing leindex entry for fresh config
if 'leindex' in config.get('mcpServers', {}):
    existing_config = config['mcpServers']['leindex']
    print(f"Notice: Server 'leindex' already configured. Removing for refresh...", file=sys.stderr)
    print(f"Debug: Old config: {existing_config}", file=sys.stderr)
    del config['mcpServers']['leindex']

# Droid requires 'type: stdio' field for stdio servers
# Source: https://docs.factory.ai/cli/configuration/mcp
print(f"Notice: Writing fresh config for 'leindex' with type='stdio'...", file=sys.stderr)
config['mcpServers']['leindex'] = {
    'type': 'stdio',
    'command': 'leindex',
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "Droid CLI (Factory AI) configured"
    print_bullet "Config: $config_file (with type: stdio field)"
}

# Gemini CLI - Corrected filename (settings.json)
# Source: https://geminicli.com/docs/tools/mcp-server/
configure_gemini_cli() {
    configure_cli_mcp "leindex" "$HOME/.gemini/settings.json" "Gemini CLI"
}

# Interactive tool selection with beautiful menu
select_tools() {
    print_step 7 $TOTAL_STEPS "Tool Integration"

    echo ""
    printf "${BOLD}${CYAN}Which tools would you like LeIndex to integrate with?${NC}\n"
    echo ""

    local options=(
        "Claude Desktop"
        "Claude Code"
        "Cursor IDE"
        "Antigravity IDE"
        "VS Code / VSCodium"
        "Zed Editor"
        "JetBrains IDEs"
        "Codex CLI"
        "Amp Code"
        "OpenCode"
        "Qwen CLI"
        "Kilocode CLI"
        "Goose CLI"
        "iFlow CLI"
        "Droid CLI"
        "Gemini CLI"
        "CLI Tools (PATH setup)"
        "All tools"
        "Detected tools only"
        "Skip integration"
        "Custom selection"
    )

    ASK_CHOICE_RESULT=""
    ask_choice "Select an option:" "${options[@]}"
    local choice=$ASK_CHOICE_RESULT

    echo ""

    case $choice in
        0) configure_claude_desktop ;;
        1) configure_claude_cli ;;
        2) configure_cursor ;;
        3) configure_antigravity ;;
        4) configure_vscode ;;
        5) configure_zed ;;
        6) configure_jetbrains ;;
        7) configure_codex_cli ;;
        8) configure_amp_code ;;
        9) configure_opencode ;;
        10) configure_qwen_cli ;;
        11) configure_kilocode_cli ;;
        12) configure_goose_cli ;;
        13) configure_iflow_cli ;;
        14) configure_droid_cli ;;
        15) configure_gemini_cli ;;
        16) configure_cli_tools ;;
        17)
            configure_claude_desktop
            configure_claude_cli
            configure_cursor
            configure_antigravity
            configure_vscode
            configure_zed
            configure_jetbrains
            configure_codex_cli
            configure_amp_code
            configure_opencode
            configure_qwen_cli
            configure_kilocode_cli
            configure_goose_cli
            configure_iflow_cli
            configure_droid_cli
            configure_gemini_cli
            configure_cli_tools
            ;;
        18)
            [[ -d "$HOME/.config/claude" ]] && configure_claude_desktop
            [[ -d "$HOME/.claude" ]] && configure_claude_cli
            [[ -d "$HOME/.cursor" ]] && configure_cursor
            [[ -d "$HOME/.config/antigravity" ]] && configure_antigravity
            ([[ -d "$HOME/.config/Code" ]] || [[ -d "$HOME/.vscode" ]]) && configure_vscode auto
            [[ -d "$HOME/.config/zed" || -d "$HOME/.var/app/dev.zed.Zed/config/zed" || -d "$HOME/.var/app/com.zed.Zed/config/zed" ]] && configure_zed
            [[ -d "$HOME/.config/JetBrains" ]] && configure_jetbrains
            command -v codex &>/dev/null && configure_codex_cli
            command -v amp &>/dev/null && configure_amp_code
            command -v opencode &>/dev/null && configure_opencode
            command -v qwen &>/dev/null && configure_qwen_cli
            command -v kilocode &>/dev/null && configure_kilocode_cli
            command -v goose &>/dev/null && configure_goose_cli
            command -v iflow &>/dev/null && configure_iflow_cli
            command -v droid &>/dev/null && configure_droid_cli
            command -v gemini &>/dev/null && configure_gemini_cli
            configure_cli_tools
            ;;
        19)
            print_warning "Skipping tool integration"
            print_info "MCP server installed and ready for manual configuration"
            ;;
        20)
            echo ""
            printf "${BOLD}Enter tools (space-separated, e.g., '1 3 4'):${NC}\n"
            read -rp "> " custom < /dev/tty
            echo ""

            for tool in $custom; do
                case "$tool" in
                    1) configure_claude_desktop ;;
                    2) configure_claude_cli ;;
                    3) configure_cursor ;;
                    4) configure_antigravity ;;
                    5) configure_vscode ;;
                    6) configure_zed ;;
                    7) configure_jetbrains ;;
                    8) configure_codex_cli ;;
                    9) configure_amp_code ;;
                    10) configure_opencode ;;
                    11) configure_qwen_cli ;;
                    12) configure_kilocode_cli ;;
                    13) configure_goose_cli ;;
                    14) configure_iflow_cli ;;
                    15) configure_droid_cli ;;
                    16) configure_gemini_cli ;;
                    17) configure_cli_tools ;;
                esac
            done
            ;;
    esac
}

# ============================================================================
# VERIFICATION
# ============================================================================

verify_installation() {
    print_step 8 $TOTAL_STEPS "Verifying Installation"

    if [[ "$PKG_MANAGER" == "uv" ]]; then
        print_success "Python package installed (via uv)"
        print_bullet "LeIndex is ready to use"
    elif $PYTHON_CMD -c "import leindex.server" 2>/dev/null; then
        VERSION=$($PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>/dev/null || echo "unknown")
        print_success "Python package installed"
        print_bullet "Version: $VERSION"
    else
        print_error "Python package not found"
        return 1
    fi

    echo ""
    printf "${BOLD}Commands:${NC}\n"
    if command -v leindex &>/dev/null; then
        print_success "leindex"
    else
        print_warning "leindex (not in PATH - use uv run leindex)"
    fi

    if command -v leindex-search &>/dev/null; then
        print_success "leindex-search"
    else
        print_warning "leindex-search (not in PATH - use uv run leindex-search)"
    fi

    echo ""
    printf "${BOLD}Configured tools:${NC}\n"
    if [[ -f "$HOME/.config/claude/claude_desktop_config.json" ]] && grep -q "leindex" "$HOME/.config/claude/claude_desktop_config.json" 2>/dev/null; then
        print_success "Claude Desktop"
    fi
    # Check primary (~/.claude/.claude.json), secondary (~/.claude.json), and fallback (~/.config/claude-code/mcp.json) locations
    if [[ -f "$HOME/.claude/.claude.json" ]] && grep -q '"leindex"' "$HOME/.claude/.claude.json" 2>/dev/null; then
        print_success "Claude Code"
    elif [[ -f "$HOME/.claude.json" ]] && grep -q '"leindex"' "$HOME/.claude.json" 2>/dev/null; then
        print_success "Claude Code"
    elif [[ -f "$HOME/.config/claude-code/mcp.json" ]] && grep -q '"leindex"' "$HOME/.config/claude-code/mcp.json" 2>/dev/null; then
        print_success "Claude Code"
    fi
    if [[ -f "$HOME/.cursor/mcp.json" ]] && grep -q "leindex" "$HOME/.cursor/mcp.json" 2>/dev/null; then
        print_success "Cursor"
    fi
    if [[ -f "$HOME/.config/Code/User/settings.json" ]] && grep -q "leindex" "$HOME/.config/Code/User/settings.json" 2>/dev/null; then
        print_success "VS Code"
    fi
    local zed_config_files=(
        "$HOME/.config/zed/settings.json"
        "$HOME/.var/app/dev.zed.Zed/config/zed/settings.json"
        "$HOME/.var/app/com.zed.Zed/config/zed/settings.json"
    )
    local zed_config_file
    for zed_config_file in "${zed_config_files[@]}"; do
        if [[ -f "$zed_config_file" ]] && grep -q "leindex" "$zed_config_file" 2>/dev/null; then
            print_success "Zed"
            break
        fi
    done

    return 0
}

# ============================================================================
# COMPLETION MESSAGE
# ============================================================================

print_completion() {
    echo ""
    printf "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}\n"
    printf "${GREEN}║${NC}${BOLD}  🎉 Installation Complete! 🎉${NC}${GREEN}                                ║${NC}\n"
    printf "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}\n"
    echo ""

    printf "${BOLD}${CYAN}What's next?${NC}\n"
    echo ""
    printf "${YELLOW}1.${NC} Restart your AI tool(s) to load LeIndex\n"
    printf "${YELLOW}2.${NC} Use MCP tools in your AI assistant:\n"
    printf "    ${GREEN}•${NC} ${CYAN}manage_project${NC} - Index code repositories\n"
    printf "    ${GREEN}•${NC} ${CYAN}search_content${NC} - Search code semantically\n"
    printf "    ${GREEN}•${NC} ${CYAN}get_diagnostics${NC} - Get project statistics\n"
    echo ""
    printf "${BOLD}${MAGENTA}New in v2.0:${NC}\n"
    printf "    ${GREEN}•${NC} ${CYAN}get_global_stats${NC} - Aggregate statistics across projects\n"
    printf "    ${GREEN}•${NC} ${CYAN}get_dashboard${NC} - Compare multiple projects side-by-side\n"
    printf "    ${GREEN}•${NC} ${CYAN}cross_project_search${NC} - Search across all repositories\n"
    printf "    ${GREEN}•${NC} ${CYAN}get_memory_status${NC} - Monitor memory usage\n"
    printf "    ${GREEN}•${NC} ${CYAN}trigger_eviction${NC} - Free memory when needed\n"
    printf "    ${GREEN}•${NC} ${CYAN}configure_memory${NC} - Adjust memory limits\n"
    echo ""
    printf "${YELLOW}3.${NC} Or use CLI commands:\n"
    printf "    ${GREEN}•${NC} ${CYAN}leindex mcp${NC} - Start MCP server\n"
    printf "    ${GREEN}•${NC} ${CYAN}leindex-search \"query\"${NC} - Search from terminal\n"
    echo ""

    # Show venv information if applicable
    if [[ -n "${LEINDEX_VENV:-}" ]] && [[ -d "$LEINDEX_VENV" ]]; then
        printf "${BOLD}${CYAN}Python Environment:${NC}\n"
        print_bullet "Using venv: ${CYAN}$LEINDEX_VENV${NC}"
        print_bullet "Python: ${CYAN}$PYTHON_CMD${NC}"
        echo ""
    fi

    printf "${BOLD}${CYAN}Resources:${NC}\n"
    print_bullet "GitHub: $REPO_URL"
    print_bullet "Documentation: See README.md and docs/"
    echo ""

    printf "${BOLD}${YELLOW}Configuration:${NC}\n"
    print_bullet "Config file: ${CYAN}$HOME/.leindex/mcp_config.yaml${NC}"
    print_bullet "Memory settings: Edit config file or use configure_memory tool"
    print_bullet "Migration: See docs/MIGRATION.md for v1 → v2 guide"
    echo ""

    printf "${BOLD}${YELLOW}Installation Log:${NC}\n"
    print_bullet "Log file: ${CYAN}$INSTALL_LOG${NC}"
    print_bullet "To see all debug info, run: ${CYAN}DEBUG=1 ./install.sh${NC}"
    echo ""

    printf "${BOLD}${YELLOW}Troubleshooting:${NC}\n"
    print_bullet "Check logs: ${CYAN}$LOG_DIR/${NC}"
    print_bullet "Test MCP: ${CYAN}$PYTHON_CMD -m leindex.server${NC}"
    print_bullet "Debug mode: ${CYAN}export LEINDEX_LOG_LEVEL=DEBUG${NC}"
    echo ""

    printf "${DIM}${CYAN}Thanks for installing LeIndex v2.0! Happy coding! 🚀${NC}\n"
    echo ""
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

main() {
    print_header
    print_welcome

    init_rollback

    detect_os > /dev/null
    detect_python
    detect_package_manager
    detect_ai_tools
    echo ""

    install_leindex
    setup_directories

    print_step 6 $TOTAL_STEPS "Setting up v2.0 Configuration"

    # v2.0: Detect and migrate v1 config
    detect_and_migrate_config

    # v2.0: Run first-time setup
    run_first_time_setup

    select_tools

    verify_installation
    print_completion

    trap - EXIT
    rollback 0
}

main "$@"
