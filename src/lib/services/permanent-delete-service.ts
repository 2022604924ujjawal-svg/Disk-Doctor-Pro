import { invoke } from '@tauri-apps/api/core';

import type { PermanentDeleteBatchResult } from '@/lib/models/permanent-delete';

export class PermanentDeleteService {
  static deleteFiles(scanId: number, selectedPaths: string[]): Promise<PermanentDeleteBatchResult> {
    return invoke<PermanentDeleteBatchResult>('delete_files_permanently', { scanId, selectedPaths });
  }
}
