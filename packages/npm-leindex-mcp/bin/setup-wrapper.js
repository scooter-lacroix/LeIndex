#!/usr/bin/env node

/**
 * LeIndex MCP - Setup Wrapper
 *
 * VAL-NPM-005 / VAL-NPM-006: `npm run setup` invokes `leindex setup`
 * using the bundled binary, with stdout/stderr visible to the user and
 * any additional flags forwarded. Example:
 *
 *   npm run setup                  # interactive wizard
 *   npm run setup -- --check       # status report
 *   npm run setup -- --neural --cpu
 *
 * Run synchronously because setup is a one-shot command, not a long-running
 * server. The same env detection the MCP wrapper uses (bundled lib/ +
 * models/) is applied so `setup` can mutate the user-level config while
 * still seeing the bundled assets. Exit status matches the underlying
 * binary so CI gates and `&&` chains behave correctly.
 */

const { spawnSync } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname);
const MODELS_DIR = path.join(__dirname, '..', 'models');
const LIB_DIR = path.join(__dirname, '..', 'lib');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';

// VAL-NPM-012: LEINDEX_BINARY_PATH also applies to the setup wrapper so
// CI and local development can run setup against a custom-built binary.
const binaryPath = process.env.LEINDEX_BINARY_PATH
  ? path.resolve(process.env.LEINDEX_BINARY_PATH)
  : path.join(BIN_DIR, binaryName);

if (!fs.existsSync(binaryPath)) {
  console.error('❌ LeIndex binary not found:', binaryPath);
  console.error('   Run: npm install');
  console.error('   Or install manually: cargo install leindex');
  console.error('   Or set LEINDEX_BINARY_PATH to use a custom binary');
  process.exit(1);
}

// Build the env in lockstep with bin/mcp-wrapper.js so setup detects the
// same bundled ORT + models the MCP server uses.
const env = Object.assign({}, process.env);

if (fs.existsSync(MODELS_DIR) && !env.LEINDEX_MODEL_PATH) {
  env.LEINDEX_MODEL_PATH = MODELS_DIR;
}

if (fs.existsSync(LIB_DIR) && !env.ORT_DYLIB_PATH) {
  const ortNames = process.platform === 'win32'
    ? ['onnxruntime.dll']
    : process.platform === 'darwin'
      ? ['libonnxruntime.dylib']
      : ['libonnxruntime.so'];
  for (const name of ortNames) {
    const candidate = path.join(LIB_DIR, name);
    try {
      if (fs.existsSync(candidate) && fs.lstatSync(candidate).size > 0) {
        env.ORT_DYLIB_PATH = candidate;
        break;
      }
    } catch (_) {
      // Try the next candidate.
    }
  }
}

// Forward everything after the wrapper script name. With `npm run setup`,
// npm passes additional args after `--`, so `process.argv.slice(2)` captures
// user flags like `--neural --cpu` or `--check`.
const userArgs = process.argv.slice(2);

const result = spawnSync(binaryPath, ['setup', ...userArgs], {
  stdio: 'inherit',
  env
});

if (result.error) {
  console.error('❌ Failed to start LeIndex setup:', result.error.message);
  process.exit(1);
}

// Preserve the binary's exit code so CI / `npm run` chains see the real
// outcome (e.g. `--check` returns non-zero when not configured).
process.exit(result.status === null ? 1 : result.status);
