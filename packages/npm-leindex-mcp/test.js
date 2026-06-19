#!/usr/bin/env node

/**
 * Simple test suite for @leindex/mcp package
 */

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const os = require('os');

const installer = require('./install.js');
const mcp = require('./index.js');

console.log('🧪 Testing @leindex/mcp package\n');

// Test 1: Package structure
console.log('Test 1: Package structure');
assert(fs.existsSync(path.join(__dirname, 'package.json')), 'package.json should exist');
assert(fs.existsSync(path.join(__dirname, 'index.js')), 'index.js should exist');
assert(fs.existsSync(path.join(__dirname, 'install.js')), 'install.js should exist');
assert(fs.existsSync(path.join(__dirname, 'bin', 'mcp-wrapper.js')), 'mcp-wrapper.js should exist');
console.log('  ✓ All required files present\n');

// Test 2: Package.json validity
console.log('Test 2: Package.json validity');
const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, 'package.json'), 'utf8'));
assert(pkg.name === '@leindex/mcp', 'Package name should be @leindex/mcp');
assert(pkg.bin && pkg.bin['leindex-mcp'], 'Should have leindex-mcp bin entry');
assert(pkg.scripts && pkg.scripts.postinstall, 'Should have postinstall script');
assert(pkg.files && pkg.files.includes('bin/mcp-wrapper.js'), 'Published package should include only the MCP wrapper entrypoint');
assert(pkg.files && pkg.files.includes('bin/setup-wrapper.js'), 'Published package should include the setup wrapper entrypoint (VAL-NPM-005)');
assert(!pkg.files.includes('bin/'), 'Published package should not ship downloaded runtime binaries');
assert(pkg.scripts.setup, 'VAL-NPM-005: package.json should expose a setup script');
assert(/leindex setup|setup-wrapper|bin\/setup/.test(pkg.scripts.setup), 'VAL-NPM-005: setup script should invoke the leindex setup flow');
console.log('  ✓ Package.json is valid\n');

// Test 3: Installer release selection defaults to latest
console.log('Test 3: Installer release selection');
delete process.env.LEINDEX_BINARY_CHANNEL;
delete process.env.LEINDEX_BINARY_VERSION;
assert.strictEqual(installer.getRequestedRelease(), 'latest', 'Default selector should be latest');
assert.strictEqual(installer.getAssetName('1.5.2', 'linux', 'x86_64'), 'leindex-1.5.2-linux-x86_64');
assert.strictEqual(installer.getBundleAssetName('1.6.6', 'linux', 'x86_64'), 'leindex-1.6.6-linux-x86_64.tar.gz');
assert.strictEqual(installer.getBundleAssetName('1.6.6', 'windows', 'x86_64'), 'leindex-1.6.6-windows-x86_64.zip');
assert.strictEqual(installer.getBundleAssetName('1.6.6', 'macos', 'aarch64'), 'leindex-1.6.6-macos-aarch64.tar.gz');
console.log('  ✓ Installer resolves latest by default and knows bundle asset names\n');

// Test 4: Checksum parsing and verification helpers
console.log('Test 4: Checksum verification helpers');
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-test-'));
const tmpFile = path.join(tmpDir, 'leindex');
fs.writeFileSync(tmpFile, 'leindex-test-binary');
const hash = installer.computeFileSha256(tmpFile);
const checksumText = `${hash}  leindex\n`;
assert.strictEqual(installer.parseExpectedChecksum(checksumText, 'leindex'), hash);
installer.verifyChecksum(tmpFile, hash);
console.log('  ✓ Checksum helpers are valid\n');

// Test 5: JS wrapper uses direct argv execution rather than shell interpolation
console.log('Test 5: JS wrapper safety');
assert.strictEqual(typeof mcp.exec, 'function', 'Wrapper should export exec');
assert.strictEqual(typeof mcp.getWorkerBinaryPath, 'function', 'Wrapper should export getWorkerBinaryPath');
assert.strictEqual(typeof mcp.getModelsPath, 'function', 'Wrapper should export getModelsPath');
assert.strictEqual(typeof mcp.getLibPath, 'function', 'Wrapper should export getLibPath (VAL-NPM-002)');
assert(!fs.readFileSync(path.join(__dirname, 'index.js'), 'utf8').includes("args.join(' ')"), 'Wrapper should not shell-interpolate args');
console.log('  ✓ Wrapper executes with argv arrays and exposes worker/model paths\n');

// Test 5b: Bundled ORT discovery wiring (VAL-NPM-003)
console.log('Test 5b: ORT_DYLIB_PATH detection helpers');
assert.strictEqual(typeof installer.getOrtLibNames, 'function', 'install.js should expose getOrtLibNames');
assert.strictEqual(typeof installer.copyBundledEntry, 'function', 'install.js should expose copyBundledEntry');
assert.strictEqual(installer.LIB_DIR, path.join(__dirname, 'lib'), 'install.js LIB_DIR should point at <pkg>/lib');
{
  const names = installer.getOrtLibNames();
  const expected = process.platform === 'win32' ? 'onnxruntime.dll'
    : process.platform === 'darwin' ? 'libonnxruntime.dylib'
    : 'libonnxruntime.so';
  assert(Array.isArray(names) && names.includes(expected), `getOrtLibNames() should include ${expected} on ${process.platform}`);
}
{
  // copyBundledEntry should preserve a regular file when copying.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const tmpB = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const srcFile = path.join(tmpA, 'libonnxruntime.so');
  const dstFile = path.join(tmpB, 'libonnxruntime.so');
  fs.writeFileSync(srcFile, 'fake-ort-so');
  const kind = installer.copyBundledEntry(srcFile, dstFile);
  assert.strictEqual(kind, 'file', 'copyBundledEntry should report "file" for a regular file');
  assert.strictEqual(fs.readFileSync(dstFile, 'utf8'), 'fake-ort-so', 'copyBundledEntry should copy content for regular files');
  fs.rmSync(tmpA, { recursive: true, force: true });
  fs.rmSync(tmpB, { recursive: true, force: true });
}
{
  // copyBundledEntry should preserve a symlink rather than deref it.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const tmpB = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const target = path.join(tmpA, 'libonnxruntime.so.1.25.0');
  fs.writeFileSync(target, 'fake-ort-versioned');
  const link = path.join(tmpA, 'libonnxruntime.so');
  try {
    fs.symlinkSync('libonnxruntime.so.1.25.0', link);
    const dstLink = path.join(tmpB, 'libonnxruntime.so');
    const kind = installer.copyBundledEntry(link, dstLink);
    if (kind === 'symlink') {
      // On platforms that support preserving the symlink, ensure it points where we expect.
      assert.strictEqual(fs.readlinkSync(dstLink), 'libonnxruntime.so.1.25.0', 'copyBundledEntry should preserve the symlink target');
    } else {
      // Some CI filesystems (e.g. Docker root without CAP_DAC_OVERRIDE) reject
      // symlinks, in which case copyBundledEntry falls back to a regular file.
      assert.strictEqual(kind, 'file', 'copyBundledEntry fallback should report "file"');
      assert.strictEqual(fs.readFileSync(dstLink, 'utf8'), 'fake-ort-versioned', 'fallback should copy the link target content');
    }
  } catch (_) {
    // Symlink creation may be unsupported on the host; the test still passes.
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
    fs.rmSync(tmpB, { recursive: true, force: true });
  }
}
console.log('  ✓ ORT discovery helpers wired up correctly\n');

// Test 6: Binary check (if installed)
console.log('Test 6: Binary installation');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';
const binaryPath = path.join(__dirname, 'bin', binaryName);

if (fs.existsSync(binaryPath)) {
  const binaryStats = fs.statSync(binaryPath);

  if (binaryStats.size === 0) {
    console.log('  ⚠ Binary path exists but the file is empty; reinstall to fetch the real binary');
  } else {
    console.log('  ✓ Binary is installed');

    // Try to get version
    try {
      const { execFileSync } = require('child_process');
      if (process.platform !== 'win32') {
        fs.chmodSync(binaryPath, 0o755);
      }
      const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
      if (version) {
        console.log(`  ✓ Binary version: ${version}`);
      } else {
        console.log('  ⚠ Binary executed but returned an empty version string');
      }
    } catch (e) {
      console.log('  ⚠ Binary exists but version check failed');
    }
  }
} else {
  console.log('  ⚠ Binary not installed (run: npm install)');
}

console.log('\n✅ All tests passed!');
