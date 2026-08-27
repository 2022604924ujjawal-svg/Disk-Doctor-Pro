import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';

/**
 * Coordinates native folder selection and directory validation. Native drag
 * events live in a separate adapter so stores can validate saved paths without
 * loading WebView-only integration code.
 */
export class FolderSelectionService {
  static async select(multiple: boolean, title: string, defaultPath?: string): Promise<string[]> {
    const selected = await open({ directory: true, multiple, title, defaultPath });
    if (!selected) return [];
    return Array.isArray(selected) ? selected : [selected];
  }

  /**
   * Native drag events contain files and directories. Core validates the
   * current filesystem type before a path enters any scanning workflow.
   */
  static filterExistingDirectories(paths: string[]): Promise<string[]> {
    return invoke<string[]>('filter_directory_paths', { paths });
  }
}
