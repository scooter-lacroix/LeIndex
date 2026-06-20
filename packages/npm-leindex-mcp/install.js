#!/usr/bin/env node

/**
 * LeIndex MCP - Post-install script
 * 
 * Automatically downloads the appropriate LeIndex bundle for the current platform.
 * The bundle includes the main binary, the ONNX worker binary (leindex-embed),
 * and model assets required for local semantic search.
 */

const fs = require('fs');
const path = require('path');
const https = require('https');
const crypto = require('crypto');
const { execFileSync, execSync } = require('child_process');
const zlib = require('zlib');
const pkg = require('./package.json');

const BIN_DIR = path.join(__dirname, 'bin');
const MODELS_DIR = path.join(__dirname, 'models');
// VAL-NPM-002: bundled ORT shared libraries are extracted here so the
// worker discovers ORT via its bundled/sibling-library path without
// requiring the user to run `leindex setup`.
const LIB_DIR = path.join(__dirname, 'lib');
const GITHUB_API_BASE = 'https://api.github.com/repos/scooter-lacroix/LeIndex';
const DEFAULT_RELEASE_SELECTOR = 'latest';
const MAX_REDIRECTS = 5;

/**
 * The unversioned ONNX Runtime shared library name on the current platform.
 * The discovery chain in `crates/leindex-embed/src/ort_discovery.rs` looks for
 * exactly these filenames at each candidate directory.
 */
function getOrtLibNames() {
  if (process.platform === 'win32') {
    return ['onnxruntime.dll'];
  }
  if (process.platform === 'darwin') {
    return ['libonnxruntime.dylib'];
  }
  return ['libonnxruntime.so'];
}

function isOrtRuntimeLibraryName(name) {
  const lower = name.toLowerCase();
  if (process.platform === 'win32') {
    return lower === 'onnxruntime.dll';
  }
  if (process.platform === 'darwin') {
    return lower === 'libonnxruntime.dylib'
      || (lower.startsWith('libonnxruntime.') && lower.endsWith('.dylib'));
  }
  return lower === 'libonnxruntime.so' || lower.startsWith('libonnxruntime.so.');
}

function isOrtBundleLibraryName(name) {
  const lower = name.toLowerCase();
  if (isOrtRuntimeLibraryName(lower)) {
    return true;
  }
  if (process.platform === 'win32') {
    return lower.startsWith('onnxruntime_providers_') && lower.endsWith('.dll');
  }
  if (process.platform === 'darwin') {
    return lower.startsWith('libonnxruntime_providers_') && lower.endsWith('.dylib');
  }
  return lower.startsWith('libonnxruntime_providers_') && (lower.endsWith('.so') || /\.so\.\d/.test(lower));
}

/**
 * Copy a single file from `src` to `dst`, preserving symlinks and the
 * executable bit. Linux/macOS ORT bundles ship versioned symlinks like
 * `libonnxruntime.so.1 -> libonnxruntime.so.1.25.0` and the worker's
 * sibling-library discovery expects the unversioned `libonnxruntime.so`
 * entry to remain a symlink (or a real file) at the same path, so we
 * faithfully reproduce the source's file type instead of dereferencing.
 *
 * Returns a short label describing what was copied, for logging.
 */
function copyBundledEntry(src, dst) {
  const stat = fs.lstatSync(src);

  if (stat.isSymbolicLink()) {
    const target = fs.readlinkSync(src);
    try {
      fs.symlinkSync(target, dst);
    } catch (err) {
      // Some filesystems / Windows without dev mode reject symlinks;
      // fall back to copying the link target so the file still exists.
      fs.copyFileSync(src, dst);
    }
    return 'symlink';
  }

  fs.copyFileSync(src, dst);
  if (process.platform !== 'win32') {
    // Preserve executable bit for shared libraries on Unix.
    try {
      fs.chmodSync(dst, stat.mode);
    } catch (_) {
      // Permissions may fail on exotic filesystems; non-fatal.
    }
  }
  return 'file';
}

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

function getWorkerBinaryName() {
  return process.platform === 'win32' ? 'leindex-embed.exe' : 'leindex-embed';
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

/**
 * Legacy asset name for backward compatibility with pre-bundle releases.
 * Returns the bare binary asset name (e.g. leindex-1.8.1-linux-x86_64).
 */
function getAssetName(version, platform, arch) {
  const ext = platform === 'windows' ? '.exe' : '';
  return `leindex-${version}-${platform}-${arch}${ext}`;
}

/**
 * Bundle archive asset name for the worker-bundle release format.
 * Returns the tar.gz or zip archive name (e.g. leindex-1.8.1-linux-x86_64.tar.gz).
 */
function getBundleAssetName(version, platform, arch) {
  if (platform === 'windows') {
    return `leindex-${version}-${platform}-${arch}.zip`;
  }
  return `leindex-${version}-${platform}-${arch}.tar.gz`;
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
  const checksumAsset = release.assets.find((asset) => asset.name === 'SHA256SUMS');

  if (!checksumAsset) {
    const error = new Error('Release checksum asset SHA256SUMS not found');
    error.code = 'INTEGRITY';
    throw error;
  }

  // Try the bundle archive first (worker-bundle format)
  const bundleAssetName = getBundleAssetName(version, platform, arch);
  const bundleAsset = release.assets.find((asset) => asset.name === bundleAssetName);

  if (bundleAsset) {
    return {
      selector: requestedRelease,
      source: requestedRelease === 'latest' ? 'github-latest' : 'github-versioned',
      version,
      assetName: bundleAssetName,
      bundleUrl: bundleAsset.browser_download_url,
      checksumUrl: checksumAsset.browser_download_url,
      isBundle: true
    };
  }

  // Fall back to legacy bare binary format
  const legacyAssetName = getAssetName(version, platform, arch);
  const legacyAsset = release.assets.find((asset) => asset.name === legacyAssetName);

  if (!legacyAsset) {
    const error = new Error(`Release asset not found: ${bundleAssetName} (tried legacy ${legacyAssetName} too)`);
    error.code = 'RELEASE_ASSET';
    throw error;
  }

  return {
    selector: requestedRelease,
    source: requestedRelease === 'latest' ? 'github-latest' : 'github-versioned',
    version,
    assetName: legacyAssetName,
    binaryUrl: legacyAsset.browser_download_url,
    checksumUrl: checksumAsset.browser_download_url,
    isBundle: false
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

/**
 * Extract a tar.gz bundle using Node's built-in zlib + tar parsing.
 * Places bin/ contents into BIN_DIR and models/ contents into MODELS_DIR.
 */
function extractTarGz(archivePath, destDir) {
  const tar = require('child_process');
  // Use system tar for reliable extraction
  tar.execSync(`tar xzf "${archivePath}" -C "${destDir}"`, { stdio: 'pipe' });
}

/**
 * Extract a zip bundle using PowerShell (Windows) or unzip.
 */
function extractZip(archivePath, destDir) {
  const tar = require('child_process');
  if (process.platform === 'win32') {
    tar.execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${destDir}' -Force"`, { stdio: 'pipe' });
  } else {
    tar.execSync(`unzip -o "${archivePath}" -d "${destDir}"`, { stdio: 'pipe' });
  }
}

/**
 * Install from a bundle archive: extract and place binaries + models.
 */
async function installFromBundle(release) {
  const tempDir = path.join(BIN_DIR, '.bundle-tmp');
  const archiveExt = release.assetName.endsWith('.zip') ? '.zip' : '.tar.gz';
  const archivePath = path.join(tempDir, release.assetName);

  // Clean up any previous attempt
  if (fs.existsSync(tempDir)) {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
  fs.mkdirSync(tempDir, { recursive: true });

  try {
    console.log(`   Downloading bundle from: ${release.bundleUrl}\n`);
    await downloadFile(release.bundleUrl, archivePath);

    // Verify archive checksum
    const checksumText = await downloadText(release.checksumUrl);
    const expectedChecksum = parseExpectedChecksum(checksumText, release.assetName);
    verifyChecksum(archivePath, expectedChecksum);
    console.log('   ✓ Bundle checksum verified');

    // Extract
    if (archiveExt === '.zip') {
      extractZip(archivePath, tempDir);
    } else {
      extractTarGz(archivePath, tempDir);
    }

    // Find the extracted bundle directory (leindex-VERSION-PLATFORM)
    const extractedDirs = fs.readdirSync(tempDir).filter(name => 
      name.startsWith('leindex-') && fs.statSync(path.join(tempDir, name)).isDirectory()
    );

    if (extractedDirs.length === 0) {
      throw new Error('Bundle archive did not contain expected directory');
    }

    const bundleDir = path.join(tempDir, extractedDirs[0]);

    // Install binaries
    if (!fs.existsSync(BIN_DIR)) {
      fs.mkdirSync(BIN_DIR, { recursive: true });
    }

    const binaryName = getBinaryName();
    const workerName = getWorkerBinaryName();
    const srcBinary = path.join(bundleDir, 'bin', binaryName);
    const srcWorker = path.join(bundleDir, 'bin', workerName);

    if (fs.existsSync(srcBinary)) {
      fs.copyFileSync(srcBinary, path.join(BIN_DIR, binaryName));
      if (process.platform !== 'win32') {
        fs.chmodSync(path.join(BIN_DIR, binaryName), 0o755);
      }
      console.log('   ✓ Main binary installed');
    } else {
      throw new Error(`Main binary not found in bundle: bin/${binaryName}`);
    }

    if (fs.existsSync(srcWorker)) {
      fs.copyFileSync(srcWorker, path.join(BIN_DIR, workerName));
      if (process.platform !== 'win32') {
        fs.chmodSync(path.join(BIN_DIR, workerName), 0o755);
      }
      console.log('   ✓ Worker binary installed');
    } else {
      console.log('   ⚠ Worker binary not found in bundle; ONNX will use in-process fallback');
    }

    // Install model assets
    const srcModels = path.join(bundleDir, 'models');
    if (fs.existsSync(srcModels)) {
      if (!fs.existsSync(MODELS_DIR)) {
        fs.mkdirSync(MODELS_DIR, { recursive: true });
      }
      const modelFiles = fs.readdirSync(srcModels);
      for (const file of modelFiles) {
        fs.copyFileSync(path.join(srcModels, file), path.join(MODELS_DIR, file));
      }
      console.log(`   ✓ Model assets installed (${modelFiles.length} files)`);
    } else {
      console.log('   ⚠ Model assets not found in bundle');
    }

    // VAL-NPM-002: Install bundled ORT shared libraries under `lib/`.
    // The worker's sibling-directory discovery path looks for
    // `libonnxruntime.{so,dylib,dll}` here, so the package works with
    // neural embeddings enabled without requiring `npm run setup` first.
    const srcLib = path.join(bundleDir, 'lib');
    if (fs.existsSync(srcLib)) {
      if (!fs.existsSync(LIB_DIR)) {
        fs.mkdirSync(LIB_DIR, { recursive: true });
      }
      // Clean any stale lib/ entries from a previous (or upgraded) bundle
      // before copying the new ones, so renamed files don't linger.
      for (const stale of fs.readdirSync(LIB_DIR)) {
        const stalePath = path.join(LIB_DIR, stale);
        try {
          fs.rmSync(stalePath, { recursive: true, force: true });
        } catch (_) {
          // Non-fatal: leave the stale entry in place if we can't remove it.
        }
      }

      const libFiles = fs.readdirSync(srcLib);
      const bundledOrtFiles = libFiles.filter(isOrtBundleLibraryName);
      const ignoredLibFiles = libFiles.filter((file) => !isOrtBundleLibraryName(file));
      if (!bundledOrtFiles.some(isOrtRuntimeLibraryName)) {
        throw new Error('bundle lib/ directory did not contain any ORT runtime libraries');
      }
      if (ignoredLibFiles.length > 0) {
        console.log(`   ⚠ Ignoring non-ORT bundle library entries: ${ignoredLibFiles.join(', ')}`);
      }

      let copiedCount = 0;
      let symlinkCount = 0;
      let copiedRuntimeCount = 0;
      for (const file of bundledOrtFiles) {
        const srcEntry = path.join(srcLib, file);
        const dstEntry = path.join(LIB_DIR, file);
        try {
          const kind = copyBundledEntry(srcEntry, dstEntry);
          if (isOrtRuntimeLibraryName(file)) {
            copiedRuntimeCount += 1;
          }
          if (kind === 'symlink') {
            symlinkCount += 1;
          } else {
            copiedCount += 1;
          }
        } catch (err) {
          console.log(`   ⚠ Failed to copy bundled library entry ${file}: ${err.message}`);
        }
      }
      console.log(`   ✓ ORT runtime libraries installed (${copiedCount} files, ${symlinkCount} symlinks) under lib/`);
      if (copiedRuntimeCount === 0) {
        throw new Error('failed to install any ORT runtime libraries from bundle lib/ directory');
      }
    } else {
      throw new Error('ORT libraries not found in release bundle lib/ directory');
    }

    // Verify main binary works
    const binaryPath = path.join(BIN_DIR, binaryName);
    const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
    console.log(`   ✓ LeIndex installed from ${release.source}: ${version}`);
    console.log('\n📦 Installation complete!');
    console.log('   Bundle includes: main binary, ONNX worker, and model assets.');
    console.log('   Add this package to your MCP configuration to use LeIndex.');

  } finally {
    // Clean up temp directory
    if (fs.existsSync(tempDir)) {
      fs.rmSync(tempDir, { recursive: true, force: true });
    }
  }
}

/**
 * Legacy install from bare binary (pre-bundle releases).
 */
async function installFromBinary(release) {
  const binaryName = getBinaryName();
  const binaryPath = path.join(BIN_DIR, binaryName);
  const tempBinaryPath = path.join(BIN_DIR, `${binaryName}.download`);

  if (fs.existsSync(tempBinaryPath)) {
    fs.unlinkSync(tempBinaryPath);
  }

  console.log(`   Downloading from: ${release.binaryUrl}\n`);
  await downloadFile(release.binaryUrl, tempBinaryPath);
  const checksumText = await downloadText(release.checksumUrl);
  const expectedChecksum = parseExpectedChecksum(checksumText, release.assetName);
  verifyChecksum(tempBinaryPath, expectedChecksum);

  if (process.platform !== 'win32') {
    fs.chmodSync(tempBinaryPath, 0o755);
  }

  fs.renameSync(tempBinaryPath, binaryPath);

  const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
  console.log(`   ✓ LeIndex installed from ${release.source}: ${version}`);
  console.log('\n📦 Installation complete (legacy binary format).');
  console.log('   Add this package to your MCP configuration to use LeIndex.');
}

function parseLeindexVersion(output) {
  const match = String(output).match(/leindex\s+([0-9]+\.[0-9]+\.[0-9]+)/);
  return match ? match[1] : null;
}

function existingBinaryMatchesPackage(binaryPath) {
  try {
    const output = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
    const version = parseLeindexVersion(output);
    return {
      ok: version === pkg.version,
      version,
      output,
    };
  } catch (error) {
    return {
      ok: false,
      version: null,
      output: error.message,
    };
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
  const workerName = getWorkerBinaryName();
  const workerPath = path.join(BIN_DIR, workerName);
  
  // Check if already installed (both main and worker)
  if (fs.existsSync(binaryPath)) {
    const existing = existingBinaryMatchesPackage(binaryPath);
    const hasWorker = fs.existsSync(workerPath);

    if (existing.ok && hasWorker) {
      console.log(`   ✓ LeIndex already installed: ${existing.output}`);
      console.log('   ✓ Worker binary present');
      console.log('\n📦 Installation complete!');
      console.log('   Add this package to your MCP configuration to use LeIndex.');
      return;
    }

    if (!existing.ok) {
      console.log(
        `   ⚠ Existing LeIndex binary is stale or unreadable: ${existing.version || existing.output}`
      );
      console.log(`   Reinstalling binary for package version ${pkg.version}...`);
    } else if (!hasWorker) {
      console.log('   ⚠ Worker binary missing; reinstalling bundled worker...');
    }

    try { fs.unlinkSync(binaryPath); } catch (_) {}
    try { fs.unlinkSync(workerPath); } catch (_) {}
  }
  
  try {
    const release = await resolveReleaseConfig(platform, arch);
    console.log(`   Resolved version: ${release.version} (${release.isBundle ? 'bundle' : 'legacy binary'})`);

    if (release.isBundle) {
      await installFromBundle(release);
    } else {
      await installFromBinary(release);
    }
    
  } catch (error) {
    // Clean up partial downloads
    const tempBinaryPath = path.join(BIN_DIR, `${binaryName}.download`);
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
      execSync('cargo install leindex --force --features onnx', { stdio: 'inherit' });
      console.log('\n   ✓ Installed leindex via cargo');
      
      // Also install the ONNX worker binary
      try {
        execSync('cargo install leindex-embed --features onnx --force', { stdio: 'inherit' });
        console.log('   ✓ Installed leindex-embed worker via cargo');
      } catch (embedErr) {
        console.log('   ⚠ leindex-embed install failed; ONNX will use in-process fallback');
      }
      
      // Link cargo-installed binary to our bin directory
      try {
        const cargoHome = process.env.CARGO_HOME || `${require('os').homedir()}/.cargo`;
        const cargoBin = path.join(cargoHome, 'bin', binaryName);
        if (fs.existsSync(cargoBin)) {
          fs.copyFileSync(cargoBin, binaryPath);
          fs.chmodSync(binaryPath, 0o755);
          console.log('   ✓ Binary linked to package directory');
        }
        // Also link worker binary if available
        const cargoWorker = path.join(cargoHome, 'bin', workerName);
        if (fs.existsSync(cargoWorker)) {
          fs.copyFileSync(cargoWorker, workerPath);
          fs.chmodSync(workerPath, 0o755);
          console.log('   ✓ Worker binary linked to package directory');
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
      process.exit(1);
    }
  }
}

module.exports = {
  BIN_DIR,
  MODELS_DIR,
  LIB_DIR,
  computeFileSha256,
  copyBundledEntry,
  getAssetName,
  getBundleAssetName,
  getOrtLibNames,
  getRequestedRelease,
  isOrtBundleLibraryName,
  isOrtRuntimeLibraryName,
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
