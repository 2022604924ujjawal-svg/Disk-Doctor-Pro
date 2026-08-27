import { invoke } from '@tauri-apps/api/core';

import { APP_DISTRIBUTION_IDS, type AppDistribution } from '@/lib/models/app-update';

export class AppDistributionService {
  static async current(): Promise<AppDistribution> {
    const distribution = await invoke<unknown>('get_app_distribution');
    if (!Object.values(APP_DISTRIBUTION_IDS).includes(distribution as AppDistribution)) {
      throw new Error('The application distribution is invalid.');
    }
    return distribution as AppDistribution;
  }
}
