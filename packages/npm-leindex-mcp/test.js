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
assert.strictEqual(typeof installer.copyRegularBundledFile, 'function', 'install.js should expose copyRegularBundledFile');
assert.strictEqual(typeof installer.copyRegularFileNoFollow, 'function', 'install.js should expose copyRegularFileNoFollow');
assert.strictEqual(typeof installer.assertSafeArchiveFileName, 'function', 'install.js should expose assertSafeArchiveFileName');
assert.strictEqual(typeof installer.assertNoUnsafeExtractedSymlinks, 'function', 'install.js should expose assertNoUnsafeExtractedSymlinks');
assert.strictEqual(typeof installer.assertSafeSymlinkTarget, 'function', 'install.js should expose assertSafeSymlinkTarget');
assert.strictEqual(typeof installer.isSafeArchiveMemberName, 'function', 'install.js should expose isSafeArchiveMemberName');
assert.strictEqual(typeof installer.isOrtBundleLibraryName, 'function', 'install.js should expose isOrtBundleLibraryName');
assert.strictEqual(typeof installer.isOrtRuntimeLibraryName, 'function', 'install.js should expose isOrtRuntimeLibraryName');
assert.strictEqual(typeof installer.hasBundledOrtRuntime, 'function', 'install.js should expose hasBundledOrtRuntime');
assert.strictEqual(typeof installer.hasRequiredModelAssets, 'function', 'install.js should expose hasRequiredModelAssets');
assert.strictEqual(typeof installer.bundledAssetsComplete, 'function', 'install.js should expose bundledAssetsComplete');
assert.strictEqual(installer.LIB_DIR, path.join(__dirname, 'lib'), 'install.js LIB_DIR should point at <pkg>/lib');
{
  const names = installer.getOrtLibNames();
  const expected = process.platform === 'win32' ? 'onnxruntime.dll'
    : process.platform === 'darwin' ? 'libonnxruntime.dylib'
    : 'libonnxruntime.so';
  assert(Array.isArray(names) && names.includes(expected), `getOrtLibNames() should include ${expected} on ${process.platform}`);
  assert(installer.isOrtRuntimeLibraryName(expected), `${expected} should be accepted as the ORT runtime`);
  const providerHelper = process.platform === 'win32' ? 'onnxruntime_providers_shared.dll'
    : process.platform === 'darwin' ? 'libonnxruntime_providers_shared.dylib'
    : 'libonnxruntime_providers_shared.so';
  assert(!installer.isOrtRuntimeLibraryName(providerHelper), 'provider helper libraries are not the ORT runtime');
  assert(installer.isOrtBundleLibraryName(expected), `${expected} should be accepted as a bundle library`);
  assert(installer.isOrtBundleLibraryName(providerHelper), 'provider helper libraries should be accepted as bundle libraries');
  assert(!installer.isOrtBundleLibraryName('README.txt'), 'non-library files should not be accepted as bundle libraries');
  assert.strictEqual(installer.assertSafeArchiveFileName('leindex-1.8.3-linux-x86_64.tar.gz'), 'leindex-1.8.3-linux-x86_64.tar.gz');
  assert.throws(() => installer.assertSafeArchiveFileName('../leindex.tar.gz'), /Unsafe release asset name/);
  assert.throws(() => installer.assertSafeArchiveFileName('..\\leindex.zip'), /Unsafe release asset name/);
  assert(installer.isSafeArchiveMemberName('leindex-1.8.3-linux-x86_64/bin/leindex'));
  assert(!installer.isSafeArchiveMemberName('../../etc/passwd'));
  assert(!installer.isSafeArchiveMemberName('/tmp/pwned'));
  assert(!installer.isSafeArchiveMemberName('C:\\temp\\pwned'));
}
{
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-assets-'));
  const libDir = path.join(tmpRoot, 'lib');
  const modelsDir = path.join(tmpRoot, 'models');
  try {
    fs.mkdirSync(libDir, { recursive: true });
    fs.mkdirSync(modelsDir, { recursive: true });
    assert.strictEqual(installer.bundledAssetsComplete(libDir, modelsDir), false, 'empty bundle assets should not satisfy installer fast path');

    const runtimeName = process.platform === 'win32' ? 'onnxruntime.dll'
      : process.platform === 'darwin' ? 'libonnxruntime.dylib'
      : 'libonnxruntime.so';
    fs.writeFileSync(path.join(libDir, runtimeName), 'fake-ort-runtime');
    assert.strictEqual(installer.hasBundledOrtRuntime(libDir), true, 'ORT runtime library should be detected');
    assert.strictEqual(installer.bundledAssetsComplete(libDir, modelsDir), false, 'missing model assets should prevent installer fast path');

    for (const file of ['qwen3-embed-0.6b.onnx', 'tokenizer.json', 'config.json']) {
      fs.writeFileSync(path.join(modelsDir, file), 'fake-model-asset');
    }
    assert.strictEqual(installer.hasRequiredModelAssets(modelsDir), true, 'required model assets should be detected');
    assert.strictEqual(installer.bundledAssetsComplete(libDir, modelsDir), true, 'complete ORT and model assets should satisfy installer fast path');
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
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
    let symlinkCreated = false;
    try {
      fs.symlinkSync('libonnxruntime.so.1.25.0', link);
      symlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (!symlinkCreated) {
      console.log('  ⚠ Symlink creation unsupported; skipped symlink preservation assertion');
    } else {
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
    }
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
    fs.rmSync(tmpB, { recursive: true, force: true });
  }
}
{
  // Extraction boundary audit should reject symlinks that escape the extracted
  // bundle, before any later copy step can dereference them.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-extract-'));
  const safeTarget = path.join(tmpA, 'inside-target');
  const safeLink = path.join(tmpA, 'inside-link');
  const unsafeLink = path.join(tmpA, 'outside-link');
  try {
    fs.writeFileSync(safeTarget, 'inside bundle');
    let safeSymlinkCreated = false;
    let unsafeSymlinkCreated = false;
    try {
      fs.symlinkSync('inside-target', safeLink);
      safeSymlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (safeSymlinkCreated) {
      installer.assertNoUnsafeExtractedSymlinks(tmpA);
      const rootLink = `${tmpA}-link`;
      try {
        fs.symlinkSync(tmpA, rootLink, 'dir');
        installer.assertNoUnsafeExtractedSymlinks(rootLink);
      } catch (err) {
        const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
        if (!unsupported) throw err;
      } finally {
        fs.rmSync(rootLink, { force: true });
      }
    }
    try {
      fs.symlinkSync('../outside-target', unsafeLink);
      unsafeSymlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (unsafeSymlinkCreated) {
      assert.throws(
        () => installer.assertNoUnsafeExtractedSymlinks(tmpA),
        /Unsafe extracted symlink escapes bundle/,
        'post-extract audit should reject symlinks that escape the extraction root'
      );
    }
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
  }
}
{
  // copyRegularBundledFile should not follow a symlink swapped into the source
  // path; on platforms with O_NOFOLLOW this rejects at open time.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-nofollow-'));
  const tmpB = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-nofollow-'));
  const target = path.join(tmpA, 'target');
  const link = path.join(tmpA, 'payload');
  try {
    fs.writeFileSync(target, 'target data');
    let symlinkCreated = false;
    try {
      fs.symlinkSync('target', link);
      symlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (symlinkCreated && fs.constants.O_NOFOLLOW) {
      assert.throws(
        () => installer.copyRegularFileNoFollow(link, path.join(tmpB, 'payload'), 0o644),
        /ELOOP|too many symbolic links|Source is not a regular file/i,
        'copyRegularFileNoFollow should reject symlink sources'
      );
    }
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
    fs.rmSync(tmpB, { recursive: true, force: true });
  }
}
{
  // Binary/model payloads must be regular files. Unlike ORT lib entries, they
  // never need symlink preservation, and dereferencing bundle symlinks can read
  // outside the extracted archive.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-payload-'));
  const tmpB = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-payload-'));
  const outside = path.join(tmpA, '..', `leindex-outside-${process.pid}`);
  const link = path.join(tmpA, 'qwen3-embed-0.6b.onnx');
  try {
    fs.writeFileSync(outside, 'outside bundle');
    let symlinkCreated = false;
    try {
      fs.symlinkSync(outside, link);
      symlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (symlinkCreated) {
      assert.throws(
        () => installer.copyRegularBundledFile(link, path.join(tmpB, 'qwen3-embed-0.6b.onnx'), 'model'),
        /Unsafe model entry is a symlink/,
        'copyRegularBundledFile should reject model symlinks instead of dereferencing them'
      );
    }
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
    fs.rmSync(tmpB, { recursive: true, force: true });
    fs.rmSync(outside, { force: true });
  }
}
{
  // copyBundledEntry must reject symlinks that escape the bundle lib directory.
  const tmpA = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const tmpB = fs.mkdtempSync(path.join(os.tmpdir(), 'leindex-mcp-lib-'));
  const link = path.join(tmpA, 'libonnxruntime.so');
  try {
    let symlinkCreated = false;
    try {
      fs.symlinkSync('../../etc/passwd', link);
      symlinkCreated = true;
    } catch (err) {
      const unsupported = ['EPERM', 'EACCES', 'ENOTSUP', 'EINVAL'].includes(err && err.code);
      if (!unsupported) throw err;
    }
    if (symlinkCreated) {
      assert.throws(
        () => installer.copyBundledEntry(link, path.join(tmpB, 'libonnxruntime.so')),
        /Unsafe symlink target/,
        'copyBundledEntry should reject symlinks that traverse outside the source directory'
      );
    }
  } finally {
    fs.rmSync(tmpA, { recursive: true, force: true });
    fs.rmSync(tmpB, { recursive: true, force: true });
  }
}
console.log('  ✓ ORT discovery helpers wired up correctly\n');

// Test 6: Binary version parity
console.log('Test 6: Binary version parity');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';
const binaryPath = path.join(__dirname, 'bin', binaryName);

if (fs.existsSync(binaryPath)) {
  const binaryStats = fs.statSync(binaryPath);

  if (binaryStats.size === 0) {
    console.log('  ⚠ Binary path exists but the file is empty; reinstall to fetch the real binary');
  } else {
    console.log('  ✓ Binary is installed');

    try {
      const { execFileSync } = require('child_process');
      if (process.platform !== 'win32') {
        fs.chmodSync(binaryPath, 0o755);
      }
      const version = execFileSync(binaryPath, ['--version'], { encoding: 'utf8' }).trim();
      const expectedVersion = pkg.version;
      const match = version.match(/leindex\s+([0-9]+\.[0-9]+\.[0-9]+)/);
      if (!match) {
        console.error(`  ❌ Binary version output is not parseable: ${version}`);
        process.exit(1);
      }
      if (match[1] !== expectedVersion) {
        console.error(`  ❌ Binary version ${match[1]} does not match package version ${expectedVersion}`);
        process.exit(1);
      }
      console.log(`  ✓ Binary version matches package: ${version}`);
    } catch (e) {
      console.error(`  ❌ Binary exists but version check failed: ${e.message}`);
      process.exit(1);
    }
  }
} else {
  console.log('  ⚠ Binary not installed (run: npm install)');
}

console.log('\n✅ All tests passed!');
