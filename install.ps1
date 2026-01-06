#############################################
# LeIndex Universal Installer
# Version: 4.0.0 - Beautiful & Interactive
# Platform: Windows (PowerShell 5.1+)
#############################################

#Requires -Version 5.1

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ============================================================================
# CONFIGURATION
# ============================================================================
$ScriptVersion = "4.0.0"
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
# LOGGING SYSTEM
# ============================================================================

# Check for debug mode
if ($env:DEBUG -eq "1" -or $env:VERBOSE -eq "1") {
    $DebugPreference = "Continue"
    Set-PSDebug -Trace 1
}

# Create log file
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$InstallLog = "$LogDir\install-$(Get-Date -Format 'yyyyMMdd-HHmmss').log"

# Initialize log file
$timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
@"
=== LeIndex Installation Log ===
Date: $timestamp
Script Version: $ScriptVersion
Platform: Windows PowerShell $($PSVersionTable.PSVersion)
DEBUG Mode: $($env:DEBUG -eq '1')
================================
"@ | Out-File -FilePath $InstallLog -Encoding UTF8

# Logging functions
function Log-Debug {
    param([string]$Message)
    $logMsg = "[DEBUG] $Message"
    Add-Content -Path $InstallLog -Value $logMsg
    if ($env:DEBUG -eq "1") {
        Write-Host $logMsg -ForegroundColor Gray
    }
}

function Log-Info {
    param([string]$Message)
    $logMsg = "[INFO] $Message"
    Add-Content -Path $InstallLog -Value $logMsg
    Write-Host $logMsg -ForegroundColor Cyan
}

function Log-Error {
    param([string]$Message)
    $logMsg = "[ERROR] $Message"
    Add-Content -Path $InstallLog -Value $logMsg
    Write-Host $logMsg -ForegroundColor Red
}

function Log-Warn {
    param([string]$Message)
    $logMsg = "[WARN] $Message"
    Add-Content -Path $InstallLog -Value $logMsg
    Write-Host $logMsg -ForegroundColor Yellow
}

function Log-Success {
    param([string]$Message)
    $logMsg = "[SUCCESS] $Message"
    Add-Content -Path $InstallLog -Value $logMsg
    Write-Host $logMsg -ForegroundColor Green
}

function Log-Command {
    param([string]$Command)
    $logMsg = "[CMD] $Command"
    Add-Content -Path $InstallLog -Value $logMsg
}

# Total steps for progress tracking
$TotalSteps = 7

# ============================================================================
# HELPER FUNCTIONS
# ============================================================================

# Write colored output with better styling
function Write-ColorText {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Message,
        [ValidateSet("Black", "DarkBlue", "DarkGreen", "DarkCyan", "DarkRed", "DarkMagenta", "DarkYellow",
                     "Gray", "DarkGray", "Blue", "Green", "Cyan", "Red", "Magenta", "Yellow", "White")]
        [string]$ForegroundColor = "White",
        [switch]$NoNewline
    )

    Write-Host $Message -ForegroundColor $ForegroundColor -NoNewline:$NoNewline
}

# Print styled header with box drawing characters
function Print-Header {
    $width = 70
    Clear-Host
    Write-ColorText "╔" -ForegroundColor Cyan
    Write-ColorText ("═" * $width) -ForegroundColor Cyan -NoNewline
    Write-ColorText "╗" -ForegroundColor Cyan

    $title = "  🚀 $ProjectName Installer v$ScriptVersion"
    $padding = " " * ($width - $title.Length - 2)
    Write-ColorText "║" -ForegroundColor Cyan -NoNewline
    Write-ColorText "$title$padding " -ForegroundColor Cyan -NoNewline
    Write-ColorText "║" -ForegroundColor Cyan

    $subtitle = "  ✨ AI-Powered Code Search & MCP Server"
    $padding = " " * ($width - $subtitle.Length - 2)
    Write-ColorText "║" -ForegroundColor Cyan -NoNewline
    Write-ColorText "$subtitle$padding " -ForegroundColor Cyan -NoNewline
    Write-ColorText "║" -ForegroundColor Cyan

    Write-ColorText "╚" -ForegroundColor Cyan -NoNewline
    Write-ColorText ("═" * $width) -ForegroundColor Cyan -NoNewline
    Write-ColorText "╝" -ForegroundColor Cyan
    Write-Host ""
}

# Print welcome message
function Print-Welcome {
    Write-ColorText "Welcome to the future of code search!" -ForegroundColor Cyan
    Write-Host ""
    Write-ColorText "Let's get you set up with LeIndex in just a few moments." -ForegroundColor Gray
    Write-ColorText "This installer will:" -ForegroundColor Gray
    Write-Host ""
    Write-ColorText "  ✓ Detect your Python environment" -ForegroundColor Green
    Write-ColorText "  ✓ Find your AI coding tools" -ForegroundColor Green
    Write-ColorText "  ✓ Install LeIndex with the best package manager" -ForegroundColor Green
    Write-ColorText "  ✓ Configure integrations with your favorite tools" -ForegroundColor Green
    Write-Host ""

    if (Ask-YesNo "Ready to begin?" $true) {
        return
    } else {
        Write-ColorText "ℹ Installation cancelled by user" -ForegroundColor Cyan
        exit 0
    }
}

# Print section header
function Print-Section {
    param([string]$Title)
    Write-Host ""
    Write-ColorText "┌─ $Title ─" -ForegroundColor Blue
    Write-ColorText "│" -ForegroundColor Blue
}

# Print success message with emoji
function Print-Success {
    param([string]$Message)
    Write-ColorText "✓ $Message" -ForegroundColor Green
}

# Print warning message with emoji
function Print-Warning {
    param([string]$Message)
    Write-ColorText "⚠ $Message" -ForegroundColor Yellow
}

# Print error message with emoji
function Print-Error {
    param([string]$Message)
    Write-ColorText "✗ $Message" -ForegroundColor Red
}

# Print info message with emoji
function Print-Info {
    param([string]$Message)
    Write-ColorText "ℹ $Message" -ForegroundColor Cyan
}

# Print bullet point
function Print-Bullet {
    param([string]$Message)
    Write-ColorText "  • $Message" -ForegroundColor Cyan
}

# Print step indicator
function Print-Step {
    param(
        [int]$Step,
        [string]$Description
    )
    Write-ColorText "[$Step/$TotalSteps] $Description" -ForegroundColor Magenta
}

# Ask yes/no question with better styling
function Ask-YesNo {
    param(
        [string]$Prompt,
        [bool]$Default = $false
    )

    $defaultPrompt = if ($Default) { "[Y/n]" } else { "[y/N]" }
    $defaultColor = if ($Default) { "Green" } else { "Yellow" }

    while ($true) {
        Write-Host ""
        Write-ColorText "? $Prompt " -ForegroundColor Yellow -NoNewline
        Write-ColorText $defaultPrompt -ForegroundColor $defaultColor -NoNewline
        Write-Host " " -NoNewline

        $answer = Read-Host
        $answer = $answer.Trim()

        if ([string]::IsNullOrEmpty($answer)) {
            Write-Host ""
            return $Default
        }

        switch ($answer.ToLower()) {
            {$_ -in "y", "yes"}
                { Write-Host ""; return $true }
            {$_ -in "n", "no"}
                { Write-Host ""; return $false }
            default
                { Write-ColorText "Please answer yes or no." -ForegroundColor Red }
        }
    }
}

# Ask for a choice from a list
function Ask-Choice {
    param(
        [string]$Prompt,
        [string[]]$Options
    )

    Write-Host ""
    Write-ColorText $Prompt -ForegroundColor Cyan
    Write-Host ""

    for ($i = 0; $i -lt $Options.Count; $i++) {
        Write-ColorText ("  {0,2}) {1}" -f ($i + 1), $Options[$i]) -ForegroundColor White
    }
    Write-Host ""

    while ($true) {
        Write-ColorText "# Enter choice [1-$($Options.Count)]: " -ForegroundColor Yellow -NoNewline
        $choice = Read-Host
        Write-Host ""

        if ($choice -match '^\d+$' -and [int]$choice -ge 1 -and [int]$choice -le $Options.Count) {
            return [int]$choice - 1
        } else {
            Print-Error "Invalid choice. Please enter a number between 1 and $($Options.Count)"
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
        Remove-Item $BackupDir -Recurse -Force -ErrorAction SilentlyContinue
        return
    }

    Write-Host ""
    Print-Error "Installation failed. Rolling back changes..."
    Write-Host ""

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
    Print-Step 1 "Detecting Python Environment"

    $pythonCmds = @("python3.13", "python3.12", "python3.11", "python3.10", "python", "python3", "py")
    $PYTHON_CMD = $null

    foreach ($cmd in $pythonCmds) {
        try {
            $result = & $cmd --version 2>&1
            if ($LASTEXITCODE -eq 0 -and $result -match "Python (\d+)\.(\d+)") {
                $major = [int]$matches[1]
                $minor = [int]$matches[2]

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
        Print-Error "Python 3.10-3.13 not found on your system"
        Write-Host ""
        Write-ColorText "Please install Python 3.10-3.13:" -ForegroundColor White
        Print-Bullet "Download from: https://www.python.org/downloads/"
        Print-Bullet "During installation, check 'Add Python to PATH'"
        Print-Bullet "Or use: winget install Python.Python.3.13"
        Print-Bullet "NOTE: Python 3.14+ is not supported (leann-backend-hnsw compatibility)"
        exit 1
    }

    $script:PYTHON_CMD = $PYTHON_CMD
    Print-Success "Python $version detected"
    Print-Bullet "Using: $PYTHON_CMD"
}

# Detect package manager
function Find-PackageManager {
    Print-Step 2 "Detecting Package Manager"

    if (Get-Command uv -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "uv"
        $script:PKG_INSTALL_CMD = "uv pip install"
        Print-Success "uv detected (⚡ fastest package manager)"
        return
    }

    if (Get-Command pip -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "pip install"
        Print-Success "pip detected"
        return
    }

    if (Get-Command pip3 -ErrorAction SilentlyContinue) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "pip3 install"
        Print-Success "pip3 detected"
        return
    }

    $testPip = & $PYTHON_CMD -m pip --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $script:PKG_MANAGER = "pip"
        $script:PKG_INSTALL_CMD = "$PYTHON_CMD -m pip install"
        Print-Success "pip (via Python module) detected"
        return
    }

    Print-Error "No package manager found"
    Write-Host ""
    Write-ColorText "Install a package manager:" -ForegroundColor White
    Print-Bullet "Install pip: $PYTHON_CMD -m ensurepip --upgrade"
    Print-Bullet "Or install uv: powershell -c `"irm https://astral.sh/uv/install.ps1 | iex`""
    exit 1
}

# Get display name for tool
function Get-ToolDisplayName {
    param([string]$ToolId)

    switch ($ToolId) {
        # CLI Tools
        "claude-cli" { return "Claude CLI" }
        "codex-cli" { return "Codex CLI" }
        "amp-code" { return "Amp Code" }
        "opencode" { return "OpenCode" }
        "qwen-cli" { return "Qwen CLI" }
        "kilocode-cli" { return "Kilocode CLI" }
        "goose-cli" { return "Goose CLI" }
        "iflow-cli" { return "iFlow CLI" }
        "droid-cli" { return "Droid CLI" }
        "gemini-cli" { return "Gemini CLI" }
        "aider" { return "Aider" }
        "mistral-cli" { return "Mistral CLI" }
        "gpt-cli" { return "GPT CLI" }
        "cursor-cli" { return "Cursor CLI" }
        "pliny-cli" { return "Pliny CLI" }
        "continue-cli" { return "Continue CLI" }
        # Editors/IDEs
        "cursor" { return "Cursor IDE" }
        "antigravity" { return "Antigravity" }
        "zed" { return "Zed Editor" }
        "vscode" { return "VS Code" }
        "vscodium" { return "VSCodium" }
        "jetbrains" { return "JetBrains IDEs" }
        "windsurf" { return "Windsurf" }
        "continue" { return "Continue" }
        "claude-desktop" { return "Claude Desktop" }
        default { return $ToolId }
    }
}

# Detect installed AI tools
function Find-AITools {
    Print-Step 3 "Detecting AI Coding Tools"

    $detectedEditors = @()
    $detectedCLIs = @()

    # Common binary locations for explicit checking (Windows)
    $commonPaths = @(
        "$env:LOCALAPPDATA\Programs",
        "C:\Program Files",
        "C:\Program Files (x86)",
        "$env:USERPROFILE\.local\bin",
        "$env:USERPROFILE\bin",
        "$env:APPDATA\Python\Scripts",
        "$env:LOCALAPPDATA\Microsoft\WindowsApps"
    )

    # Helper function to check for executable
    function Test-CommandExists {
        param([string]$Command)

        # Check PATH first using Get-Command
        if (Get-Command $Command -ErrorAction SilentlyContinue) {
            return $true
        }

        # Check common paths for executable
        foreach ($path in $commonPaths) {
            if (Test-Path "$path\$Command.exe") { return $true }
            if (Test-Path "$path\$Command.cmd") { return $true }
            if (Test-Path "$path\$Command.bat") { return $true }
        }

        return $false
    }

    # ============================================================================
    # EDITORS / IDEs - Check config directories (Windows paths)
    # ============================================================================

    # Claude Desktop
    if (Test-Path "$env:APPDATA\Claude") { $detectedEditors += "claude-desktop" }

    # Cursor IDE
    if (Test-Path "$env:APPDATA\Cursor") { $detectedEditors += "cursor" }

    # Antigravity
    if (Test-Path "$env:APPDATA\Antigravity") { $detectedEditors += "antigravity" }

    # VS Code / VSCodium
    if (Test-Path "$env:APPDATA\Code") { $detectedEditors += "vscode" }
    if (Test-Path "$env:APPDATA\VSCodium") { $detectedEditors += "vscodium" }

    # Zed Editor
    if (Test-Path "$env:APPDATA\Zed") { $detectedEditors += "zed" }

    # JetBrains IDEs
    if (Test-Path "$env:APPDATA\JetBrains") { $detectedEditors += "jetbrains" }

    # Windsurf
    if (Test-Path "$env:APPDATA\windsurf") { $detectedEditors += "windsurf" }

    # Continue
    if (Test-Path "$env:APPDATA\continue") { $detectedEditors += "continue" }

    # ============================================================================
    # CLI TOOLS - Check executables (with alternative names)
    # ============================================================================

    # Claude CLI
    if (Test-CommandExists "claude") { $detectedCLIs += "claude-cli" }

    # Codex CLI
    if (Test-CommandExists "codex") { $detectedCLIs += "codex-cli" }

    # Amp Code (amp or amp-code)
    if (Test-CommandExists "amp") { $detectedCLIs += "amp-code" }
    if (Test-CommandExists "amp-code") { $detectedCLIs += "amp-code" }

    # OpenCode
    if (Test-CommandExists "opencode") { $detectedCLIs += "opencode" }

    # Qwen CLI (qwen or qwen-cli)
    if (Test-CommandExists "qwen") { $detectedCLIs += "qwen-cli" }
    if (Test-CommandExists "qwen-cli") { $detectedCLIs += "qwen-cli" }

    # Kilocode CLI (kilocode or kilocode-cli)
    if (Test-CommandExists "kilocode") { $detectedCLIs += "kilocode-cli" }
    if (Test-CommandExists "kilocode-cli") { $detectedCLIs += "kilocode-cli" }

    # Goose CLI
    if (Test-CommandExists "goose") { $detectedCLIs += "goose-cli" }

    # iFlow CLI
    if (Test-CommandExists "iflow") { $detectedCLIs += "iflow-cli" }

    # Droid CLI
    if (Test-CommandExists "droid") { $detectedCLIs += "droid-cli" }

    # Gemini CLI
    if (Test-CommandExists "gemini") { $detectedCLIs += "gemini-cli" }

    # Aider
    if (Test-CommandExists "aider") { $detectedCLIs += "aider" }

    # Mistral CLI
    if (Test-CommandExists "mistral") { $detectedCLIs += "mistral-cli" }

    # GPT CLI (gpt or gpt-cli)
    if (Test-CommandExists "gpt") { $detectedCLIs += "gpt-cli" }
    if (Test-CommandExists "gpt-cli") { $detectedCLIs += "gpt-cli" }

    # Cursor CLI
    if (Test-CommandExists "cursor-cli") { $detectedCLIs += "cursor-cli" }

    # Pliny CLI
    if (Test-CommandExists "pliny") { $detectedCLIs += "pliny-cli" }

    # Continue CLI
    if (Test-CommandExists "continue-cli") { $detectedCLIs += "continue-cli" }

    # ============================================================================
    # DISPLAY RESULTS
    # ============================================================================

    $totalCount = $detectedEditors.Count + $detectedCLIs.Count

    if ($totalCount -gt 0) {
        Print-Success "Great news! Found $totalCount AI tool(s) on your system:"

        # Display Editors & IDEs
        if ($detectedEditors.Count -gt 0) {
            Write-Host ""
            Write-ColorText "Editors & IDEs:" -ForegroundColor Cyan
            foreach ($tool in $detectedEditors) {
                Print-Bullet (Get-ToolDisplayName $tool)
            }
        }

        # Display CLI Tools
        if ($detectedCLIs.Count -gt 0) {
            Write-Host ""
            Write-ColorText "CLI Tools:" -ForegroundColor Cyan
            foreach ($tool in $detectedCLIs) {
                Print-Bullet (Get-ToolDisplayName $tool)
            }
        }
    } else {
        Print-Warning "No AI tools detected"
        Print-Info "That's okay! We'll install LeIndex as a standalone MCP server"
    }
}

# ============================================================================
# INSTALLATION
# ============================================================================

# Install LeIndex package
function Install-LeIndexPackage {
    Print-Step 4 "Installing LeIndex"

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

    # Force reinstall to ensure new version is used (fixes old elasticsearch import issue)
    Print-Info "Removing old $PypiPackage installation (if present)..."
    try {
        if ($PKG_MANAGER -eq "uv") {
            uv pip uninstall $PypiPackage -y 2>$null | Out-Null
        } else {
            & $PYTHON_CMD -m pip uninstall $PypiPackage -y 2>$null | Out-Null
        }
    } catch {
        # Uninstall may fail if not installed, that's okay
    }

    Print-Info "Installing fresh $PypiPackage..."
    $installArgs = $PKG_INSTALL_CMD.Split(" ")
    & $installArgs[0] $installArgs[1..($installArgs.Length - 1)] $PypiPackage

    if ($LASTEXITCODE -ne 0) {
        Print-Error "Failed to install $PypiPackage"
        exit 1
    }

    Print-Success "$ProjectName installed successfully"

    if ($PKG_MANAGER -eq "uv") {
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
}

# Setup directory structure
function Initialize-Directories {
    Print-Step 5 "Setting up Directories"

    $dirs = @($ConfigDir, $DataDir, $LogDir)
    foreach ($dir in $dirs) {
        if (-not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force | Out-Null
            Print-Success "Created: $dir"
        }
    }
}

# ============================================================================
# TOOL INTEGRATION
# ============================================================================

# Merge JSON configuration for Claude Desktop/Cursor (no disabled/env fields)
function Merge-JsonConfig {
    param(
        [string]$ConfigFile,
        [string]$ServerName = "leindex",
        [string]$ServerCommand = "leindex"
    )

    Log-Info "Merge-JsonConfig called with: ConfigFile=$ConfigFile, ServerName=$ServerName, Command=$ServerCommand"
    Log-Debug "Config file path: $ConfigFile"

    $pythonScript = @"
import json
import sys
import os

config_file = r'$ConfigFile'
server_name = '$ServerName'
server_command = '$ServerCommand'

print(f"[DEBUG] Config file: {config_file}", file=sys.stderr)
print(f"[DEBUG] Server name: {server_name}", file=sys.stderr)
print(f"[DEBUG] Server command: {server_command}", file=sys.stderr)

# Ensure directory exists
config_dir = os.path.dirname(config_file)
if config_dir and not os.path.exists(config_dir):
    try:
        os.makedirs(config_dir, exist_ok=True)
        print(f"[INFO] Created directory: {config_dir}", file=sys.stderr)
    except Exception as e:
        print(f"[ERROR] Error creating directory {config_dir}: {e}", file=sys.stderr)
        sys.exit(1)

# Validate existing config
try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            try:
                config = json.loads(existing_content)
                print(f"[INFO] Loaded existing config from {config_file}", file=sys.stderr)
            except json.JSONDecodeError as e:
                print(f"[WARN] Existing config is invalid JSON: {e}", file=sys.stderr)
                print(f"[INFO] Backing up invalid config and creating new one.", file=sys.stderr)
                config = {}
        else:
            print(f"[INFO] Config file exists but is empty", file=sys.stderr)
            config = {}
except FileNotFoundError:
    config = {}
    print(f"[INFO] Config file not found, will create: {config_file}", file=sys.stderr)
except Exception as e:
    print(f"[ERROR] Error reading config: {e}", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    config = {}

if 'mcpServers' not in config:
    print(f"[INFO] Creating mcpServers section", file=sys.stderr)
    config['mcpServers'] = {}

# Check if server already exists
if server_name in config.get('mcpServers', {}):
    existing_config = config['mcpServers'][server_name]
    print(f"[INFO] Server '{server_name}' already configured.", file=sys.stderr)
    print(f"[DEBUG] Existing config: {existing_config}", file=sys.stderr)

# Claude Desktop/Cursor do NOT support 'disabled' or 'env' fields
new_server_config = {
    'command': server_command,
    'args': ['mcp']
}
print(f"[DEBUG] New server config: {new_server_config}", file=sys.stderr)
config['mcpServers'][server_name] = new_server_config

# Validate config can be serialized
try:
    json.dumps(config)
    print(f"[DEBUG] Config is valid JSON", file=sys.stderr)
except (TypeError, ValueError) as e:
    print(f"[ERROR] Invalid configuration structure: {e}", file=sys.stderr)
    sys.exit(1)

# Write to file with explicit error handling
try:
    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')
    print(f"[SUCCESS] Successfully wrote: {config_file}", file=sys.stderr)
    print(f"[DEBUG] Final config: {json.dumps(config, indent=2)}", file=sys.stderr)
except Exception as e:
    print(f"[ERROR] Error writing config file {config_file}: {e}", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    sys.exit(1)

print(f"Updated: {config_file}")
"@

    Log-Debug "Starting Python script execution..."
    Log-Command "& `$PYTHON_CMD -c `<JSON merge script>`"

    $output = & $PYTHON_CMD -c $pythonScript 2>&1
    $exitCode = $LASTEXITCODE

    Log-Debug "Python script exit code: $exitCode"
    Add-Content -Path $InstallLog -Value $output

    if ($exitCode -ne 0) {
        Log-Error "Python script failed with exit code $exitCode"
        Log-Error "Python output:"
        $output | ForEach-Object { Log-Error "  $_" }
        throw "Python script failed with exit code $exitCode"
    }

    Log-Debug "Python script output:"
    $output | ForEach-Object { Log-Debug "  $_" }

    Log-Success "Successfully merged JSON config to $ConfigFile"
}

# Configure Claude Desktop
function Configure-ClaudeDesktop {
    Print-Section "Configuring Claude Desktop"
    Log-Info "Starting Claude Desktop configuration"

    # Search for Claude Desktop config (NOT Claude Code CLI!)
    $claudeConfigs = @(
        "$env:APPDATA\Claude\claude_desktop_config.json",
        "$env:USERPROFILE\.config\claude\claude_desktop_config.json",
        "$env:USERPROFILE\.config\Claude\claude_desktop_config.json"
    )

    $configFile = $null
    $configDir = $null

    Log-Info "Searching for Claude Desktop config..."
    foreach ($conf in $claudeConfigs) {
        Log-Debug "Checking for config at: $conf"
        if (Test-Path $conf) {
            $configFile = $conf
            $configDir = Split-Path $conf -Parent
            Log-Info "Found existing config at: $configFile"
            Print-Bullet "Found config at: $configFile"
            break
        }
    }

    # If no existing config found, create in default Windows location
    if (-not $configFile) {
        $configDir = "$env:APPDATA\Claude"
        $configFile = "$configDir\claude_desktop_config.json"
        Log-Info "No existing config found. Will create: $configFile"
        Print-Info "No existing config found. Will create: $configFile"
    }

    Log-Debug "Config directory: $configDir"
    Log-Debug "Config file: $configFile"

    try {
        Log-Debug "Creating config directory if needed..."
        New-Item -ItemType Directory -Path $configDir -Force -ErrorAction Stop | Out-Null
        Log-Success "Created/confirmed config directory: $configDir"

        Log-Debug "Backing up existing config file..."
        Backup-FileSafe $configFile
        Log-Debug "Backup completed"

        Log-Info "Calling Merge-JsonConfig for Claude Desktop..."
        Merge-JsonConfig $configFile "leindex" "leindex"
        Log-Success "Claude Desktop configured successfully"
        Print-Success "Claude Desktop configured"
        Print-Bullet "Config: $configFile"
    } catch {
        Log-Error "Failed to configure Claude Desktop: $_"
        Log-Error "Exception: $($_.Exception.Message)"
        Log-Error "Stack Trace: $($_.ScriptStackTrace)"
        Print-Error "Failed to configure Claude Desktop: $_"
        Print-Info "Check log file for details: $InstallLog"
        return 2
    }
}

# Configure Claude Code CLI (different from Claude Desktop!)
function Configure-ClaudeCLI {
    Print-Section "Configuring Claude Code CLI"

    # Claude Code CLI uses ~/.claude.json
    $configFile = "$env:USERPROFILE\.claude.json"
    $configDir = Split-Path $configFile -Parent

    if (Test-Path $configFile) {
        Print-Bullet "Found config at: $configFile"
    } else {
        Print-Info "No existing config found. Will create: $configFile"
    }

    try {
        New-Item -ItemType Directory -Path $configDir -Force -ErrorAction Stop | Out-Null
        Backup-FileSafe $configFile

        $pythonScript = @"
import json
import sys

config_file = r'$configFile'
server_name = 'leindex'
server_command = 'leindex'

try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            config = json.loads(existing_content)
        else:
            config = {}
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

if 'mcpServers' not in config:
    config['mcpServers'] = {}

config['mcpServers'][server_name] = {
    'command': server_command,
    'args': ['mcp']
}

with open(config_file, 'w') as f:
    json.dump(config, f, indent=2)
    f.write('\n')

print(f"Updated: {config_file}")
"@

        & $PYTHON_CMD -c $pythonScript
        if ($LASTEXITCODE -eq 0) {
            Print-Success "Claude Code CLI configured"
            Print-Bullet "Config: $configFile"
        } else {
            throw "Python script failed"
        }
    } catch {
        Print-Warning "Failed to configure Claude Code CLI: $_"
        return 2
    }
}

# Configure Cursor IDE
function Configure-Cursor {
    Print-Section "Configuring Cursor IDE"

    # Search for Cursor config in multiple locations (system-agnostic)
    $cursorConfigs = @(
        "$env:APPDATA\Cursor\mcp.json",  # Primary Windows location
        "$env:USERPROFILE\.cursor\mcp.json",
        "$env:USERPROFILE\.claude.json",  # Some setups use this for Cursor too
        "$env:USERPROFILE\.config\cursor\mcp.json"
    )

    $configFile = $null
    $configDir = $null

    foreach ($conf in $cursorConfigs) {
        if (Test-Path $conf) {
            $configFile = $conf
            $configDir = Split-Path $conf -Parent
            Print-Bullet "Found config at: $configFile"
            break
        }
    }

    # If no existing config found, create in default Windows location
    if (-not $configFile) {
        $configDir = "$env:APPDATA\Cursor"
        $configFile = "$configDir\mcp.json"
        Print-Info "No existing config found. Will create: $configFile"
    }

    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Backup-FileSafe $configFile

    Merge-JsonConfig $configFile "leindex" "leindex"

    Print-Success "Cursor configured"
    Print-Bullet "Config: $configFile"
}

# Merge JSON configuration for VS Code (extension-specific)
function Merge-VSCodeConfig {
    param(
        [string]$ConfigFile,
        [string]$ServerName = "leindex",
        [string]$ServerCommand = "leindex",
        [string]$ExtensionKey = "cline.mcpServers"
    )

    $pythonScript = @"
import json
import sys
import os

config_file = r'$ConfigFile'
server_name = '$ServerName'
server_command = '$ServerCommand'
extension_key = '$ExtensionKey'

# Ensure directory exists
config_dir = os.path.dirname(config_file)
if config_dir and not os.path.exists(config_dir):
    try:
        os.makedirs(config_dir, exist_ok=True)
        print(f"Created directory: {config_dir}", file=sys.stderr)
    except Exception as e:
        print(f"Error creating directory {config_dir}: {e}", file=sys.stderr)
        sys.exit(1)

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
except FileNotFoundError:
    config = {}
    print(f"Config file not found, will create: {config_file}", file=sys.stderr)
except Exception as e:
    print(f"Error reading config: {e}", file=sys.stderr)
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

# Write to file with explicit error handling
try:
    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')
    print(f"Successfully wrote: {config_file}", file=sys.stderr)
except Exception as e:
    print(f"Error writing config file {config_file}: {e}", file=sys.stderr)
    sys.exit(1)

print(f"Updated: {config_file}")
"@

    & $PYTHON_CMD -c $pythonScript

    # Check exit code and throw on failure
    if ($LASTEXITCODE -ne 0) {
        throw "Python script failed with exit code $LASTEXITCODE"
    }
}

# Configure VS Code
function Configure-VSCode {
    param(
        [string]$Mode = "interactive"  # "interactive" or "auto"
    )

    Print-Section "Configuring VS Code Family"

    $extensionKey = "cline.mcpServers"
    $extensionName = "Cline"

    # Only ask for extension choice if not in auto mode
    if ($Mode -ne "auto") {
        # Ask which MCP extension the user uses
        Write-Host ""
        Write-ColorText "Which VS Code MCP extension are you using?" -ForegroundColor Cyan
        Write-Host ""

        $options = @(
            "Cline (saoudrizwan.claude)",
            "Continue (Continue.continue)",
            "Skip VS Code configuration"
        )

        $choice = Ask-Choice "Select your MCP extension:" $options

        Write-Host ""

        if ($choice -eq 2) {
            Print-Warning "Skipping VS Code configuration"
            return
        }

        if ($choice -eq 1) {
            $extensionKey = "continue.mcpServers"
            $extensionName = "Continue"
        }
    } else {
        Print-Info "Using Cline extension as default (most popular)"
        Write-Host ""
    }

    $vscodeConfigs = @(
        "$env:APPDATA\Code\User\settings.json",
        "$env:APPDATA\Code - Insiders\User\settings.json",
        "$env:APPDATA\VSCodium\User\settings.json"
    )

    foreach ($configFile in $vscodeConfigs) {
        $configDir = Split-Path $configFile -Parent

        if (Test-Path (Split-Path $configDir -Parent)) {
            New-Item -ItemType Directory -Path $configDir -Force | Out-Null
            Backup-FileSafe $configFile

            Merge-VSCodeConfig $configFile "leindex" "leindex" $extensionKey
            Print-Success "VS Code configured ($extensionName): $configFile"
        }
    }

    Print-Info "Note: Make sure you have the $extensionName extension installed"
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
import sys

config_file = r'$configFile'

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
"@

    & $PYTHON_CMD -c $pythonScript

    Print-Success "Zed Editor configured"
    Print-Bullet "Config: $configFile"
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
}

# Generic CLI tool MCP configuration
function Configure-CLIMCP {
    param(
        [string]$ToolName,
        [string]$ConfigFile,
        [string]$DisplayName
    )

    Print-Section "Configuring $DisplayName"

    $configDir = Split-Path $ConfigFile -Parent

    try {
        New-Item -ItemType Directory -Path $configDir -Force -ErrorAction Stop | Out-Null
        Backup-FileSafe $ConfigFile

        $pythonScript = @"
import json
import sys

config_file = r'$ConfigFile'

try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            config = json.loads(existing_content)
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
"@

        & $PYTHON_CMD -c $pythonScript
        if ($LASTEXITCODE -eq 0) {
            Print-Success "$DisplayName configured"
            Print-Bullet "Config: $ConfigFile"
        } else {
            throw "Python script failed"
        }
    } catch {
        Print-Warning "Failed to configure $DisplayName`: $_"
        return 2
    }
}

# Configure Antigravity IDE
function Configure-Antigravity {
    Print-Section "Configuring Antigravity IDE"

    $configDir = "$env:APPDATA\Antigravity"
    $configFile = "$configDir\mcp_config.json"

    try {
        New-Item -ItemType Directory -Path $configDir -Force -ErrorAction Stop | Out-Null
        Backup-FileSafe $configFile

        $pythonScript = @"
import json
import sys

config_file = r'$configFile'

try:
    with open(config_file, 'r') as f:
        existing_content = f.read()
        if existing_content.strip():
            config = json.loads(existing_content)
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
"@

        & $PYTHON_CMD -c $pythonScript
        if ($LASTEXITCODE -eq 0) {
            Print-Success "Antigravity configured"
            Print-Bullet "Config: $configFile"
        } else {
            throw "Python script failed"
        }
    } catch {
        Print-Warning "Failed to configure Antigravity: $_"
        return 2
    }
}

# Configure specific CLI tools
function Configure-CodexCLI {
    Configure-CLIMCP "codex" "$env:USERPROFILE\.codex\config.json" "Codex CLI"
}

function Configure-AmpCode {
    Configure-CLIMCP "amp" "$env:USERPROFILE\.amp\mcp_config.json" "Amp Code"
}

function Configure-OpenCode {
    Configure-CLIMCP "opencode" "$env:USERPROFILE\.opencode\mcp_config.json" "OpenCode"
}

function Configure-QwenCLI {
    Configure-CLIMCP "qwen" "$env:USERPROFILE\.qwen\mcp_config.json" "Qwen CLI"
}

function Configure-KilocodeCLI {
    Configure-CLIMCP "kilocode" "$env:USERPROFILE\.kilocode\mcp_settings.json" "Kilocode CLI"
}

function Configure-GooseCLI {
    Configure-CLIMCP "goose" "$env:USERPROFILE\.config\goose\mcp_config.json" "Goose CLI"
}

function Configure-iFlowCLI {
    Configure-CLIMCP "iflow" "$env:USERPROFILE\.iflow\mcp_config.json" "iFlow CLI"
}

function Configure-DroidCLI {
    Configure-CLIMCP "droid" "$env:USERPROFILE\.droid\mcp_config.json" "Droid CLI"
}

function Configure-GeminiCLI {
    Configure-CLIMCP "gemini" "$env:USERPROFILE\.gemini\mcp_config.json" "Gemini CLI"
}

# Configure CLI tools (PATH setup for leindex command)
function Configure-CLITools {
    Print-Section "Configuring CLI Tools"

    $leindexCmd = Get-Command leindex -ErrorAction SilentlyContinue

    if ($leindexCmd) {
        Print-Success "'leindex' command available in PATH"
    } else {
        Print-Warning "'leindex' command not in PATH"
        Print-Info "Python Scripts directory needs to be in PATH:"

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

    if (Get-Command leindex-search -ErrorAction SilentlyContinue) {
        Print-Success "'leindex-search' command available"
    }
}

# Interactive tool selection
function Select-Tools {
    Print-Step 6 "Tool Integration"

    Write-Host ""
    Write-ColorText "Which tools would you like LeIndex to integrate with?" -ForegroundColor Cyan
    Write-Host ""

    $options = @(
        "Claude Desktop",
        "Claude Code CLI",
        "Cursor IDE",
        "Antigravity IDE",
        "VS Code / VSCodium",
        "Zed Editor",
        "JetBrains IDEs",
        "Codex CLI",
        "Amp Code",
        "OpenCode",
        "Qwen CLI",
        "Kilocode CLI",
        "Goose CLI",
        "iFlow CLI",
        "Droid CLI",
        "Gemini CLI",
        "CLI Tools (PATH setup)",
        "All tools",
        "Detected tools only",
        "Skip integration",
        "Custom selection"
    )

    $choice = Ask-Choice "Select an option:" $options

    Write-Host ""

    switch ($choice) {
        0 { Configure-ClaudeDesktop }
        1 { Configure-ClaudeCLI }
        2 { Configure-Cursor }
        3 { Configure-Antigravity }
        4 { Configure-VSCode }
        5 { Configure-Zed }
        6 { Configure-JetBrains }
        7 { Configure-CodexCLI }
        8 { Configure-AmpCode }
        9 { Configure-OpenCode }
        10 { Configure-QwenCLI }
        11 { Configure-KilocodeCLI }
        12 { Configure-GooseCLI }
        13 { Configure-iFlowCLI }
        14 { Configure-DroidCLI }
        15 { Configure-GeminiCLI }
        16 { Configure-CLITools }
        17 {
            Configure-ClaudeDesktop
            Configure-ClaudeCLI
            Configure-Cursor
            Configure-Antigravity
            Configure-VSCode
            Configure-Zed
            Configure-JetBrains
            Configure-CodexCLI
            Configure-AmpCode
            Configure-OpenCode
            Configure-QwenCLI
            Configure-KilocodeCLI
            Configure-GooseCLI
            Configure-iFlowCLI
            Configure-DroidCLI
            Configure-GeminiCLI
            Configure-CLITools
        }
        18 {
            if (Test-Path "$env:APPDATA\Claude") { Configure-ClaudeDesktop }
            if (Test-Path "$env:USERPROFILE\.claude.json") { Configure-ClaudeCLI }
            if (Test-Path "$env:APPDATA\Cursor") { Configure-Cursor }
            if (Test-Path "$env:APPDATA\Antigravity") { Configure-Antigravity }
            if (Test-Path "$env:APPDATA\Code") { Configure-VSCode -Mode auto }
            if (Test-Path "$env:APPDATA\Zed") { Configure-Zed }
            if (Test-Path "$env:APPDATA\JetBrains") { Configure-JetBrains }
            if (Get-Command codex -ErrorAction SilentlyContinue) { Configure-CodexCLI }
            if (Get-Command amp -ErrorAction SilentlyContinue) { Configure-AmpCode }
            if (Get-Command opencode -ErrorAction SilentlyContinue) { Configure-OpenCode }
            if (Get-Command qwen -ErrorAction SilentlyContinue) { Configure-QwenCLI }
            if (Get-Command kilocode -ErrorAction SilentlyContinue) { Configure-KilocodeCLI }
            if (Get-Command goose -ErrorAction SilentlyContinue) { Configure-GooseCLI }
            if (Get-Command iflow -ErrorAction SilentlyContinue) { Configure-iFlowCLI }
            if (Get-Command droid -ErrorAction SilentlyContinue) { Configure-DroidCLI }
            if (Get-Command gemini -ErrorAction SilentlyContinue) { Configure-GeminiCLI }
            Configure-CLITools
        }
        19 {
            Print-Warning "Skipping tool integration"
            Print-Info "MCP server installed and ready for manual configuration"
        }
        20 {
            Write-Host ""
            Write-ColorText "Enter tools (comma-separated, e.g., '1,3,4'):" -ForegroundColor White
            $custom = Read-Host ">"
            Write-Host ""

            foreach ($tool in $custom -split ',') {
                switch ($tool.Trim()) {
                    "1" { Configure-ClaudeDesktop }
                    "2" { Configure-ClaudeCLI }
                    "3" { Configure-Cursor }
                    "4" { Configure-Antigravity }
                    "5" { Configure-VSCode }
                    "6" { Configure-Zed }
                    "7" { Configure-JetBrains }
                    "8" { Configure-CodexCLI }
                    "9" { Configure-AmpCode }
                    "10" { Configure-OpenCode }
                    "11" { Configure-QwenCLI }
                    "12" { Configure-KilocodeCLI }
                    "13" { Configure-GooseCLI }
                    "14" { Configure-iFlowCLI }
                    "15" { Configure-DroidCLI }
                    "16" { Configure-GeminiCLI }
                    "17" { Configure-CLITools }
                }
            }
        }
    }
}

# ============================================================================
# VERIFICATION
# ============================================================================

function Test-Installation {
    Print-Step 7 "Verifying Installation"

    $allGood = $true

    if ($PKG_MANAGER -eq "uv") {
        Print-Success "Python package installed (via uv)"
        Print-Bullet "LeIndex is ready to use"
    } else {
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

    Write-Host ""
    Write-ColorText "Commands:" -ForegroundColor White
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

    Write-Host ""
    Write-ColorText "Configured tools:" -ForegroundColor White

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

    return $allGood
}

# ============================================================================
# COMPLETION MESSAGE
# ============================================================================

function Show-Completion {
    Write-Host ""
    Write-ColorText "╔════════════════════════════════════════════════════════════╗" -ForegroundColor Green
    Write-ColorText "║  🎉 Installation Complete! 🎉                                ║" -ForegroundColor Green
    Write-ColorText "╚════════════════════════════════════════════════════════════╝" -ForegroundColor Green
    Write-Host ""

    Write-ColorText "What's next?" -ForegroundColor Cyan
    Write-Host ""
    Write-ColorText "1. Restart your AI tool(s) to load $ProjectName" -ForegroundColor White
    Write-ColorText "2. Use MCP tools in your AI assistant:" -ForegroundColor White
    Write-ColorText "    • manage_project - Index code repositories" -ForegroundColor Cyan
    Write-ColorText "    • search_content - Search code semantically" -ForegroundColor Cyan
    Write-ColorText "    • get_diagnostics - Get project statistics" -ForegroundColor Cyan
    Write-Host ""
    Write-ColorText "3. Or use CLI commands:" -ForegroundColor White
    Write-ColorText "    • leindex mcp - Start MCP server" -ForegroundColor Cyan
    Write-ColorText "    • leindex-search `"query`" - Search from terminal" -ForegroundColor Cyan
    Write-Host ""

    Write-ColorText "Resources:" -ForegroundColor Cyan
    Print-Bullet "GitHub: $RepoUrl"
    Print-Bullet "Documentation: See README.md"
    Write-Host ""

    Write-ColorText "Installation Log:" -ForegroundColor Yellow
    Print-Bullet "Log file: $InstallLog"
    Print-Bullet "To see all debug info, run: `$env:DEBUG = '1'; .\install.ps1"
    Write-Host ""

    Write-ColorText "Troubleshooting:" -ForegroundColor Yellow
    Print-Bullet "Check logs: $LogDir\"
    Print-Bullet "Test MCP: $PYTHON_CMD -m leindex.server"
    Print-Bullet "Debug mode: `$env:LEINDEX_LOG_LEVEL=`"DEBUG`""
    Write-Host ""

    Write-ColorText "Thanks for installing LeIndex! Happy coding! 🚀" -ForegroundColor Cyan
    Write-Host ""
}

# ============================================================================
# MAIN INSTALLATION FLOW
# ============================================================================

function Main {
    Print-Header
    Print-Welcome

    # Initialize rollback
    Initialize-Rollback

    # Environment detection
    Find-Python
    Find-PackageManager
    Find-AITools
    Write-Host ""

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

# Error handling - only rollback on critical failures (exit code 1)
# Tool configuration failures (exit code 2) should not trigger full rollback
trap {
    if ($LASTEXITCODE -eq 1) {
        Invoke-Rollback -ExitCode 1
        exit 1
    } elseif ($LASTEXITCODE -gt 1) {
        Write-Host ""
        Print-Warning "Some tool configurations failed, but LeIndex is installed"
        Print-Info "You can configure tools manually later"
        Write-Host ""
        Print-Warning "Installation log: $InstallLog"
        Print-Info "Run with DEBUG=1 for detailed output:"
        Print-Bullet "`$env:DEBUG = '1'; .\install.ps1"
        exit 0  # Don't fail the entire installation
    }
}

# Run installation
Main
