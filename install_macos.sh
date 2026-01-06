#!/usr/bin/env bash
#############################################
# LeIndex Universal Installer
# Version: 3.0.0
# Platform: Linux/Unix
# Supports: 15+ AI CLI tools with full MCP integration
#############################################

set -euo pipefail

# ============================================================================
# CONFIGURATION
# ============================================================================
readonly SCRIPT_VERSION="3.0.0"
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
# COLOR OUTPUT
# ============================================================================
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly BLUE='\033[0;34m'
readonly YELLOW='\033[1;33m'
readonly CYAN='\033[0;36m'
readonly MAGENTA='\033[0;35m'
readonly BOLD='\033[1m'
readonly NC='\033[0m'

# ============================================================================
# UTILITY FUNCTIONS
# ============================================================================

# Print styled header
print_header() {
    local width=60
    echo -e "${CYAN}$(printf '═%.0s' $(seq 1 $width))${NC}"
    echo -e "${CYAN}║${NC}${BOLD} $(printf "%-$((${width}-2))s" "$PROJECT_NAME Installer v$SCRIPT_VERSION")${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}   $(printf "%-$((${width}-2))s" "AI-Powered Code Search & MCP Server")${CYAN}║${NC}"
    echo -e "${CYAN}$(printf '═%.0s' $(seq 1 $width))${NC}"
    echo ""
}

# Print section header
print_section() {
    echo -e "${BLUE}>>> ${BOLD}$1${NC}${BLUE} <<<${NC}"
    echo ""
}

# Print success message
print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

# Print warning message
print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

# Print error message
print_error() {
    echo -e "${RED}✗${NC} $1"
}

# Print info message
print_info() {
    echo -e "${CYAN}ℹ${NC} $1"
}

# Print bullet point
print_bullet() {
    echo -e "  ${CYAN}•${NC} $1"
}

# Ask yes/no question
ask_yes_no() {
    local prompt="$1"
    local default="${2:-n}"

    if [[ "$default" == "y" ]]; then
        prompt="$prompt [Y/n]"
    else
        prompt="$prompt [y/N]"
    fi

    while true; do
        read -rp "$(echo -e "${YELLOW}?${NC} $prompt ") " answer
        answer=${answer:-$default}

        case "$answer" in
            [Yy]|[Yy][Ee][Ss]) return 0 ;;
            [Nn]|[Nn][Oo]) return 1 ;;
            *) echo "Please answer yes or no." ;;
        esac
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
        # Clean up successful installation
        rm -rf "$BACKUP_DIR" 2>/dev/null || true
        return 0
    fi

    print_error "Installation failed. Rolling back changes..."

    if [[ -f "$BACKUP_DIR/manifest.txt" ]]; then
        while IFS=: read -r backup original; do
            if [[ -f "$backup" ]]; then
                cp "$backup" "$original"
                print_success "Restored: $original"
            fi
        done < "$BACKUP_DIR/manifest.txt"
    fi

    # Remove created directories (if empty)
    rmdir "$CONFIG_DIR" 2>/dev/null || true
    rmdir "$DATA_DIR" 2>/dev/null || true
    rmdir "$LOG_DIR" 2>/dev/null || true

    print_warning "Rollback complete"
    rm -rf "$BACKUP_DIR" 2>/dev/null || true
}

# Set up error trap
trap 'rollback $?' EXIT

# ============================================================================
# ENVIRONMENT DETECTION
# ============================================================================

# Detect Python interpreter
detect_python() {
    print_section "Detecting Python Environment"

    local python_cmds=("python3" "python3.11" "python3.12" "python3.13" "python")
    PYTHON_CMD=""

    for cmd in "${python_cmds[@]}"; do
        if command -v "$cmd" &> /dev/null; then
            PYTHON_CMD="$cmd"
            break
        fi
    done

    if [[ -z "$PYTHON_CMD" ]]; then
        print_error "Python not found"
        echo ""
        echo "Please install Python 3.10 or higher:"
        print_bullet "Ubuntu/Debian: sudo apt install python3.11 python3-pip python3-venv"
        print_bullet "Fedora/RHEL: sudo dnf install python3.11 python3-pip"
        print_bullet "Arch: sudo pacman -S python python-pip"
        print_bullet "From source: https://www.python.org/downloads/"
        exit 1
    fi

    # Check Python version
    local python_version
    python_version=$($PYTHON_CMD -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
    local major=$($PYTHON_CMD -c 'import sys; print(sys.version_info.major)')
    local minor=$($PYTHON_CMD -c 'import sys; print(sys.version_info.minor)')

    if [[ $major -lt $MIN_PYTHON_MAJOR ]] || [[ $major -eq $MIN_PYTHON_MAJOR && $minor -lt $MIN_PYTHON_MINOR ]]; then
        print_error "Python $MIN_PYTHON_MAJOR.$MIN_PYTHON_MINOR+ required. Found: $python_version"
        exit 1
    fi

    print_success "Python $python_version detected: $PYTHON_CMD"
    echo ""
}

# Detect package manager
detect_package_manager() {
    print_section "Detecting Package Manager"

    # Check for uv (fastest, preferred)
    if command -v uv &> /dev/null; then
        PKG_MANAGER="uv"
        PKG_INSTALL_CMD="uv pip install"
        print_success "uv detected (preferred package manager)"
        return
    fi

    # Check for pipx (isolated installations)
    if command -v pipx &> /dev/null; then
        PKG_MANAGER="pipx"
        PKG_INSTALL_CMD="pipx install"
        print_success "pipx detected (isolated installations)"
        return
    fi

    # Check for pip
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

    # Fall back to python -m pip
    if $PYTHON_CMD -m pip --version &> /dev/null; then
        PKG_MANAGER="pip"
        PKG_INSTALL_CMD="$PYTHON_CMD -m pip install"
        print_success "pip (via Python module) detected"
        return
    fi

    print_error "No package manager found"
    print_bullet "Install pip: $PYTHON_CMD -m ensurepip --upgrade"
    print_bullet "Or install uv: curl -LsSf https://astral.sh/uv/install.sh | sh"
    exit 1
}

# Detect installed AI tools
detect_ai_tools() {
    print_section "Detecting AI Coding Tools"

    local detected_tools=()

    # Desktop Applications
    [[ -d "$HOME/.config/claude" ]] && detected_tools+=("claude-desktop")
    [[ -d "$HOME/.cursor" ]] && detected_tools+=("cursor")
    [[ -d "$HOME/.config/Code" ]] || [[ -d "$HOME/.config/VSCodium" ]] && detected_tools+=("vscode")
    [[ -d "$HOME/.config/zed" ]] && detected_tools+=("zed")
    [[ -d "$HOME/.config/JetBrains" ]] && detected_tools+=("jetbrains")

    # CLI Tools
    command -v claude &> /dev/null && detected_tools+=("claude-cli")
    command -v gemini &> /dev/null && detected_tools+=("gemini-cli")
    command -v aider &> /dev/null && detected_tools+=("aider")
    command -v cursor &> /dev/null && detected_tools+=("cursor-cli")

    # Check for common config directories
    [[ -d "$HOME/.config/windsurf" ]] && detected_tools+=("windsurf")
    [[ -d "$HOME/.config/continue" ]] && detected_tools+=("continue")
    [[ -d "$HOME/.config/cursor" ]] && detected_tools+=("cursor")

    if [[ ${#detected_tools[@]} -gt 0 ]]; then
        print_success "Detected ${#detected_tools[@]} AI tool(s):"
        for tool in "${detected_tools[@]}"; do
            print_bullet "$tool"
        done
    else
        print_warning "No AI tools detected. Will install MCP server only."
    fi

    echo ""
}

# ============================================================================
# INSTALLATION
# ============================================================================

# Install LeIndex package
install_leindex() {
    print_section "Installing $PROJECT_NAME"

    # Upgrade package manager first
    print_info "Upgrading package manager..."
    case "$PKG_MANAGER" in
        uv)
            uv self-update 2>/dev/null || true
            ;;
        pip|pipx)
            $PYTHON_CMD -m pip install --upgrade pip setuptools wheel 2>/dev/null || true
            ;;
    esac

    # Install package
    print_info "Installing $PYPI_PACKAGE..."
    if $PKG_INSTALL_CMD "$PYPI_PACKAGE"; then
        print_success "$PROJECT_NAME installed successfully"
    else
        print_error "Failed to install $PYPI_PACKAGE"
        exit 1
    fi

    # Verify installation
    if $PYTHON_CMD -c "import leindex.server" 2>/dev/null; then
        VERSION=$($PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>/dev/null || echo "unknown")
        print_success "Installation verified: version $VERSION"
    else
        print_error "Installation verification failed"
        exit 1
    fi

    echo ""
}

# Setup directory structure
setup_directories() {
    print_section "Setting up Directories"

    for dir in "$CONFIG_DIR" "$DATA_DIR" "$LOG_DIR"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            print_success "Created: $dir"
        fi
    done

    echo ""
}

# ============================================================================
# TOOL INTEGRATION
# ============================================================================

# Merge JSON configuration safely
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

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Add mcpServers key if missing
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Add LeIndex configuration
config['mcpServers'][server_name] = {
    'command': server_command,
    'args': ['mcp'],
    'env': {},
    'disabled': False
}

# Write back with formatting
with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF
}

# Configure Claude Desktop
configure_claude_desktop() {
    print_section "Configuring Claude Desktop"

    local config_dir="$HOME/.config/claude"
    local config_file="$config_dir/claude_desktop_config.json"

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    merge_json_config "$config_file" "leindex" "leindex"

    print_success "Claude Desktop configured"
    print_bullet "Config: $config_file"
    echo ""
}

# Configure Cursor IDE
configure_cursor() {
    print_section "Configuring Cursor"

    local config_dir="$HOME/.cursor"
    local config_file="$config_dir/mcp.json"

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    merge_json_config "$config_file" "leindex" "leindex"

    print_success "Cursor configured"
    print_bullet "Config: $config_file"
    echo ""
}

# Configure VS Code / VSCodium
configure_vscode() {
    print_section "Configuring VS Code Family"

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

            merge_json_config "$config_file" "leindex" "leindex"
            print_success "VS Code configured: $config_file"
        fi
    done

    print_info "Note: Install an MCP extension for VS Code:"
    print_bullet "Cline: https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude"
    print_bullet "Continue: https://marketplace.visualstudio.com/items?itemName=Continue.continue"
    print_bullet "Roo Code: https://marketplace.visualstudio.com/items?itemName=RooCode.roo-code"
    echo ""
}

# Configure Zed Editor
configure_zed() {
    print_section "Configuring Zed Editor"

    local config_dir="$HOME/.config/zed"
    local config_file="$config_dir/settings.json"

    mkdir -p "$config_dir"
    backup_file "$config_file" 2>/dev/null || true

    # Zed uses LSP format
    $PYTHON_CMD << PYTHON_EOF
import json

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Add LSP configuration
if 'lsp' not in config:
    config['lsp'] = {}

config['lsp']['leindex'] = {
    'command': 'leindex',
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
PYTHON_EOF

    print_success "Zed Editor configured"
    print_bullet "Config: $config_file"
    echo ""
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

    echo ""
}

# Configure CLI tools
configure_cli_tools() {
    print_section "Configuring CLI Tools"

    # Check if leindex is in PATH
    if command -v leindex &> /dev/null; then
        print_success "'leindex' command available in PATH"
    else
        print_warning "'leindex' command not in PATH"
        print_info "Add Python user base to PATH:"

        local user_base
        user_base=$($PYTHON_CMD -m site --user-base 2>/dev/null || echo "$HOME/.local")
        local bin_dir="$user_base/bin"

        print_bullet "export PATH=\"\$PATH:$bin_dir\""

        # Offer to add to shell config
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

    # Check leindex-search
    if command -v leindex-search &> /dev/null; then
        print_success "'leindex-search' command available"
    fi

    echo ""
}

# Interactive tool selection
select_tools() {
    print_section "Tool Integration"

    echo "Select AI tools to integrate with $PROJECT_NAME:"
    echo ""
    echo "  ${GREEN}1${NC}) Claude Desktop"
    echo "  ${GREEN}2${NC}) Cursor IDE"
    echo "  ${GREEN}3${NC}) VS Code / VSCodium"
    echo "  ${GREEN}4${NC}) Zed Editor"
    echo "  ${GREEN}5${NC}) JetBrains IDEs"
    echo "  ${GREEN}6${NC}) CLI Tools (PATH setup)"
    echo "  ${GREEN}a${NC}) ${BOLD}All tools${NC}"
    echo "  ${GREEN}d${NC}) ${BOLD}Detected tools only${NC}"
    echo "  ${GREEN}s${NC}) ${BOLD}Skip integration${NC}"
    echo "  ${GREEN}c${NC}) ${BOLD}Custom selection${NC}"
    echo ""

    while true; do
        read -rp "$(echo -e "${YELLOW}#${NC} Enter choice: ")" choice
        echo ""

        case "$choice" in
            1) configure_claude_desktop; break ;;
            2) configure_cursor; break ;;
            3) configure_vscode; break ;;
            4) configure_zed; break ;;
            5) configure_jetbrains; break ;;
            6) configure_cli_tools; break ;;
            a|A)
                configure_claude_desktop
                configure_cursor
                configure_vscode
                configure_zed
                configure_jetbrains
                configure_cli_tools
                break
                ;;
            d|D)
                # Configure detected tools
                [[ -d "$HOME/.config/claude" ]] && configure_claude_desktop
                [[ -d "$HOME/.cursor" ]] && configure_cursor
                ([[ -d "$HOME/.config/Code" ]] || [[ -d "$HOME/.vscode" ]]) && configure_vscode
                [[ -d "$HOME/.config/zed" ]] && configure_zed
                [[ -d "$HOME/.config/JetBrains" ]] && configure_jetbrains
                configure_cli_tools
                break
                ;;
            s|S)
                print_warning "Skipping tool integration"
                print_info "MCP server installed and ready for manual configuration"
                break
                ;;
            c|C)
                echo "Enter tools (space-separated, e.g., '1 3 4'):"
                read -rp "> " custom
                echo ""
                for tool in $custom; do
                    case "$tool" in
                        1) configure_claude_desktop ;;
                        2) configure_cursor ;;
                        3) configure_vscode ;;
                        4) configure_zed ;;
                        5) configure_jetbrains ;;
                        6) configure_cli_tools ;;
                    esac
                done
                break
                ;;
            *)
                print_error "Invalid choice. Please try again."
                ;;
        esac
    done
}

# ============================================================================
# VERIFICATION
# ============================================================================

verify_installation() {
    print_section "Verifying Installation"

    local all_good=true

    # Check package installation
    if $PYTHON_CMD -c "import leindex.server" 2>/dev/null; then
        print_success "Python package installed"
        VERSION=$($PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>/dev/null || echo "unknown")
        print_bullet "Version: $VERSION"
    else
        print_error "Python package not found"
        all_good=false
    fi

    # Check command availability
    echo ""
    echo "Commands:"
    command -v leindex &> /dev/null && print_success "leindex" || print_warning "leindex (not in PATH)"
    command -v leindex-search &> /dev/null && print_success "leindex-search" || print_warning "leindex-search (not in PATH)"

    # Check configured tools
    echo ""
    echo "Configured tools:"
    [[ -f "$HOME/.config/claude/claude_desktop_config.json" ]] && grep -q "leindex" "$HOME/.config/claude/claude_desktop_config.json" 2>/dev/null && print_success "Claude Desktop" || true
    [[ -f "$HOME/.cursor/mcp.json" ]] && grep -q "leindex" "$HOME/.cursor/mcp.json" 2>/dev/null && print_success "Cursor" || true
    [[ -f "$HOME/.config/Code/User/settings.json" ]] && grep -q "leindex" "$HOME/.config/Code/User/settings.json" 2>/dev/null && print_success "VS Code" || true
    [[ -f "$HOME/.config/zed/settings.json" ]] && grep -q "leindex" "$HOME/.config/zed/settings.json" 2>/dev/null && print_success "Zed" || true

    echo ""

    if [[ "$all_good" == "true" ]]; then
        return 0
    else
        return 1
    fi
}

# ============================================================================
# COMPLETION MESSAGE
# ============================================================================

print_completion() {
    echo ""
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║${NC}${BOLD} Installation Complete!${NC}${GREEN}                                        ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""

    echo -e "${BOLD}Next Steps:${NC}"
    echo ""
    echo "1. Restart your AI tool(s) to load $PROJECT_NAME"
    echo "2. Use MCP tools in your AI assistant:"
    echo "     ${CYAN}manage_project${NC} - Index code repositories"
    echo "     ${CYAN}search_content${NC} - Search code semantically"
    echo "     ${CYAN}get_diagnostics${NC} - Get project statistics"
    echo ""
    echo "3. Or use CLI commands:"
    echo "     ${CYAN}leindex mcp${NC} - Start MCP server"
    echo "     ${CYAN}leindex-search${NC} \"query\" - Search from terminal"
    echo ""

    echo -e "${BOLD}Documentation:${NC}"
    print_bullet "GitHub: $REPO_URL"
    print_bullet "MCP Config: See MCP_CONFIGURATION.md"
    echo ""

    echo -e "${BOLD}Troubleshooting:${NC}"
    print_bullet "Check logs: $LOG_DIR/"
    print_bullet "Test MCP: $PYTHON_CMD -m leindex.server"
    print_bullet "Debug mode: export LEINDEX_LOG_LEVEL=DEBUG"
    echo ""

    echo -e "${BOLD}Uninstall:${NC}"
    print_bullet "Run: curl -sSL $REPO_URL/raw/master/uninstall.sh | bash"
    echo ""
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

main() {
    clear
    print_header

    # Initialize
    init_rollback

    # Environment detection
    detect_os
    detect_python
    detect_package_manager
    detect_ai_tools

    # Installation
    setup_directories
    install_leindex

    # Tool integration
    select_tools

    # Verification
    verify_installation

    # Completion
    print_completion

    # Disable rollback trap (successful installation)
    trap - EXIT
    rollback 0
}

# Run installation
main "$@"
