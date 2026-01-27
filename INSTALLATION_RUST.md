# LeIndex Rust Installation Guide

Complete guide to installing LeIndex, the pure Rust code search and analysis engine.

---

## System Requirements

### Minimum Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| **Rust** | 1.75+ | Latest stable |
| **RAM** | 4GB | 8GB+ |
| **Disk** | 1GB | 2GB+ SSD |
| **CPU** | 4 cores | 8+ cores |

### Supported Platforms

- ✅ **Linux** - x86_64, aarch64
- ✅ **macOS** - x86_64, arm64 (Apple Silicon)
- ✅ **Windows** - x86_64

---

## Prerequisites

### 1. Install Rust

LeIndex requires Rust 1.75 or later. Install via rustup:

**Linux/macOS:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**Windows:**
```powershell
# Download and run rustup-init.exe from https://rustup.rs/
```

Verify installation:
```bash
rustc --version
cargo --version
```

### 2. System Dependencies

**Linux (Ubuntu/Debian):**
```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev
```

**macOS:**
```bash
# Install Xcode Command Line Tools
xcode-select --install
```

**Windows:**
- Install [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
- Install [Git](https://git-scm.com/)

---

## Installation Methods

### Method 1: Build from Source (Recommended)

This is the recommended method for most users.

```bash
# Clone the repository
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex

# Build release binaries
cargo build --release --bins

# The binary will be at:
# - target/release/leindex (Linux/macOS)
# - target\release\leindex.exe (Windows)
```

### Method 2: Using the Installer

The installer handles Rust detection, building, and PATH configuration.

**Linux/macOS:**
```bash
./install.sh
```

**Windows (PowerShell):**
```powershell
.\install.ps1
```

The installer will:
1. Check for Rust installation (install if missing)
2. Build LeIndex from source
3. Install to `~/.leindex/bin/`
4. Update your PATH
5. Configure directories

### Method 3: Cargo Install (Future)

Once published to crates.io:

```bash
cargo install leindex
```

*Note: Not yet available - use Method 1 or 2.*

---

## Post-Installation Setup

### 1. Verify Installation

```bash
leindex --version
# Output: LeIndex 0.1.0
```

### 2. Check Your PATH

The `leindex` command should be available. If not, add to your PATH:

**Linux/macOS (bash):**
```bash
export PATH="$HOME/.leindex/bin:$PATH"
echo 'export PATH="$HOME/.leindex/bin:$PATH"' >> ~/.bashrc
```

**Linux/macOS (zsh):**
```bash
export PATH="$HOME/.leindex/bin:$PATH"
echo 'export PATH="$HOME/.leindex/bin:$PATH"' >> ~/.zshrc
```

**Windows (PowerShell):**
```powershell
$env:Path += ";$env:USERPROFILE\.leindex\bin"
[System.Environment]::SetEnvironmentVariable("Path", $env:Path, "User")
```

### 3. Create Configuration

LeIndex will use default configuration. Create `leindex.toml` in your project root for custom settings:

```toml
# Memory settings
[memory]
total_budget_mb = 3072
soft_limit_percent = 0.80
hard_limit_percent = 0.93
emergency_percent = 0.98

# File filtering
[file_filtering]
max_file_size = 1073741824
exclude_patterns = [
    "**/node_modules/**",
    "**/.git/**",
    "**/target/**",
    "**/build/**"
]
```

---

## Verification

### Run Diagnostics

```bash
leindex diagnostics
```

This will display:
- LeIndex version
- System information
- Memory status
- Parser availability

### Test Indexing

```bash
# Index a small project
leindex index /path/to/project

# Search the indexed code
leindex search "function"
```

---

## Troubleshooting

### Rust Not Found

**Problem:** `rustc: command not found`

**Solution:**
1. Install Rust via rustup (see Prerequisites)
2. Restart your terminal
3. Verify with `rustc --version`

### Build Fails

**Problem:** Compilation errors during `cargo build`

**Solutions:**
1. Update Rust: `rustup update`
2. Clean build: `cargo clean && cargo build --release`
3. Check dependencies: `cargo fetch`
4. Ensure sufficient disk space (>5GB)

### Permissions Error

**Problem:** Permission denied when installing

**Solutions:**
1. Use `--prefix` with cargo: `cargo install --prefix ~/.local`
2. Or run the installer which uses user directory

### Binary Not Found

**Problem:** `leindex: command not found` after installation

**Solutions:**
1. Check binary exists: `ls ~/.leindex/bin/leindex`
2. Add to PATH (see Post-Installation Setup)
3. Restart terminal

---

## Uninstallation

### Remove Binaries

```bash
# Remove installed binary
rm -f ~/.leindex/bin/leindex

# Remove from PATH (edit ~/.bashrc, ~/.zshrc, or Windows environment variables)
```

### Remove Data

```bash
# Remove all LeIndex data (WARNING: deletes indexed data)
rm -rf ~/.leindex
```

---

## Next Steps

- [Configuration Reference](README.md#configuration) - Customize LeIndex
- [Architecture](ARCHITECTURE.md) - Understand the system
- [MCP Integration](MCP_COMPATIBILITY.md) - Set up AI assistant integration
- [Migration Guide](MIGRATION.md) - Upgrading from Python v2.0.2

---

## Getting Help

- **GitHub Issues:** [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)
- **Documentation:** [https://github.com/scooter-lacroix/leindex](https://github.com/scooter-lacroix/leindex)

---

**Happy indexing!** 🚀
