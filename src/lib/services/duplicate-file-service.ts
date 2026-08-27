import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { EVENT_NAMES } from '@/lib/models/telemetry';
import type { DuplicateFilesResult, DuplicateGroupBatch, DuplicateGroupPage } from '@/lib/models/duplicate-file';
import type { PermanentDeleteBatchResult, PermanentDeleteCandidate } from '@/lib/models/permanent-delete';
import type { TraversalProgress } from '@/lib/models/progress';

export class DuplicateFileService {
  static find(roots: string[], minimumBytes: number): Promise<DuplicateFilesResult> {
    return invoke<DuplicateFilesResult>('find_duplicate_files', { roots, minimumBytes });
  }

  static listenProgress(handler: (progress: TraversalProgress) => void): Promise<UnlistenFn> {
    return listen<TraversalProgress>(EVENT_NAMES.duplicateFilesProgress, event => handler(event.payload));
  }

  static listenGroups(handler: (batch: DuplicateGroupBatch) => void): Promise<UnlistenFn> {
    return listen<DuplicateGroupBatch>(EVENT_NAMES.duplicateFileGroups, event => handler(event.payload));
  }

  static page(scanId: number, offset: number, limit: number): Promise<DuplicateGroupPage> {
    return invoke<DuplicateGroupPage>('get_duplicate_file_groups', { scanId, offset, limit });
  }

  static deletePermanently(
    scanId: number,
    candidates: PermanentDeleteCandidate[]
  ): Promise<PermanentDeleteBatchResult> {
    return invoke<PermanentDeleteBatchResult>('delete_duplicate_files_permanently', { scanId, candidates });
  }

  static cancel(): Promise<void> {
    return invoke<void>('cancel_duplicate_files');
  }
}
