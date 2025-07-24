#!/usr/bin/env node

console.log('🚀 Hello from Banderole test app! (UPDATED)');
console.log('Node.js version:', process.version);
console.log('Platform:', process.platform);
console.log('Architecture:', process.arch);

// Test command line arguments
if (process.argv.length > 2) {
    console.log('Arguments received:', process.argv.slice(2));
}

// Test environment
console.log('Current working directory:', process.cwd());

// Test file system access
const fs = require('fs');
const path = require('path');

try {
    const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
    console.log('📦 Package name:', packageJson.name);
    console.log('📦 Package version:', packageJson.version);
} catch (err) {
    console.log('❌ Failed to read package.json:', err.message);
}

// Simple functionality test
const message = 'Banderole bundling works perfectly!';
console.log('✅', message);

// Test async functionality
setTimeout(() => {
    console.log('⏰ Async operation completed');
    process.exit(0);
}, 100);