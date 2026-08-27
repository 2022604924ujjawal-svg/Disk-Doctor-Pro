import { defineStore } from 'pinia';

import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import type { LargeFileEntry, LargeFilesResult } from '@/lib/models/large-file';
import type { TraversalProgress } from '@/lib/models/progress';
import { LargeFileService } from '@/lib/services/large-file-service';
import { LoggerService } from '@/lib/services/logger-service';
import { PermanentDeleteService } from '@/lib/services/permanent-delete-service';
import { LargeFileResultUtils } from '@/lib/utils/large-file-result';

import { useAppStore } from './app-store';
import { useHistoryStore } from './history-store';

interface LargeFilesState {
  result: LargeFilesResult | null;
  progress: TraversalProgress | null;
  loading: boolean;
  cancelling: boolean;
  deleting: boolean;
}

export const useLargeFilesStore = defineStore('large-files', {
  state: (): LargeFilesState => ({
    result: null,
    progress: null,
    loading: false,
    cancelling: false,
    deleting: false,
  }),
  actions: {
    async find(path: string | undefined, minimumBytes: number, refresh = false) {
      if (this.loading || this.deleting) return;
      const appStore = useAppStore();
      this.loading = true;
      this.cancelling = false;
      this.progress = null;
      appStore.clearError();
      let unlisten: (() => void) | undefined;
      try {
        unlisten = await LargeFileService.listenProgress(progress => {
          this.progress = progress;
        });
        const result = await LargeFileService.find(path, minimumBytes, refresh);
        // A scan started with an old threshold cannot replace current results.
        if (appStore.settings.largeFileMinimumBytes === minimumBytes) {
          this.result = result;
        } else {
          LoggerService.info(LOG_DOMAINS.largeFiles, LOG_EVENTS.staleScanResultIgnored, {
            requestedMinimumBytes: minimumBytes,
            currentMinimumBytes: appStore.settings.largeFileMinimumBytes,
          });
        }
      } catch (error) {
        if (!this.cancelling) appStore.reportError(error);
      } finally {
        unlisten?.();
        this.progress = null;
        this.loading = false;
        this.cancelling = false;
      }
    },
    async cancel() {
      if (!this.loading || this.cancelling) return;
      this.cancelling = true;
      try {
        await LargeFileService.cancel();
      } catch (error) {
        useAppStore().reportError(error);
        this.cancelling = false;
      }
    },
    async deletePermanently(path: string) {
      const entry = this.result?.entries.find(item => item.path === path);
      if (!entry) return;
      await this.deleteManyPermanently([entry]);
    },
    async deleteManyPermanently(entries: LargeFileEntry[]) {
      if (this.loading || this.deleting || !entries.length) return;
      const appStore = useAppStore();
      const sourceResult = this.result;
      this.deleting = true;
      appStore.clearError();
      try {
        if (!sourceResult) return;
        const result = await PermanentDeleteService.deleteFiles(
          sourceResult.scanId,
          entries.map(entry => entry.path)
        );
        if (sourceResult && this.result === sourceResult) {
          const removedPaths = new Set(result.removedPaths);
          this.result = LargeFileResultUtils.removePaths(sourceResult, removedPaths, result.releasedBytes);
        }
        await useHistoryStore().load({ reportError: false });
        if (result.failed.length) {
          // Item-level failures are part of a completed batch and are shown by
          // the page as one warning summary. Logging only aggregate evidence
          // avoids a contradictory global error and keeps private paths out.
          LoggerService.warn(LOG_DOMAINS.largeFiles, LOG_EVENTS.deleteCompletedWithFailures, {
            removedCount: result.removedPaths.length,
            failedCount: result.failed.length,
            releasedBytes: result.releasedBytes,
          });
        }
        return result;
      } catch (error) {
        appStore.reportError(error);
      } finally {
        this.deleting = false;
      }
    },
  },
});
