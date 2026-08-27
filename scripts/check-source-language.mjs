/**
 * Prevents untranslated developer prose and user-facing text from leaking
 * into production source. Generated UI, locale resources, and explicit test
 * fixture strings are handled by narrow exceptions instead of a broad allow.
 *
 * Run with `pnpm check:source-language`. The command exits with a non-zero
 * status and prints the file and line number for every violation.
 */
import { readFileSync, readdirSync } from 'node:fs';
import { extname, join, relative, resolve } from 'node:path';

const projectRoot = resolve(import.meta.dirname, '..');
const sourceRoots = [
  'src',
  'src-tauri/src',
  'src-tauri/xtask/src',
  'src-tauri/crates/mangodisk-cli/src',
  'src-tauri/crates/mangodisk-core/src',
  'src-tauri/crates/mangodisk-core/examples',
  'src-tauri/crates/mangodisk-platform/src',
  'src-tauri/crates/mangodisk-platform/examples',
];
const allowedExtensions = new Set(['.rs', '.ts', '.vue']);
const hanCharacter = /\p{Script=Han}/u;

const violations = [];
for (const sourceRoot of sourceRoots) {
  const absoluteRoot = join(projectRoot, sourceRoot);
  for (const path of collectFiles(absoluteRoot)) {
    const displayPath = relative(projectRoot, path).replaceAll('\\', '/');
    const lines = readFileSync(path, 'utf8').split(/\r?\n/u);
    for (const [index, line] of lines.entries()) {
      if (hanCharacter.test(line) && !isAllowedLocalizedLine(displayPath, line)) {
        violations.push(`${displayPath}:${index + 1}`);
      }
    }
  }
}

if (violations.length > 0) {
  console.error('Source language validation failed. Move UI text to locales or use English developer text:');
  for (const violation of violations) console.error(`- ${violation}`);
  process.exitCode = 1;
} else {
  console.log('Source language validation passed');
}

function collectFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return collectFiles(path);
    return entry.isFile() && allowedExtensions.has(extname(entry.name)) ? [path] : [];
  });
}

function isAllowedLocalizedLine(path, line) {
  // Generated UI is not project-owned source. Localized test fixtures remain
  // string literals; Chinese test descriptions and comments still fail.
  if (path.startsWith('src/components/ui/')) return true;
  return path.endsWith('.test.ts') && line.includes("'");
}
