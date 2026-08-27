/**
 * Protects the boundary between generated UI primitives and MangoDisk product
 * code. Generated components must remain presentation-only, while localized
 * behavior and application workflows belong in project-owned wrappers.
 *
 * Run with `pnpm check:generated-ui`. The command exits with a non-zero status
 * and lists every invalid dependency or direct dialog usage it finds.
 */
import { readFileSync, readdirSync } from 'node:fs';
import { extname, join, relative, resolve } from 'node:path';

const projectRoot = resolve(import.meta.dirname, '..');
const generatedUiRoot = join(projectRoot, 'src/components/ui');
const sourceExtensions = new Set(['.ts', '.vue']);
const forbiddenDependencies = [
  { pattern: /from\s+['"]vue-i18n['"]/u, reason: 'localization belongs in a project-owned wrapper' },
  {
    pattern: /from\s+['"]@\/components\/(?:custom|icons)(?:\/|['"])/u,
    reason: 'project components must wrap generated UI instead of being imported by it',
  },
  {
    pattern: /from\s+['"]@\/(?:layouts|pages|stores)(?:\/|['"])/u,
    reason: 'application workflows must not enter generated UI',
  },
  {
    pattern: /from\s+['"]@\/lib\/(?:models|services)(?:\/|['"])/u,
    reason: 'business protocols and side effects belong outside generated UI',
  },
];

const violations = [];
for (const path of collectFiles(generatedUiRoot)) {
  const content = readFileSync(path, 'utf8');
  for (const dependency of forbiddenDependencies) {
    if (dependency.pattern.test(content)) {
      const displayPath = relative(projectRoot, path).replaceAll('\\', '/');
      violations.push(`${displayPath}: ${dependency.reason}`);
    }
  }
}

// The generated dialog content intentionally stays presentation-neutral. All
// application call sites use the wrapper that owns the localized close action.
const dialogWrapperPath = 'src/components/custom/md-dialog-content.vue';
const generatedDialogImport =
  /import\s*\{[^}]*\bDialogContent\b[^}]*\}\s*from\s*['"]@\/components\/ui\/dialog['"]|from\s*['"]@\/components\/ui\/dialog\/DialogContent\.vue['"]/su;
for (const path of collectFiles(join(projectRoot, 'src'))) {
  const displayPath = relative(projectRoot, path).replaceAll('\\', '/');
  if (displayPath === dialogWrapperPath || displayPath.startsWith('src/components/ui/')) continue;
  if (generatedDialogImport.test(readFileSync(path, 'utf8'))) {
    violations.push(`${displayPath}: use ${dialogWrapperPath} for application dialogs`);
  }
}

if (violations.length > 0) {
  console.error('Generated UI boundary validation failed:');
  for (const violation of violations) console.error(`- ${violation}`);
  process.exitCode = 1;
} else {
  console.log('Generated UI boundary validation passed');
}

function collectFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return collectFiles(path);
    return entry.isFile() && sourceExtensions.has(extname(entry.name)) ? [path] : [];
  });
}
