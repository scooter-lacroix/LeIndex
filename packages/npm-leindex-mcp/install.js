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
const crypto = require('crypto');
const { execFileSync, execSync } = require('child_process');
const pkg = require('./package.json');

const BIN_DIR = path.join(__dirname, 'bin');
const GITHUB_API_BASE = 'https://api.github.com/repos/scooter-lacroix/LeIndex';
const DEFAULT_RELEASE_SELECTOR = 'latest';
const MAX_REDIRECTS = 5;

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

function getBinaryName() {
  return process.platform === 'win32' ? 'leindex.exe' : 'leindex';
}

function requestResponse(url, options = {}, redirectCount = 0) {
  return new Promise((resolve, reject) => {
    https.get(url, options, (response) => {
      const statusCode = response.statusCode || 0;
      const isRedirect = [301, 302, 303, 307, 308].includes(statusCode);

      if (isRedirect && response.headers.location) {
        response.resume();

        if (redirectCount >= MAX_REDIRECTS) {
          const error = new Error(`Too many redirects while fetching ${url}`);
          error.code = 'DOWNLOAD';
          reject(error);
          return;
        }

        resolve(requestResponse(response.headers.location, options, redirectCount + 1));
        return;
      }

      resolve(response);
    }).on('error', reject);
  });
}

async function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);

    requestResponse(url).then((response) => {
      if (response.statusCode !== 200) {
        response.resume();
        fs.unlink(dest, () => {});
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

      response.on('error', (err) => {
        fs.unlink(dest, () => {});
        reject(err);
      });
    }).catch((err) => {
      fs.unlink(dest, () => {});
      reject(err);
    });
  });
}

async function fetchJson(url) {
  const options = {
    headers: {
      'User-Agent': '@leindex/mcp installer',
      'Accept': 'application/vnd.github+json'
    }
  };
  const response = await requestResponse(url, options);
  let body = '';

  return new Promise((resolve, reject) => {
    response.on('data', (chunk) => {
      body += chunk.toString();
    });

    response.on('end', () => {
      if (response.statusCode !== 200) {
        const error = new Error(`GitHub API request failed: HTTP ${response.statusCode}`);
        error.code = 'GITHUB_API';
        reject(error);
        return;
      }

      try {
        resolve(JSON.parse(body));
      } catch (error) {
        error.code = 'GITHUB_API';
        reject(error);
      }
    });

    response.on('error', (error) => {
      error.code = 'GITHUB_API';
      reject(error);
    });
  });
}

async function downloadText(url) {
  const response = await requestResponse(url);
  return new Promise((resolve, reject) => {
      if (response.statusCode !== 200) {
        const error = new Error(`Download failed: HTTP ${response.statusCode}`);
        error.code = 'CHECKSUM_DOWNLOAD';
        reject(error);
        return;
      }

      let body = '';
      response.on('data', (chunk) => {
        body += chunk.toString();
      });
      response.on('end', () => resolve(body));
      response.on('error', (error) => {
      error.code = 'CHECKSUM_DOWNLOAD';
      reject(error);
      });
  });
}

function getRequestedRelease() {
  return process.env.LEINDEX_BINARY_VERSION || process.env.LEINDEX_BINARY_CHANNEL || DEFAULT_RELEASE_SELECTOR;
}

function getAssetName(version, platform, arch) {
  const ext = platform === 'windows' ? '.exe' : '';
  return `leindex-${version}-${platform}-${arch}${ext}`;
}

function parseReleaseVersion(tagName) {
  return tagName.replace(/^v/, '');
}

async function resolveReleaseConfig(platform, arch, requestedRelease = getRequestedRelease()) {
  const endpoint = requestedRelease === 'latest'
    ? `${GITHUB_API_BASE}/releases/latest`
    : `${GITHUB_API_BASE}/releases/tags/v${requestedRelease}`;
  let release;

  try {
    release = await fetchJson(endpoint);
  } catch (error) {
    if (error.code === 'GITHUB_API' && /HTTP 404/.test(error.message)) {
      const notFoundError = new Error(
        requestedRelease === 'latest'
          ? 'No GitHub release is published yet for the latest channel'
          : `GitHub release v${requestedRelease} was not found`
      );
      notFoundError.code = 'GITHUB_RELEASE';
      throw notFoundError;
    }

    throw error;
  }

  const version = parseReleaseVersion(release.tag_name);
  const assetName = getAssetName(version, platform, arch);
  const binaryAsset = release.assets.find((asset) => asset.name === assetName);
  const checksumAsset = release.assets.find((asset) => asset.name === 'SHA256SUMS');

  if (!binaryAsset) {
    const error = new Error(`Release asset not found: ${assetName}`);
    error.code = 'RELEASE_ASSET';
    throw error;
  }

  if (!checksumAsset) {
    const error = new Error('Release checksum asset SHA256SUMS not found');
    error.code = 'INTEGRITY';
    throw error;
  }

  return {
    selector: requestedRelease,
    source: requestedRelease === 'latest' ? 'github-latest' : 'github-versioned',
    version,
    assetName,
    binaryUrl: binaryAsset.browser_download_url,
    checksumUrl: checksumAsset.browser_download_url
  };
}

function parseExpectedChecksum(checksumText, assetName) {
  const line = checksumText
    .split(/\r?\n/)
    .map((entry) => entry.trim())
    .find((entry) => entry.endsWith(` ${assetName}`) || entry.endsWith(` *${assetName}`));

  if (!line) {
    const error = new Error(`Checksum entry not found for ${assetName}`);
    error.code = 'INTEGRITY';
    throw error;
  }

  const checksum = line.split(/\s+/)[0];
  if (!/^[a-f0-9]{64}$/i.test(checksum)) {
    const error = new Error(`Invalid checksum format for ${assetName}`);
    error.code = 'INTEGRITY';
    throw error;
  }

  return checksum.toLowerCase();
}

function computeFileSha256(filePath) {
  const hash = crypto.createHash('sha256');
  hash.update(fs.readFileSync(filePath));
  return hash.digest('hex');
}

function verifyChecksum(filePath, expectedChecksum) {
  const actualChecksum = computeFileSha256(filePath);
  if (actualChecksum !== expectedChecksum) {
    const error = new Error(`Checksum mismatch for ${path.basename(filePath)}`);
    error.code = 'INTEGRITY';
    throw error;
  }
}

async function install() {
  console.log('🔧 LeIndex MCP Installer');
  console.log(`   Wrapper version: ${pkg.version}`);
  
  const { platform, arch } = getPlatform();
  console.log(`   Platform: ${platform} (${arch})`);
  console.log(`   Binary selector: ${getRequestedRelease()}\n`);
  
  // Ensure bin directory exists
  if (!fs.existsSync(BIN_DIR)) {
    fs.mkdirSync(BIN_DIR, { recursive: true });
  }
  
  const binaryName = getBinaryName();
  const binaryPath = path.join(BIN_DIR, binaryName);
  const tempBinaryPath = path.join(BIN_DIR, `${binaryName}.download`);
  
  // Check if already installed
  if (fs.existsSync(binaryPath)) {
    try {
      const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
      console.log(`   ✓ LeIndex already installed: ${version}`);
      console.log('\n📦 Installation complete!');
      console.log('   Add this package to your MCP configuration to use LeIndex.');
      return;
    } catch (e) {
      // Version check failed, continue with download
      fs.unlinkSync(binaryPath);
    }
  }
  
  try {
    const release = await resolveReleaseConfig(platform, arch);
    console.log(`   Resolved binary version: ${release.version}`);
    console.log(`   Downloading from: ${release.binaryUrl}\n`);

    if (fs.existsSync(tempBinaryPath)) {
      fs.unlinkSync(tempBinaryPath);
    }

    await downloadFile(release.binaryUrl, tempBinaryPath);
    const checksumText = await downloadText(release.checksumUrl);
    const expectedChecksum = parseExpectedChecksum(checksumText, release.assetName);
    verifyChecksum(tempBinaryPath, expectedChecksum);
    
    // Make executable on Unix
    if (process.platform !== 'win32') {
      fs.chmodSync(tempBinaryPath, 0o755);
    }

    fs.renameSync(tempBinaryPath, binaryPath);
    
    // Verify installation
    const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
    console.log(`   ✓ LeIndex installed from ${release.source}: ${version}`);
    console.log('\n📦 Installation complete!');
    console.log('   Add this package to your MCP configuration to use LeIndex.');
    
  } catch (error) {
    if (fs.existsSync(tempBinaryPath)) {
      fs.unlinkSync(tempBinaryPath);
    }

    if (error.code === 'INTEGRITY') {
      console.error(`\n❌ Installation failed integrity verification: ${error.message}`);
      console.error('\n   Refusing to execute an unverified binary.');
      process.exit(1);
    }

    console.error(`\n❌ Download failed: ${error.message}`);
    console.error('\n   Falling back to cargo because the GitHub release could not be resolved or downloaded.');
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

module.exports = {
  computeFileSha256,
  getAssetName,
  getRequestedRelease,
  parseExpectedChecksum,
  parseReleaseVersion,
  resolveReleaseConfig,
  verifyChecksum
};

if (require.main === module) {
  install().catch((err) => {
    console.error('❌ Installation error:', err);
    process.exit(1);
  });
}
