import { describe, expect, it } from 'vitest';

import type { StartupArtifact } from '@/lib/models/startup';

import { StartupCommandUtils } from './startup-command';

function artifact(arguments_: string[]): StartupArtifact {
  return {
    itemId: 'fixture',
    sourceId: 'fixture',
    sourceKind: 'registryRun',
    scope: 'currentUser',
    triggers: ['userLogon'],
    displayName: 'Fixture',
    configurationPath: null,
    target: { kind: 'executable', path: '/Applications/Fixture', executableName: 'Fixture', arguments: arguments_ },
    ownerName: null,
    publisher: null,
    summary: null,
    summarySource: 'unavailable',
    version: null,
    iconPath: null,
    identityConfidence: 'unresolved',
    configuredState: 'enabled',
    runtimeState: 'unknown',
    controlCapability: 'toggleable',
    trust: 'unknown',
    modifiedAtMs: null,
    diagnostics: [],
    removableOrphan: false,
  };
}

describe('StartupCommandUtils', () => {
  it('redacts inline and following sensitive values by default', () => {
    expect(StartupCommandUtils.display(artifact(['--token=private', '--password', 'secret']), false)).toBe(
      '/Applications/Fixture --token=•••• --password ••••'
    );
  });

  it('reveals arguments only after an explicit request', () => {
    expect(StartupCommandUtils.display(artifact(['--token=private']), true)).toBe(
      '/Applications/Fixture --token=private'
    );
  });
});
