#############################################
# LeIndex Installer for Windows
# Version: 2.0.0
# Supports: Claude Code, Cursor, VS Code, Zed, CLI tools
#############################################

$ErrorActionPreference = "Stop"

# Color output functions
function Print-Header {
    Write-Host "╔════════════════════════════════════════════════════════════╗" -Cyan
    Write-Host "║ LeIndex Installer v2.0.0 for Windows                      ║" -Cyan
    Write-Host "║ AI-Powered Code Search & MCP Server                       ║" -Cyan
    Write-Host "╚════════════════════════════════════════════════════════════╝" -Cyan
    Write-Host ""
}

function Print-Success {
    param([string]$Message)
    Write-Host "✓ $Message" -Green
}

function Print-Warning {
    param([string]$Message)
    Write-Host "⚠ $Message" -Yellow
}

function Print-Error {
    param([string]$Message)
    Write-Host "✗ $Message" -Red
}

function Print-Section {
    param([string]$Message)
    Write-Host ">>> $Message" -Blue
}

# Check Python version (requires 3.10+)
function Test-PythonVersion {
    Print-Section "Checking Python version"

    $pythonCmd = $null
    $pythonVersion = $null

    # Try python first, then py launcher
    try {
        $result = & python --version 2>&1
        if ($LASTEXITCODE -eq 0) {
            $pythonCmd = "python"
            $pythonVersion = $result.ToString().Replace("Python ", "")
        }
    } catch {
        # python not found, try py launcher
    }

    if (-not $pythonCmd) {
        try {
            $result = & py --version 2>&1
            if ($LASTEXITCODE -eq 0) {
                $pythonCmd = "py"
                $pythonVersion = $result.ToString().Replace("Python ", "")
            }
        } catch {
            Print-Error "Python not found. Please install Python 3.10+ first."
            Write-Host "  Download from: https://www.python.org/downloads/"
            Write-Host "  During installation, check 'Add Python to PATH'"
            exit 1
        }
    }

    # Parse version
    try {
        $versionParts = $pythonVersion.Split('.')
        $major = [int]$versionParts[0]
        $minor = [int]$versionParts[1]

        if ($major -lt 3 -or ($major -eq 3 -and $minor -lt 10)) {
            Print-Error "Python 3.10+ required. Found: $pythonVersion"
            exit 1
        }

        Print-Success "Python $pythonVersion found"
    } catch {
        Print-Error "Failed to parse Python version: $pythonVersion"
        exit 1
    }

    Write-Host ""
    return $pythonCmd
}

# Check pip availability
function Test-Pip {
    param([string]$PythonCmd)

    Print-Section "Checking pip availability"

    $pipCmd = $null

    # Try pip
    try {
        $result = & pip --version 2>&1
        if ($LASTEXITCODE -eq 0) {
            $pipCmd = "pip"
            Print-Success "pip found"
        }
    } catch {
        # pip not found
    }

    # Try pip3
    if (-not $pipCmd) {
        try {
            $result = & pip3 --version 2>&1
            if ($LASTEXITCODE -eq 0) {
                $pipCmd = "pip3"
                Print-Success "pip3 found"
            }
        } catch {
            # pip3 not found
        }
    }

    # Try python -m pip
    if (-not $pipCmd) {
        try {
            $result = & $PythonCmd -m pip --version 2>&1
            if ($LASTEXITCODE -eq 0) {
                $pipCmd = "$PythonCmd -m pip"
                Print-Success "pip (via python module) found"
            }
        } catch {
            # pip not available
        }
    }

    if (-not $pipCmd) {
        Print-Error "pip not found. Please install pip first."
        Write-Host "  Run: python -m ensurepip --upgrade"
        Write-Host "  Or download get-pip.py from https://bootstrap.pypa.io/get-pip.py"
        exit 1
    }

    Write-Host ""
    return $pipCmd
}

# Install LeIndex package
function Install-LeIndexPackage {
    param([string]$PipCmd)

    Print-Section "Installing LeIndex package"

    # Try to upgrade pip first
    Write-Host "Upgrading pip..."
    try {
        & $PipCmd install --upgrade pip setuptools wheel | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Print-Warning "Failed to upgrade pip (continuing anyway)"
        }
    } catch {
        Print-Warning "Failed to upgrade pip (continuing anyway)"
    }

    # Install LeIndex
    Write-Host "Installing LeIndex..."
    try {
        & $PipCmd install leindex
        if ($LASTEXITCODE -eq 0) {
            Print-Success "LeIndex installed successfully"

            # Verify installation
            try {
                & $PipCmd show leindex | Out-Null
                if ($LASTEXITCODE -eq 0) {
                    Print-Success "LeIndex package verified"
                }
            } catch {
                Print-Warning "LeIndex installation may have issues"
            }
        } else {
            Print-Error "Failed to install LeIndex"
            exit 1
        }
    } catch {
        Print-Error "Failed to install LeIndex: $_"
        exit 1
    }

    Write-Host ""
}

# Python function to merge JSON configs (embedded in PowerShell)
function Merge-JsonConfig {
    param(
        [string]$ConfigFile,
        [string]$ServerName = "leindex",
        [string]$ServerCommand = "leindex"
    )

    $pythonScript = @"
import json
import sys

config_file = '$ConfigFile'
server_name = '$ServerName'
server_command = '$ServerCommand'

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

config['mcpServers'][server_name] = {
    'command': server_command,
    'args': []
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)

print(f"Config updated: {config_file}")
"@

    $pythonCmd = (Get-Command python -ErrorAction SilentlyContinue).Source
    if (-not $pythonCmd) {
        $pythonCmd = (Get-Command py -ErrorAction SilentlyContinue).Source
    }

    if ($pythonCmd) {
        $output = & $pythonCmd -c $pythonScript 2>&1
        Write-Host $output
    } else {
        Print-Error "Python not found for JSON merging"
    }
}

# Backup existing config
function Backup-Config {
    param([string]$ConfigFile)

    if (Test-Path $ConfigFile) {
        $timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
        $backupFile = "$ConfigFile.backup.$timestamp"
        Copy-Item $ConfigFile $backupFile
        Print-Warning "Backed up existing config to: $backupFile"
    }
}

# Configure Claude Code Desktop
function Configure-ClaudeCode {
    Print-Section "Configuring Claude Code Desktop"

    $configDir = Join-Path $env:APPDATA "Claude"
    $configFile = Join-Path $configDir "claude_desktop_config.json"

    # Create directory
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    # Backup existing config
    Backup-Config $configFile

    # Merge config
    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "Claude Code configured"
    Write-Host "  Config: $configFile"
    Write-Host ""
}

# Configure Cursor
function Configure-Cursor {
    Print-Section "Configuring Cursor"

    $configDir = Join-Path $env:APPDATA "Cursor"
    $configFile = Join-Path $configDir "mcp.json"

    # Create directory
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    # Backup existing config
    Backup-Config $configFile

    # Merge config
    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "Cursor configured"
    Write-Host "  Config: $configFile"
    Write-Host ""
}

# Configure VS Code (Cline, Continue, Roo Code)
function Configure-VSCode {
    Print-Section "Configuring VS Code Extensions"

    $configDir = Join-Path $env:APPDATA "Code\User"
    $configFile = Join-Path $configDir "settings.json"

    # Create directory
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    # Backup existing config
    Backup-Config $configFile

    # Merge config
    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "VS Code configured (Cline, Continue, Roo Code)"
    Write-Host "  Config: $configFile"
    Write-Host "  Note: Make sure you have one of these extensions installed:"
    Write-Host "    - Cline"
    Write-Host "    - Continue"
    Write-Host "    - Roo Code"
    Write-Host ""
}

# Configure Zed Editor
function Configure-Zed {
    Print-Section "Configuring Zed Editor"

    $configDir = Join-Path $env:APPDATA "Zed"
    $configFile = Join-Path $configDir "settings.json"

    # Create directory
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null

    # Backup existing config
    Backup-Config $configFile

    # Zed uses different format (lsp instead of mcpServers)
    $pythonScript = @"
import json

config_file = '$configFile'

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

if 'lsp' not in config:
    config['lsp'] = {}

config['lsp']['leindex'] = {
    'command': 'leindex',
    'args': []
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)

print(f"Zed config updated: {config_file}")
"@

    $pythonCmd = (Get-Command python -ErrorAction SilentlyContinue).Source
    if (-not $pythonCmd) {
        $pythonCmd = (Get-Command py -ErrorAction SilentlyContinue).Source
    }

    if ($pythonCmd) {
        $output = & $pythonCmd -c $pythonScript 2>&1
        Write-Host $output
    }

    Print-Success "Zed Editor configured"
    Write-Host "  Config: $configFile"
    Write-Host ""
}

# Configure CLI tools
function Configure-CLITools {
    Print-Section "Configuring CLI Tools"

    # LeIndex CLI is available via leindex-search command
    $leindexSearch = Get-Command "leindex-search" -ErrorAction SilentlyContinue

    if ($leindexSearch) {
        Print-Success "LeIndex CLI available: leindex-search"
    } else {
        Print-Warning "LeIndex CLI not in PATH, but package is installed"
        Write-Host "  You may need to restart your terminal for PATH changes to take effect"
        Write-Host "  Or use: python -m leindex.cli"
    }

    Write-Host ""
}

# Display tool menu
function Show-ToolMenu {
    Write-Host "Select AI tools to integrate with LeIndex:" -Blue
    Write-Host ""
    Write-Host "  1) Claude Code (Desktop)"
    Write-Host "  2) Cursor"
    Write-Host "  3) VS Code (Cline, Continue, Roo Code)"
    Write-Host "  4) Zed Editor"
    Write-Host "  5) CLI Tools (Gemini, OpenCode, etc.)"
    Write-Host "  6) All tools"
    Write-Host "  7) Skip tool integration (MCP server only)"
    Write-Host "  8) Custom selection"
    Write-Host ""

    $choice = Read-Host "Enter your choice (1-8)"
    Write-Host ""

    switch ($choice) {
        "1" { Configure-ClaudeCode }
        "2" { Configure-Cursor }
        "3" { Configure-VSCode }
        "4" { Configure-Zed }
        "5" { Configure-CLITools }
        "6" {
            Configure-ClaudeCode
            Configure-Cursor
            Configure-VSCode
            Configure-Zed
            Configure-CLITools
        }
        "7" {
            Print-Warning "Skipping tool integration"
            Write-Host "LeIndex MCP server is installed and ready to use manually"
            Write-Host ""
        }
        "8" {
            Write-Host "Select tools to configure (comma-separated, e.g., '1,2,4'):"
            Write-Host "  1) Claude Code  2) Cursor  3) VS Code  4) Zed  5) CLI"
            $customChoices = Read-Host ">"

            $choices = $customChoices -split ','
            foreach ($c in $choices) {
                switch ($c.Trim()) {
                    "1" { Configure-ClaudeCode }
                    "2" { Configure-Cursor }
                    "3" { Configure-VSCode }
                    "4" { Configure-Zed }
                    "5" { Configure-CLITools }
                }
            }
        }
        default {
            Print-Error "Invalid choice. Skipping tool integration."
            Write-Host ""
        }
    }
}

# Verify installation
function Test-Installation {
    Print-Section "Verifying Installation"

    $pythonCmd = (Get-Command python -ErrorAction SilentlyContinue).Source
    if (-not $pythonCmd) {
        $pythonCmd = (Get-Command py -ErrorAction SilentlyContinue).Source
    }

    # Check if LeIndex is installed
    try {
        $result = & $pythonCmd -c "import leindex.server" 2>&1
        if ($LASTEXITCODE -eq 0) {
            Print-Success "LeIndex package installed"

            # Show version
            try {
                $version = & $pythonCmd -c "import leindex; print(leindex.__version__)" 2>&1
                Write-Host "  Version: $version"
            } catch {
                Write-Host "  Version: unknown"
            }
        } else {
            Print-Error "LeIndex package not found"
            return $false
        }
    } catch {
        Print-Error "LeIndex package not found"
        return $false
    }

    # Check configured tools
    Write-Host ""
    Write-Host "Configured tools:"

    $claudeConfig = Join-Path $env:APPDATA "Claude\claude_desktop_config.json"
    if (Test-Path $claudeConfig) {
        $content = Get-Content $claudeConfig -Raw
        if ($content -match "leindex") {
            Print-Success "Claude Code"
        }
    }

    $cursorConfig = Join-Path $env:APPDATA "Cursor\mcp.json"
    if (Test-Path $cursorConfig) {
        $content = Get-Content $cursorConfig -Raw
        if ($content -match "leindex") {
            Print-Success "Cursor"
        }
    }

    $vscodeConfig = Join-Path $env:APPDATA "Code\User\settings.json"
    if (Test-Path $vscodeConfig) {
        $content = Get-Content $vscodeConfig -Raw
        if ($content -match "leindex") {
            Print-Success "VS Code"
        }
    }

    $zedConfig = Join-Path $env:APPDATA "Zed\settings.json"
    if (Test-Path $zedConfig) {
        $content = Get-Content $zedConfig -Raw
        if ($content -match "leindex") {
            Print-Success "Zed Editor"
        }
    }

    Write-Host ""
    return $true
}

# Print completion message
function Print-Completion {
    Write-Host "╔════════════════════════════════════════════════════════════╗" -Green
    Write-Host "║ Installation Complete!                                     ║" -Green
    Write-Host "╚════════════════════════════════════════════════════════════╝" -Green
    Write-Host ""
    Write-Host "Next Steps:" -Blue
    Write-Host ""
    Write-Host "1. Restart your AI tool(s) to load LeIndex"
    Write-Host "2. LeIndex will be available as an MCP server"
    Write-Host "3. Use the MCP tools in your AI assistant to:"
    Write-Host "     - Index code repositories"
    Write-Host "     - Search code with natural language"
    Write-Host "     - Analyze code patterns"
    Write-Host ""
    Write-Host "Documentation:" -Cyan
    Write-Host "  https://github.com/scooter-lacroix/leindex"
    Write-Host ""
    Write-Host "Windows Notes:" -Yellow
    Write-Host "  - If 'leindex' command is not recognized, restart your terminal"
    Write-Host "  - Or add Python Scripts to PATH:"
    Write-Host "    $env:APPDATA\Python\Python3*\Scripts\"
    Write-Host ""
    Write-Host "Troubleshooting:" -Yellow
    Write-Host "  If LeIndex doesn't appear in your AI tool:"
    Write-Host "  1. Check the config file for syntax errors"
    Write-Host "  2. Ensure 'leindex' command is in your PATH"
    Write-Host "  3. Restart the AI tool completely"
    Write-Host "  4. Check AI tool logs for MCP errors"
    Write-Host ""
}

# Rollback function
function Invoke-Rollback {
    Print-Error "Installation failed or interrupted"
    Write-Host ""
    Write-Host "Rolling back changes..."

    # Find and restore backups
    $backupFiles = Get-ChildItem -Path $env:APPDATA -Recurse -Filter "*.backup.*" -ErrorAction SilentlyContinue

    foreach ($backup in $backupFiles) {
        $original = $backup.FullName -replace '\.backup\.\d{8}_\d{6}$', ''
        if (Test-Path $backup.FullName) {
            Write-Host "Restoring: $original"
            Copy-Item $backup.FullName $original -Force
            Remove-Item $backup.FullName -Force
        }
    }

    Write-Host "Rollback complete"
}

# Main installation flow
function Main {
    try {
        Print-Header

        # Check prerequisites
        $pythonCmd = Test-PythonVersion
        $pipCmd = Test-Pip $pythonCmd

        # Install LeIndex
        Install-LeIndexPackage $pipCmd

        # Configure tools
        Show-ToolMenu

        # Verify installation
        Test-Installation

        # Print completion message
        Print-Completion
    } catch {
        Invoke-Rollback
        exit 1
    }
}

# Run main function
Main
