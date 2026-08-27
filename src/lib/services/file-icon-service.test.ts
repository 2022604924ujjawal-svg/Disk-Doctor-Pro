import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => false,
  invoke: invokeMock,
}));

import { FileIconService } from '@/lib/services/file-icon-service';

describe('FileIconService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('coalesces files from one render pass into a native batch', async () => {
    invokeMock.mockImplementation(async (_command: string, args: { requests: Array<{ path: string }> }) => ({
      assignments: args.requests.map(request => ({ ...request, iconKey: 'ext:batch-test' })),
      assets: [{ iconKey: 'ext:batch-test', dataUrl: 'data:image/png;base64,batch' }],
    }));

    const icons = await Promise.all([
      FileIconService.resolve({ path: '/tmp/batch-test.one', kind: 'file', mode: 'automatic' }),
      FileIconService.resolve({ path: '/tmp/batch-test.two', kind: 'file', mode: 'automatic' }),
    ]);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock.mock.calls[0]?.[1].requests).toHaveLength(2);
    expect(icons).toEqual(['data:image/png;base64,batch', 'data:image/png;base64,batch']);
  });

  it('reuses a resolved file type without another native call', async () => {
    invokeMock.mockImplementation(async (_command: string, args: { requests: Array<{ path: string }> }) => ({
      assignments: args.requests.map(request => ({ ...request, iconKey: 'ext:sessionpdf' })),
      assets: [{ iconKey: 'ext:sessionpdf', dataUrl: 'data:image/png;base64,pdf' }],
    }));

    const first = await FileIconService.resolve({
      path: '/tmp/first.sessionpdf',
      kind: 'file',
      mode: 'automatic',
    });
    const second = await FileIconService.resolve({
      path: '/tmp/second.sessionpdf',
      kind: 'file',
      mode: 'automatic',
    });

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(first).toBe('data:image/png;base64,pdf');
    expect(second).toBe(first);
  });

  it('keeps file and directory requests for the same path distinct', async () => {
    invokeMock.mockImplementation(
      async (_command: string, args: { requests: Array<{ path: string; kind: 'file' | 'directory' }> }) => ({
        assignments: args.requests.map(request => ({
          ...request,
          iconKey: request.kind === 'directory' ? 'kind:folder' : 'kind:file',
        })),
        assets: [
          { iconKey: 'kind:folder', dataUrl: 'data:image/png;base64,folder' },
          { iconKey: 'kind:file', dataUrl: 'data:image/png;base64,file' },
        ],
      })
    );

    const [fileIcon, directoryIcon] = await Promise.all([
      FileIconService.resolve({ path: '/tmp/replaced-item', kind: 'file', mode: 'automatic' }),
      FileIconService.resolve({ path: '/tmp/replaced-item', kind: 'directory', mode: 'automatic' }),
    ]);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock.mock.calls[0]?.[1].requests).toHaveLength(2);
    expect(fileIcon).toBe('data:image/png;base64,file');
    expect(directoryIcon).toBe('data:image/png;base64,folder');
  });

  it('revalidates successful session entries after their ttl', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-27T00:00:00Z'));
    invokeMock.mockImplementation(async (_command: string, args: { requests: Array<{ path: string }> }) => ({
      assignments: args.requests.map(request => ({ ...request, iconKey: 'ext:ttl-refresh' })),
      assets: [{ iconKey: 'ext:ttl-refresh', dataUrl: 'data:image/png;base64,ttl' }],
    }));

    const request = { path: '/tmp/example.ttl-refresh', kind: 'file' as const, mode: 'automatic' as const };
    await FileIconService.resolve(request);
    await FileIconService.resolve(request);
    expect(invokeMock).toHaveBeenCalledTimes(1);

    vi.setSystemTime(new Date('2026-07-27T00:05:01Z'));
    await FileIconService.resolve(request);
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it('retries transient failures after a short ttl', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-07-27T01:00:00Z'));
    invokeMock.mockResolvedValue({ assignments: [], assets: [] });

    const request = {
      path: '/tmp/example.transient-failure',
      kind: 'file' as const,
      mode: 'automatic' as const,
    };
    expect(await FileIconService.resolve(request)).toBeNull();
    expect(await FileIconService.resolve(request)).toBeNull();
    expect(invokeMock).toHaveBeenCalledTimes(1);

    vi.setSystemTime(new Date('2026-07-27T01:00:16Z'));
    expect(await FileIconService.resolve(request)).toBeNull();
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it('evicts old path-specific icons when the session cache reaches its limit', async () => {
    invokeMock.mockImplementation(
      async (_command: string, args: { requests: Array<{ path: string; kind: 'file' | 'directory' }> }) => ({
        assignments: args.requests.map(request => ({
          ...request,
          iconKey: `path:${request.path}`,
        })),
        assets: args.requests.map(request => ({
          iconKey: `path:${request.path}`,
          dataUrl: `data:image/png;base64,${request.path}`,
        })),
      })
    );

    const requests = Array.from({ length: 2049 }, (_, index) => ({
      path: `/tmp/cache-limit-${index}.exe`,
      kind: 'file' as const,
      mode: 'automatic' as const,
    }));
    await Promise.all(requests.map(request => FileIconService.resolve(request)));
    const callsAfterInitialLoad = invokeMock.mock.calls.length;

    await FileIconService.resolve(requests[0]!);

    expect(callsAfterInitialLoad).toBe(Math.ceil(requests.length / 96));
    expect(invokeMock).toHaveBeenCalledTimes(callsAfterInitialLoad + 1);
  });

  it('reuses one generic system folder icon across directory paths', async () => {
    invokeMock.mockImplementation(async (_command: string, args: { requests: Array<{ path: string }> }) => ({
      assignments: args.requests.map(request => ({ ...request, iconKey: 'kind:folder' })),
      assets: [{ iconKey: 'kind:folder', dataUrl: 'data:image/png;base64,folder-shared' }],
    }));

    const callsBefore = invokeMock.mock.calls.length;
    const first = await FileIconService.resolve({
      path: '/tmp/generic-folder-one',
      kind: 'directory',
      mode: 'generic',
    });
    const secondRequest = {
      path: '/tmp/generic-folder-two',
      kind: 'directory' as const,
      mode: 'generic' as const,
    };

    expect(FileIconService.peek(secondRequest)).toBe(first);
    expect(await FileIconService.resolve(secondRequest)).toBe(first);
    expect(invokeMock.mock.calls.length - callsBefore).toBeLessThanOrEqual(1);
  });
});
