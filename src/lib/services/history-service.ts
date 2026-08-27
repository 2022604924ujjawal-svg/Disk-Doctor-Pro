import { invoke } from '@tauri-apps/api/core';

import type { OperationRecord } from '@/lib/models/history';

export class HistoryService {
  static list(): Promise<OperationRecord[]> {
    return invoke<OperationRecord[]>('list_history');
  }

  static clear(): Promise<void> {
    return invoke<void>('clear_history');
  }
}
