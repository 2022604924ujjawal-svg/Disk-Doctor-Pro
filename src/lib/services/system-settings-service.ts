import { invoke } from '@tauri-apps/api/core';

import type {
  SystemSettingsCatalog,
  SystemSettingsChangePlan,
  SystemSettingsChangeResult,
  SystemSettingsChangeSelection,
} from '@/lib/models/system-settings';

export class SystemSettingsService {
  static scan(): Promise<SystemSettingsCatalog> {
    return invoke<SystemSettingsCatalog>('scan_system_settings');
  }

  static cancelScan(): Promise<void> {
    return invoke<void>('cancel_system_settings_scan');
  }

  static prepareChange(selection: SystemSettingsChangeSelection): Promise<SystemSettingsChangePlan> {
    return invoke<SystemSettingsChangePlan>('prepare_system_settings_change', { selection });
  }

  static executeChange(planId: string): Promise<SystemSettingsChangeResult> {
    return invoke<SystemSettingsChangeResult>('execute_system_settings_change', { planId });
  }
}
