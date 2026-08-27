import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  APP_DISTRIBUTION_IDS,
  APP_UPDATE_ACTION_IDS,
  APP_UPDATE_CHECK_TIMEOUT_MS,
  APP_UPDATE_DOWNLOAD_TIMEOUT_MS,
} from '@/lib/models/app-update';
import { AppUpdateMetadataService } from '@/lib/services/app-update-metadata-service';
import { AppUpdateService } from '@/lib/services/app-update-service';

const { checkMock, getVersionMock, relaunchMock } = vi.hoisted(() => ({
  checkMock: vi.fn(),
  getVersionMock: vi.fn(),
  relaunchMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/app', () => ({ getVersion: getVersionMock }));
vi.mock('@tauri-apps/plugin-updater', () => ({ check: checkMock }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: relaunchMock }));

describe('AppUpdateService', () => {
  const requestHeaders = {
    'Accept-Language': 'en-US',
    'x-mangodisk-distribution': 'installed',
    'x-mangodisk-install-id': '019c0b3d-a9ef-7d11-89a3-d5ea10df4001',
    'x-mangodisk-os-version': '15.6.1',
  };

  beforeEach(() => {
    vi.restoreAllMocks();
    checkMock.mockReset();
    getVersionMock.mockReset();
    relaunchMock.mockReset();
    vi.spyOn(AppUpdateMetadataService, 'createHeaders').mockResolvedValue(requestHeaders);
  });

  afterEach(async () => {
    await AppUpdateService.dispose();
  });

  it('uses client metadata when checking for updates', async () => {
    checkMock.mockResolvedValue(null);

    await expect(AppUpdateService.check('en-US', APP_DISTRIBUTION_IDS.installed)).resolves.toBeNull();
    expect(AppUpdateMetadataService.createHeaders).toHaveBeenCalledWith('en-US', APP_DISTRIBUTION_IDS.installed);
    expect(checkMock).toHaveBeenCalledWith({
      headers: requestHeaders,
      timeout: APP_UPDATE_CHECK_TIMEOUT_MS,
    });
  });

  it('downloads and installs a checked update as separate steps', async () => {
    const close = vi.fn(async () => undefined);
    const download = vi.fn(async () => undefined);
    const install = vi.fn(async () => undefined);
    checkMock.mockResolvedValue({
      body: 'Release notes',
      close,
      currentVersion: '1.0.0',
      date: '2026-07-31T00:00:00.000Z',
      download,
      install,
      version: '1.1.0',
    });

    await expect(AppUpdateService.check('en-US', APP_DISTRIBUTION_IDS.installed)).resolves.toEqual({
      action: APP_UPDATE_ACTION_IDS.automaticInstall,
      currentVersion: '1.0.0',
      date: '2026-07-31T00:00:00.000Z',
      notes: 'Release notes',
      version: '1.1.0',
    });
    await AppUpdateService.download(() => undefined);

    expect(download).toHaveBeenCalledWith(expect.any(Function), {
      headers: requestHeaders,
      timeout: APP_UPDATE_DOWNLOAD_TIMEOUT_MS,
    });
    expect(install).not.toHaveBeenCalled();
    expect(relaunchMock).not.toHaveBeenCalled();

    await AppUpdateService.installDownloaded();

    expect(install).toHaveBeenCalledOnce();
    expect(relaunchMock).not.toHaveBeenCalled();

    await AppUpdateService.restartApplication();

    expect(relaunchMock).toHaveBeenCalledOnce();
  });

  it('does not install before the update download completes', async () => {
    const close = vi.fn(async () => undefined);
    checkMock.mockResolvedValue({
      body: '',
      close,
      currentVersion: '1.0.0',
      date: undefined,
      download: vi.fn(async () => undefined),
      install: vi.fn(async () => undefined),
      version: '1.1.0',
    });
    await AppUpdateService.check('en-US', APP_DISTRIBUTION_IDS.installed);

    await expect(AppUpdateService.installDownloaded()).rejects.toThrow('No downloaded update');
  });

  it('returns a manual download without retaining portable update state', async () => {
    const close = vi.fn(async () => undefined);
    const download = vi.fn(async () => undefined);
    checkMock.mockResolvedValue({
      body: 'Portable release notes',
      close,
      currentVersion: '1.0.0',
      date: '2026-08-20T00:00:00.000Z',
      download,
      install: vi.fn(async () => undefined),
      rawJson: {
        url: 'https://mangodisk.app/api/updates/1.1.0/windows/x86_64/download?distribution=portable',
      },
      version: '1.1.0',
    });

    await expect(AppUpdateService.check('en-US', APP_DISTRIBUTION_IDS.portable)).resolves.toEqual({
      action: APP_UPDATE_ACTION_IDS.manualDownload,
      currentVersion: '1.0.0',
      date: '2026-08-20T00:00:00.000Z',
      manualDownloadUrl: 'https://mangodisk.app/api/updates/1.1.0/windows/x86_64/download?distribution=portable',
      notes: 'Portable release notes',
      version: '1.1.0',
    });
    expect(close).toHaveBeenCalledOnce();
    expect(download).not.toHaveBeenCalled();
    await expect(AppUpdateService.download(() => undefined)).rejects.toThrow('No checked update');
  });

  it('rejects an untrusted portable download URL and releases updater state', async () => {
    const close = vi.fn(async () => undefined);
    checkMock.mockResolvedValue({
      body: '',
      close,
      currentVersion: '1.0.0',
      date: undefined,
      download: vi.fn(async () => undefined),
      install: vi.fn(async () => undefined),
      rawJson: {
        url: 'https://example.com/MangoDisk.exe',
      },
      version: '1.1.0',
    });

    await expect(AppUpdateService.check('en-US', APP_DISTRIBUTION_IDS.portable)).rejects.toThrow(
      'unsupported download URL'
    );
    expect(close).toHaveBeenCalledOnce();
  });
});
