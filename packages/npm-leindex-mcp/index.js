#!/usr/bin/env node

/**
 * LeIndex MCP - Main Entry Point
 * 
 * This package provides LeIndex MCP server functionality through npm.
 * The binary is automatically downloaded during installation.
 */

const { execFileSync, spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname, 'bin');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';
const binaryPath = path.join(BIN_DIR, binaryName);

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
  
  return spawn(bin, ['mcp', '--stdio'], {
    stdio: ['pipe', 'pipe', 'pipe']
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
  exec,
  startMcpServer,
  getVersion
};

// CLI usage
if (require.main === module) {
  console.log('LeIndex MCP Package');
  console.log('Version:', getVersion());
  console.log('Binary:', getBinaryPath());
  console.log('\nUse "npx @leindex/mcp" in your MCP configuration.');
}
