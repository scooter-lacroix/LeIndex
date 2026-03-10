#!/usr/bin/env node

/**
 * LeIndex MCP - Post-install script
 * 
 * Automatically downloads the appropriate LeIndex binary for the current platform.
 * This provides the leanest LeIndex distribution - MCP server only, no dashboard.
 */

const fs = require('fs');
const path = require('path');
const https = require('https');
const { execSync } = require('child_process');

const VERSION = '1.5.0';
const BIN_DIR = path.join(__dirname, 'bin');

// Platform mapping
const platforms = {
  'darwin': 'macos',
  'linux': 'linux',
  'win32': 'windows'
};

// Architecture mapping
const architectures = {
  'x64': 'x86_64',
  'arm64': 'aarch64'
};

function getPlatform() {
  const platform = process.platform;
  const arch = process.arch;
  
  if (!platforms[platform]) {
    console.error(`❌ Unsupported platform: ${platform}`);
    console.error('   LeIndex supports: macOS, Linux, Windows');
    process.exit(1);
  }
  
  if (!architectures[arch]) {
    console.error(`❌ Unsupported architecture: ${arch}`);
    console.error('   LeIndex supports: x64, arm64');
    process.exit(1);
  }
  
  return {
    platform: platforms[platform],
    arch: architectures[arch]
  };
}

function getDownloadUrl(platform, arch) {
  const ext = platform === 'windows' ? '.exe' : '';
  return `https://github.com/scooter-lacroix/leindex/releases/download/v${VERSION}/leindex-${VERSION}-${platform}-${arch}${ext}`;
}

function getBinaryName() {
  return process.platform === 'win32' ? 'leindex.exe' : 'leindex';
}

async function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    
    https.get(url, { followRedirect: true }, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        // Follow redirect
        downloadFile(response.headers.location, dest).then(resolve).catch(reject);
        return;
      }
      
      if (response.statusCode !== 200) {
        reject(new Error(`Download failed: HTTP ${response.statusCode}`));
        return;
      }
      
      const totalSize = parseInt(response.headers['content-length'], 10);
      let downloadedSize = 0;
      
      response.on('data', (chunk) => {
        downloadedSize += chunk.length;
        if (totalSize) {
          const percent = Math.round((downloadedSize / totalSize) * 100);
          process.stdout.write(`\r   Downloading... ${percent}%`);
        }
      });
      
      response.pipe(file);
      
      file.on('finish', () => {
        process.stdout.write('\n');
        file.close();
        resolve();
      });
      
      file.on('error', (err) => {
        fs.unlink(dest, () => {});
        reject(err);
      });
    }).on('error', (err) => {
      fs.unlink(dest, () => {});
      reject(err);
    });
  });
}

async function install() {
  console.log('🔧 LeIndex MCP Installer');
  console.log(`   Version: ${VERSION}\n`);
  
  const { platform, arch } = getPlatform();
  console.log(`   Platform: ${platform} (${arch})`);
  
  // Ensure bin directory exists
  if (!fs.existsSync(BIN_DIR)) {
    fs.mkdirSync(BIN_DIR, { recursive: true });
  }
  
  const binaryName = getBinaryName();
  const binaryPath = path.join(BIN_DIR, binaryName);
  
  // Check if already installed
  if (fs.existsSync(binaryPath)) {
    try {
      const version = execSync(`"${binaryPath}" --version`, { encoding: 'utf8' }).trim();
      console.log(`   ✓ LeIndex already installed: ${version}`);
      console.log('\n📦 Installation complete!');
      console.log('   Add this package to your MCP configuration to use LeIndex.');
      return;
    } catch (e) {
      // Version check failed, continue with download
      fs.unlinkSync(binaryPath);
    }
  }
  
  // Download binary
  const url = getDownloadUrl(platform, arch);
  console.log(`   Downloading from: ${url}\n`);
  
  try {
    await downloadFile(url, binaryPath);
    
    // Make executable on Unix
    if (process.platform !== 'win32') {
      fs.chmodSync(binaryPath, 0o755);
    }
    
    // Verify installation
    const version = execSync(`"${binaryPath}" --version`, { encoding: 'utf8' }).trim();
    console.log(`   ✓ LeIndex installed: ${version}`);
    console.log('\n📦 Installation complete!');
    console.log('   Add this package to your MCP configuration to use LeIndex.');
    
  } catch (error) {
    console.error(`\n❌ Download failed: ${error.message}`);
    console.error('\n   This is expected if the GitHub release has not been published yet.');
    console.error('\n   Attempting fallback to cargo install...');
    
    try {
      // Use --force to overwrite any old workspace crate installations
      execSync('cargo install leindex --force', { stdio: 'inherit' });
      console.log('\n   ✓ Installed via cargo');
      
      // Link cargo-installed binary to our bin directory
      try {
        const cargoHome = process.env.CARGO_HOME || `${require('os').homedir()}/.cargo`;
        const cargoBin = path.join(cargoHome, 'bin', binaryName);
        if (fs.existsSync(cargoBin)) {
          fs.copyFileSync(cargoBin, binaryPath);
          fs.chmodSync(binaryPath, 0o755);
          console.log('   ✓ Binary linked to package directory');
        }
      } catch (linkErr) {
        console.log('   ⚠ Could not link binary, but cargo install succeeded');
      }
    } catch (cargoError) {
      console.error('\n❌ Installation failed');
      console.error('\n   Please install manually:');
      console.error('   1. cargo install leindex --force');
      console.error('   2. Or build from source: git clone + cargo build --release');
      console.error('   3. Or wait for GitHub release and reinstall this package');
      console.error('\n   Note: If you have old workspace crates installed (lepasserelle, etc.),');
      console.error('         run: cargo uninstall lepasserelle leserve leedit leparse legraphe lestockage lerecherche lephase leglobal levalidation');
      process.exit(1);
    }
  }
}

install().catch((err) => {
  console.error('❌ Installation error:', err);
  process.exit(1);
});
