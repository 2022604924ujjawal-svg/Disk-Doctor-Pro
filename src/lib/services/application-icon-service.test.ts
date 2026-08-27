import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => false,
  invoke: invokeMock,
}));

import { ApplicationIconService } from '@/lib/services/application-icon-service';

describe('ApplicationIconService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('loads every requested icon across native batch limits', async () => {
    invokeMock.mockImplementation(async (_command: string, args: { paths: string[] }) =>
      args.paths.map(path => ({ path, dataUrl: `data:image/png;base64,${path}` }))
    );
    const paths = Array.from({ length: 65 }, (_, index) => `/Applications/Test-${index}.app`);

    const icons = await ApplicationIconService.resolve(paths);

    expect(invokeMock).toHaveBeenCalledTimes(3);
    expect(invokeMock.mock.calls.map(([, args]) => args.paths.length)).toEqual([32, 32, 1]);
    expect(icons.size).toBe(paths.length);
    expect(icons.get(paths.at(-1)!)).toContain(paths.at(-1)!);
  });

  it('reuses icons that were already resolved during the current session', async () => {
    const path = '/Applications/Session-Cached-Test.app';
    invokeMock.mockResolvedValue([{ path, dataUrl: 'data:image/png;base64,cached' }]);

    const first = await ApplicationIconService.resolve([path]);
    const second = await ApplicationIconService.resolve([path]);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(first.get(path)).toBe('data:image/png;base64,cached');
    expect(second.get(path)).toBe('data:image/png;base64,cached');
  });

  it('publishes each completed native batch progressively', async () => {
    invokeMock.mockImplementation(async (_command: string, args: { paths: string[] }) =>
      args.paths.map(path => ({ path, dataUrl: `data:image/png;base64,${path}` }))
    );
    const paths = Array.from({ length: 33 }, (_, index) => `/Applications/Progressive-${index}.app`);
    const publishedSizes: number[] = [];

    const icons = await ApplicationIconService.resolveIncrementally(paths, update => {
      publishedSizes.push(update.size);
    });

    expect(publishedSizes).toEqual([0, 32, 33]);
    expect(icons.size).toBe(33);
  });
});
