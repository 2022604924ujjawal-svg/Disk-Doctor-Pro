import { beforeEach, describe, expect, it, vi } from 'vitest';

import { APP_DISTRIBUTION_IDS } from '@/lib/models/app-update';
import { AppDistributionService } from '@/lib/services/app-distribution-service';

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

describe('AppDistributionService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('reads the distribution embedded in the desktop binary', async () => {
    invokeMock.mockResolvedValue(APP_DISTRIBUTION_IDS.portable);

    await expect(AppDistributionService.current()).resolves.toBe(APP_DISTRIBUTION_IDS.portable);
    expect(invokeMock).toHaveBeenCalledWith('get_app_distribution');
  });

  it('rejects an unknown distribution instead of enabling automatic updates', async () => {
    invokeMock.mockResolvedValue('unknown');

    await expect(AppDistributionService.current()).rejects.toThrow('The application distribution is invalid.');
  });
});
