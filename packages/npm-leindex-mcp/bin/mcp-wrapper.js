#!/usr/bin/env node

/**
 * LeIndex MCP Wrapper
 * 
 * Launches the LeIndex binary in MCP stdio mode.
 * This wrapper is used by MCP clients to communicate with LeIndex.
 */

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BIN_DIR = path.join(__dirname);
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';

// Allow override with environment variable for testing/development
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

// Launch LeIndex in MCP mode
const leindex = spawn(binaryPath, ['mcp', '--stdio'], {
  stdio: ['pipe', 'pipe', 'pipe']
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
