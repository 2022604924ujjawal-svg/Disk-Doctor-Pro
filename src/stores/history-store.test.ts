import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { HistoryService } from '@/lib/services/history-service';

import { useAppStore } from './app-store';
import { useHistoryStore } from './history-store';

describe('history store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.restoreAllMocks();
  });

  it('keeps a background refresh failure out of the global error state', async () => {
    vi.spyOn(HistoryService, 'list').mockRejectedValue(new Error('history refresh failed'));
    const appStore = useAppStore();
    const reportError = vi.spyOn(appStore, 'reportError');
    const historyStore = useHistoryStore();

    await historyStore.load({ reportError: false });

    expect(reportError).not.toHaveBeenCalled();
    expect(historyStore.loading).toBe(false);
  });

  it('reports an explicit history load failure by default', async () => {
    const error = new Error('history load failed');
    vi.spyOn(HistoryService, 'list').mockRejectedValue(error);
    const appStore = useAppStore();
    const reportError = vi.spyOn(appStore, 'reportError');

    await useHistoryStore().load();

    expect(reportError).toHaveBeenCalledWith(error);
  });
});
