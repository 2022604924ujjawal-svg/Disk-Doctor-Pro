import { invoke } from '@tauri-apps/api/core';

import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import { LoggerService } from '@/lib/services/logger-service';

/** Opens fixed macOS settings destinations exposed by the native adapter. */
export class MacOsSystemSettingsService {
  static async openLoginItems(): Promise<void> {
    LoggerService.info(LOG_DOMAINS.startup, LOG_EVENTS.startupSystemSettingsOpenRequested);
    await invoke<void>('open_macos_login_items_settings');
  }
}
