import { describe, expect, it } from 'vitest';

import type { StartupArtifact, StartupChangePlan, StartupOwnerGroup } from '@/lib/models/startup';

import {
  artifactsForStartupGroup,
  canManageStartupArtifact,
  defaultStartupGroups,
  displayedArtifactsForGroup,
  filterAndSortStartupGroups,
  indexStartupArtifacts,
  isDefaultStartupGroup,
  isInformativeReadOnlyStartupArtifact,
  isRemovableOrphanStartupArtifact,
  manageableArtifactsForGroup,
  manageableState,
  removableOrphanArtifactsForGroup,
  needsBackgroundTaskPermission,
  nextStartupDesiredState,
  startupArtifactRevealPath,
  startupFilterCounts,
  startupGroupManageableState,
  startupGroupStartTiming,
  startupGroupSubtitle,
  startupRevealPath,
  startupPlanRequiresReview,
} from './startup-view';

function artifact(overrides: Partial<StartupArtifact> = {}): StartupArtifact {
  return {
    itemId: 'a'.repeat(64),
    sourceId: 'macos.launchd.user_agents',
    sourceKind: 'launchAgent',
    scope: 'currentUser',
    triggers: ['userLogon'],
    displayName: 'Fixture',
    configurationPath: null,
    target: { kind: 'executable', path: null, executableName: null, arguments: [] },
    ownerName: 'Fixture',
    publisher: null,
    summary: null,
    summarySource: 'sourceLabel',
    version: null,
    iconPath: null,
    identityConfidence: 'strong',
    configuredState: 'enabled',
    runtimeState: 'unknown',
    controlCapability: 'toggleable',
    trust: 'unknown',
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
    summarySource: 'sourceLabel',
    version: null,
    iconPath: null,
    identityConfidence: 'strong',
    itemIds: ['a'.repeat(64)],
    sourceKinds: ['launchAgent'],
    triggers: ['userLogon'],
    scopes: ['currentUser'],
    configuredState: 'allEnabled',
    controlState: 'allToggleable',
    systemItem: false,
    ...overrides,
  };
}

function plan(overrides: Partial<StartupChangePlan> = {}): StartupChangePlan {
  return {
    schemaVersion: 1,
    planId: 'startup-plan-1234567890abcdef12345678',
    scanId: 'scan-1',
    catalogRevision: 'revision-1',
    createdAtMs: 1,
    expiresAtMs: 2,
    desiredState: 'disabled',
    items: [
      {
        itemId: 'a'.repeat(64),
        displayName: 'Fixture',
        sourceKind: 'launchAgent',
        scope: 'currentUser',
        previousState: 'enabled',
        desiredState: 'disabled',
        warnings: [],
        requiresElevation: false,
      },
    ],
    skippedItems: [],
    requiresConfirmation: true,
    ...overrides,
  };
}

describe('startup default view', () => {
  it('indexes artifacts and ignores missing group members', () => {
    const item = artifact();
    const artifacts = indexStartupArtifacts([item]);

    expect(artifactsForStartupGroup(group({ itemIds: [item.itemId, 'missing'] }), artifacts)).toEqual([item]);
  });

  it('shows manageable groups and identified background apps but hides other read-only items', () => {
    const item = artifact();
    const artifacts = new Map([[item.itemId, item]]);

    expect(isDefaultStartupGroup(group(), artifacts)).toBe(true);
    expect(isDefaultStartupGroup(group({ systemItem: true }), artifacts)).toBe(false);
    expect(isDefaultStartupGroup(group(), new Map([[item.itemId, artifact({ controlCapability: 'viewOnly' })]]))).toBe(
      false
    );
    expect(
      isDefaultStartupGroup(
        group(),
        new Map([
          [item.itemId, artifact({ sourceKind: 'backgroundTask', target: { ...item.target, kind: 'application' } })],
        ])
      )
    ).toBe(true);
  });

  it('limits informative read-only entries to app-backed background items with a known state', () => {
    const backgroundApp = artifact({
      sourceKind: 'backgroundTask',
      target: { kind: 'application', path: '/Applications/Fixture.app', executableName: 'Fixture', arguments: [] },
      controlCapability: 'systemManaged',
    });

    expect(isInformativeReadOnlyStartupArtifact(backgroundApp)).toBe(true);
    expect(isInformativeReadOnlyStartupArtifact({ ...backgroundApp, configuredState: 'unknown' })).toBe(false);
    expect(isInformativeReadOnlyStartupArtifact({ ...backgroundApp, diagnostics: ['missingTarget'] })).toBe(false);
    expect(
      isInformativeReadOnlyStartupArtifact({ ...backgroundApp, target: { ...backgroundApp.target, kind: 'service' } })
    ).toBe(false);
  });

  it('keeps third-party machine items manageable when elevation is required', () => {
    expect(
      canManageStartupArtifact(
        artifact({ scope: 'machine', sourceKind: 'service', controlCapability: 'elevationRequired' })
      )
    ).toBe(true);
  });

  it('returns only actionable artifacts from a mixed owner group', () => {
    const manageable = artifact();
    const readOnly = artifact({ itemId: 'b'.repeat(64), controlCapability: 'viewOnly' });
    const artifacts = new Map([
      [manageable.itemId, manageable],
      [readOnly.itemId, readOnly],
    ]);

    expect(manageableArtifactsForGroup(group({ itemIds: [...artifacts.keys()] }), artifacts)).toEqual([manageable]);
  });

  it('shows and selects only allowlisted orphan startup configurations', () => {
    const orphan = artifact({
      configurationPath: '/Users/fixture/Library/LaunchAgents/com.example.fixture.plist',
      diagnostics: ['missingTarget'],
      removableOrphan: true,
    });
    const service = artifact({
      itemId: 'b'.repeat(64),
      sourceKind: 'service',
      diagnostics: ['missingTarget'],
      controlCapability: 'elevationRequired',
    });
    const artifacts = indexStartupArtifacts([orphan, service]);
    const owner = group({ itemIds: [orphan.itemId, service.itemId] });

    expect(isRemovableOrphanStartupArtifact(orphan)).toBe(true);
    expect(isRemovableOrphanStartupArtifact({ ...orphan, removableOrphan: false })).toBe(false);
    expect(isRemovableOrphanStartupArtifact(service)).toBe(false);
    expect(removableOrphanArtifactsForGroup(owner, artifacts)).toEqual([orphan]);
  });

  it('keeps an orphan with unavailable state out of enabled and disabled counts', () => {
    const orphan = artifact({
      configuredState: 'unknown',
      diagnostics: ['missingTarget'],
      removableOrphan: true,
    });
    const artifacts = indexStartupArtifacts([orphan]);
    const owner = group({ itemIds: [orphan.itemId] });

    expect(startupGroupManageableState(owner, artifacts)).toBe('unknown');
    expect(startupFilterCounts([owner], artifacts)).toEqual({ all: 1, enabled: 0, disabled: 0 });
  });

  it('keeps an app-backed background item beside a manageable artifact for display', () => {
    const manageable = artifact();
    const background = artifact({
      itemId: 'b'.repeat(64),
      sourceKind: 'backgroundTask',
      target: { kind: 'application', path: '/Applications/Fixture.app', executableName: 'Fixture', arguments: [] },
      controlCapability: 'systemManaged',
    });
    const artifacts = new Map([
      [manageable.itemId, manageable],
      [background.itemId, background],
    ]);

    expect(displayedArtifactsForGroup(group({ itemIds: [...artifacts.keys()] }), artifacts)).toEqual([
      manageable,
      background,
    ]);
  });

  it('derives a compact state from manageable artifacts only', () => {
    expect(manageableState([artifact()])).toBe('enabled');
    expect(manageableState([artifact({ configuredState: 'disabled' })])).toBe('disabled');
    expect(manageableState([artifact(), artifact({ itemId: 'b'.repeat(64), configuredState: 'disabled' })])).toBe(
      'mixed'
    );
  });

  it('derives group state, filter counts, and desired state consistently', () => {
    const enabled = artifact();
    const disabled = artifact({ itemId: 'b'.repeat(64), configuredState: 'disabled' });
    const artifacts = indexStartupArtifacts([enabled, disabled]);
    const groups = [group(), group({ groupId: 'group-2', name: 'Second', itemIds: [disabled.itemId] })];

    expect(startupGroupManageableState(groups[0]!, artifacts)).toBe('enabled');
    expect(startupGroupManageableState(groups[1]!, artifacts)).toBe('disabled');
    expect(startupFilterCounts(groups, artifacts)).toEqual({ all: 2, enabled: 1, disabled: 1 });
    expect(nextStartupDesiredState('enabled')).toBe('disabled');
    expect(nextStartupDesiredState('disabled')).toBe('enabled');
  });

  it('filters searchable fields and sorts matching groups for the requested locale', () => {
    const alpha = artifact({ displayName: 'Alpha Helper', target: { ...artifact().target, executableName: 'alpha' } });
    const beta = artifact({ itemId: 'b'.repeat(64), configuredState: 'disabled' });
    const artifacts = indexStartupArtifacts([alpha, beta]);
    const groups = [
      group({ groupId: 'group-b', name: 'Beta', publisher: 'Example Studio', itemIds: [beta.itemId] }),
      group({ groupId: 'group-a', name: 'Alpha', itemIds: [alpha.itemId] }),
    ];

    expect(filterAndSortStartupGroups(groups, artifacts, '', 'all', 'en-US').map(item => item.name)).toEqual([
      'Alpha',
      'Beta',
    ]);
    expect(filterAndSortStartupGroups(groups, artifacts, 'studio', 'all', 'en-US')).toEqual([groups[0]]);
    expect(filterAndSortStartupGroups(groups, artifacts, 'alpha helper', 'all', 'en-US')).toEqual([groups[1]]);
    expect(filterAndSortStartupGroups(groups, artifacts, '', 'disabled', 'en-US')).toEqual([groups[0]]);
  });

  it('keeps default filtering and presentation labels deterministic', () => {
    const item = artifact({ configurationPath: '/Library/LaunchAgents/fixture.plist' });
    const artifacts = indexStartupArtifacts([item]);
    const groups = [group({ summary: 'Keeps Fixture ready' }), group({ groupId: 'system', systemItem: true })];

    expect(defaultStartupGroups(groups, artifacts)).toEqual([groups[0]]);
    expect(startupArtifactRevealPath(item)).toBe('/Library/LaunchAgents/fixture.plist');
    expect(startupGroupSubtitle(groups[0]!)).toBe('Keeps Fixture ready');
    expect(startupGroupSubtitle(group({ summary: null, publisher: 'ABCDEFGHIJ' }))).toBeNull();
    expect(startupGroupStartTiming(group({ triggers: ['boot'] }))).toBe('boot');
    expect(startupGroupStartTiming(group({ triggers: ['keepAlive'] }))).toBe('background');
  });

  it('prefers the startup configuration when revealing a startup group', () => {
    const item = artifact({
      configurationPath: '/Library/LaunchAgents/fixture.plist',
      target: { kind: 'executable', path: '/Library/Helper', executableName: 'Helper', arguments: [] },
    });
    const artifacts = new Map([[item.itemId, item]]);

    expect(startupRevealPath(group({ iconPath: '/Applications/Fixture.app' }), artifacts)).toBe(
      '/Library/LaunchAgents/fixture.plist'
    );
    const withoutConfiguration = new Map([[item.itemId, { ...item, configurationPath: null }]]);
    expect(startupRevealPath(group({ iconPath: '/Applications/Fixture.app' }), withoutConfiguration)).toBe(
      '/Applications/Fixture.app'
    );
    expect(startupRevealPath(group(), withoutConfiguration)).toBe('/Library/Helper');
    expect(startupRevealPath(group(), new Map())).toBeNull();
  });

  it('does not offer a missing target as a reveal destination', () => {
    const missing = artifact({
      target: {
        kind: 'application',
        path: '/Applications/Removed.app',
        executableName: null,
        arguments: [],
      },
      diagnostics: ['missingTarget'],
    });
    const artifacts = indexStartupArtifacts([missing]);

    expect(startupArtifactRevealPath(missing)).toBeNull();
    expect(startupRevealPath(group({ iconPath: '/Applications/Removed.app' }), artifacts)).toBeNull();
  });

  it('executes a single ordinary or currently running item without another confirmation', () => {
    expect(startupPlanRequiresReview(plan(), 1)).toBe(false);
    expect(
      startupPlanRequiresReview(plan({ items: [{ ...plan().items[0]!, warnings: ['itemCurrentlyRunning'] }] }), 1)
    ).toBe(false);
  });

  it('requires review for grouped, skipped, or broader-trigger changes', () => {
    expect(startupPlanRequiresReview(plan(), 2)).toBe(true);
    expect(
      startupPlanRequiresReview(
        plan({ skippedItems: [{ itemId: 'b'.repeat(64), displayName: 'Skipped', reason: 'itemChanged' }] }),
        1
      )
    ).toBe(true);
    expect(
      startupPlanRequiresReview(plan({ items: [{ ...plan().items[0]!, warnings: ['affectsOtherTriggers'] }] }), 1)
    ).toBe(true);
  });

  it('always requires confirmation before removing orphaned startup settings', () => {
    expect(
      startupPlanRequiresReview(
        plan({
          desiredState: 'removed',
          items: [{ ...plan().items[0]!, desiredState: 'removed' }],
        }),
        1
      )
    ).toBe(true);
  });

  it('requests macOS background task permission only for an access denial', () => {
    const coverage = [
      {
        sourceId: 'macos.background_tasks',
        required: false,
        status: 'unavailable' as const,
        reason: 'accessDenied' as const,
        itemCount: 0,
        elapsedMs: 1,
      },
    ];

    expect(needsBackgroundTaskPermission(true, coverage)).toBe(true);
    expect(needsBackgroundTaskPermission(false, coverage)).toBe(false);
    expect(needsBackgroundTaskPermission(true, [{ ...coverage[0], reason: 'invalidData' }])).toBe(false);
    expect(needsBackgroundTaskPermission(true, [{ ...coverage[0], sourceId: 'macos.launchd.user_agents' }])).toBe(
      false
    );
  });
});
