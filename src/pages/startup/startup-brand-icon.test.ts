import { describe, expect, it } from 'vitest';

import googleChromeIconUrl from '@/assets/brands/google-chrome.svg?url';
import microsoft365IconUrl from '@/assets/brands/microsoft-365.svg?url';
import microsoftIconUrl from '@/assets/brands/microsoft.svg?url';
import type { StartupArtifact, StartupOwnerGroup } from '@/lib/models/startup';

import { startupGroupIconUrl } from './startup-brand-icon';

function artifact(overrides: Partial<StartupArtifact> = {}): StartupArtifact {
  return {
    itemId: 'a'.repeat(64),
    sourceId: 'macos.launchd.system_daemons',
    sourceKind: 'launchDaemon',
    scope: 'machine',
    triggers: ['boot'],
    displayName: 'Fixture',
    configurationPath: null,
    target: { kind: 'executable', path: null, executableName: null, arguments: [] },
    ownerName: null,
    publisher: null,
    summary: null,
    summarySource: 'unavailable',
    version: null,
    iconPath: null,
    identityConfidence: 'unresolved',
    configuredState: 'enabled',
    runtimeState: 'unknown',
    controlCapability: 'elevationRequired',
    trust: 'verified',
    modifiedAtMs: null,
    diagnostics: [],
    removableOrphan: false,
    ...overrides,
  };
}

function group(overrides: Partial<StartupOwnerGroup> = {}): StartupOwnerGroup {
  return {
    groupId: 'group-1',
    name: 'Fixture',
    publisher: null,
    summary: null,
    summarySource: 'unavailable',
    version: null,
    iconPath: null,
    identityConfidence: 'unresolved',
    itemIds: ['a'.repeat(64)],
    sourceKinds: ['launchDaemon'],
    triggers: ['boot'],
    scopes: ['machine'],
    configuredState: 'allEnabled',
    controlState: 'requiresElevation',
    systemItem: false,
    ...overrides,
  };
}

describe('startup brand icons', () => {
  it('prefers a native application icon over a bundled fallback', () => {
    const item = artifact({ displayName: 'com.microsoft.office.licensingV2.helper' });

    expect(startupGroupIconUrl(group(), [item], 'native-icon')).toBe('native-icon');
  });

  it('uses Microsoft 365 for the Office licensing helper', () => {
    const item = artifact({
      configurationPath: '/Library/LaunchDaemons/com.microsoft.office.licensingV2.helper.plist',
      target: {
        kind: 'executable',
        path: '/Library/PrivilegedHelperTools/com.microsoft.office.licensingV2.helper',
        executableName: 'com.microsoft.office.licensingV2.helper',
        arguments: [],
      },
    });

    expect(startupGroupIconUrl(group(), [item], '')).toBe(microsoft365IconUrl);
  });

  it('uses Chrome for GoogleUpdater launchd entries', () => {
    const item = artifact({
      configurationPath: '/Library/LaunchDaemons/com.google.GoogleUpdater.wake.system.plist',
    });

    expect(startupGroupIconUrl(group({ name: 'GoogleUpdater' }), [item], '')).toBe(googleChromeIconUrl);
  });

  it('uses Microsoft for an isolated Microsoft AutoUpdate helper', () => {
    const item = artifact({ displayName: 'com.microsoft.autoupdate.helper' });

    expect(startupGroupIconUrl(group(), [item], '')).toBe(microsoftIconUrl);
  });

  it('does not infer a brand from an ordinary display name', () => {
    expect(startupGroupIconUrl(group({ name: 'Microsoft maintenance script' }), [artifact()], '')).toBe('');
  });
});
