#############################################
# LeIndex Windows Installer
# Version: 1.5.2
# Platform: Windows PowerShell
#
# Installer:
#   iwr https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.ps1 -OutFile "$env:TEMP\install-leindex.ps1"
#   powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-leindex.ps1"
#
# Cargo install alternative:
#   cargo install leindex
#############################################

param(
    [switch]$Force = $false
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# ============================================================================
# CONFIGURATION
# ============================================================================
$ScriptVersion = "1.5.2"
$ProjectName = "LeIndex"
$ProjectSlug = "leindex"
$ExpectedVersion = "1.5.2"
$MinRustMajor = 1
$MinRustMinor = 75
$RepoUrl = "https://github.com/scooter-lacroix/LeIndex"
$RepoStarEndpoint = "user/starred/scooter-lacroix/LeIndex"

# Installation paths
$LEINDEX_HOME = if ($env:LEINDEX_HOME) { $env:LEINDEX_HOME } else { Join-Path $env:USERPROFILE ".leindex" }
$ConfigDir = Join-Path $LEINDEX_HOME "config"
$DataDir = Join-Path $LEINDEX_HOME "data"
$LogDir = Join-Path $LEINDEX_HOME "logs"
$CargoHomeDir = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$InstallBinDir = Join-Path $CargoHomeDir "bin"
$InstallBinaryPath = Join-Path $InstallBinDir "$ProjectSlug.exe"
$LegacyBinDir = Join-Path $LEINDEX_HOME "bin"
$LegacyBinaryPath = Join-Path $LegacyBinDir "$ProjectSlug.exe"
$StarMarkerPath = Join-Path $LEINDEX_HOME ".github-starred"

# ============================================================================
# LOGGING
# ============================================================================
$InstallLog = Join-Path $LogDir "install-$(Get-Date -Format 'yyyyMMdd-HHmmss').log"

function Ensure-Directory {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Ensure-LogDirectory {
    Ensure-Directory -Path $LogDir
}

function Write-Log {
    param([string]$Message, [string]$Level = "INFO")

    $timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    $logMessage = "[$timestamp] [$Level] $Message"
    Add-Content -Path $InstallLog -Value $logMessage
}

function Write-Info {
    param([string]$Message)

    Write-Log -Message $Message -Level "INFO"
    Write-Host $Message -ForegroundColor Cyan
}

function Write-Success {
    param([string]$Message)

    Write-Log -Message $Message -Level "SUCCESS"
    Write-Host $Message -ForegroundColor Green
}

function Write-ErrorLog {
    param([string]$Message)

    Write-Log -Message $Message -Level "ERROR"
    Write-Host $Message -ForegroundColor Red
}

function Write-WarnLog {
    param([string]$Message)

    Write-Log -Message $Message -Level "WARN"
    Write-Host $Message -ForegroundColor Yellow
}

function Write-Header {
    param([string]$Title)

    Write-Host ""
    Write-Host "==================================================" -ForegroundColor Cyan
    Write-Host "  $Title" -ForegroundColor Cyan
    Write-Host "==================================================" -ForegroundColor Cyan
    Write-Host ""
}

function Normalize-PathValue {
    param([string]$PathValue)

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return ""
    }

    try {
        return [System.IO.Path]::GetFullPath($PathValue).TrimEnd('\').ToLowerInvariant()
    } catch {
        return $PathValue.TrimEnd('\').ToLowerInvariant()
    }
}

function Refresh-ProcessPath {
    $userPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    $machinePath = [System.Environment]::GetEnvironmentVariable("Path", "Machine")

    $segments = @()
    if ($userPath) { $segments += $userPath }
    if ($machinePath) { $segments += $machinePath }

    $env:Path = ($segments -join ';')
}

# ============================================================================
# RUST DETECTION
# ============================================================================

function Test-RustInstallation {
    try {
        $rustcOutput = rustc --version 2>&1
        if ($LASTEXITCODE -eq 0 -and $rustcOutput -match '(\d+)\.(\d+)\.(\d+)') {
            $major = [int]$matches[1]
            $minor = [int]$matches[2]

            if ($major -gt $MinRustMajor -or ($major -eq $MinRustMajor -and $minor -ge $MinRustMinor)) {
                Write-Success "Rust $($matches[0]) detected"
                return $true
            }

            Write-ErrorLog "Rust $($matches[0]) is too old. Minimum required: $MinRustMajor.$MinRustMinor"
            return $false
        }
    } catch {
        # Rust not found
    }

    Write-ErrorLog "Rust/Cargo not found."
    return $false
}

function Install-Rustup {
    Write-Header "Installing Rust Toolchain"
    Write-Info "Downloading rustup-init.exe..."

    $rustupUrl = "https://win.rustup.rs/x86_64"
    $rustupPath = Join-Path $env:TEMP "rustup-init.exe"

    try {
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupPath -UseBasicParsing
        Write-Success "Downloaded rustup-init.exe"

        Write-Info "Running rustup installer..."
        & $rustupPath -y
        if ($LASTEXITCODE -ne 0) {
            Write-ErrorLog "Rust installation failed"
            return $false
        }

        Refresh-ProcessPath

        if (Test-RustInstallation) {
            Write-Success "Rust installed successfully"
            return $true
        }

        Write-ErrorLog "Rust installation failed"
        return $false
    } finally {
        Remove-Item $rustupPath -Force -ErrorAction SilentlyContinue
    }
}

function Ensure-CargoBinDirectory {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-WarnLog "Cargo is not available and $InstallBinDir does not exist yet."
        if (-not $Force) {
            $response = Read-Host "Install Rust/Cargo and create $InstallBinDir now? [y/N]"
            if ($response -ne 'y' -and $response -ne 'Y') {
                Write-ErrorLog "Cargo is required to install LeIndex into $InstallBinDir"
                return $false
            }
        }

        if (-not (Install-Rustup)) {
            return $false
        }
    }

    Ensure-Directory -Path $InstallBinDir
    return $true
}

# ============================================================================
# INSTALLATION
# ============================================================================

function Remove-LegacyInstallations {
    if (Test-Path $LegacyBinaryPath) {
        try {
            Remove-Item $LegacyBinaryPath -Force
            Write-Success "Removed legacy install: $LegacyBinaryPath"
        } catch {
            Write-WarnLog "Could not remove legacy install: $LegacyBinaryPath"
        }
    }

    if (Test-Path $LegacyBinDir) {
        try {
            $remaining = Get-ChildItem -Path $LegacyBinDir -Force -ErrorAction Stop
            if ($remaining.Count -eq 0) {
                Remove-Item $LegacyBinDir -Force
            }
        } catch {
            # Ignore non-empty or inaccessible directory cleanup
        }
    }
}

function Install-LeIndex {
    Write-Host "[Step 2/4] Installing LeIndex" -ForegroundColor Blue
    Write-Info "Installing $ProjectSlug $ExpectedVersion into $InstallBinDir..."

    $cargoArgs = @("install", $ProjectSlug, "--locked", "--force", "--version", $ExpectedVersion)

    $installOutput = & cargo @cargoArgs 2>&1
    Add-Content -Path $InstallLog -Value ($installOutput -join [Environment]::NewLine)

    if ($LASTEXITCODE -ne 0) {
        Write-ErrorLog "cargo install failed"
        Write-Host ($installOutput -join [Environment]::NewLine)
        return $false
    }

    Remove-LegacyInstallations
    Write-Success "Installed binary to: $InstallBinaryPath"
    return $true
}

function Test-Installation {
    Write-Host "[Step 3/4] Verifying Installation" -ForegroundColor Blue

    if (-not (Test-Path $InstallBinaryPath)) {
        Write-ErrorLog "Binary not found: $InstallBinaryPath"
        return $false
    }

    try {
        $versionOutput = & $InstallBinaryPath --version 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-ErrorLog "Installation verification failed"
            return $false
        }

        $versionText = ($versionOutput -join " ")
        if ($versionText -notmatch [regex]::Escape($ExpectedVersion)) {
            Write-ErrorLog "Installed version does not match expected version $ExpectedVersion"
            Write-Host $versionText
            return $false
        }

        Write-Success "Binary check passed: $versionText"

        & $InstallBinaryPath phase --help *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-ErrorLog "Installed binary does not expose 'phase' command"
            return $false
        }
        Write-Success "Phase command detected"

        & $InstallBinaryPath mcp --help *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-ErrorLog "Installed binary does not expose 'mcp' command"
            return $false
        }
        Write-Success "MCP command detected"

        return $true
    } catch {
        Write-ErrorLog "Installation verification failed"
        return $false
    }
}

function Initialize-Directories {
    Write-Host "[Step 4/4] Finalizing Installation" -ForegroundColor Blue

    $directories = @($ConfigDir, $DataDir, $LogDir, $InstallBinDir)
    foreach ($dir in $directories) {
        Ensure-Directory -Path $dir
    }
}

function Update-UserPath {
    Write-Header "Update PATH"

    $currentPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @()
    if ($currentPath) {
        $entries = $currentPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    }

    $legacyBinNormalized = Normalize-PathValue -PathValue $LegacyBinDir
    $installBinNormalized = Normalize-PathValue -PathValue $InstallBinDir
    $seen = @{}
    $updatedEntries = New-Object System.Collections.Generic.List[string]

    $updatedEntries.Add($InstallBinDir)
    $seen[$installBinNormalized] = $true

    foreach ($entry in $entries) {
        $normalized = Normalize-PathValue -PathValue $entry
        if ($normalized -eq $legacyBinNormalized -or $seen.ContainsKey($normalized)) {
            continue
        }

        $seen[$normalized] = $true
        $updatedEntries.Add($entry)
    }

    [System.Environment]::SetEnvironmentVariable("Path", ($updatedEntries -join ';'), "User")
    Refresh-ProcessPath

    Write-Success "Ensured $InstallBinDir is first in the user PATH"
    if ($entries | Where-Object { (Normalize-PathValue -PathValue $_) -eq $legacyBinNormalized }) {
        Write-Success "Removed legacy PATH entry: $LegacyBinDir"
    }
}

function Warn-IfPathShadowed {
    try {
        $resolvedPath = (Get-Command $ProjectSlug -ErrorAction Stop).Source
    } catch {
        Write-WarnLog "Could not resolve $ProjectSlug on PATH. Add $InstallBinDir to PATH and restart your terminal."
        return
    }

    if ((Normalize-PathValue -PathValue $resolvedPath) -eq (Normalize-PathValue -PathValue $InstallBinaryPath)) {
        Write-Success "$ProjectSlug resolves to the installed cargo binary"
        return
    }

    Write-WarnLog "$ProjectSlug currently resolves to $resolvedPath instead of $InstallBinaryPath"
    Write-Host "  Remediation: remove the older binary or move $InstallBinDir earlier in PATH"
}

function Maybe-StarRepo {
    Ensure-Directory -Path $LEINDEX_HOME

    Write-Info "Thank you for installing LeIndex."

    if (Test-Path $StarMarkerPath) {
        Write-Success "GitHub star already recorded for this installation."
        return
    }

    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if ($gh) {
        & gh auth status *> $null
        if ($LASTEXITCODE -eq 0) {
            & gh api -X PUT -H "Accept: application/vnd.github+json" $RepoStarEndpoint *> $null
            if ($LASTEXITCODE -eq 0) {
                Set-Content -Path $StarMarkerPath -Value "starred" -NoNewline
                Write-Success "Starred scooter-lacroix/LeIndex on GitHub."
                return
            }
        }
    }

    Write-WarnLog "Could not star the repository automatically. You can star it here: $RepoUrl"
}

# ============================================================================
# MAIN
# ============================================================================

function Main {
    Ensure-LogDirectory

    Write-Header "LeIndex Rust Installer"

    Write-Host "  Project:     $ProjectName"
    Write-Host "  Version:     $ScriptVersion"
    Write-Host "  Target:      $ExpectedVersion"
    Write-Host "  Repository:  $RepoUrl"
    Write-Host ""

    Write-Host "[Step 1/4] Checking Rust Toolchain" -ForegroundColor Blue
    if (-not (Test-RustInstallation)) {
        Write-Host ""
        Write-WarnLog "Rust is not installed or is too old"
        Write-Host ""
        if ($Force) {
            if (-not (Install-Rustup)) {
                Write-ErrorLog "Rust installation failed. Please install manually from https://rustup.rs/"
                exit 1
            }
        } else {
            $response = Read-Host "Would you like to install Rust now? [y/N]"
            if ($response -eq 'y' -or $response -eq 'Y') {
                if (-not (Install-Rustup)) {
                    Write-ErrorLog "Rust installation failed. Please install manually from https://rustup.rs/"
                    exit 1
                }
            } else {
                Write-ErrorLog "Rust is required to install LeIndex"
                exit 1
            }
        }
    }

    if (-not (Ensure-CargoBinDirectory)) {
        exit 1
    }

    if (-not (Install-LeIndex)) {
        Write-ErrorLog "Installation failed"
        exit 1
    }

    Initialize-Directories

    if (-not (Test-Installation)) {
        Write-ErrorLog "Installation verification failed"
        exit 1
    }

    Update-UserPath
    Warn-IfPathShadowed
    Maybe-StarRepo

    Write-Header "Installation Complete"

    Write-Success "LeIndex has been installed successfully."
    Write-Host ""
    Write-Host "  Binary:       $InstallBinaryPath"
    Write-Host "  Config:       $ConfigDir"
    Write-Host "  Data:         $DataDir"
    Write-Host "  Install log:  $InstallLog"
    Write-Host ""
    Write-Host "To get started:"
    Write-Host "  1. Restart your terminal"
    Write-Host "  2. Verify installation: $ProjectSlug --version"
    Write-Host "  3. Index a project: $ProjectSlug index C:\path\to\project"
    Write-Host "  4. Run diagnostics: $ProjectSlug diagnostics"
}

Main
