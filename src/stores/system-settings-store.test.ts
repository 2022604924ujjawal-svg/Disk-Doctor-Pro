import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  SystemSettingItem,
  SystemSettingsCatalog,
  SystemSettingsChangePlan,
  SystemSettingsChangeResult,
} from '@/lib/models/system-settings';
import { HistoryService } from '@/lib/services/history-service';
import { SystemSettingsService } from '@/lib/services/system-settings-service';

import { useSystemSettingsStore } from './system-settings-store';

const setting: SystemSettingItem = {
  settingId: 'windows.content.disable-suggestions',
  category: 'privacy',
  selectionKind: 'oneClick',
  riskLevel: 'standard',
  status: 'recommended',
  selectedByDefault: true,
  requiresRestart: false,
  requiresElevation: false,
  diagnostic: null,
};

function catalog(items: SystemSettingItem[] = [setting]): SystemSettingsCatalog {
  return {
    schemaVersion: 3,
    scanId: 'scan-1',
    catalogRevision: 'revision-1',
    platform: 'windows',
    scannedAtMs: 1,
    items,
    summary: {
      itemCount: items.length,
      recommendedCount: items.filter(item => item.status === 'recommended').length,
      optimizedCount: items.filter(item => item.status === 'optimized').length,
      selectedCount: items.filter(item => item.selectedByDefault).length,
      unavailableCount: items.filter(item => item.status === 'unavailable').length,
    },
    elapsedMs: 1,
    recoveryAvailable: false,
  };
}

const plan: SystemSettingsChangePlan = {
  schemaVersion: 1,
  planId: 'system-settings-plan-1234567890abcdef',
  scanId: 'scan-1',
  catalogRevision: 'revision-1',
  createdAtMs: 1,
  expiresAtMs: 2,
  items: [
    {
      settingId: setting.settingId,
      category: setting.category,
      target: 'optimized',
      requiresRestart: false,
      requiresElevation: false,
    },
  ],
  skippedItems: [],
  requiresConfirmation: false,
  requiresRestart: false,
};

describe('system settings store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.restoreAllMocks();
  });

  it('refreshes operation history after an executed change', async () => {
    const updatedCatalog = catalog([{ ...setting, status: 'optimized', selectedByDefault: false }]);
    const result: SystemSettingsChangeResult = {
      planId: plan.planId,
      changedCount: 1,
      failedCount: 0,
      requiresRestart: false,
      recoveryAvailable: true,
      items: [{ settingId: setting.settingId, status: 'changed', verified: true, failureReason: null }],
      catalog: updatedCatalog,
    };
    vi.spyOn(SystemSettingsService, 'executeChange').mockResolvedValue(result);
    const refreshHistory = vi.spyOn(HistoryService, 'list').mockResolvedValue([]);
    const store = useSystemSettingsStore();
    store.catalog = catalog();
    store.pendingPlan = plan;

    await store.execute();

    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(store.catalog).toEqual(updatedCatalog);
    expect(store.pendingPlan).toBeNull();
  });

  it('keeps a failed manual switch in the requested draft state', async () => {
    const result: SystemSettingsChangeResult = {
      planId: plan.planId,
      changedCount: 0,
      failedCount: 1,
      requiresRestart: false,
      recoveryAvailable: false,
      items: [
        {
          settingId: setting.settingId,
          status: 'failed',
          verified: false,
          failureReason: 'permissionDenied',
        },
      ],
      catalog: catalog(),
    };
    vi.spyOn(SystemSettingsService, 'executeChange').mockResolvedValue(result);
    vi.spyOn(HistoryService, 'list').mockResolvedValue([]);
    const store = useSystemSettingsStore();
    store.catalog = catalog();
    store.pendingPlan = plan;
    store.optimizationMode = 'manual';
    store.desiredOptimizedIds = [setting.settingId];

    await store.execute();

    expect(store.desiredOptimizedIds).toContain(setting.settingId);
  });
});
