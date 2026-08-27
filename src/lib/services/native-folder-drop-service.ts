import type { UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWebview, type DragDropEvent } from '@tauri-apps/api/webview';

/**
 * Owns the WebView-specific folder drop subscription. Keeping this adapter
 * separate prevents ordinary folder validation and persisted-scope restoration
 * from pulling window event integration into their dependency graph.
 */
export class NativeFolderDropService {
  static listen(listener: (event: DragDropEvent) => void): Promise<UnlistenFn> {
    return getCurrentWebview().onDragDropEvent(listener);
  }
}
