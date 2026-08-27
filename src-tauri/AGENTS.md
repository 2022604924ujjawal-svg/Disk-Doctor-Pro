# Rust and Tauri Guidelines

This file applies only to `src-tauri/` and inherits the repository-wide rules in [`../AGENTS.md`](../AGENTS.md).

## Workspace boundaries

- `crates/mangodisk-core`: platform-neutral product domains, use cases, safety policy, persistence, and reporting. It must not depend on Tauri or a WebView.
- `crates/mangodisk-platform`: OS contracts and macOS/Windows implementations. It reports typed capabilities and safe fallbacks; it does not decide product workflows.
- `src/`: thin Tauri adapter. Commands validate transport input, call Core, translate typed errors, and publish events.
- `plugins/`: isolated Tauri plugin integration only when a capability genuinely needs a plugin.
- A formal CLI is a separate console binary over Core. Engineering-only maintenance commands belong in `xtask`, not in the GUI binary or public CLI.

Core is organized around `cleanup`, `storage`, `applications`, `filesystem`, `history`, and `reporting`. `storage::analysis`, `storage::large_files`, and `storage::duplicates` remain separate implementations and must not become a new giant `StorageService`.

## Rust organization and naming

- Files and modules use `snake_case`; types use precise domain nouns; functions use verbs that state observable behavior.
- `duplicates` names the domain. `duplicate_files` is valid only for a file-specific entity, use case, adapter command, or wire event.
- Avoid `utils` for domain behavior. Put a helper beside its owner or in a narrowly named infrastructure module.
- Keep visibility minimal. A new `pub` or `pub(crate)` API must represent a stable collaboration boundary, not an expedient way around module ownership.
- Prefer small typed request/result structures over long parameter lists and unrelated tuples.
- Source comments, logs, errors, tests, and assertions must be clear and consistent. Explain safety assumptions, ownership, performance tradeoffs, and fallback reasons.

## Errors, logs, and protocols

- Domain and platform code return typed errors or stable error codes. Convert to Tauri transport errors only in the adapter.
- Logs use the centralized Rust logging entry point and stable domain/event/field names. Log operation IDs, counts, timings, fallback reasons, and error digests—not private full paths or file contents.
- Persisted and cross-process structures require an explicit schema version and a documented read, migrate, rebuild, or reject policy.
- Derived indexes may be rebuilt on incompatible versions. User history and settings require backward-compatible readers or an explicit migration.
- Keep command names and event payloads versionable. Do not retain permanent old/new aliases after a migration window.

## Rules and cleanup safety

- Declarative filesystem rules live under `crates/mangodisk-core/rules/filesystem`; project artifact rules live under `crates/mangodisk-core/rules/project-artifacts`.
- Contributors should add validated TOML for ordinary cleanup coverage without editing Rust match branches.
- Rule resources expose stable machine data only. UI localization belongs to frontend locale files.
- A specialized cleaner is justified only for a system command, application API, structured package, or safety verification that TOML cannot express.
- All destructive flows preserve preview/dry-run, protected-path validation, link/reparse-point policy, preflight, explicit user intent, execution verification, and cache synchronization.
- Missing permission, unavailable tools, unsupported change tracking, and platform uncertainty must fail closed or use a documented slower safe path.

Read [`crates/mangodisk-core/rules/README.md`](crates/mangodisk-core/rules/README.md) before changing cleanup rules or their filesystem schema.

## Platform code

- Define the contract before moving an implementation. Platform facts must not import cleanup or UI concepts.
- Keep `cfg` at narrow module or item boundaries. A platform-only field, import, or accessor must be conditionally compiled rather than silenced with dead-code allowances.
- Native fast paths require a correct fallback and diagnostics that distinguish fast, cached, incremental, and full traversal behavior.
- Tests that cannot run on the current OS may use explicit `cfg`; do not make a platform test pass by replacing its behavior with a mock in production code.
- Privileged operations require a separately reviewed capability boundary. Do not expand Tauri permissions or simulate a privileged helper inside an ordinary cleaner.

## Tauri adapter and plugins

- Command handlers remain async adapters and contain no scan, cleanup, or persistence algorithms.
- Register every command, permission, capability, frontend binding, and plugin initialization in the same change.
- Generated plugin or Specta bindings are generated artifacts; regenerate them through their source tool instead of editing them by hand.
- Keep capability scopes minimal and platform-specific where appropriate.
- App startup must not perform long scans or blocking filesystem work before the first window is rendered.

## Validation

For Rust changes, run:

```sh
pnpm rust:fmt:check
pnpm rust:clippy
pnpm rust:check
cargo test --manifest-path src-tauri/Cargo.toml -p mangodisk-core
```

Run `pnpm check` before submitting or merging a change. Cross-platform changes must run applicable checks in macOS and Windows environments. If a platform is unavailable, document the unvalidated scope. Changes to rules, scan engines, indexes, persistence, native fast paths, or performance require tests or a reproducible measurement appropriate to the affected behavior; keep raw machine evidence and private datasets outside the repository.
