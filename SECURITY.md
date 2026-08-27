# Security Policy

## Supported versions

Security updates are provided for the latest MangoDisk release. Older releases
may not receive security fixes, so users should upgrade to the latest available
version.

## Reporting a vulnerability

Do not disclose a suspected vulnerability in a public issue, discussion, or
pull request. Report it privately through GitHub Security Advisories. Include:

- the affected version or commit;
- the operating system and relevant permissions;
- the minimum reproduction steps;
- the expected security boundary;
- the observed impact;
- any logs after removing private paths and file contents.

The maintainers will acknowledge a complete report, assess its severity, and
coordinate disclosure after a fix is available. Cleanup authorization bypasses,
protected-path failures, arbitrary file deletion, command execution, and
sensitive-path disclosure receive priority.

## Security boundaries

MangoDisk treats scan results as untrusted until preflight. Destructive
operations must preserve protected-path checks, link and reparse-point policy,
explicit user intent, execution verification, and safe failure when platform
capabilities are unavailable.

Reports should never include credentials, personal file contents, complete
private paths, or an unredacted copy of an in-memory scan result.
