import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { StartupCatalog, StartupChangePlan, StartupChangeResult } from '@/lib/models/startup';
import { StartupService } from '@/lib/services/startup-service';

import { useAppStore } from './app-store';
import { useStartupStore } from './startup-store';

const catalog: StartupCatalog = {
  schemaVersion: 1,
  scanId: 'scan-1',
  catalogRevision: 'revision-1',
  scannedAtMs: 1,
  complete: true,
  artifacts: [],
  groups: [],
  coverage: [],
  summary: {
    itemCount: 0,
    groupCount: 0,
    enabledCount: 0,
    disabledCount: 0,
    unknownStateCount: 0,
    elevationRequiredCount: 0,
    systemItemCount: 0,
  },
  elapsedMs: 1,
};
const plan: StartupChangePlan = {
  schemaVersion: 1,
  planId: 'startup-plan-1234567890abcdef12345678',
  scanId: catalog.scanId,
  catalogRevision: catalog.catalogRevision,
  createdAtMs: 1,
  expiresAtMs: 2,
  desiredState: 'disabled',
  items: [],
  skippedItems: [],
  requiresConfirmation: false,
};
const authorizationPrompt = 'MangoDisk needs administrator permission to change startup settings';

describe('startup store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.restoreAllMocks();
  });

  it('keeps the last successful catalog when a refresh fails', async () => {
    vi.spyOn(StartupService, 'scanCatalog').mockResolvedValueOnce(catalog).mockRejectedValueOnce(new Error('failed'));
    const store = useStartupStore();

    await store.scan();
    await store.scan();

    expect(store.catalog).toEqual(catalog);
    expect(store.scanning).toBe(false);
  });

  it('does not report a typed cancellation as an application error', async () => {
    vi.spyOn(StartupService, 'scanCatalog').mockRejectedValue({
      code: 'operationCancelled',
      details: {},
      message: 'cancelled',
      retryable: true,
    });
    const appStore = useAppStore();
    const reportError = vi.spyOn(appStore, 'reportError');

    await useStartupStore().scan();

    expect(reportError).not.toHaveBeenCalled();
  });

  it('prepares changes from the active server-owned scan session', async () => {
    const prepare = vi.spyOn(StartupService, 'prepareChange').mockResolvedValue(plan);
    const store = useStartupStore();
    store.catalog = catalog;

    await store.prepareChange(['a'.repeat(64)], 'disabled');

    expect(prepare).toHaveBeenCalledWith({
      scanId: catalog.scanId,
      itemIds: ['a'.repeat(64)],
      desiredState: 'disabled',
    });
    expect(store.pendingPlan).toEqual(plan);
    expect(store.preparingChange).toBe(false);
  });

  it('publishes the verified catalog returned by change execution', async () => {
    const refreshed = { ...catalog, scanId: 'scan-2', catalogRevision: 'revision-2' };
    const result: StartupChangeResult = {
      planId: plan.planId,
      changedCount: 1,
      failedCount: 0,
      items: [],
      catalog: refreshed,
    };
    const execute = vi.spyOn(StartupService, 'executeChange').mockResolvedValue(result);
    const store = useStartupStore();
    store.catalog = catalog;
    store.pendingPlan = plan;

    await store.executeChange(authorizationPrompt);

    expect(execute).toHaveBeenCalledWith(plan.planId, authorizationPrompt);
    expect(store.catalog).toEqual(refreshed);
    expect(store.lastChangeResult).toEqual(result);
    expect(store.pendingPlan).toBeNull();
    expect(store.executingChange).toBe(false);
  });

  it('retains the previous catalog when post-change readback is unavailable', async () => {
    const result: StartupChangeResult = {
      planId: plan.planId,
      changedCount: 1,
      failedCount: 0,
      items: [],
      catalog: null,
    };
    vi.spyOn(StartupService, 'executeChange').mockResolvedValue(result);
    const store = useStartupStore();
    store.catalog = catalog;
    store.pendingPlan = plan;

    await store.executeChange(authorizationPrompt);

    expect(store.catalog).toEqual(catalog);
    expect(store.lastChangeResult).toEqual(result);
    expect(store.pendingPlan).toBeNull();
  });

  it('does not retain a consumed plan after an execution error', async () => {
    vi.spyOn(StartupService, 'executeChange').mockRejectedValue(new Error('failed'));
    const store = useStartupStore();
    store.catalog = catalog;
    store.pendingPlan = plan;

    await store.executeChange(authorizationPrompt);

    expect(store.pendingPlan).toBeNull();
    expect(store.lastChangeResult).toBeNull();
    expect(store.executingChange).toBe(false);
  });

  it('requests cancellation for an active startup change', async () => {
    const cancel = vi.spyOn(StartupService, 'cancelChange').mockResolvedValue();
    const store = useStartupStore();
    store.executingChange = true;

    await store.cancelChange();

    expect(cancel).toHaveBeenCalledOnce();
    expect(store.cancellingChange).toBe(true);
  });
});
