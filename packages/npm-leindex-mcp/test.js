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
assert(!pkg.files.includes('bin/'), 'Published package should not ship downloaded runtime binaries');
console.log('  ✓ Package.json is valid\n');

// Test 3: Installer release selection defaults to latest
console.log('Test 3: Installer release selection');
delete process.env.LEINDEX_BINARY_CHANNEL;
delete process.env.LEINDEX_BINARY_VERSION;
assert.strictEqual(installer.getRequestedRelease(), 'latest', 'Default selector should be latest');
assert.strictEqual(installer.getAssetName('1.5.2', 'linux', 'x86_64'), 'leindex-1.5.2-linux-x86_64');
console.log('  ✓ Installer resolves latest by default\n');

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
assert(!fs.readFileSync(path.join(__dirname, 'index.js'), 'utf8').includes("args.join(' ')"), 'Wrapper should not shell-interpolate args');
console.log('  ✓ Wrapper executes with argv arrays\n');

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
