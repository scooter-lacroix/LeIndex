#############################################
# LeIndex Windows Installer
# Version: 5.0.0 - Rust Edition
# Platform: Windows PowerShell
#############################################

param(
    [switch]$Force = $false
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

# ============================================================================
# CONFIGURATION
# ============================================================================
$ScriptVersion = "5.0.0"
$ProjectName = "LeIndex"
$ProjectSlug = "leindex"
$MinRustMajor = 1
$MinRustMinor = 75
$RepoUrl = "https://github.com/scooter-lacroix/leindex"

# Installation paths
$LEINDEX_HOME = if ($env:LEINDEX_HOME) { $env:LEINDEX_HOME } else { Join-Path $env:USERPROFILE ".leindex" }
$ConfigDir = Join-Path $LEINDEX_HOME "config"
$DataDir = Join-Path $LEINDEX_HOME "data"
$LogDir = Join-Path $LEINDEX_HOME "logs"
$BinDir = Join-Path $LEINDEX_HOME "bin"

# ============================================================================
# LOGGING
# ============================================================================
$InstallLog = Join-Path $LogDir "install-$(Get-Date -Format 'yyyyMMdd-HHmmss').log"

function Ensure-LogDirectory {
    if (-not (Test-Path $LogDir)) {
        New-Item -ItemType Directory -Path $LogDir -Force | Out-Null
    }
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
    Write-Host "═══════════════════════════════════════════════════" -ForegroundColor Cyan
    Write-Host "  $Title" -ForegroundColor Cyan
    Write-Host "═══════════════════════════════════════════════════" -ForegroundColor Cyan
    Write-Host ""
}

# ============================================================================
# RUST DETECTION
# ============================================================================

function Test-RustInstallation {
    try {
        $rustcOutput = rustc --version 2>&1
        if ($LASTEXITCODE -eq 0) {
            if ($rustcOutput -match '(\d+)\.(\d+)\.(\d+)') {
                $major = [int]$matches[1]
                $minor = [int]$matches[2]

                if ($major -gt $MinRustMajor -or ($major -eq $MinRustMajor -and $minor -ge $MinRustMinor)) {
                    Write-Success "Rust $($matches[0]) detected"
                    return $true
                } else {
                    Write-ErrorLog "Rust $($matches[0]) is too old. Minimum required: $MinRustMajor.$MinRustMinor"
                    return $false
                }
            }
        }
    } catch {
        # Rust not found
    }

    Write-ErrorLog "Rust not found. Please install Rust first."
    return $false
}

function Install-Rustup {
    Write-Header "Installing Rust Toolchain"

    Write-Info "Downloading rustup-init.exe..."

    $rustupUrl = "https://win.rustup.rs/x86_64"
    $rustupPath = "$env:TEMP\rustup-init.exe"

    try {
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupPath -UseBasicParsing
        Write-Success "Downloaded rustup-init.exe"

        Write-Info "Running rustup installer..."
        & $rustupPath -y
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "User") + ";" + [System.Environment]::GetEnvironmentVariable("Path", "Machine")

        if (Test-RustInstallation) {
            Write-Success "Rust installed successfully"
            return $true
        } else {
            Write-ErrorLog "Rust installation failed"
            return $false
        }
    } finally {
        Remove-Item $rustupPath -Force -ErrorAction SilentlyContinue
    }
}

# ============================================================================
# INSTALLATION
# ============================================================================

function Install-LeIndex {
    Write-Host "[Step 2/4] Building LeIndex" -ForegroundColor Blue

    if (-not (Test-Path "Cargo.toml")) {
        Write-ErrorLog "Not in LeIndex repository directory"
        Write-Info "Please run this script from the root of the LeIndex repository"
        return $false
    }

    $cargoContent = Get-Content "Cargo.toml" -Raw
    if ($cargoContent -notmatch $ProjectSlug) {
        Write-ErrorLog "Invalid LeIndex repository"
        return $false
    }

    Write-Info "Building from source..."

    $buildOutput = cargo build --release --bins 2>&1
    $buildLog = $buildOutput -join "`n"
    Add-Content -Path $InstallLog -Value $buildLog

    if ($LASTEXITCODE -eq 0) {
        Write-Success "Build completed successfully"
    } else {
        Write-ErrorLog "Build failed"
        Write-Host $buildLog
        return $false
    }

    $binary = "target\release\$ProjectSlug.exe"
    if (-not (Test-Path $binary)) {
        Write-ErrorLog "Binary not found after build: $binary"
        return $false
    }

    if (-not (Test-Path $BinDir)) {
        New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    }

    Copy-Item $binary -Destination (Join-Path $BinDir "$ProjectSlug.exe") -Force
    Write-Success "Binary installed to: $BinDir\$ProjectSlug.exe"

    return $true
}

function Test-Installation {
    Write-Host "[Step 3/4] Verifying Installation" -ForegroundColor Blue

    $binary = Join-Path $BinDir "$ProjectSlug.exe"

    if (-not (Test-Path $binary)) {
        Write-ErrorLog "Binary not found: $binary"
        return $false
    }

    try {
        $versionOutput = & $binary --version 2>&1
        if ($LASTEXITCODE -eq 0) {
            Write-Success "Installation verified: $versionOutput"
            return $true
        }
    } catch {
        Write-ErrorLog "Installation verification failed"
        return $false
    }

    return $false
}

function Initialize-Directories {
    Write-Host "[Step 4/4] Setting up Directories" -ForegroundColor Blue

    $directories = @($ConfigDir, $DataDir, $LogDir, $BinDir)

    foreach ($dir in $directories) {
        if (-not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force | Out-Null
            Write-Success "Created: $dir"
        }
    }
}

function Add-ToPath {
    Write-Header "Update PATH"

    $pathEntry = $BinDir

    # Check if already in PATH
    $currentPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    if ($currentPath -split ';' -contains $pathEntry) {
        Write-Info "PATH already configured"
        return
    }

    $newPath = "$pathEntry;$currentPath"
    [System.Environment]::SetEnvironmentVariable("Path", $newPath, "User")

    Write-Success "Added to PATH (user)"
    Write-WarnLog "You may need to restart your terminal for changes to take effect"
}

# ============================================================================
# MAIN
# ============================================================================

function Main {
    Ensure-LogDirectory

    Write-Header "LeIndex Rust Installer"

    Write-Host "  Project:     $ProjectName"
    Write-Host "  Version:     $ScriptVersion"
    Write-Host "  Repository:  $RepoUrl"
    Write-Host ""

    # Step 1: Check Rust
    Write-Host "[Step 1/4] Checking Rust Toolchain" -ForegroundColor Blue

    if (-not (Test-RustInstallation)) {
        Write-Host ""
        Write-WarnLog "Rust is not installed or is too old"
        Write-Host ""
        $response = Read-Host "Would you like to install Rust now? [y/N]"
        if ($response -eq 'y' -or $response -eq 'Y') {
            if (-not (Install-Rustup)) {
                Write-ErrorLog "Rust installation failed. Please install manually from https://rustup.rs/"
                exit 1
            }
        } else {
            Write-ErrorLog "Rust is required to build LeIndex"
            exit 1
        }
    }

    # Step 2: Build
    if (-not (Install-LeIndex)) {
        Write-ErrorLog "Installation failed"
        exit 1
    }

    # Step 3: Verify
    if (-not (Test-Installation)) {
        Write-ErrorLog "Installation verification failed"
        exit 1
    }

    # Step 4: Setup directories
    Initialize-Directories

    # Update PATH
    Add-ToPath

    # Success
    Write-Header "Installation Complete!"

    Write-Success "LeIndex has been installed successfully!"
    Write-Host ""
    Write-Host "  Binary:       $BinDir\$ProjectSlug.exe"
    Write-Host "  Config:       $ConfigDir"
    Write-Host "  Data:         $DataDir"
    Write-Host "  Install log:  $InstallLog"
    Write-Host ""
    Write-Host "To get started:"
    Write-Host "  1. Restart your terminal"
    Write-Host "  2. Verify installation: $ProjectSlug --version"
    Write-Host "  3. Index a project: $ProjectSlug index C:\path\to\project"
    Write-Host "  4. Run diagnostics: $ProjectSlug diagnostics"
    Write-Host ""
    Write-Host "Happy indexing!" -ForegroundColor Green
}

# Run main
Main
