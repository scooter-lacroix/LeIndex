#!/usr/bin/env node

/**
 * LeIndex MCP - Main Entry Point
 * 
 * This package provides LeIndex MCP server functionality through npm.
 * The binary and ONNX Runtime bundle is downloaded during installation.
 * Model provisioning is handled by `leindex setup`.
 */

const { execFileSync, spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname, 'bin');
const LIB_DIR = path.join(__dirname, 'lib');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';
const workerBinaryName = process.platform === 'win32' ? 'leindex-embed.exe' : 'leindex-embed';
const binaryPath = path.join(BIN_DIR, binaryName);
const workerBinaryPath = path.join(BIN_DIR, workerBinaryName);

/**
 * Get the path to the LeIndex binary
 * @returns {string} Path to the LeIndex binary
 */
function getBinaryPath() {
  if (!fs.existsSync(binaryPath)) {
    throw new Error('LeIndex binary not found. Run: npm install');
  }
  return binaryPath;
}

/**
 * Get the path to the LeIndex ONNX worker binary
 * @returns {string|null} Path to the worker binary, or null if not installed
 */
function getWorkerBinaryPath() {
  if (fs.existsSync(workerBinaryPath)) {
    return workerBinaryPath;
  }
  return null;
}

/**
 * Get the path to bundled ORT shared libraries.
 * @returns {string|null} Path to the lib directory, or null if not present
 */
function getLibPath() {
  if (fs.existsSync(LIB_DIR)) {
    return LIB_DIR;
  }
  return null;
}

/**
 * Execute LeIndex with given arguments
 * @param {string[]} args - Arguments to pass to LeIndex
 * @returns {Buffer} Command output
 */
function exec(args = []) {
  const bin = getBinaryPath();
  return execFileSync(bin, args, { encoding: 'utf8' });
}

/**
 * Start LeIndex MCP server
 * @returns {ChildProcess} The spawned process
 */
function startMcpServer() {
  const bin = getBinaryPath();

  const env = Object.assign({}, process.env);
  const libPath = getLibPath();
  if (libPath && !env.ORT_DYLIB_PATH) {
    const ortNames = process.platform === 'win32'
      ? ['onnxruntime.dll']
      : process.platform === 'darwin'
        ? ['libonnxruntime.dylib']
        : ['libonnxruntime.so'];
    for (const name of ortNames) {
      const candidate = path.join(libPath, name);
      try {
        if (fs.existsSync(candidate) && fs.lstatSync(candidate).size > 0) {
          env.ORT_DYLIB_PATH = candidate;
          break;
        }
      } catch (_) { /* try next */ }
    }
  }

  return spawn(bin, ['mcp', '--stdio'], {
    stdio: ['pipe', 'pipe', 'pipe'],
    env: env
  });
}

/**
 * Get LeIndex version
 * @returns {string} Version string
 */
function getVersion() {
  return exec(['--version']).trim();
}

module.exports = {
  getBinaryPath,
  getWorkerBinaryPath,
  getLibPath,
  exec,
  startMcpServer,
  getVersion
};

// CLI usage
if (require.main === module) {
  console.log('LeIndex MCP Package');
  console.log('Version:', getVersion());
  console.log('Binary:', getBinaryPath());
  const workerPath = getWorkerBinaryPath();
  if (workerPath) {
    console.log('Worker:', workerPath);
  }
  const lib = getLibPath();
  if (lib) {
    console.log('Lib:', lib);
  }
  console.log('\nUse "npx @leindex/mcp" in your MCP configuration.');
}
