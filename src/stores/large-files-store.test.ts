import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { LargeFileEntry, LargeFilesResult } from '@/lib/models/large-file';
import { LoggerService } from '@/lib/services/logger-service';
import { PermanentDeleteService } from '@/lib/services/permanent-delete-service';

import { useAppStore } from './app-store';
import { useHistoryStore } from './history-store';
import { useLargeFilesStore } from './large-files-store';

const removed: LargeFileEntry = {
  name: 'removed.bin',
  path: '/fixture/removed.bin',
  parentPath: '/fixture',
  bytes: 128,
  modifiedAtMs: 1,
};
const failed: LargeFileEntry = {
  name: 'failed.bin',
  path: '/fixture/failed.bin',
  parentPath: '/fixture',
  bytes: 256,
  modifiedAtMs: 2,
};

function createResult(): LargeFilesResult {
  return {
    scanId: 9,
    root: '/fixture',
    scannedAtMs: 1,
    minimumBytes: 1,
    totalBytes: removed.bytes + failed.bytes,
    totalCount: 2,
    returnedCount: 2,
    truncated: false,
    skippedCount: 0,
    cacheReused: false,
    entries: [removed, failed],
  };
}

describe('large files store deletion', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.restoreAllMocks();
  });

  it('returns partial deletion results without raising a second global error', async () => {
    const operation = {
      removedPaths: [removed.path],
      failed: [{ path: failed.path, message: 'fixture item failure' }],
      releasedBytes: removed.bytes,
    };
    vi.spyOn(PermanentDeleteService, 'deleteFiles').mockResolvedValue(operation);
    vi.spyOn(useHistoryStore(), 'load').mockResolvedValue();
    const warn = vi.spyOn(LoggerService, 'warn').mockImplementation(() => undefined);
    const appStore = useAppStore();
    const store = useLargeFilesStore();
    store.result = createResult();

    const result = await store.deleteManyPermanently([removed, failed]);

    expect(result).toEqual(operation);
    expect(appStore.errorCode).toBeNull();
    expect(store.result?.entries).toEqual([failed]);
    expect(warn).toHaveBeenCalledWith('large-files', 'delete_completed_with_failures', {
      removedCount: 1,
      failedCount: 1,
      releasedBytes: removed.bytes,
    });
  });

  it('rejects deletion while a scan is active', async () => {
    const remove = vi.spyOn(PermanentDeleteService, 'deleteFiles');
    const store = useLargeFilesStore();
    store.result = createResult();
    store.loading = true;

    const result = await store.deleteManyPermanently([removed]);

    expect(result).toBeUndefined();
    expect(remove).not.toHaveBeenCalled();
    expect(store.deleting).toBe(false);
  });
});
