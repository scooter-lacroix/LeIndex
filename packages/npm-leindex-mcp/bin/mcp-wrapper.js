#!/usr/bin/env node

/**
 * LeIndex MCP Wrapper
 *
 * Launches the LeIndex binary in MCP stdio mode.
 * This wrapper is used by MCP clients to communicate with LeIndex.
 * Sets up the environment so the ONNX worker can discover bundled model
 * assets and the bundled ONNX Runtime shared library.
 *
 * VAL-NPM-003: ORT_DYLIB_PATH is set from the bundled `lib/` directory
 * before spawning, so the worker reliably loads the bundled ORT
 * regardless of any system ORT installation.
 * VAL-NPM-007: If ORT cannot be resolved, the spawned binary still
 * starts in TF-IDF fallback mode (handled inside the worker via the
 * discovery chain), so the MCP connection stays alive.
 */

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname);
const MODELS_DIR = path.join(__dirname, '..', 'models');
const LIB_DIR = path.join(__dirname, '..', 'lib');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';

/**
 * The unversioned ONNX Runtime shared library name on the current platform.
 * Mirrors `ort_lib_names()` in `crates/leindex-embed/src/ort_discovery.rs`.
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

/**
 * Locate the bundled ONNX Runtime shared library under the package `lib/`
 * directory. Returns the absolute path to the matching file, or `null` if
 * the lib directory or the library is not present (e.g., the user deleted
 * `lib/` or the bundle did not include ORT).
 *
 * VAL-NPM-003: the result feeds `env.ORT_DYLIB_PATH` when spawning leindex.
 */
function findBundledOrt() {
  if (!fs.existsSync(LIB_DIR)) {
    return null;
  }
  for (const name of getOrtLibNames()) {
    const candidate = path.join(LIB_DIR, name);
    try {
      // Use lstatSync so symlinks like `libonnxruntime.so -> libonnxruntime.so.1`
      // still resolve to themselves here (the symlink exists), letting the
      // dynamic loader follow the link at runtime.
      if (fs.existsSync(candidate) && fs.lstatSync(candidate).size > 0) {
        return candidate;
      }
    } catch (_) {
      // Ignore individual candidate failures; try the next name.
    }
  }
  return null;
}

// Allow override with environment variable for testing/development
// VAL-NPM-012: LEINDEX_BINARY_PATH makes the wrapper spawn a custom-built
// leindex instead of the downloaded bundle (used for CI and local dev).
const binaryPath = process.env.LEINDEX_BINARY_PATH
  ? path.resolve(process.env.LEINDEX_BINARY_PATH)
  : path.join(BIN_DIR, binaryName);

// Check if binary exists
if (!fs.existsSync(binaryPath)) {
  console.error('❌ LeIndex binary not found:', binaryPath);
  console.error('   Run: npm install');
  console.error('   Or install manually: cargo install leindex');
  console.error('   Or set LEINDEX_BINARY_PATH to use a custom binary');
  process.exit(1);
}

// Prepare environment for worker discovery
const env = Object.assign({}, process.env);

// Point the worker to bundled models if available and not already overridden
if (fs.existsSync(MODELS_DIR) && !env.LEINDEX_MODEL_PATH) {
  env.LEINDEX_MODEL_PATH = MODELS_DIR;
}

// VAL-NPM-003: point the worker at the bundled ORT shared library. We
// never overwrite an explicit user override so manual ORT selection
// still wins. When the bundled lib/ is missing we let the worker run
// its full discovery chain, which falls back to TF-IDF if ORT cannot
// be loaded anywhere (VAL-NPM-007).
const bundledOrt = findBundledOrt();
if (bundledOrt && !env.ORT_DYLIB_PATH) {
  env.ORT_DYLIB_PATH = bundledOrt;
}

// Launch LeIndex in MCP mode
const leindex = spawn(binaryPath, ['mcp', '--stdio'], {
  stdio: ['pipe', 'pipe', 'pipe'],
  env: env
});

// Forward stdin to LeIndex
process.stdin.pipe(leindex.stdin);

// Forward LeIndex stdout to process stdout
leindex.stdout.pipe(process.stdout);

// Handle stderr (log but don't forward to avoid protocol corruption)
leindex.stderr.on('data', (data) => {
  // Write to stderr of the wrapper process
  process.stderr.write(data);
});

// Handle process exit
leindex.on('exit', (code) => {
  process.exit(code);
});

leindex.on('error', (err) => {
  console.error('❌ Failed to start LeIndex:', err.message);
  process.exit(1);
});

// Handle wrapper process signals
process.on('SIGINT', () => {
  leindex.kill('SIGINT');
});

process.on('SIGTERM', () => {
  leindex.kill('SIGTERM');
});

// Export helpers for unit testing
module.exports = { findBundledOrt, getOrtLibNames, LIB_DIR, MODELS_DIR };
