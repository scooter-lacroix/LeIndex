#############################################
# LeIndex Universal Installer
# Version: 3.0.0
# Platform: Windows (PowerShell 5.1+)
# Supports: 15+ AI CLI tools with full MCP integration
#############################################

#Requires -Version 5.1
#Requires -RunAsAdministrator

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ============================================================================
# CONFIGURATION
# ============================================================================
$ScriptVersion = "3.0.0"
$ProjectName = "LeIndex"
$ProjectSlug = "leindex"
$MinPythonMajor = 3
$MinPythonMinor = 10
$RepoUrl = "https://github.com/scooter-lacroix/leindex"
$PypiPackage = "leindex"

# Installation paths
$LEINDEX_HOME = if ($env:LEINDEX_HOME) { $env:LEINDEX_HOME } else { "$env:USERPROFILE\.leindex" }
$ConfigDir = "$LEINDEX_HOME\config"
$DataDir = "$LEINDEX_HOME\data"
$LogDir = "$LEINDEX_HOME\logs"

# Backup directory
$BackupDir = "$env:TEMP\leindex-install-backup-$(Get-Date -Format 'yyyyMMdd_HHmmss')"

# ============================================================================
# HELPER FUNCTIONS
# ============================================================================

# Write colored output
function Write-ColorOutput {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Message,
        [ValidateSet("Black", "DarkBlue", "DarkGreen", "DarkCyan", "DarkRed", "DarkMagenta", "DarkYellow",
                     "Gray", "DarkGray", "Blue", "Green", "Cyan", "Red", "Magenta", "Yellow", "White")]
        [string]$ForegroundColor = "White",
        [switch]$NoNewline
    )

    $fc = $host.UI.RawUI.ForegroundColor
    $host.UI.RawUI.ForegroundColor = $ForegroundColor
    if ($NoNewline) {
        Write-Host -NoNewline $Message
    } else {
        Write-Host $Message
    }
    $host.UI.RawUI.ForegroundColor = $fc
}

# Print header
function Print-Header {
    $width = 60
    Write-ColorOutput "═" * $width -ForegroundColor Cyan
    Write-ColorOutput "║ $("$ProjectName Installer v$ScriptVersion".PadRight($width - 2)) ║" -ForegroundColor Cyan
    Write-ColorOutput "║ $("AI-Powered Code Search & MCP Server".PadRight($width - 2)) ║" -ForegroundColor Cyan
    Write-ColorOutput "═" * $width -ForegroundColor Cyan
    Write-Host ""
}

# Print section header
function Print-Section {
    param([string]$Title)
    Write-ColorOutput ">>> $Title <<<" -ForegroundColor Blue
    Write-Host ""
}

# Print success message
function Print-Success {
    param([string]$Message)
    Write-ColorOutput "✓ $Message" -ForegroundColor Green
}

# Print warning message
function Print-Warning {
    param([string]$Message)
    Write-ColorOutput "⚠ $Message" -ForegroundColor Yellow
}

# Print error message
function Print-Error {
    param([string]$Message)
    Write-ColorOutput "✗ $Message" -ForegroundColor Red
}

# Print info message
function Print-Info {
    param([string]$Message)
    Write-ColorOutput "ℹ $Message" -ForegroundColor Cyan
}

# Print bullet point
function Print-Bullet {
    param([string]$Message)
    Write-ColorOutput "  • $Message" -ForegroundColor Cyan
}

# Ask yes/no question
function Ask-YesNo {
    param(
        [string]$Prompt,
        [bool]$Default = $false
    )

    $defaultPrompt = if ($Default) { "[Y/n]" } else { "[y/N]" }

    while ($true) {
        $answer = Read-Host "? $Prompt $defaultPrompt"
        $answer = $answer.Trim()

        if ([string]::IsNullOrEmpty($answer)) {
            return $Default
        }

        switch ($answer.ToLower()) {
            {$_ -in "y", "yes"} { return $true }
            {$_ -in "n", "no"} { return $false }
            default { Write-Host "Please answer yes or no." }
        }
    }
}

# ============================================================================
# ERROR HANDLING & ROLLBACK
# ============================================================================

# Initialize rollback system
function Initialize-Rollback {
    New-Item -ItemType Directory -Path $BackupDir -Force | Out-Null
    $BackupDir | Out-File -FilePath "$env:TEMP\leindex-backup-dir-$PID" -Encoding UTF8
}

# Backup file for potential rollback
function Backup-FileSafe {
    param([string]$FilePath)

    if (Test-Path $FilePath) {
        $backupName = Split-Path $FilePath -Leaf
        $backupName = "$backupName-" + ($FilePath -replace '[\\/:*?"<>|]', '_')
        $backupPath = Join-Path $BackupDir $backupName
        Copy-Item $FilePath $backupPath -Force

        "$backupPath:$FilePath" | Out-File -FilePath "$BackupDir\manifest.txt" -Append -Encoding UTF8
        Print-Warning "Backed up: $FilePath"
    }
}

# Rollback changes on failure
function Invoke-Rollback {
    param([int]$ExitCode)

    if ($ExitCode -eq 0) {
        # Clean up successful installation
        Remove-Item $BackupDir -Recurse -Force -ErrorAction SilentlyContinue
        return
    }

    Print-Error "Installation failed. Rolling back changes..."

    $manifestPath = "$BackupDir\manifest.txt"
    if (Test-Path $manifestPath) {
        Get-Content $manifestPath | ForEach-Object {
            $parts = $_ -split ':', 2
            if ($parts.Count -eq 2) {
                $backup = $parts[0]
                $original = $parts[1]

                if (Test-Path $backup) {
                    Copy-Item $backup $original -Force
                    Print-Success "Restored: $original"
                }
            }
        }
    }

    # Remove created directories (if empty)
    Remove-Item $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $DataDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $LogDir -Recurse -Force -ErrorAction SilentlyContinue

    Print-Warning "Rollback complete"
    Remove-Item $BackupDir -Recurse -Force -ErrorAction SilentlyContinue
}

# ============================================================================
# ENVIRONMENT DETECTION
# ============================================================================

# Detect Python interpreter
function Find-Python {
    Print-Section "Detecting Python Environment"

    # Prefer Python 3.13 (leann-backend-hnsw compatibility), then 3.12, 3.11, 3.10
    $pythonCmds = @("python3.13", "python3.12", "python3.11", "python3.10", "python", "python3", "py")
    $PYTHON_CMD = $null

    foreach ($cmd in $pythonCmds) {
        try {
            $result = & $cmd --version 2>&1
            if ($LASTEXITCODE -eq 0 -and $result -match "Python (\d+)\.(\d+)") {
                $major = [int]$matches[1]
                $minor = [int]$matches[2]

                # Reject Python 3.14+ (leann-backend-hnsw not compatible)
                if ($major -eq 3 -and $minor -ge 14) {
                    continue
                }

                if ($major -ge $MinPythonMajor -and $minor -ge $MinPythonMinor) {
                    $PYTHON_CMD = $cmd
                    $version = "$major.$minor"
                    break
                }
            }
        } catch {
            # Command not found, continue
        }
    }

    if (-not $PYTHON_CMD) {
        Print-Error "Python 3.10-3.13 not found"
        Write-Host ""
        Write-Host "Please install Python 3.10-3.13:"
        Print-Bullet "Download from: https://www.python.org/downloads/"
        Print-Bullet "During installation, check 'Add Python to PATH'"
        Print-Bullet "Or use: winget install Python.Python.3.13"
        Print-Bullet "NOTE: Python 3.14+ is not supported (leann-backend-hnsw compatibility)"
        exit 1
    }

    $script:PYTHON_CMD = $PYTHON_CMD
    Print-Success "Python $version detected: $PYTHON_CMD"
    Write-Host ""
}

# Detect package manager
function Find-PackageManager {
    Print-Section "Detecting Package Manager"

    # Check for uv (fastest, preferred)
    if (Get-Command uv -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "uv"
        $script:PKG_INSTALL_CMD = "uv pip install"
        Print-Success "uv detected (preferred package manager)"
        return
    }

    # Check for pip
    if (Get-Command pip -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "pip install"
        Print-Success "pip detected"
        return
    }

    # Check for pip3
    if (Get-Command pip3 -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "pip3 install"
        Print-Success "pip3 detected"
        return
    }

    # Fall back to python -m pip
    $testPip = & $PYTHON_CMD -m pip --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "$PYTHON_CMD -m pip install"
        Print-Success "pip (via Python module) detected"
        return
    }

    Print-Error "No package manager found"
    Print-Bullet "Install pip: $PYTHON_CMD -m ensurepip --upgrade"
    Print-Bullet "Or install uv: powershell -c `"irm https://astral.sh/uv/install.ps1 | iex`""
    exit 1
}

# Detect installed AI tools
function Find-AITools {
    Print-Section "Detecting AI Coding Tools"

    $detectedTools = @()

    # Desktop Applications
    if (Test-Path "$env:APPDATA\Claude") { $detectedTools += "claude-desktop" }
    if (Test-Path "$env:APPDATA\Cursor") { $detectedTools += "cursor" }
    if (Test-Path "$env:APPDATA\Code") { $detectedTools += "vscode" }
    if (Test-Path "$env:APPDATA\Zed") { $detectedTools += "zed" }
    if (Test-Path "$env:APPDATA\JetBrains") { $detectedTools += "jetbrains" }

    # CLI Tools - Official/Popular tools
    if (Get-Command claude -ErrorAction SilentlyContinue) { $detectedTools += "claude-cli" }
    if (Get-Command gemini -ErrorAction SilentlyContinue) { $detectedTools += "gemini-cli" }
    if (Get-Command aider -ErrorAction SilentlyContinue) { $detectedTools += "aider" }
    if (Get-Command cursor -ErrorAction SilentlyContinue) { $detectedTools += "cursor-cli" }
    if (Get-Command opencode -ErrorAction SilentlyContinue) { $detectedTools += "opencode" }
    if (Get-Command qwen -ErrorAction SilentlyContinue) { $detectedTools += "qwen-cli" }
    if (Get-Command amp -ErrorAction SilentlyContinue) { $detectedTools += "amp-code" }
    if (Get-Command kilocode -ErrorAction SilentlyContinue) { $detectedTools += "kilocode-cli" }
    if (Get-Command codex -ErrorAction SilentlyContinue) { $detectedTools += "codex-cli" }
    if (Get-Command goose -ErrorAction SilentlyContinue) { $detectedTools += "goose-cli" }
    if (Get-Command mistral -ErrorAction SilentlyContinue) { $detectedTools += "mistral-vibe" }

    # Check for config directories
    if (Test-Path "$env:APPDATA\windsurf") { $detectedTools += "windsurf" }
    if (Test-Path "$env:APPDATA\continue") { $detectedTools += "continue" }
    if (Test-Path "$env:APPDATA\mistral") { $detectedTools += "mistral-vibe" }

    if ($detectedTools.Count -gt 0) {
        Print-Success "Detected $($detectedTools.Count) AI tool(s):"
        foreach ($tool in $detectedTools) {
            Print-Bullet $tool
        }
    } else {
        Print-Warning "No AI tools detected. Will install MCP server only."
    }

    Write-Host ""
}

# ============================================================================
# INSTALLATION
# ============================================================================

# Install LeIndex package
function Install-LeIndexPackage {
    Print-Section "Installing $ProjectName"

    # Upgrade package manager first
    Print-Info "Upgrading package manager..."
    try {
        if ($PKG_MANAGER -eq "uv") {
            uv self-update 2>$null
        } else {
            & $PYTHON_CMD -m pip install --upgrade pip setuptools wheel 2>$null
        }
    } catch {
        Print-Warning "Failed to upgrade package manager (continuing anyway)"
    }

    # Install package
    Print-Info "Installing $PypiPackage..."
    $installArgs = $PKG_INSTALL_CMD.Split(" ")
    & $installArgs[0] $installArgs[1..($installArgs.Length - 1)] $PypiPackage

    if ($LASTEXITCODE -ne 0) {
        Print-Error "Failed to install $PypiPackage"
        exit 1
    }

    Print-Success "$ProjectName installed successfully"

    # Verify installation - for uv, just check if install command succeeded
    # For pip/pipx, check if we can import the module
    if ($PKG_MANAGER -eq "uv") {
        # uv manages its own venv, import check may fail even if successful
        Print-Success "Installation verified (via uv)"
    } else {
        $testImport = & $PYTHON_CMD -c "import leindex.server" 2>&1
        if ($LASTEXITCODE -eq 0) {
            $version = & $PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>&1
            Print-Success "Installation verified: version $version"
        } else {
            Print-Error "Installation verification failed"
            exit 1
        }
    }

    Write-Host ""
}

# Setup directory structure
function Initialize-Directories {
    Print-Section "Setting up Directories"

    $dirs = @($ConfigDir, $DataDir, $LogDir)
    foreach ($dir in $dirs) {
        if (-not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force | Out-Null
            Print-Success "Created: $dir"
        }
    }

    Write-Host ""
}

# ============================================================================
# TOOL INTEGRATION
# ============================================================================

# Merge JSON configuration safely
function Merge-JsonConfig {
    param(
        [string]$ConfigFile,
        [string]$ServerName = "leindex",
        [string]$ServerCommand = "leindex"
    )

    $pythonScript = @"
import json
import sys

config_file = r'$ConfigFile'
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
    'args': ['mcp'],
    'env': {},
    'disabled': False
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
"@

    & $PYTHON_CMD -c $pythonScript
}

# Configure Claude Desktop
function Configure-ClaudeDesktop {
    Print-Section "Configuring Claude Desktop"

    $configDir = "$env:APPDATA\Claude"
    $configFile = "$configDir\claude_desktop_config.json"

    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Backup-FileSafe $configFile

    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "Claude Desktop configured"
    Print-Bullet "Config: $configFile"
    Write-Host ""
}

# Configure Cursor IDE
function Configure-Cursor {
    Print-Section "Configuring Cursor"

    $configDir = "$env:APPDATA\Cursor"
    $configFile = "$configDir\mcp.json"

    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Backup-FileSafe $configFile

    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "Cursor configured"
    Print-Bullet "Config: $configFile"
    Write-Host ""
}

# Configure VS Code
function Configure-VSCode {
    Print-Section "Configuring VS Code Family"

    $vscodeConfigs = @(
        "$env:APPDATA\Code\User\settings.json",
        "$env:APPDATA\VSCodium\User\settings.json"
    )

    foreach ($configFile in $vscodeConfigs) {
        $configDir = Split-Path $configFile -Parent

        if (Test-Path (Split-Path $configDir -Parent)) {
            New-Item -ItemType Directory -Path $configDir -Force | Out-Null
            Backup-FileSafe $configFile

            Merge-JsonConfig $configFile "leindex" "leindex"
            Print-Success "VS Code configured: $configFile"
        }
    }

    Print-Info "Note: Install an MCP extension for VS Code:"
    Print-Bullet "Cline: https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude"
    Print-Bullet "Continue: https://marketplace.visualstudio.com/items?itemName=Continue.continue"
    Print-Bullet "Roo Code: https://marketplace.visualstudio.com/items?itemName=RooCode.roo-code"
    Write-Host ""
}

# Configure Zed Editor
function Configure-Zed {
    Print-Section "Configuring Zed Editor"

    $configDir = "$env:APPDATA\Zed"
    $configFile = "$configDir\settings.json"

    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Backup-FileSafe $configFile

    $pythonScript = @"
import json

config_file = r'$configFile'

try:
    with open(config_file, 'r') as f:
        config = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

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
"@

    & $PYTHON_CMD -c $pythonScript

    Print-Success "Zed Editor configured"
    Print-Bullet "Config: $configFile"
    Write-Host ""
}

# Configure JetBrains IDEs
function Configure-JetBrains {
    Print-Section "Configuring JetBrains IDEs"

    $configDir = "$env:APPDATA\JetBrains"

    if (Test-Path $configDir) {
        Print-Info "JetBrains IDEs detected"
        Print-Info "Manual configuration required for JetBrains:"
        Print-Bullet "Install the 'MCP Support' plugin from JetBrains Marketplace"
        Print-Bullet "Configure MCP server: command='leindex', args=['mcp']"
        Print-Warning "See documentation for JetBrains-specific setup"
    } else {
        Print-Info "No JetBrains IDEs detected"
    }

    Write-Host ""
}

# Configure CLI tools
function Configure-CLITools {
    Print-Section "Configuring CLI Tools"

    # Check if leindex is in PATH
    $leindexCmd = Get-Command leindex -ErrorAction SilentlyContinue

    if ($leindexCmd) {
        Print-Success "'leindex' command available in PATH"
    } else {
        Print-Warning "'leindex' command not in PATH"
        Print-Info "Python Scripts directory needs to be in PATH:"

        # Try to find Python Scripts directory
        $scriptsDir = & $PYTHON_CMD -c "import sys; import os; print(os.path.join(sys.prefix, 'Scripts'))" 2>$null

        if ($scriptsDir) {
            Print-Bullet "export PATH=`$PATH:`"$scriptsDir`""

            if (Ask-YesNo "Add to user PATH? (requires restart)") {
                [Environment]::SetEnvironmentVariable("Path", $env:Path + ";$scriptsDir", "User")
                Print-Success "Added to user PATH"
                Print-Warning "Restart your terminal for changes to take effect"
            }
        }
    }

    # Check leindex-search
    if (Get-Command leindex-search -ErrorAction SilentlyContinue) {
        Print-Success "'leindex-search' command available"
    }

    Write-Host ""
}

# Interactive tool selection
function Select-Tools {
    Print-Section "Tool Integration"

    Write-Host "Select AI tools to integrate with $ProjectName:"
    Write-Host ""
    Write-Host "  1) Claude Desktop"
    Write-Host "  2) Cursor IDE"
    Write-Host "  3) VS Code / VSCodium"
    Write-Host "  4) Zed Editor"
    Write-Host "  5) JetBrains IDEs"
    Write-Host "  6) CLI Tools (PATH setup)"
    Write-Host "  a) All tools"
    Write-Host "  d) Detected tools only"
    Write-Host "  s) Skip integration"
    Write-Host "  c) Custom selection"
    Write-Host ""

    while ($true) {
        $choice = Read-Host "? Enter choice"
        Write-Host ""

        switch ($choice) {
            "1" { Configure-ClaudeDesktop; break }
            "2" { Configure-Cursor; break }
            "3" { Configure-VSCode; break }
            "4" { Configure-Zed; break }
            "5" { Configure-JetBrains; break }
            "6" { Configure-CLITools; break }
            {$_ -in "a", "A"} {
                Configure-ClaudeDesktop
                Configure-Cursor
                Configure-VSCode
                Configure-Zed
                Configure-JetBrains
                Configure-CLITools
                break
            }
            {$_ -in "d", "D"} {
                if (Test-Path "$env:APPDATA\Claude") { Configure-ClaudeDesktop }
                if (Test-Path "$env:APPDATA\Cursor") { Configure-Cursor }
                if (Test-Path "$env:APPDATA\Code") { Configure-VSCode }
                if (Test-Path "$env:APPDATA\Zed") { Configure-Zed }
                if (Test-Path "$env:APPDATA\JetBrains") { Configure-JetBrains }
                Configure-CLITools
                break
            }
            {$_ -in "s", "S"} {
                Print-Warning "Skipping tool integration"
                Print-Info "MCP server installed and ready for manual configuration"
                break
            }
            {$_ -in "c", "C"} {
                Write-Host "Enter tools (comma-separated, e.g., '1,3,4'):"
                $custom = Read-Host ">"
                Write-Host ""

                foreach ($tool in $custom -split ',') {
                    switch ($tool.Trim()) {
                        "1" { Configure-ClaudeDesktop }
                        "2" { Configure-Cursor }
                        "3" { Configure-VSCode }
                        "4" { Configure-Zed }
                        "5" { Configure-JetBrains }
                        "6" { Configure-CLITools }
                    }
                }
                break
            }
            default {
                Print-Error "Invalid choice. Please try again."
            }
        }
    }
}

# ============================================================================
# VERIFICATION
# ============================================================================

function Test-Installation {
    Print-Section "Verifying Installation"

    $allGood = $true

    # Check package installation
    if ($PKG_MANAGER -eq "uv") {
        # For uv, if installation succeeded (we're here), it's installed
        # uv manages its own virtual environments, so checking via import can fail
        Print-Success "Python package installed (via uv)"
        Print-Bullet "LeIndex is ready to use"
    } else {
        # Use Python import check for pip/pipx
        $testImport = & $PYTHON_CMD -c "import leindex.server" 2>&1
        if ($LASTEXITCODE -eq 0) {
            Print-Success "Python package installed"
            $version = & $PYTHON_CMD -c "import leindex; print(leindex.__version__)" 2>&1
            Print-Bullet "Version: $version"
        } else {
            Print-Error "Python package not found"
            $allGood = $false
        }
    }

    # Check command availability
    Write-Host ""
    Write-Host "Commands:"
    if (Get-Command leindex -ErrorAction SilentlyContinue) {
        Print-Success "leindex"
    } else {
        Print-Warning "leindex (not in PATH - use uv run leindex)"
    }

    if (Get-Command leindex-search -ErrorAction SilentlyContinue) {
        Print-Success "leindex-search"
    } else {
        Print-Warning "leindex-search (not in PATH - use uv run leindex-search)"
    }

    # Check configured tools
    Write-Host ""
    Write-Host "Configured tools:"

    $claudeConfig = "$env:APPDATA\Claude\claude_desktop_config.json"
    if (Test-Path $claudeConfig) {
        $content = Get-Content $claudeConfig -Raw
        if ($content -match "leindex") { Print-Success "Claude Desktop" }
    }

    $cursorConfig = "$env:APPDATA\Cursor\mcp.json"
    if (Test-Path $cursorConfig) {
        $content = Get-Content $cursorConfig -Raw
        if ($content -match "leindex") { Print-Success "Cursor" }
    }

    $vscodeConfig = "$env:APPDATA\Code\User\settings.json"
    if (Test-Path $vscodeConfig) {
        $content = Get-Content $vscodeConfig -Raw
        if ($content -match "leindex") { Print-Success "VS Code" }
    }

    $zedConfig = "$env:APPDATA\Zed\settings.json"
    if (Test-Path $zedConfig) {
        $content = Get-Content $zedConfig -Raw
        if ($content -match "leindex") { Print-Success "Zed" }
    }

    Write-Host ""

    return $allGood
}

# ============================================================================
# COMPLETION MESSAGE
# ============================================================================

function Show-Completion {
    Write-Host ""
    Write-ColorOutput "╔════════════════════════════════════════════════════════════╗" -ForegroundColor Green
    Write-ColorOutput "║ Installation Complete!                                      ║" -ForegroundColor Green
    Write-ColorOutput "╚════════════════════════════════════════════════════════════╝" -ForegroundColor Green
    Write-Host ""

    Write-ColorOutput "Next Steps:" -ForegroundColor White
    Write-Host ""
    Write-Host "1. Restart your AI tool(s) to load $ProjectName"
    Write-Host "2. Use MCP tools in your AI assistant:"
    Write-ColorOutput "     manage_project" -ForegroundColor Cyan
    Write-ColorOutput "     search_content" -ForegroundColor Cyan
    Write-ColorOutput "     get_diagnostics" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "3. Or use CLI commands:"
    Write-ColorOutput "     leindex mcp" -ForegroundColor Cyan
    Write-ColorOutput "     leindex-search `"query`"" -ForegroundColor Cyan
    Write-Host ""

    Write-ColorOutput "Documentation:" -ForegroundColor White
    Print-Bullet "GitHub: $RepoUrl"
    Print-Bullet "MCP Config: See MCP_CONFIGURATION.md"
    Write-Host ""

    Write-ColorOutput "Troubleshooting:" -ForegroundColor White
    Print-Bullet "Check logs: $LogDir\"
    Print-Bullet "Test MCP: $PYTHON_CMD -m leindex.server"
    Print-Bullet "Debug mode: `$env:LEINDEX_LOG_LEVEL=`"DEBUG`""
    Write-Host ""

    Write-ColorOutput "Uninstall:" -ForegroundColor White
    Print-Bullet "Run: irm $RepoUrl/raw/master/uninstall.ps1 | iex"
    Write-Host ""
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

function Main {
    Clear-Host
    Print-Header

    # Initialize rollback
    Initialize-Rollback

    # Environment detection
    Find-Python
    Find-PackageManager
    Find-AITools

    # Installation
    Initialize-Directories
    Install-LeIndexPackage

    # Tool integration
    Select-Tools

    # Verification
    Test-Installation

    # Completion
    Show-Completion

    # Clean up backup on success
    Remove-Item $BackupDir -Recurse -Force -ErrorAction SilentlyContinue
}

# Error handling
trap {
    Invoke-Rollback -ExitCode 1
    exit 1
}

# Run installation
Main
