import { beforeEach, describe, expect, it, vi } from 'vitest';

import { MacOsSystemSettingsService } from '@/lib/services/macos-system-settings-service';

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@/lib/services/logger-service', () => ({
  LoggerService: {
    info: vi.fn(),
  },
}));

describe('MacOsSystemSettingsService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('opens the fixed Login Items settings destination', async () => {
    await MacOsSystemSettingsService.openLoginItems();

    expect(invokeMock).toHaveBeenCalledWith('open_macos_login_items_settings');
  });
});
