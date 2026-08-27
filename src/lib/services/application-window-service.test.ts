import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ApplicationWindowService } from '@/lib/services/application-window-service';
import { LoggerService } from '@/lib/services/logger-service';

const { windowMock } = vi.hoisted(() => ({
  windowMock: {
    show: vi.fn(),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    isMaximized: vi.fn(),
    onResized: vi.fn(),
    close: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => windowMock,
}));

describe('ApplicationWindowService', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    Object.values(windowMock).forEach(method => method.mockReset().mockResolvedValue(undefined));
    windowMock.isMaximized.mockResolvedValue(false);
    windowMock.onResized.mockResolvedValue(() => undefined);
  });

  it.each([
    ['showAfterMount', 'show', 'main_window_shown'],
    ['minimize', 'minimize', 'main_window_minimized'],
    ['toggleMaximize', 'toggleMaximize', 'main_window_maximize_toggled'],
    ['close', 'close', 'main_window_close_requested'],
  ] as const)('runs %s against the current native window', async (serviceMethod, windowMethod, event) => {
    const info = vi.spyOn(LoggerService, 'info').mockImplementation(() => undefined);

    await ApplicationWindowService[serviceMethod]();

    expect(windowMock[windowMethod]).toHaveBeenCalledOnce();
    expect(info).toHaveBeenCalledWith('application-window', event);
  });

  it('records native window action failures without rejecting the UI event', async () => {
    const error = vi.spyOn(LoggerService, 'error').mockImplementation(() => undefined);
    windowMock.toggleMaximize.mockRejectedValueOnce(new Error('window unavailable'));

    await expect(ApplicationWindowService.toggleMaximize()).resolves.toBeUndefined();

    expect(error).toHaveBeenCalledWith('application-window', 'main_window_maximize_toggle_failed', {
      error: 'window unavailable',
    });
  });

  it('keeps maximize state synchronized with native resize events', async () => {
    const info = vi.spyOn(LoggerService, 'info').mockImplementation(() => undefined);
    const unlisten = vi.fn();
    let resized: (() => void) | undefined;
    windowMock.isMaximized.mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    windowMock.onResized.mockImplementationOnce(async handler => {
      resized = handler;
      return unlisten;
    });
    const states: boolean[] = [];

    const stop = await ApplicationWindowService.observeMaximized(maximized => states.push(maximized));
    expect(states).toEqual([true]);

    resized?.();
    await vi.waitFor(() => expect(states).toEqual([true, false]));

    stop();
    expect(unlisten).toHaveBeenCalledOnce();
    expect(info).toHaveBeenCalledWith('application-window', 'main_window_maximize_observer_ready');
  });
});
