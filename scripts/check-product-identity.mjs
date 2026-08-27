/**
 * Verifies that public product names, bundle identifiers, binary names, and
 * required brand assets remain consistent across the frontend, Tauri, and CLI
 * entry points.
 *
 * Run with `pnpm check:identity`. The command throws with the mismatched field
 * or missing asset when a product identity regression is detected.
 */
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const projectRoot = process.cwd();

function readJson(relativePath) {
  return JSON.parse(readFileSync(join(projectRoot, relativePath), 'utf8'));
}

function readText(relativePath) {
  return readFileSync(join(projectRoot, relativePath), 'utf8');
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label} must be ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}.`);
  }
}

function assertContains(content, expected, label) {
  if (!content.includes(expected)) {
    throw new Error(`${label} must contain ${JSON.stringify(expected)}.`);
  }
}

function assertExists(relativePath) {
  if (!existsSync(join(projectRoot, relativePath))) {
    throw new Error(`Required product asset is missing: ${relativePath}.`);
  }
}

const packageJson = readJson('package.json');
const tauriConfig = readJson('src-tauri/tauri.conf.json');
const tauriManifest = readText('src-tauri/Cargo.toml');
const cliManifest = readText('src-tauri/crates/mangodisk-cli/Cargo.toml');
const readme = readText('README.md');
const indexHtml = readText('index.html');
const tauriLibrary = readText('src-tauri/src/lib.rs');
const coreLibrary = readText('src-tauri/crates/mangodisk-core/src/lib.rs');
const tauriMain = readText('src-tauri/src/main.rs');
const macosChangeTracking = readText('src-tauri/crates/mangodisk-platform/src/macos/change_tracking.rs');

assertEqual(packageJson.name, 'mangodisk', 'npm package name');
assertEqual(tauriConfig.productName, 'MangoDisk', 'Tauri product name');
assertEqual(tauriConfig.identifier, 'app.mangodisk.desktop', 'Tauri bundle identifier');
assertContains(
  coreLibrary,
  'pub const APPLICATION_IDENTIFIER: &str = "app.mangodisk.desktop";',
  'Core application identifier'
);
assertEqual(tauriConfig.mainBinaryName, 'MangoDisk', 'Tauri binary name');
assertEqual(tauriConfig.app?.windows?.[0]?.title, 'MangoDisk', 'main window title');

assertContains(tauriManifest, 'name = "mangodisk"', 'Tauri package manifest');
assertContains(tauriManifest, 'name = "mangodisk_lib"', 'Tauri library manifest');
assertContains(cliManifest, 'name = "mangodisk-cli"', 'CLI package manifest');
assertContains(cliManifest, 'name = "mangodisk"', 'CLI binary manifest');

assertExists('public/mangodisk.png');
assertExists('public/mangodisk.svg');
assertExists('src/components/icons/md-icon-mangodisk.vue');

assertContains(readme, 'mangodisk clean', 'README CLI usage');
assertContains(indexHtml, 'MangoDisk', 'HTML application shell');
assertContains(tauriLibrary, 'MangoDisk', 'Tauri library');
assertContains(tauriMain, 'mangodisk_lib', 'Tauri entry point');
assertContains(macosChangeTracking, 'app.mangodisk.cache-dirty-monitor', 'macOS cache monitor identity');

console.log('Product identity is consistent.');
