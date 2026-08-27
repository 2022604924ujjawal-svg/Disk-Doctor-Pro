import { describe, expect, it } from 'vitest';

import type { SystemSettingItem, SystemSettingsCatalog } from '@/lib/models/system-settings';

import {
  systemOptimizationDesiredIdsForMode,
  systemOptimizationHighRiskEnables,
  systemOptimizationModesForPlatform,
  systemOptimizationPendingChanges,
  systemOptimizationSelectionForMode,
} from './system-settings-mode';

function item(
  settingId: string,
  options: Partial<
    Pick<SystemSettingItem, 'category' | 'riskLevel' | 'selectedByDefault' | 'selectionKind' | 'status'>
  > = {}
): SystemSettingItem {
  return {
    settingId,
    category: options.category ?? 'privacy',
    selectionKind: options.selectionKind ?? 'custom',
    riskLevel: options.riskLevel ?? 'standard',
    status: options.status ?? 'recommended',
    selectedByDefault: options.selectedByDefault ?? false,
    requiresRestart: false,
    requiresElevation: false,
    diagnostic: null,
  };
}

function catalog(platform: SystemSettingsCatalog['platform'], items: SystemSettingItem[]): SystemSettingsCatalog {
  return {
    schemaVersion: 3,
    scanId: 'scan',
    catalogRevision: 'revision',
    platform,
    scannedAtMs: 1,
    items,
    summary: {
      itemCount: items.length,
      recommendedCount: items.length,
      optimizedCount: 0,
      selectedCount: 0,
      unavailableCount: 0,
    },
    elapsedMs: 1,
    recoveryAvailable: false,
  };
}

describe('system optimization modes', () => {
  it('exposes only presets that differ on the current platform', () => {
    expect(systemOptimizationModesForPlatform('macos')).toEqual(['smart', 'performance', 'privacy']);
    expect(systemOptimizationModesForPlatform('windows')).toEqual(['smart', 'performance', 'privacy']);
  });

  it('expands macOS smart and privacy presets with practical settings', () => {
    const current = catalog('macos', [
      item('macos.finder.show-file-extensions', { category: 'productivity', selectedByDefault: true }),
      item('macos.finder.default-list-view', { category: 'productivity' }),
      item('macos.activity-monitor.show-all-processes', { category: 'productivity' }),
      item('macos.dock.enable-spring-loading', { category: 'productivity' }),
      item('macos.privacy.disable-personalized-ads', { category: 'privacy', selectedByDefault: true }),
      item('macos.sharing.disable-airdrop', { category: 'privacy' }),
      item('macos.finder.remove-old-trash-items', { category: 'storage', riskLevel: 'caution' }),
    ]);

    expect(systemOptimizationSelectionForMode(current, 'smart')).toEqual([
      'macos.finder.show-file-extensions',
      'macos.finder.default-list-view',
      'macos.activity-monitor.show-all-processes',
      'macos.dock.enable-spring-loading',
      'macos.privacy.disable-personalized-ads',
    ]);
    expect(systemOptimizationSelectionForMode(current, 'privacy')).toEqual([
      'macos.finder.show-file-extensions',
      'macos.finder.default-list-view',
      'macos.activity-monitor.show-all-processes',
      'macos.dock.enable-spring-loading',
      'macos.privacy.disable-personalized-ads',
      'macos.sharing.disable-airdrop',
    ]);
  });

  it('extends smart mode with practical user and recovery settings', () => {
    const current = catalog('windows', [
      item('windows.content.disable-suggestions', { selectedByDefault: true }),
      item('windows.explorer.show-hidden-files'),
      item('windows.taskbar.hide-widgets'),
      item('windows.filesystem.enable-long-paths'),
      item('windows.recovery.enable-registry-backups'),
      item('windows.explorer.confirm-file-delete'),
      item('windows.update.enable-restart-notifications'),
      item('windows.services.disable-sysmain'),
    ]);

    expect(systemOptimizationSelectionForMode(current, 'smart')).toEqual([
      'windows.content.disable-suggestions',
      'windows.explorer.show-hidden-files',
      'windows.taskbar.hide-widgets',
      'windows.filesystem.enable-long-paths',
      'windows.recovery.enable-registry-backups',
      'windows.explorer.confirm-file-delete',
      'windows.update.enable-restart-notifications',
    ]);
  });

  it('aggressively covers performance and gaming categories without selecting high-risk items', () => {
    const current = catalog('windows', [
      item('windows.content.disable-suggestions', { selectedByDefault: true }),
      item('windows.services.disable-sysmain', { category: 'performance' }),
      item('windows.performance.reduce-crash-dump', { category: 'storage' }),
      item('windows.power.disable-modern-standby', { category: 'performance' }),
      item('windows.gaming.disable-dynamic-lighting', { category: 'gaming' }),
      item('windows.security.disable-defender', { category: 'performance', riskLevel: 'high' }),
      item('windows.security.disable-vbs', { category: 'gaming', riskLevel: 'high' }),
    ]);

    expect(systemOptimizationSelectionForMode(current, 'performance')).toEqual([
      'windows.content.disable-suggestions',
      'windows.services.disable-sysmain',
      'windows.performance.reduce-crash-dump',
      'windows.power.disable-modern-standby',
      'windows.gaming.disable-dynamic-lighting',
    ]);
  });

  it('expands privacy mode without weakening core security features', () => {
    const current = catalog('windows', [
      item('windows.cloud.disable-onedrive-sync'),
      item('windows.office.disable-optional-telemetry', { category: 'privacy' }),
      item('windows.update.disable-peer-sharing', { category: 'privacy' }),
      item('windows.privacy.disable-remote-assistance', { category: 'privacy' }),
      item('windows.network.disable-smb1', { category: 'privacy' }),
      item('windows.edge.limit-diagnostic-data', { category: 'privacy' }),
      item('windows.security.disable-autorun', { category: 'privacy' }),
      item('windows.network.disable-llmnr', { category: 'privacy' }),
      item('windows.network.disable-smb2', { category: 'privacy', riskLevel: 'high' }),
      item('windows.security.disable-smartscreen', { category: 'privacy', riskLevel: 'high' }),
    ]);

    expect(systemOptimizationSelectionForMode(current, 'privacy')).toEqual([
      'windows.cloud.disable-onedrive-sync',
      'windows.office.disable-optional-telemetry',
      'windows.update.disable-peer-sharing',
      'windows.privacy.disable-remote-assistance',
      'windows.network.disable-smb1',
      'windows.edge.limit-diagnostic-data',
      'windows.security.disable-autorun',
      'windows.network.disable-llmnr',
    ]);
  });

  it('skips optimized and unavailable preset items', () => {
    const current = catalog('windows', [
      item('windows.services.disable-diagnostic-tracking', { status: 'optimized' }),
      item('windows.services.disable-error-reporting', { status: 'unavailable' }),
      item('windows.search.disable-cortana-policy'),
    ]);

    expect(systemOptimizationSelectionForMode(current, 'privacy')).toEqual(['windows.search.disable-cortana-policy']);
  });

  it('keeps existing optimizations when a preset builds its draft', () => {
    const current = catalog('windows', [
      item('windows.content.disable-suggestions', { selectedByDefault: true }),
      item('windows.security.disable-defender', { status: 'optimized' }),
    ]);

    expect(systemOptimizationDesiredIdsForMode(current, 'smart')).toEqual([
      'windows.content.disable-suggestions',
      'windows.security.disable-defender',
    ]);
  });

  it('builds a minimal bidirectional change batch from the draft', () => {
    const current = catalog('windows', [
      item('windows.content.disable-suggestions'),
      item('windows.services.disable-sysmain', { status: 'optimized' }),
      item('windows.security.disable-vbs', { status: 'unavailable' }),
    ]);

    expect(
      systemOptimizationPendingChanges(current, ['windows.content.disable-suggestions', 'windows.security.disable-vbs'])
    ).toEqual([
      {
        settingId: 'windows.content.disable-suggestions',
        target: 'optimized',
      },
      {
        settingId: 'windows.services.disable-sysmain',
        target: 'default',
      },
    ]);
  });

  it('requires confirmation only when enabling a high-risk setting', () => {
    const defender = item('windows.security.disable-defender', {
      riskLevel: 'high',
    });
    const updates = item('windows.update.disable-automatic-updates', {
      riskLevel: 'high',
      status: 'optimized',
    });
    const current = catalog('windows', [defender, updates]);
    const changes = systemOptimizationPendingChanges(current, [defender.settingId]);

    expect(systemOptimizationHighRiskEnables(current, changes).map(item => item.settingId)).toEqual([
      defender.settingId,
    ]);
  });
});
