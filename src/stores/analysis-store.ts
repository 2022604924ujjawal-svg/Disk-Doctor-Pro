import { defineStore } from 'pinia';

import { ANALYSIS_RESULT_CACHE_LIMIT } from '@/lib/models/analysis';
import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import type { AnalysisResult, DirectoryEntryInfo } from '@/lib/models/analysis';
import type { TraversalProgress } from '@/lib/models/progress';
import { AnalysisService } from '@/lib/services/analysis-service';
import { LoggerService } from '@/lib/services/logger-service';
import { AnalysisCacheUtils } from '@/lib/utils/analysis-cache';
import { PathUtils } from '@/lib/utils/path';

import { useAppStore } from './app-store';

interface AnalysisState {
  result: AnalysisResult | null;
  cache: Record<string, AnalysisResult>;
  cacheOrder: string[];
  homePath: string;
  progress: TraversalProgress | null;
  pending: boolean;
  cancelling: boolean;
  deleting: boolean;
}

export const useAnalysisStore = defineStore('analysis', {
  state: (): AnalysisState => ({
    result: null,
    cache: {},
    cacheOrder: [],
    homePath: '',
    progress: null,
    pending: false,
    cancelling: false,
    deleting: false,
  }),
  actions: {
    async analyze(path?: string, refresh = false, setHome = false) {
      if (this.pending || this.deleting) return;
      const appStore = useAppStore();
      const target = path?.trim() || appStore.disk?.mountPoint;
      const targetKey = target ? AnalysisCacheUtils.key(target) : '';
      if (!refresh && targetKey && this.cache[targetKey]) {
        this.result = this.cache[targetKey];
        this.cacheOrder = AnalysisCacheUtils.touch(this.cacheOrder, targetKey);
        if (setHome) this.homePath = PathUtils.display(this.result.root);
        return;
      }
      if (refresh && targetKey) {
        this.cache = Object.fromEntries(
          Object.entries(this.cache).filter(([key]) => !PathUtils.isSameOrChildKey(key, targetKey))
        );
        this.cacheOrder = AnalysisCacheUtils.retainExisting(this.cacheOrder, this.cache);
      }
      this.pending = true;
      this.cancelling = false;
      this.progress = null;
      appStore.clearError();
      let unlisten: (() => void) | undefined;
      try {
        unlisten = await AnalysisService.listenProgress(progress => {
          this.progress = progress;
        });
        const result = await AnalysisService.analyze(target, refresh);
        this.result = result;
        const cached = AnalysisCacheUtils.store(this.cache, this.cacheOrder, result, ANALYSIS_RESULT_CACHE_LIMIT);
        this.cache = cached.cache;
        this.cacheOrder = cached.order;
        if (setHome || !this.homePath) this.homePath = PathUtils.display(result.root);
      } catch (error) {
        if (!this.cancelling) appStore.reportError(error);
      } finally {
        unlisten?.();
        this.progress = null;
        this.pending = false;
        this.cancelling = false;
      }
    },
    async cancel() {
      if (!this.pending || this.cancelling) return;
      this.cancelling = true;
      try {
        await AnalysisService.cancel();
      } catch (error) {
        useAppStore().reportError(error);
        this.cancelling = false;
      }
    },
    async deletePermanently(entry: DirectoryEntryInfo) {
      if (!this.result || this.pending || this.deleting) return;
      const appStore = useAppStore();
      this.deleting = true;
      appStore.clearError();
      try {
        const removed = await AnalysisService.deletePermanently(this.result.scanId, entry.path);

        // Remove descendant results and update every cached ancestor so
        // navigation cannot reveal entries that were already deleted.
        this.cache = AnalysisCacheUtils.syncAfterDelete(
          this.cache,
          removed.removedPath,
          removed.releasedBytes,
          removed.removedFileCount
        );
        this.cacheOrder = AnalysisCacheUtils.retainExisting(this.cacheOrder, this.cache);
        // Refresh the currently visible result rather than the path where the
        // operation started, preserving correctness if a future UI can navigate.
        const visibleResultKey = this.result ? AnalysisCacheUtils.key(this.result.root) : null;
        this.result = visibleResultKey ? (this.cache[visibleResultKey] ?? null) : null;
        LoggerService.info(LOG_DOMAINS.analysis, LOG_EVENTS.analysisCacheSyncedAfterDelete, {
          path: removed.removedPath,
          releasedBytes: removed.releasedBytes,
        });
      } catch (error) {
        appStore.reportError(error);
      } finally {
        this.deleting = false;
      }
    },
  },
});
