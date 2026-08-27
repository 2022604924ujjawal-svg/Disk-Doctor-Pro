import { invoke } from '@tauri-apps/api/core';

import type {
  StartupCatalog,
  StartupChangePlan,
  StartupChangeResult,
  StartupChangeSelection,
} from '@/lib/models/startup';

export class StartupService {
  static scanCatalog(): Promise<StartupCatalog> {
    return invoke<StartupCatalog>('scan_startup_catalog');
  }

  static cancelCatalogScan(): Promise<void> {
    return invoke<void>('cancel_startup_catalog_scan');
  }

  static prepareChange(selection: StartupChangeSelection): Promise<StartupChangePlan> {
    return invoke<StartupChangePlan>('prepare_startup_change', { selection });
  }

  static executeChange(planId: string, authorizationPrompt: string): Promise<StartupChangeResult> {
    return invoke<StartupChangeResult>('execute_startup_change', { planId, authorizationPrompt });
  }

  static cancelChange(): Promise<void> {
    return invoke<void>('cancel_startup_change');
  }
}
