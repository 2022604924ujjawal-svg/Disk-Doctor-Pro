/**
 * Validates contributor-owned cleanup rule sources before Rust compilation.
 * Rule comments and evidence must use English, and declarative TOML must not
 * contain UI localization fields that couple execution data to presentation.
 *
 * Run with `pnpm check:rule-sources`. The command exits with a non-zero status
 * and reports each source file that violates the rule boundary.
 */
import { readFileSync, readdirSync } from 'node:fs';
import { extname, join, relative, resolve } from 'node:path';

const projectRoot = resolve(import.meta.dirname, '..');
const ruleRoot = join(projectRoot, 'src-tauri/crates/mangodisk-core/rules');
const filesystemRuleRoot = join(ruleRoot, 'filesystem');
const projectRuleRoot = join(ruleRoot, 'project-artifacts');
const cleanupRoot = join(projectRoot, 'src-tauri/crates/mangodisk-core/src/cleanup');
const sourceFiles = [
  ...collectFiles(filesystemRuleRoot, new Set(['.toml'])),
  ...collectFiles(projectRuleRoot, new Set(['.toml'])),
  ...collectFiles(cleanupRoot, new Set(['.rs'])),
  join(projectRoot, 'src-tauri/crates/mangodisk-core/build.rs'),
  join(projectRoot, 'src-tauri/crates/mangodisk-core/src/history/service.rs'),
];
const violations = [];
const hanCharacter = /\p{Script=Han}/u;
const presentationField = /^(?:name_key|description_key|impact_key|locale|i18n)\s*=/mu;

for (const path of sourceFiles) {
  const content = readFileSync(path, 'utf8');
  const developerText = extname(path) === '.toml' ? extractRuleDeveloperText(content) : content;
  if (hanCharacter.test(developerText)) {
    violations.push(`${displayPath(path)} contains non-English developer text`);
  }
  if (extname(path) === '.toml' && presentationField.test(content)) {
    violations.push(`${displayPath(path)} couples execution rules to UI presentation`);
  }
}

if (violations.length) {
  console.error('Cleanup rule source validation failed:');
  for (const violation of violations) console.error(`- ${violation}`);
  process.exitCode = 1;
} else {
  console.log(`Validated ${sourceFiles.length} cleanup rule source files`);
}

function collectFiles(directory, extensions) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return collectFiles(path, extensions);
    return entry.isFile() && extensions.has(extname(entry.name)) ? [path] : [];
  });
}

function displayPath(path) {
  return relative(projectRoot, path).replaceAll('\\', '/');
}

/**
 * Rule values may contain real platform identifiers such as a localized
 * executable name. Those values are machine facts rather than developer or UI
 * prose, so rejecting them would weaken process protection. Comments and
 * verification evidence remain English-only and are the only prose fields in
 * the execution schema.
 */
function extractRuleDeveloperText(content) {
  return content
    .split(/\r?\n/u)
    .filter(line => {
      const trimmed = line.trimStart();
      return trimmed.startsWith('#') || trimmed.startsWith('evidence =');
    })
    .join('\n');
}
