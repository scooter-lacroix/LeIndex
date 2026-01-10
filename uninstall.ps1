#############################################
# LeIndex Uninstaller
# Version: 3.0.0
# Platform: Windows (PowerShell 5.1+)
#############################################

#Requires -Version 5.1

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ScriptVersion = "3.0.0"
$ProjectName = "LeIndex"
$ProjectSlug = "leindex"
$LEINDEX_HOME = if ($env:LEINDEX_HOME) { $env:LEINDEX_HOME } else { "$env:USERPROFILE\.leindex" }

# Color output
function Write-ColorOutput {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Message,
        [string]$ForegroundColor = "White"
    )
    $fc = $host.UI.RawUI.ForegroundColor
    $host.UI.RawUI.ForegroundColor = $ForegroundColor
    Write-Host $Message
    $host.UI.RawUI.ForegroundColor = $fc
}

function Print-Header {
    Write-ColorOutput "╔════════════════════════════════════════════════════════════╗" -Red
    Write-ColorOutput "║ Uninstalling $ProjectName v$ScriptVersion                   ║" -Red
    Write-ColorOutput "╚════════════════════════════════════════════════════════════╝" -Red
    Write-Host ""
}

function Print-Warning {
    param([string]$Message)
    Write-ColorOutput "⚠ $Message" -Yellow
}

function Print-Success {
    param([string]$Message)
    Write-ColorOutput "✓ $Message" -Green
}

function Print-Error {
    param([string]$Message)
    Write-ColorOutput "✗ $Message" -Red
}

function Print-Bullet {
    param([string]$Message)
    Write-ColorOutput "  • $Message" -Cyan
}

function Ask-YesNo {
    param(
        [string]$Prompt,
        [bool]$Default = $false
    )
    $defaultPrompt = if ($Default) { "[Y/n]" } else { "[y/N]" }
    while ($true) {
        $answer = Read-Host "? $Prompt $defaultPrompt"
        $answer = $answer.Trim()
        if ([string]::IsNullOrEmpty($answer)) { return $Default }
        switch ($answer.ToLower()) {
            {$_ -in "y", "yes"} { return $true }
            {$_ -in "n", "no"} { return $false }
            default { Write-Host "Please answer yes or no." }
        }
    }
}

function Remove-Package {
    Write-Host ""
    Write-ColorOutput ">>> Removing Python Package" -Blue
    Write-Host ""

    $pythonCmd = $null
    foreach ($cmd in @("python", "python3", "py")) {
        try {
            $result = & $cmd --version 2>&1
            if ($LASTEXITCODE -eq 0) {
                $pythonCmd = $cmd
                break
            }
        } catch {}
    }

    if (-not $pythonCmd) {
        Print-Warning "Python not found, skipping package removal"
        return
    }

    foreach ($pipCmd in @("pip", "pip3", "$pythonCmd -m pip")) {
        try {
            $result = & $pipCmd show $ProjectSlug 2>&1
            if ($LASTEXITCODE -eq 0) {
                Write-Host "Uninstalling from: $pipCmd"
                & $pipCmd uninstall -y $ProjectSlug 2>$null
                Print-Success "Package removed via $pipCmd"
                Write-Host ""
                return
            }
        } catch {}
    }

    Print-Warning "Package not found in any package manager"
    Write-Host ""
}

function Remove-McpConfigs {
    Write-ColorOutput ">>> Removing MCP Configurations" -Blue
    Write-Host ""

    $pythonCmd = $null
    foreach ($cmd in @("python", "python3", "py")) {
        try {
            & $cmd --version 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) {
                $pythonCmd = $cmd
                break
            }
        } catch {}
    }

    $configs = @(
        "$env:APPDATA\Claude\claude_desktop_config.json",
        "$env:APPDATA\Cursor\mcp.json",
        "$env:APPDATA\Code\User\settings.json",
        "$env:APPDATA\VSCodium\User\settings.json",
        "$env:APPDATA\Zed\settings.json"
    )

    foreach ($configFile in $configs) {
        if (Test-Path $configFile) {
            if ($pythonCmd) {
                $pythonScript = @"
import json

config_file = r'$configFile'

try:
    with open(config_file, 'r') as f:
        config = json.load(f)

	    if 'mcpServers' in config and 'leindex' in config['mcpServers']:
	        del config['mcpServers']['leindex']
	        if not config['mcpServers']:
	            del config['mcpServers']

	    if 'context_servers' in config and isinstance(config['context_servers'], dict) and 'leindex' in config['context_servers']:
	        del config['context_servers']['leindex']
	        if not config['context_servers']:
	            del config['context_servers']

	    if 'language_models' in config and isinstance(config['language_models'], dict):
	        mcp_servers = config['language_models'].get('mcp_servers')
	        if isinstance(mcp_servers, dict) and 'leindex' in mcp_servers:
	            del mcp_servers['leindex']
	            if not mcp_servers:
	                config['language_models'].pop('mcp_servers', None)
	        if not config['language_models']:
	            del config['language_models']

	    if 'lsp' in config and 'leindex' in config['lsp']:
	        del config['lsp']['leindex']
	        if not config['lsp']:
	            del config['lsp']

    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)
        f.write('\n')

except (FileNotFoundError, json.JSONDecodeError):
    pass
"@
                & $pythonCmd -c $pythonScript 2>$null
            }
        }
    }

    Print-Success "MCP configurations cleaned"
    Write-Host ""
}

function Remove-DataDirectory {
    Write-ColorOutput ">>> Removing Data Directory" -Blue
    Write-Host ""

    if (Test-Path $LEINDEX_HOME) {
        Print-Warning "This will delete all $ProjectName data:"
        Print-Bullet "Configuration files"
        Print-Bullet "Indexed data"
        Print-Bullet "Log files"
        Print-Bullet "Search indices"
        Write-Host ""

        if (Ask-YesNo "Remove $LEINDEX_HOME?") {
            Remove-Item $LEINDEX_HOME -Recurse -Force
            Print-Success "Data directory removed"
        } else {
            Print-Warning "Data directory preserved"
        }
    } else {
        Write-ColorOutput "ℹ No data directory found" -Cyan
    }

    Write-Host ""
}

function Main {
    Clear-Host
    Print-Header

    Write-ColorOutput "WARNING: This will completely remove $ProjectName from your system." -Yellow
    Write-Host ""
    Write-Host "This uninstaller will:"
    Print-Bullet "Remove the Python package"
    Print-Bullet "Remove MCP server configurations"
    Print-Bullet "Optionally remove all data and indices"
    Print-Bullet "Clean shell configuration files"
    Write-Host ""

    if (-not (Ask-YesNo "Continue with uninstallation?")) {
        Write-Host "Aborted."
        exit 0
    }

    Write-Host ""

    Remove-Package
    Remove-McpConfigs
    Remove-DataDirectory

    Write-ColorOutput "╔════════════════════════════════════════════════════════════╗" -Green
    Write-ColorOutput "║ Uninstallation Complete!                                     ║" -Green
    Write-ColorOutput "╚════════════════════════════════════════════════════════════╝" -Green
    Write-Host ""

    Write-Host "Thank you for using $ProjectName!"
    Write-Host "We'd love to hear your feedback: https://github.com/scooter-lacroix/leindex/issues"
    Write-Host ""
}

Main
