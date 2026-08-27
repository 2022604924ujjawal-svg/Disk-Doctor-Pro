# Rust crates

This directory contains reusable Rust capabilities:

- `mangodisk-core` owns product domains, use cases, rules, indexing, cleanup,
  history, and reporting.
- `mangodisk-platform` implements macOS and Windows contracts for volumes,
  paths, links, system exclusions, application inventory, and permanent deletion.
- `mangodisk-cli` is a sibling adapter over Core use cases.

The Tauri crate only assembles the application, converts command arguments,
and forwards progress events. It does not own platform policy or scanning
behavior.
