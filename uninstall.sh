#!/usr/bin/env bash
#############################################
# LeIndex Uninstaller
# Version: 3.0.0
# Platform: Linux/Unix/macOS
#############################################

set -euo pipefail

# ============================================================================
# CONFIGURATION
# ============================================================================
readonly SCRIPT_VERSION="3.0.0"
readonly PROJECT_NAME="LeIndex"
readonly PROJECT_SLUG="leindex"

# Installation paths
LEINDEX_HOME="${LEINDEX_HOME:-$HOME/.leindex}"

# ============================================================================
# COLOR OUTPUT
# ============================================================================
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly BLUE='\033[0;34m'
readonly YELLOW='\033[1;33m'
readonly CYAN='\033[0;36m'
readonly BOLD='\033[1m'
readonly NC='\033[0m'

# ============================================================================
# UTILITY FUNCTIONS
# ============================================================================

print_header() {
    local width=60
    echo -e "${RED}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║${NC} ${BOLD}Uninstalling $PROJECT_NAME v$SCRIPT_VERSION${NC} ${RED}                   ║${NC}"
    echo -e "${RED}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_bullet() {
    echo -e "  ${CYAN}•${NC} $1"
}

ask_yes_no() {
    local prompt="$1"
    local default="${2:-n}"

    if [[ "$default" == "y" ]]; then
        prompt="$prompt [Y/n]"
    else
        prompt="$prompt [y/N]"
    fi

    while true; do
        read -rp "$(echo -e "${YELLOW}?${NC} $prompt ")" answer
        answer=${answer:-$default}

        case "$answer" in
            [Yy]|[Yy][Ee][Ss]) return 0 ;;
            [Nn]|[Nn][Oo]) return 1 ;;
            *) echo "Please answer yes or no." ;;
        esac
    done
}

# ============================================================================
# REMOVAL FUNCTIONS
# ============================================================================

# Remove Python package
remove_package() {
    echo -e "${BLUE}>>> Removing Python Package${NC}"
    echo ""

    # Find Python
    PYTHON_CMD=""
    if command -v python3 &> /dev/null; then
        PYTHON_CMD="python3"
    elif command -v python &> /dev/null; then
        PYTHON_CMD="python"
    else
        print_warning "Python not found, skipping package removal"
        return
    fi

    # Try pip, pip3, pipx
    for pip_cmd in "pip3" "pip" "pipx" "$PYTHON_CMD -m pip"; do
        if $pip_cmd show "$PROJECT_SLUG" &> /dev/null 2>&1; then
            print_info "Uninstalling from: $pip_cmd"
            $pip_cmd uninstall -y "$PROJECT_SLUG" 2>/dev/null || true
            print_success "Package removed via $pip_cmd"
            echo ""
            return
        fi
    done

    print_warning "Package not found in any package manager"
    echo ""
}

# Remove MCP configurations
remove_mcp_configs() {
    echo -e "${BLUE}>>> Removing MCP Configurations${NC}"
    echo ""

    local configs=(
        "$HOME/.config/claude/claude_desktop_config.json"
        "$HOME/.cursor/mcp.json"
        "$HOME/.config/Code/User/settings.json"
        "$HOME/.config/VSCodium/User/settings.json"
        "$HOME/.vscode/settings.json"
        "$HOME/.config/zed/settings.json"
        "$HOME/.var/app/dev.zed.Zed/config/zed/settings.json"
        "$HOME/.var/app/com.zed.Zed/config/zed/settings.json"
    )

    for config_file in "${configs[@]}"; do
        if [[ -f "$config_file" ]]; then
            # Remove leindex from config
            if command -v python3 &> /dev/null; then
                python3 << PYTHON_EOF
import json

config_file = "$config_file"

try:
    with open(config_file, 'r') as f:
        config = json.load(f)

	    # Remove leindex from mcpServers
	    if 'mcpServers' in config and 'leindex' in config['mcpServers']:
	        del config['mcpServers']['leindex']
	        print(f"Removed from: {config_file}")

	        # Clean up empty mcpServers
	        if not config['mcpServers']:
	            del config['mcpServers']

	    # Remove leindex from context_servers (Zed MCP)
	    if 'context_servers' in config and isinstance(config['context_servers'], dict) and 'leindex' in config['context_servers']:
	        del config['context_servers']['leindex']
	        print(f"Removed from: {config_file}")

	        # Clean up empty context_servers
	        if not config['context_servers']:
	            del config['context_servers']

	    # Remove leindex from legacy language_models.mcp_servers (older installer versions)
	    if 'language_models' in config and isinstance(config['language_models'], dict):
	        mcp_servers = config['language_models'].get('mcp_servers')
	        if isinstance(mcp_servers, dict) and 'leindex' in mcp_servers:
	            del mcp_servers['leindex']
	            print(f"Removed from: {config_file}")
	            if not mcp_servers:
	                config['language_models'].pop('mcp_servers', None)
	        if not config['language_models']:
	            del config['language_models']

	    # Remove leindex from lsp (Zed)
	    if 'lsp' in config and 'leindex' in config['lsp']:
	        del config['lsp']['leindex']
	        print(f"Removed from: {config_file}")

        # Clean up empty lsp
        if not config['lsp']:
            del config['lsp']

    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')

except (FileNotFoundError, json.JSONDecodeError):
    pass
PYTHON_EOF
            fi
        fi
    done

    print_success "MCP configurations cleaned"
    echo ""
}

# Remove data directory
remove_data_directory() {
    echo -e "${BLUE}>>> Removing Data Directory${NC}"
    echo ""

    if [[ -d "$LEINDEX_HOME" ]]; then
        print_warning "This will delete all $PROJECT_NAME data:"
        print_bullet "Configuration files"
        print_bullet "Indexed data"
        print_bullet "Log files"
        print_bullet "Search indices"
        echo ""

        if ask_yes_no "Remove $LEINDEX_HOME?" "n"; then
            rm -rf "$LEINDEX_HOME"
            print_success "Data directory removed"
        else
            print_warning "Data directory preserved"
        fi
    else
        print_info "No data directory found"
    fi

    echo ""
}

# Remove from shell configs
remove_shell_configs() {
    echo -e "${BLUE}>>> Cleaning Shell Configurations${NC}"
    echo ""

    local shell_configs=("$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.bash_profile" "$HOME/.profile")
    local modified=false

    for config_file in "${shell_configs[@]}"; do
        if [[ -f "$config_file" ]]; then
            # Remove LeIndex PATH additions
            if grep -q "LeIndex" "$config_file" 2>/dev/null; then
                # Create backup
                cp "$config_file" "${config_file}.backup.$(date +%Y%m%d_%H%M%S)"

                # Remove LeIndex lines
                sed -i'.tmp' '/# LeIndex/,/^$/d' "$config_file" 2>/dev/null || true
                rm -f "${config_file}.tmp" 2>/dev/null || true

                print_success "Cleaned: $config_file"
                modified=true
            fi
        fi
    done

    if [[ "$modified" == "false" ]]; then
        print_info "No shell configurations found"
    fi

    echo ""
}

# ============================================================================
# MAIN UNINSTALLATION FLOW
# ============================================================================

main() {
    clear
    print_header

    echo -e "${BOLD}WARNING: This will completely remove $PROJECT_NAME from your system.${NC}"
    echo ""
    echo "This uninstaller will:"
    print_bullet "Remove the Python package"
    print_bullet "Remove MCP server configurations"
    print_bullet "Optionally remove all data and indices"
    print_bullet "Clean shell configuration files"
    echo ""

    if ! ask_yes_no "Continue with uninstallation?" "n"; then
        echo "Aborted."
        exit 0
    fi

    echo ""

    # Remove components
    remove_package
    remove_mcp_configs
    remove_shell_configs
    remove_data_directory

    # Final message
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║${NC} ${BOLD}Uninstallation Complete!${NC} ${GREEN}                                    ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""

    echo "Thank you for using $PROJECT_NAME!"
    echo "We'd love to hear your feedback: https://github.com/scooter-lacroix/leindex/issues"
    echo ""
}

# Run uninstallation
main "$@"
