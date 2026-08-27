import { beforeEach, describe, expect, it, vi } from 'vitest';

import { APP_DISTRIBUTION_IDS } from '@/lib/models/app-update';
import { LOG_EVENTS } from '@/lib/models/telemetry';
import { AppUpdateMetadataService } from '@/lib/services/app-update-metadata-service';
import { InstallationIdentityService } from '@/lib/services/installation-identity-service';
import { LoggerService } from '@/lib/services/logger-service';

const { versionMock } = vi.hoisted(() => ({
  versionMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-os', () => ({
  version: versionMock,
}));

describe('AppUpdateMetadataService', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    versionMock.mockReset();
    versionMock.mockReturnValue('15.6.1');
    vi.spyOn(InstallationIdentityService, 'getOrCreateInstallId').mockResolvedValue(
      '019c0b3d-a9ef-7d11-89a3-d5ea10df4001'
    );
    vi.spyOn(LoggerService, 'warn').mockImplementation(() => undefined);
  });

  it('builds the metadata expected by the update API', async () => {
    const headers = await AppUpdateMetadataService.createHeaders('zh-CN', APP_DISTRIBUTION_IDS.portable);

    expect(headers).toEqual({
      'Accept-Language': 'zh-CN',
      'x-mangodisk-distribution': 'portable',
      'x-mangodisk-install-id': '019c0b3d-a9ef-7d11-89a3-d5ea10df4001',
      'x-mangodisk-locale': 'zh-CN',
      'x-mangodisk-os-version': '15.6.1',
      'x-mangodisk-timezone': expect.any(String),
    });
  });

  it('keeps update checks available when installation identity storage fails', async () => {
    vi.spyOn(InstallationIdentityService, 'getOrCreateInstallId').mockRejectedValue(new Error('store unavailable'));

    await expect(AppUpdateMetadataService.createHeaders('zh-CN', APP_DISTRIBUTION_IDS.installed)).resolves.toEqual({
      'Accept-Language': 'zh-CN',
      'x-mangodisk-distribution': 'installed',
      'x-mangodisk-locale': 'zh-CN',
    });
    expect(LoggerService.warn).toHaveBeenCalledWith(
      'app-update',
      LOG_EVENTS.updateMetadataUnavailable,
      expect.objectContaining({
        source: 'installation_identity',
      })
    );
  });

  it('keeps available metadata when optional operating-system fields fail', async () => {
    versionMock.mockImplementation(() => {
      throw new Error('version unavailable');
    });
    const headers = await AppUpdateMetadataService.createHeaders('en-US', APP_DISTRIBUTION_IDS.installed);

    expect(headers).toEqual({
      'Accept-Language': 'en-US',
      'x-mangodisk-distribution': 'installed',
      'x-mangodisk-install-id': '019c0b3d-a9ef-7d11-89a3-d5ea10df4001',
      'x-mangodisk-locale': 'en-US',
      'x-mangodisk-timezone': expect.any(String),
    });
    expect(LoggerService.warn).toHaveBeenCalledTimes(1);
  });
});
