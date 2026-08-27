import { getVersion } from '@tauri-apps/api/app';
import type { DownloadEvent, Update } from '@tauri-apps/plugin-updater';

import {
  APP_DISTRIBUTION_IDS,
  APP_UPDATE_ACTION_IDS,
  APP_UPDATE_CHECK_TIMEOUT_MS,
  APP_UPDATE_DOWNLOAD_TIMEOUT_MS,
  type AppDistribution,
  type AppUpdateDownloadProgress,
  type AppUpdateInfo,
} from '@/lib/models/app-update';
import { AppUpdateMetadataService } from '@/lib/services/app-update-metadata-service';

export class AppUpdateService {
  private static pendingUpdate: Update | null = null;
  private static pendingRequestHeaders: Record<string, string> | null = null;
  private static checkPromise: Promise<AppUpdateInfo | null> | null = null;
  private static downloaded = false;

  static currentVersion(): Promise<string> {
    return getVersion();
  }

  static check(language: string, distribution: AppDistribution): Promise<AppUpdateInfo | null> {
    if (AppUpdateService.checkPromise) return AppUpdateService.checkPromise;

    AppUpdateService.checkPromise = AppUpdateService.performCheck(language, distribution).finally(() => {
      AppUpdateService.checkPromise = null;
    });
    return AppUpdateService.checkPromise;
  }

  static async download(onProgress: (progress: AppUpdateDownloadProgress) => void): Promise<void> {
    const update = AppUpdateService.pendingUpdate;
    const headers = AppUpdateService.pendingRequestHeaders;
    if (!update || !headers) throw new Error('No checked update is available for download.');
    if (AppUpdateService.downloaded) return;

    let downloadedBytes = 0;
    let totalBytes: number | null = null;
    const reportProgress = (event: DownloadEvent) => {
      if (event.event === 'Started') {
        totalBytes = event.data.contentLength ?? null;
      } else if (event.event === 'Progress') {
        downloadedBytes += event.data.chunkLength;
      }
      onProgress({
        downloadedBytes,
        totalBytes,
        finished: event.event === 'Finished',
      });
    };

    await update.download(reportProgress, {
      headers,
      timeout: APP_UPDATE_DOWNLOAD_TIMEOUT_MS,
    });
    AppUpdateService.downloaded = true;
  }

  static async installDownloaded(): Promise<void> {
    const update = AppUpdateService.pendingUpdate;
    if (!update || !AppUpdateService.downloaded) throw new Error('No downloaded update is available for installation.');

    await update.install();
    AppUpdateService.pendingUpdate = null;
    AppUpdateService.pendingRequestHeaders = null;
    AppUpdateService.downloaded = false;
  }

  static async restartApplication(): Promise<void> {
    const { relaunch } = await import('@tauri-apps/plugin-process');
    await relaunch();
  }

  static async dispose(): Promise<void> {
    const update = AppUpdateService.pendingUpdate;
    AppUpdateService.pendingUpdate = null;
    AppUpdateService.pendingRequestHeaders = null;
    AppUpdateService.downloaded = false;
    if (update) await update.close();
  }

  private static async performCheck(language: string, distribution: AppDistribution): Promise<AppUpdateInfo | null> {
    await AppUpdateService.dispose();
    const headers = await AppUpdateMetadataService.createHeaders(language, distribution);
    const { check } = await import('@tauri-apps/plugin-updater');
    const update = await check({
      headers,
      timeout: APP_UPDATE_CHECK_TIMEOUT_MS,
    });
    if (!update) return null;

    const info = {
      currentVersion: update.currentVersion,
      version: update.version,
      date: update.date,
      notes: update.body?.trim() ?? '',
    };
    if (distribution === APP_DISTRIBUTION_IDS.portable) {
      try {
        return {
          ...info,
          action: APP_UPDATE_ACTION_IDS.manualDownload,
          manualDownloadUrl: AppUpdateService.resolvePortableDownloadUrl(update),
        };
      } finally {
        await update.close();
      }
    }

    AppUpdateService.pendingUpdate = update;
    AppUpdateService.pendingRequestHeaders = headers;
    return {
      ...info,
      action: APP_UPDATE_ACTION_IDS.automaticInstall,
    };
  }

  private static resolvePortableDownloadUrl(update: Update): string {
    const rawUrl = update.rawJson.url;
    if (typeof rawUrl !== 'string') throw new Error('The portable update response is missing its download URL.');

    const url = new URL(rawUrl);
    const expectedPath = `/api/updates/${encodeURIComponent(update.version)}/windows/x86_64/download`;
    if (
      url.origin !== 'https://mangodisk.app' ||
      url.pathname !== expectedPath ||
      url.searchParams.size !== 1 ||
      url.searchParams.get('distribution') !== APP_DISTRIBUTION_IDS.portable ||
      url.hash
    ) {
      throw new Error('The portable update response contains an unsupported download URL.');
    }

    return url.href;
  }
}
