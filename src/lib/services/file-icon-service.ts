import { invoke } from '@tauri-apps/api/core';

import type { FileIconBatch, FileIconRequest } from '@/lib/models/file-icon';
import { LoggerService } from '@/lib/services/logger-service';

interface QueuedRequest {
  request: FileIconRequest;
  resolve: (dataUrl: string | null) => void;
}

interface CacheEntry<T> {
  value: T;
  expiresAt: number;
}

/**
 * Coalesces icon requests created by list rows in the same render pass.
 *
 * The native layer owns authoritative classification and persistent caching.
 * This session layer avoids bridge calls for paths already displayed and, once
 * a reusable type identity is known, immediately serves later files such as
 * PDFs from the same data URL.
 */
export class FileIconService {
  private static readonly batchSize = 96;
  private static readonly pathCacheLimit = 2048;
  private static readonly typeCacheLimit = 512;
  private static readonly successfulCacheTtlMs = 5 * 60 * 1000;
  private static readonly failedCacheTtlMs = 15 * 1000;
  private static readonly pathCache = new Map<string, CacheEntry<string | null>>();
  private static readonly typeCache = new Map<string, CacheEntry<string>>();
  private static readonly queue = new Map<string, QueuedRequest>();
  private static readonly pending = new Map<string, Promise<string | null>>();
  private static readonly pendingType = new Map<string, Promise<string | null>>();
  private static flushScheduled = false;

  /** Returns a session-cached icon without scheduling native work. */
  static peek(request: FileIconRequest): string | null | undefined {
    const requestKey = FileIconService.requestKey(request);
    const cachedPath = FileIconService.readCacheEntry(FileIconService.pathCache, requestKey);
    if (cachedPath) return cachedPath.value;

    const reusableKey = FileIconService.reusableTypeKey(request);
    const reusableIcon = reusableKey
      ? FileIconService.readCacheEntry(FileIconService.typeCache, reusableKey)
      : undefined;
    if (!reusableIcon) return undefined;

    FileIconService.writeCacheEntry(
      FileIconService.pathCache,
      requestKey,
      reusableIcon,
      FileIconService.pathCacheLimit
    );
    return reusableIcon.value;
  }

  static resolve(request: FileIconRequest): Promise<string | null> {
    const requestKey = FileIconService.requestKey(request);
    const cached = FileIconService.peek(request);
    if (cached !== undefined) return Promise.resolve(cached);

    const existing = FileIconService.pending.get(requestKey);
    if (existing) return existing;

    const reusableKey = FileIconService.reusableTypeKey(request);
    const existingType = reusableKey ? FileIconService.pendingType.get(reusableKey) : undefined;
    if (existingType) {
      const pending = existingType.then(dataUrl => {
        FileIconService.cacheResolvedPath(requestKey, dataUrl);
        FileIconService.pending.delete(requestKey);
        return dataUrl;
      });
      FileIconService.pending.set(requestKey, pending);
      return pending;
    }

    const pending = new Promise<string | null>(resolve => {
      FileIconService.queue.set(requestKey, { request, resolve });
      FileIconService.scheduleFlush();
    });
    FileIconService.pending.set(requestKey, pending);
    if (reusableKey) {
      FileIconService.pendingType.set(reusableKey, pending);
      void pending.finally(() => {
        if (FileIconService.pendingType.get(reusableKey) === pending) {
          FileIconService.pendingType.delete(reusableKey);
        }
      });
    }
    return pending;
  }

  private static scheduleFlush() {
    if (FileIconService.flushScheduled) return;
    FileIconService.flushScheduled = true;
    queueMicrotask(() => void FileIconService.flush());
  }

  private static async flush() {
    FileIconService.flushScheduled = false;
    const batch = [...FileIconService.queue.values()].slice(0, FileIconService.batchSize);
    for (const queued of batch) FileIconService.queue.delete(FileIconService.requestKey(queued.request));
    if (batch.length === 0) return;

    let response: FileIconBatch | undefined;
    try {
      response = await invoke<FileIconBatch>('get_file_icons', {
        requests: batch.map(queued => queued.request),
      });
    } catch (error) {
      LoggerService.warn('file-icons', 'load-failed', {
        requestedCount: batch.length,
        error: String(error),
      });
    }

    let assets = new Map<string, string>();
    let assignmentByRequest = new Map<string, string>();
    try {
      assets = new Map(response?.assets.map(asset => [asset.iconKey, asset.dataUrl]) ?? []);
      const expiresAt = Date.now() + FileIconService.successfulCacheTtlMs;
      for (const asset of response?.assets ?? []) {
        if (asset.iconKey.startsWith('ext:') || asset.iconKey.startsWith('kind:')) {
          FileIconService.writeCacheEntry(
            FileIconService.typeCache,
            asset.iconKey,
            { value: asset.dataUrl, expiresAt },
            FileIconService.typeCacheLimit
          );
        }
      }
      assignmentByRequest = new Map(
        response?.assignments.map(assignment => [FileIconService.requestKey(assignment), assignment.iconKey]) ?? []
      );
    } catch (error) {
      // A malformed internal response must degrade to fallback icons rather than
      // leaving every row in the batch with a permanently pending Promise.
      LoggerService.warn('file-icons', 'invalid-response', {
        requestedCount: batch.length,
        error: String(error),
      });
    }

    for (const queued of batch) {
      const requestKey = FileIconService.requestKey(queued.request);
      const iconKey = assignmentByRequest.get(requestKey);
      const reusableIcon = iconKey ? FileIconService.readCacheEntry(FileIconService.typeCache, iconKey) : undefined;
      const dataUrl = iconKey ? (assets.get(iconKey) ?? reusableIcon?.value ?? null) : null;
      FileIconService.cacheResolvedPath(requestKey, dataUrl);
      FileIconService.pending.delete(requestKey);
      queued.resolve(dataUrl);
    }

    if (FileIconService.queue.size > 0) FileIconService.scheduleFlush();
  }

  private static requestKey(request: FileIconRequest): string {
    // The same filesystem path may be replaced with a different item kind
    // between scans. Including the kind prevents a stale file icon from being
    // reused after the path becomes a directory, or the reverse.
    return `${request.kind}\0${request.mode}\0${request.path}`;
  }

  private static cacheResolvedPath(requestKey: string, dataUrl: string | null) {
    FileIconService.writeCacheEntry(
      FileIconService.pathCache,
      requestKey,
      {
        value: dataUrl,
        expiresAt: Date.now() + (dataUrl ? FileIconService.successfulCacheTtlMs : FileIconService.failedCacheTtlMs),
      },
      FileIconService.pathCacheLimit
    );
  }

  private static readCacheEntry<T>(cache: Map<string, CacheEntry<T>>, key: string): CacheEntry<T> | undefined {
    const entry = cache.get(key);
    if (!entry) return undefined;
    if (entry.expiresAt <= Date.now()) {
      cache.delete(key);
      return undefined;
    }

    // Map insertion order provides a small LRU without another index. Refreshing
    // the entry on access keeps frequently visible rows resident.
    cache.delete(key);
    cache.set(key, entry);
    return entry;
  }

  private static writeCacheEntry<T>(
    cache: Map<string, CacheEntry<T>>,
    key: string,
    entry: CacheEntry<T>,
    limit: number
  ) {
    cache.delete(key);
    cache.set(key, entry);
    while (cache.size > limit) {
      const oldestKey = cache.keys().next().value;
      if (oldestKey === undefined) return;
      cache.delete(oldestKey);
    }
  }

  private static reusableTypeKey(request: FileIconRequest): string | null {
    if (request.kind === 'directory') {
      // Generic result surfaces intentionally use one operating-system folder
      // icon. Path-aware mode remains available for Downloads, Documents, and
      // other locations whose shell icon communicates their special purpose.
      return request.mode === 'generic' ? 'kind:folder' : null;
    }
    const extension = request.path.split(/[\\/]/).at(-1)?.split('.').at(-1)?.toLowerCase();
    if (!extension || extension === request.path.split(/[\\/]/).at(-1)?.toLowerCase()) {
      return 'kind:file';
    }
    if (['app', 'appex', 'bundle', 'exe', 'ico', 'icns', 'lnk', 'plugin', 'prefpane', 'url'].includes(extension)) {
      return null;
    }
    return `ext:${extension}`;
  }
}
