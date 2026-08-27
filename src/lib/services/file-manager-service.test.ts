import { beforeEach, describe, expect, it, vi } from 'vitest';

import { FileManagerService } from '@/lib/services/file-manager-service';

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));

describe('FileManagerService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it('reveals the requested item through the native adapter', async () => {
    await FileManagerService.reveal('/private/item');

    expect(invokeMock).toHaveBeenCalledWith('reveal_in_file_manager', { path: '/private/item' });
  });

  it('binds an external open request to the authoritative analysis result', async () => {
    await FileManagerService.openAnalysisEntry(17, '/private/item');

    expect(invokeMock).toHaveBeenCalledWith('open_analysis_entry', {
      scanId: 17,
      selectedPath: '/private/item',
    });
  });

  it('binds an external open request to the authoritative large-file result', async () => {
    await FileManagerService.openLargeFileEntry(18, '/private/large.bin');

    expect(invokeMock).toHaveBeenCalledWith('open_large_file_entry', {
      scanId: 18,
      selectedPath: '/private/large.bin',
    });
  });

  it('binds an external open request to the authoritative duplicate-file result', async () => {
    await FileManagerService.openDuplicateFileEntry(19, '/private/duplicate.bin');

    expect(invokeMock).toHaveBeenCalledWith('open_duplicate_file_entry', {
      scanId: 19,
      selectedPath: '/private/duplicate.bin',
    });
  });

  it('opens the application log directory without receiving a path from the page', async () => {
    await FileManagerService.openApplicationLogs();

    expect(invokeMock).toHaveBeenCalledWith('open_application_log_directory');
  });
});
