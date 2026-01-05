#!/bin/bash
set -e

#############################################
# LeIndex Installer for Linux/Unix
# Version: 2.0.0
# Supports: Claude Code, Cursor, VS Code, Zed, CLI tools
#############################################

# Color definitions
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Print header
print_header() {
    echo -e "${CYAN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC} ${GREEN}LeIndex Installer v2.0.0${NC} ${CYAN}                              ║${NC}"
    echo -e "${CYAN}║${NC} ${BLUE}AI-Powered Code Search & MCP Server${NC} ${CYAN}                   ║${NC}"
    echo -e "${CYAN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

# Print section header
print_section() {
    echo -e "${BLUE}>>> $1${NC}"
}

# Print success
print_success() {
    echo -e "${GREEN}✓ $1${NC}"
}

# Print warning
print_warning() {
    echo -e "${YELLOW}⚠ $1${NC}"
}

# Print error
print_error() {
    echo -e "${RED}✗ $1${NC}"
}

# Check Python version (requires 3.10+)
check_python() {
    print_section "Checking Python version"

    # Try python3 first, then python
    PYTHON_CMD=""
    if command -v python3 &> /dev/null; then
        PYTHON_CMD="python3"
    elif command -v python &> /dev/null; then
        PYTHON_CMD="python"
    else
        print_error "Python not found. Please install Python 3.10+ first."
        echo "  Ubuntu/Debian: sudo apt install python3.11 python3-pip"
        echo "  Fedora/RHEL: sudo dnf install python3.11 python3-pip"
        echo "  Arch: sudo pacman -S python python-pip"
        exit 1
    fi

    # Get version
    PYTHON_VERSION=$($PYTHON_CMD -c 'import sys; print(".".join(map(str, sys.version_info[:2])))')
    PYTHON_MAJOR=$($PYTHON_CMD -c 'import sys; print(sys.version_info.major)')
    PYTHON_MINOR=$($PYTHON_CMD -c 'import sys; print(sys.version_info.minor)')

    # Validate version
    if [ "$PYTHON_MAJOR" -lt 3 ] || ([ "$PYTHON_MAJOR" -eq 3 ] && [ "$PYTHON_MINOR" -lt 10 ]); then
        print_error "Python 3.10+ required. Found: $PYTHON_VERSION"
        exit 1
    fi

    print_success "Python $PYTHON_VERSION found"
    echo ""
}

# Check pip availability
check_pip() {
    print_section "Checking pip availability"

    PIP_CMD=""
    if command -v pip3 &> /dev/null; then
        PIP_CMD="pip3"
        print_success "pip3 found"
    elif command -v pip &> /dev/null; then
        PIP_CMD="pip"
        print_success "pip found"
    elif $PYTHON_CMD -m pip --version &> /dev/null; then
        PIP_CMD="$PYTHON_CMD -m pip"
        print_success "pip (via python module) found"
    else
        print_error "pip not found. Please install pip first."
        echo "  Ubuntu/Debian: sudo apt install python3-pip"
        echo "  Fedora/RHEL: sudo dnf install python3-pip"
        echo "  Arch: sudo pacman -S python-pip"
        exit 1
    fi
    echo ""
}

# Install LeIndex package
install_leindex() {
    print_section "Installing LeIndex package"

    # Try to upgrade pip first
    echo "Upgrading pip..."
    $PIP_CMD install --upgrade pip setuptools wheel || print_warning "Failed to upgrade pip (continuing anyway)"

    # Install LeIndex
    echo "Installing LeIndex..."
    if $PIP_CMD install leindex; then
        print_success "LeIndex installed successfully"

        # Verify installation
        if command -v leindex &> /dev/null; then
            print_success "LeIndex command available in PATH"
            LEINDEX_CMD="leindex"
        elif $PYTHON_CMD -c "import leindex.server" &> /dev/null; then
            print_warning "LeIndex installed but 'leindex' command not in PATH"
            echo "  You may need to: export PATH=\"\$PATH:\$HOME/.local/bin\""
            LEINDEX_CMD="$PYTHON_CMD -m leindex.server"
        else
            print_error "LeIndex installation verification failed"
            exit 1
        fi
    else
        print_error "Failed to install LeIndex"
        exit 1
    fi
    echo ""
}

# Python function to merge JSON configs
merge_json_config() {
    local config_file=$1
    local server_name=$2
    local server_command=$3

    $PYTHON_CMD << PYTHON_EOF
import json
import sys
import os

config_file = "$config_file"
server_name = "$server_name"
server_command = "$server_command"

# Read existing config or create new
try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure mcpServers exists
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Add LeIndex server
config['mcpServers'][server_name] = {
    'command': server_command,
    'args': []
}

# Write back with proper formatting
with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)

print(f"Config updated: {config_file}")
PYTHON_EOF
}

# Backup existing config
backup_config() {
    local config_file=$1
    if [ -f "$config_file" ]; then
        local backup_file="${config_file}.backup.$(date +%Y%m%d_%H%M%S)"
        cp "$config_file" "$backup_file"
        print_warning "Backed up existing config to: $backup_file"
    fi
}

# Configure Claude Code Desktop
configure_claude_code() {
    print_section "Configuring Claude Code Desktop"

    local config_dir="$HOME/.config/claude"
    local config_file="$config_dir/claude_desktop_config.json"

    # Create directory
    mkdir -p "$config_dir"

    # Backup existing config
    backup_config "$config_file"

    # Merge config
    merge_json_config "$config_file" "leindex" "leindex"

    print_success "Claude Code configured"
    echo "  Config: $config_file"
    echo ""
}

# Configure Cursor
configure_cursor() {
    print_section "Configuring Cursor"

    local config_dir="$HOME/.cursor"
    local config_file="$config_dir/mcp.json"

    # Create directory
    mkdir -p "$config_dir"

    # Backup existing config
    backup_config "$config_file"

    # Merge config
    merge_json_config "$config_file" "leindex" "leindex"

    print_success "Cursor configured"
    echo "  Config: $config_file"
    echo ""
}

# Configure VS Code (Cline, Continue, Roo Code)
configure_vscode() {
    print_section "Configuring VS Code Extensions"

    local config_dir="$HOME/.config/Code/User"
    local config_file="$config_dir/settings.json"

    # Also check .vscode directory
    if [ ! -d "$config_dir" ] && [ -d "$HOME/.vscode" ]; then
        config_dir="$HOME/.vscode"
        config_file="$config_dir/settings.json"
    fi

    # Create directory
    mkdir -p "$config_dir"

    # Backup existing config
    backup_config "$config_file"

    # For VS Code, we need to use the MCP settings format
    # Check if mcpServers key exists in settings.json
    if [ -f "$config_file" ]; then
        # Use Python to properly merge the config
        $PYTHON_CMD << PYTHON_EOF
import json

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Ensure mcpServers exists
if 'mcpServers' not in config:
    config['mcpServers'] = {}

# Add LeIndex server
config['mcpServers']['leindex'] = {
    'command': 'leindex',
    'args': []
}

# Write back
with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)

print(f"VS Code config updated: {config_file}")
PYTHON_EOF
    else
        # Create new config
        cat > "$config_file" << EOF
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": []
    }
  }
}
EOF
    fi

    print_success "VS Code configured (Cline, Continue, Roo Code)"
    echo "  Config: $config_file"
    echo "  Note: Make sure you have one of these extensions installed:"
    echo "    - Cline"
    echo "    - Continue"
    echo "    - Roo Code"
    echo ""
}

# Configure Zed Editor
configure_zed() {
    print_section "Configuring Zed Editor"

    local config_dir="$HOME/.config/zed"
    local config_file="$config_dir/settings.json"

    # Create directory
    mkdir -p "$config_dir"

    # Backup existing config
    backup_config "$config_file"

    # Zed uses a different format for MCP servers
    $PYTHON_CMD << PYTHON_EOF
import json

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# Zed uses 'lsp' for MCP servers
if 'lsp' not in config:
    config['lsp'] = {}

config['lsp']['leindex'] = {
    'command': 'leindex',
    'args': []
}

# Write back
with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)

print(f"Zed config updated: {config_file}")
PYTHON_EOF

    print_success "Zed Editor configured"
    echo "  Config: $config_file"
    echo ""
}

# Configure CLI tools
configure_cli_tools() {
    print_section "Configuring CLI Tools"

    # Most CLI tools just need the package installed
    # LeIndex CLI is already available as 'leindex-search' command

    if command -v leindex-search &> /dev/null; then
        print_success "LeIndex CLI available: leindex-search"
        echo ""
    else
        print_warning "LeIndex CLI not in PATH, but available via: python -m leindex.cli"
        echo ""
    fi
}

# Display tool menu
show_tool_menu() {
    echo -e "${BLUE}Select AI tools to integrate with LeIndex:${NC}"
    echo ""
    echo "  1) Claude Code (Desktop)"
    echo "  2) Cursor"
    echo "  3) VS Code (Cline, Continue, Roo Code)"
    echo "  4) Zed Editor"
    echo "  5) CLI Tools (Gemini, OpenCode, etc.)"
    echo "  6) All tools"
    echo "  7) Skip tool integration (MCP server only)"
    echo "  8) Custom selection"
    echo ""
    read -p "Enter your choice (1-8): " choice
    echo ""

    case $choice in
        1)
            configure_claude_code
            ;;
        2)
            configure_cursor
            ;;
        3)
            configure_vscode
            ;;
        4)
            configure_zed
            ;;
        5)
            configure_cli_tools
            ;;
        6)
            configure_claude_code
            configure_cursor
            configure_vscode
            configure_zed
            configure_cli_tools
            ;;
        7)
            print_warning "Skipping tool integration"
            echo "LeIndex MCP server is installed and ready to use manually"
            echo ""
            ;;
        8)
            echo "Select tools to configure (space-separated, e.g., '1 2 4'):"
            echo "  1) Claude Code  2) Cursor  3) VS Code  4) Zed  5) CLI"
            read -p "> " custom_choices
            echo ""
            for choice in $custom_choices; do
                case $choice in
                    1) configure_claude_code ;;
                    2) configure_cursor ;;
                    3) configure_vscode ;;
                    4) configure_zed ;;
                    5) configure_cli_tools ;;
                esac
            done
            ;;
        *)
            print_error "Invalid choice. Skipping tool integration."
            echo ""
            ;;
    esac
}

# Verify installation
verify_installation() {
    print_section "Verifying Installation"

    # Check if LeIndex is installed
    if $PYTHON_CMD -c "import leindex.server" 2>/dev/null; then
        print_success "LeIndex package installed"

        # Show version
        VERSION=$($PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>/dev/null || echo "unknown")
        echo "  Version: $VERSION"
    else
        print_error "LeIndex package not found"
        return 1
    fi

    # Check configured tools
    echo ""
    echo "Configured tools:"

    if [ -f "$HOME/.config/claude/claude_desktop_config.json" ]; then
        if grep -q "leindex" "$HOME/.config/claude/claude_desktop_config.json" 2>/dev/null; then
            print_success "Claude Code"
        fi
    fi

    if [ -f "$HOME/.cursor/mcp.json" ]; then
        if grep -q "leindex" "$HOME/.cursor/mcp.json" 2>/dev/null; then
            print_success "Cursor"
        fi
    fi

    if [ -f "$HOME/.config/Code/User/settings.json" ] || [ -f "$HOME/.vscode/settings.json" ]; then
        if grep -q "leindex" "$HOME/.config/Code/User/settings.json" 2>/dev/null || \
           grep -q "leindex" "$HOME/.vscode/settings.json" 2>/dev/null; then
            print_success "VS Code"
        fi
    fi

    if [ -f "$HOME/.config/zed/settings.json" ]; then
        if grep -q "leindex" "$HOME/.config/zed/settings.json" 2>/dev/null; then
            print_success "Zed Editor"
        fi
    fi

    echo ""
}

# Print success message
print_completion() {
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║${NC} ${BLUE}Installation Complete!${NC} ${GREEN}                                   ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${BLUE}Next Steps:${NC}"
    echo ""
    echo "1. Restart your AI tool(s) to load LeIndex"
    echo "2. LeIndex will be available as an MCP server"
    echo "3. Use the MCP tools in your AI assistant to:"
    echo "     - Index code repositories"
    echo "     - Search code with natural language"
    echo "     - Analyze code patterns"
    echo ""
    echo -e "${CYAN}Documentation:${NC}"
    echo "  https://github.com/scooter-lacroix/leindex"
    echo ""
    echo -e "${YELLOW}Troubleshooting:${NC}"
    echo "  If LeIndex doesn't appear in your AI tool:"
    echo "  1. Check the config file for syntax errors"
    echo "  2. Ensure 'leindex' command is in your PATH"
    echo "  3. Restart the AI tool completely"
    echo "  4. Check AI tool logs for MCP errors"
    echo ""
}

# Rollback function
rollback() {
    print_error "Installation failed or interrupted"
    echo ""
    echo "Rolling back changes..."

    # Find and restore backups
    find "$HOME/.config" -name "*.backup.*" -type f 2>/dev/null | while read -r backup; do
        original="${backup%.backup.*}"
        echo "Restoring: $original"
        cp "$backup" "$original"
        rm "$backup"
    done

    echo "Rollback complete"
}

# Main installation flow
main() {
    # Set up rollback on error
    trap rollback ERR

    print_header

    # Check prerequisites
    check_python
    check_pip

    # Install LeIndex
    install_leindex

    # Configure tools
    show_tool_menu

    # Verify installation
    verify_installation

    # Print completion message
    print_completion

    # Disable rollback trap
    trap - ERR
}

# Run main function
main "$@"
