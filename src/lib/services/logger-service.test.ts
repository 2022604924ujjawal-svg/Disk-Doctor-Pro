import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { LoggerService } from '@/lib/services/logger-service';

const browserConsole = globalThis.console;
const { isTauriMock, nativeErrorMock, nativeInfoMock, nativeWarnMock } = vi.hoisted(() => ({
  isTauriMock: vi.fn(),
  nativeErrorMock: vi.fn(),
  nativeInfoMock: vi.fn(),
  nativeWarnMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ isTauri: isTauriMock }));
vi.mock('@tauri-apps/plugin-log', () => ({
  error: nativeErrorMock,
  info: nativeInfoMock,
  warn: nativeWarnMock,
}));

describe('LoggerService', () => {
  beforeEach(() => {
    isTauriMock.mockReset().mockReturnValue(false);
    nativeErrorMock.mockReset().mockResolvedValue(undefined);
    nativeInfoMock.mockReset().mockResolvedValue(undefined);
    nativeWarnMock.mockReset().mockResolvedValue(undefined);
    vi.spyOn(browserConsole, 'info').mockImplementation(() => undefined);
    vi.spyOn(browserConsole, 'warn').mockImplementation(() => undefined);
    vi.spyOn(browserConsole, 'error').mockImplementation(() => undefined);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('keeps browser previews on the console-only fallback', () => {
    LoggerService.info('application-shell', 'started', { path: 'private-path' });

    expect(browserConsole.info).toHaveBeenCalledWith('[application-shell] started', { path: 'private-path' });
    expect(nativeInfoMock).not.toHaveBeenCalled();
  });

  it('persists only stable domain and event identifiers in Tauri', () => {
    isTauriMock.mockReturnValue(true);

    LoggerService.error('application-shell', 'operation_failed', { path: 'private-path' });

    expect(nativeErrorMock).toHaveBeenCalledWith('[application-shell] operation_failed');
    expect(nativeErrorMock).not.toHaveBeenCalledWith(expect.stringContaining('private-path'));
    expect(browserConsole.error).toHaveBeenCalledWith('[application-shell] operation_failed', { path: 'private-path' });
  });
});
