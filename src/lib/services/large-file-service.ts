import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { EVENT_NAMES } from '@/lib/models/telemetry';
import type { LargeFilesResult } from '@/lib/models/large-file';
import type { TraversalProgress } from '@/lib/models/progress';

export class LargeFileService {
  static find(path: string | undefined, minimumBytes: number, refresh = false): Promise<LargeFilesResult> {
    return invoke<LargeFilesResult>('find_large_files', {
      path: path?.trim() || null,
      minimumBytes,
      refresh,
    });
  }

  static listenProgress(handler: (progress: TraversalProgress) => void): Promise<UnlistenFn> {
    return listen<TraversalProgress>(EVENT_NAMES.largeFilesProgress, event => handler(event.payload));
  }

  static cancel(): Promise<void> {
    return invoke<void>('cancel_large_files');
  }
}
