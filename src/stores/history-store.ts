import { defineStore } from 'pinia';

import type { OperationRecord } from '@/lib/models/history';
import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import { HistoryService } from '@/lib/services/history-service';
import { LoggerService } from '@/lib/services/logger-service';

import { useAppStore } from './app-store';

interface HistoryState {
  records: OperationRecord[];
  loading: boolean;
}

interface HistoryLoadOptions {
  reportError?: boolean;
}

export const useHistoryStore = defineStore('history', {
  state: (): HistoryState => ({ records: [], loading: false }),
  actions: {
    async load({ reportError = true }: HistoryLoadOptions = {}) {
      this.loading = true;
      try {
        this.records = await HistoryService.list();
      } catch (error) {
        LoggerService.warn(LOG_DOMAINS.history, LOG_EVENTS.historyRefreshFailed, { error });
        if (reportError) useAppStore().reportError(error);
      } finally {
        this.loading = false;
      }
    },
    async clear() {
      this.loading = true;
      try {
        await HistoryService.clear();
        this.records = [];
        return true;
      } catch (error) {
        useAppStore().reportError(error);
        return false;
      } finally {
        this.loading = false;
      }
    },
  },
});
