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
readonly SCRIPT_VERSION="4.0.0"
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

    printf "${CYAN}║${NC}${BOLD}  🚀 %s %s${NC}" "$PROJECT_NAME" "Installer v$SCRIPT_VERSION"
    local remaining=$(($width - ${#PROJECT_NAME} - ${#SCRIPT_VERSION} - 14))
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
    printf "${DIM}Let's get you set up with LeIndex in just a few moments.${NC}\n"
    printf "${DIM}This installer will:${NC}\n"
    echo ""
    printf "  ${GREEN}✓${NC} Detect your Python environment\n"
    printf "  ${GREEN}✓${NC} Find your AI coding tools\n"
    printf "  ${GREEN}✓${NC} Install LeIndex with the best package manager\n"
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
        read -r answer
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
        read -r choice
        echo ""

        if [[ "$choice" =~ ^[0-9]+$ ]] && [ "$choice" -ge 1 ] && [ "$choice" -le "${#options[@]}" ]; then
            return $((choice - 1))
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
    fi
}

trap 'handle_error' EXIT

# ============================================================================
# ENVIRONMENT DETECTION
# ============================================================================

# Total steps for progress tracking
TOTAL_STEPS=7

# Detect Python interpreter
detect_python() {
    print_step 1 $TOTAL_STEPS "Detecting Python Environment"

    local python_cmds=("python3.13" "python3.12" "python3.11" "python3.10" "python3" "python")
    PYTHON_CMD=""

    for cmd in "${python_cmds[@]}"; do
        if command -v "$cmd" &> /dev/null; then
            PYTHON_CMD="$cmd"
            break
        fi
    done

    if [[ -z "$PYTHON_CMD" ]]; then
        print_error "Python not found on your system"
        echo ""
        printf "${BOLD}Please install Python 3.10-3.13:${NC}\n"
        print_bullet "Ubuntu/Debian: ${CYAN}sudo apt install python3.13 python3-pip python3-venv${NC}"
        print_bullet "Fedora/RHEL: ${CYAN}sudo dnf install python3.13 python3-pip${NC}"
        print_bullet "Arch: ${CYAN}sudo pacman -S python python-pip${NC}"
        print_bullet "From source: ${CYAN}https://www.python.org/downloads/${NC}"
        exit 1
    fi

    local python_version
    python_version=$($PYTHON_CMD -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
    local major=$($PYTHON_CMD -c 'import sys; print(sys.version_info.major)')
    local minor=$($PYTHON_CMD -c 'import sys; print(sys.version_info.minor)')

    if [[ $major -eq 3 && $minor -ge 14 ]]; then
        print_error "Python 3.14+ not supported (leann-backend-hnsw compatibility)"
        print_bullet "Found: $python_version"
        print_bullet "Please use Python 3.10-3.13"
        exit 1
    fi

    if [[ $major -lt $MIN_PYTHON_MAJOR ]] || [[ $major -eq $MIN_PYTHON_MAJOR && $minor -lt $MIN_PYTHON_MINOR ]]; then
        print_error "Python $MIN_PYTHON_MAJOR.$MIN_PYTHON_MINOR+ required. Found: $python_version"
        exit 1
    fi

    print_success "Python $python_version detected"
    print_bullet "Using: ${CYAN}$PYTHON_CMD${NC}"
}

# Detect package manager
detect_package_manager() {
    print_step 2 $TOTAL_STEPS "Detecting Package Manager"

    if command -v uv &> /dev/null; then
        PKG_MANAGER="uv"
        PKG_INSTALL_CMD="uv pip install"
        print_success "uv detected (⚡ fastest package manager)"
        return
    fi

    if command -v pipx &> /dev/null; then
        PKG_MANAGER="pipx"
        PKG_INSTALL_CMD="pipx install"
        print_success "pipx detected (isolated installations)"
        return
    fi

    if command -v pip3 &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="pip3 install"
        print_success "pip3 detected"
        return
    fi

    if command -v pip &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="pip install"
        print_success "pip detected"
        return
    fi

    if $PYTHON_CMD -m pip --version &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="$PYTHON_CMD -m pip install"
        print_success "pip (via Python module) detected"
        return
    fi

    print_error "No package manager found"
    echo ""
    printf "${BOLD}Install a package manager:${NC}\n"
    print_bullet "Install pip: ${CYAN}$PYTHON_CMD -m ensurepip --upgrade${NC}"
    print_bullet "Or install uv: ${CYAN}curl -LsSf https://astral.sh/uv/install.sh | sh${NC}"
    exit 1
}

# Get display name for tool
get_tool_display_name() {
    case "$1" in
        # CLI Tools
        "claude-cli") echo "Claude CLI" ;;
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

    # Zed Editor
    [[ -d "$HOME/.config/zed" ]] && detected_editors+=("zed")

    # JetBrains IDEs
    [[ -d "$HOME/.config/JetBrains" ]] && detected_editors+=("jetbrains")

    # Windsurf
    [[ -d "$HOME/.config/windsurf" ]] && detected_editors+=("windsurf")

    # Continue
    [[ -d "$HOME/.config/continue" ]] && detected_editors+=("continue")

    # ============================================================================
    # CLI TOOLS - Check executables (with alternative names)
    # ============================================================================

    # Claude CLI
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

    # Force reinstall to ensure new version is used (fixes old elasticsearch import issue)
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

# ============================================================================
# TOOL INTEGRATION
# ============================================================================

# Merge JSON configuration for Claude Desktop/Cursor (no disabled/env fields)
merge_json_config() {
    local config_file="$1"
    local server_name="$2"
    local server_command="${3:-leindex}"

    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"
server_name = "$server_name"
server_command = "$server_command"

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                print(f"Backing up invalid config and creating new one.", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Check if server already exists
if server_name in config.get('mcpServers', {}):
    existing_config = config['mcpServers'][server_name]
    print(f"Notice: Server '{server_name}' already configured.", file=sys.stderr)
    print(f"Existing config: {existing_config}", file=sys.stderr)

# Claude Desktop/Cursor do NOT support 'disabled' or 'env' fields
config['mcpServers'][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

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
}

# Configure Claude Desktop
configure_claude_desktop() {
    print_section "Configuring Claude Desktop"

    # Search for Claude Desktop config (NOT Claude Code CLI!)
    # Claude Desktop uses claude_desktop_config.json
    local claude_configs=(
        "$HOME/.config/claude/claude_desktop_config.json"
        "$HOME/.config/Claude/claude_desktop_config.json"
    )

    local config_file=""
    local config_dir=""

    for conf in "${claude_configs[@]}"; do
        if [[ -f "$conf" ]]; then
            config_file="$conf"
            config_dir=$(dirname "$conf")
            print_bullet "Found config at: $config_file"
            break
        fi
    done

    # If no existing config found, create in default location
    if [[ -z "$config_file" ]]; then
        config_dir="$HOME/.config/claude"
        config_file="$config_dir/claude_desktop_config.json"
        print_info "No existing config found. Will create: $config_file"
    fi

    mkdir -p "$config_dir" || { print_warning "Failed to create config directory"; return 2; }
    backup_file "$config_file" 2>/dev/null || true

    if merge_json_config "$config_file" "leindex" "leindex"; then
        print_success "Claude Desktop configured"
        print_bullet "Config: $config_file"
    else
        print_warning "Failed to configure Claude Desktop"
        return 2
    fi
}

# Configure Claude Code CLI (different from Claude Desktop!)
configure_claude_cli() {
    print_section "Configuring Claude Code CLI"

    # Claude Code CLI uses ~/.claude.json
    local config_file="$HOME/.claude.json"
    local config_dir="$HOME"

    if [[ -f "$config_file" ]]; then
        print_bullet "Found config at: $config_file"
    else
        print_info "No existing config found. Will create: $config_file"
    fi

    mkdir -p "$config_dir" || { print_warning "Failed to create config directory"; return 2; }
    backup_file "$config_file" 2>/dev/null || true

    # Claude Code CLI uses a different format - projects-based
    if $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"
server_name = "leindex"
server_command = "leindex"

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

# Claude Code CLI format: mcpServers under each project or global
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Check if server already exists
if server_name in config.get('mcpServers', {}):
    existing_config = config['mcpServers'][server_name]
    print(f"Notice: Server '{server_name}' already configured.", file=sys.stderr)

config['mcpServers'][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF
    then
        print_success "Claude Code CLI configured"
        print_bullet "Config: $config_file"
    else
        print_warning "Failed to configure Claude Code CLI"
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

    if merge_json_config "$config_file" "leindex" "leindex"; then
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

    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"
server_name = "$server_name"
server_command = "$server_command"
extension_key = "$extension_key"

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                print(f"Backing up invalid config and creating new one.", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure extension-specific key exists
if extension_key not in config:
    config[extension_key] = {}

# Check if server already exists
if extension_key in config and server_name in config[extension_key]:
    existing_config = config[extension_key][server_name]
    print(f"Notice: Server '{server_name}' already configured in {extension_key}.", file=sys.stderr)
    print(f"Existing config: {existing_config}", file=sys.stderr)

# Add server to extension-specific config
config[extension_key][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

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

        ask_choice "Select your MCP extension:" "${options[@]}"
        local choice=$?
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

            merge_vscode_config "$config_file" "leindex" "leindex" "$extension_key"
            print_success "VS Code configured ($extension_name): $config_file"
        fi
    done

    print_info "Note: Make sure you have the $extension_name extension installed"
}

# Configure Zed Editor
configure_zed() {
    print_section "Configuring Zed Editor"

    local config_dir="$HOME/.config/zed"
    local config_file="$config_dir/settings.json"

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    $PYTHON_CMD << PYTHON_EOF
import json
import sys

config_file = "$config_file"

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
            except json.JSONDecodeError as e:
                print(f"Warning: Existing config is invalid JSON: {e}", file=sys.stderr)
                print(f"Backing up invalid config and creating new one.", file=sys.stderr)
                config = {}
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure language_models exists
if 'language_models' not in config:
    config['language_models'] = {}

# Ensure mcp_servers exists
if 'mcp_servers' not in config['language_models']:
    config['language_models']['mcp_servers'] = {}

# Check if server already exists
if 'language_models' in config and 'mcp_servers' in config['language_models']:
    if 'leindex' in config['language_models']['mcp_servers']:
        existing_config = config['language_models']['mcp_servers']['leindex']
        print(f"Notice: Server 'leindex' already configured in Zed MCP servers.", file=sys.stderr)
        print(f"Existing config: {existing_config}", file=sys.stderr)

# Add MCP server configuration
config['language_models']['mcp_servers']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

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

    print_success "Zed Editor configured"
    print_bullet "Config: $config_file"
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

# Most CLI tools use mcpServers key
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Add LeIndex server
config['mcpServers']['leindex'] = {
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

config['mcpServers']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "Antigravity configured"
    print_bullet "Config: $config_file"
}

# Configure specific CLI tools
configure_codex_cli() {
    configure_cli_mcp "codex" "$HOME/.codex/config.json" "Codex CLI"
}

configure_amp_code() {
    configure_cli_mcp "amp" "$HOME/.amp/mcp_config.json" "Amp Code"
}

configure_opencode() {
    configure_cli_mcp "opencode" "$HOME/.opencode/mcp_config.json" "OpenCode"
}

configure_qwen_cli() {
    configure_cli_mcp "qwen" "$HOME/.qwen/mcp_config.json" "Qwen CLI"
}

configure_kilocode_cli() {
    configure_cli_mcp "kilocode" "$HOME/.kilocode/mcp_settings.json" "Kilocode CLI"
}

configure_goose_cli() {
    # Goose uses YAML for main config, but may support JSON MCP config
    configure_cli_mcp "goose" "$HOME/.config/goose/mcp_config.json" "Goose CLI"
}

configure_iflow_cli() {
    configure_cli_mcp "iflow" "$HOME/.iflow/mcp_config.json" "iFlow CLI"
}

configure_droid_cli() {
    configure_cli_mcp "droid" "$HOME/.droid/mcp_config.json" "Droid CLI"
}

configure_gemini_cli() {
    configure_cli_mcp "gemini" "$HOME/.gemini/mcp_config.json" "Gemini CLI"
}

# Interactive tool selection with beautiful menu
select_tools() {
    print_step 6 $TOTAL_STEPS "Tool Integration"

    echo ""
    printf "${BOLD}${CYAN}Which tools would you like LeIndex to integrate with?${NC}\n"
    echo ""

    local options=(
        "Claude Desktop"
        "Claude Code CLI"
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

    ask_choice "Select an option:" "${options[@]}"
    local choice=$?

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
            [[ -f "$HOME/.claude.json" ]] && configure_claude_cli
            [[ -d "$HOME/.cursor" ]] && configure_cursor
            [[ -d "$HOME/.config/antigravity" ]] && configure_antigravity
            ([[ -d "$HOME/.config/Code" ]] || [[ -d "$HOME/.vscode" ]]) && configure_vscode auto
            [[ -d "$HOME/.config/zed" ]] && configure_zed
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
            read -rp "> " custom
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
    print_step 7 $TOTAL_STEPS "Verifying Installation"

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
    if [[ -f "$HOME/.cursor/mcp.json" ]] && grep -q "leindex" "$HOME/.cursor/mcp.json" 2>/dev/null; then
        print_success "Cursor"
    fi
    if [[ -f "$HOME/.config/Code/User/settings.json" ]] && grep -q "leindex" "$HOME/.config/Code/User/settings.json" 2>/dev/null; then
        print_success "VS Code"
    fi
    if [[ -f "$HOME/.config/zed/settings.json" ]] && grep -q "leindex" "$HOME/.config/zed/settings.json" 2>/dev/null; then
        print_success "Zed"
    fi

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
    printf "${YELLOW}3.${NC} Or use CLI commands:\n"
    printf "    ${GREEN}•${NC} ${CYAN}leindex mcp${NC} - Start MCP server\n"
    printf "    ${GREEN}•${NC} ${CYAN}leindex-search \"query\"${NC} - Search from terminal\n"
    echo ""

    printf "${BOLD}${CYAN}Resources:${NC}\n"
    print_bullet "GitHub: $REPO_URL"
    print_bullet "Documentation: See README.md"
    echo ""

    printf "${BOLD}${YELLOW}Troubleshooting:${NC}\n"
    print_bullet "Check logs: ${CYAN}$LOG_DIR/${NC}"
    print_bullet "Test MCP: ${CYAN}$PYTHON_CMD -m leindex.server${NC}"
    print_bullet "Debug mode: ${CYAN}export LEINDEX_LOG_LEVEL=DEBUG${NC}"
    echo ""

    printf "${DIM}${CYAN}Thanks for installing LeIndex! Happy coding! 🚀${NC}\n"
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

    select_tools

    verify_installation
    print_completion

    trap - EXIT
    rollback 0
}

main "$@"
