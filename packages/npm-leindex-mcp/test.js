#!/usr/bin/env node

/**
 * Simple test suite for @leindex/mcp package
 */

const assert = require('assert');
const fs = require('fs');
const path = require('path');

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
console.log('  ✓ Package.json is valid\n');

// Test 3: Binary check (if installed)
console.log('Test 3: Binary installation');
const binaryName = process.platform === 'win32' ? 'leindex.exe' : 'leindex';
const binaryPath = path.join(__dirname, 'bin', binaryName);

if (fs.existsSync(binaryPath)) {
  console.log('  ✓ Binary is installed');
  
  // Try to get version
  try {
    const { execSync } = require('child_process');
    const version = execSync(`"${binaryPath}" --version`, { encoding: 'utf8' }).trim();
    console.log(`  ✓ Binary version: ${version}`);
  } catch (e) {
    console.log('  ⚠ Binary exists but version check failed');
  }
} else {
  console.log('  ⚠ Binary not installed (run: npm install)');
}

console.log('\n✅ All tests passed!');
