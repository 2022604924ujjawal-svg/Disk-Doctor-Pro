import { invoke } from '@tauri-apps/api/core';

/** Exposes native file-system item actions without leaking Tauri into pages. */
export class FileManagerService {
  static openAnalysisEntry(scanId: number, selectedPath: string): Promise<void> {
    return invoke<void>('open_analysis_entry', { scanId, selectedPath });
  }

  static openLargeFileEntry(scanId: number, selectedPath: string): Promise<void> {
    return invoke<void>('open_large_file_entry', { scanId, selectedPath });
  }

  static openDuplicateFileEntry(scanId: number, selectedPath: string): Promise<void> {
    return invoke<void>('open_duplicate_file_entry', { scanId, selectedPath });
  }

  static reveal(path: string): Promise<void> {
    return invoke<void>('reveal_in_file_manager', { path });
  }

  static openApplicationLogs(): Promise<void> {
    return invoke<void>('open_application_log_directory');
  }
}
