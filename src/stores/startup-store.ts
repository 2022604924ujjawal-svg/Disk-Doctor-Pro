import { defineStore } from 'pinia';

import type { StartupCatalog, StartupChangePlan, StartupChangeResult, StartupDesiredState } from '@/lib/models/startup';
import { StartupService } from '@/lib/services/startup-service';
import { parseCommandError } from '@/lib/utils/error';

import { useAppStore } from './app-store';

interface StartupState {
  catalog: StartupCatalog | null;
  scanning: boolean;
  cancelling: boolean;
  preparingChange: boolean;
  executingChange: boolean;
  cancellingChange: boolean;
  pendingPlan: StartupChangePlan | null;
  lastChangeResult: StartupChangeResult | null;
}

export const useStartupStore = defineStore('startup', {
  state: (): StartupState => ({
    catalog: null,
    scanning: false,
    cancelling: false,
    preparingChange: false,
    executingChange: false,
    cancellingChange: false,
    pendingPlan: null,
    lastChangeResult: null,
  }),
  actions: {
    async scan() {
      if (this.scanning) return;
      const appStore = useAppStore();
      this.scanning = true;
      this.cancelling = false;
      appStore.clearError();
      try {
        this.catalog = await StartupService.scanCatalog();
      } catch (error) {
        if (parseCommandError(error)?.code !== 'operationCancelled') appStore.reportError(error);
      } finally {
        this.scanning = false;
        this.cancelling = false;
      }
    },
    async cancelScan() {
      if (!this.scanning || this.cancelling) return;
      this.cancelling = true;
      try {
        await StartupService.cancelCatalogScan();
      } catch (error) {
        useAppStore().reportError(error);
        this.cancelling = false;
      }
    },
    async prepareChange(itemIds: string[], desiredState: StartupDesiredState) {
      if (!this.catalog || this.preparingChange || this.executingChange) return;
      this.preparingChange = true;
      this.pendingPlan = null;
      useAppStore().clearError();
      try {
        this.pendingPlan = await StartupService.prepareChange({
          scanId: this.catalog.scanId,
          itemIds,
          desiredState,
        });
      } catch (error) {
        if (parseCommandError(error)?.code !== 'operationCancelled') useAppStore().reportError(error);
      } finally {
        this.preparingChange = false;
        this.cancellingChange = false;
      }
    },
    clearPendingPlan() {
      if (this.executingChange) return;
      this.pendingPlan = null;
    },
    async executeChange(authorizationPrompt: string) {
      if (!this.pendingPlan || this.executingChange) return;
      const planId = this.pendingPlan.planId;
      this.executingChange = true;
      this.lastChangeResult = null;
      useAppStore().clearError();
      try {
        const result = await StartupService.executeChange(planId, authorizationPrompt);
        if (result.catalog) this.catalog = result.catalog;
        this.lastChangeResult = result;
      } catch (error) {
        if (parseCommandError(error)?.code !== 'operationCancelled') useAppStore().reportError(error);
      } finally {
        this.pendingPlan = null;
        this.executingChange = false;
        this.cancellingChange = false;
      }
    },
    async cancelChange() {
      if ((!this.preparingChange && !this.executingChange) || this.cancellingChange) {
        return;
      }
      this.cancellingChange = true;
      try {
        await StartupService.cancelChange();
      } catch (error) {
        if (parseCommandError(error)?.code !== 'operationCancelled') useAppStore().reportError(error);
        this.cancellingChange = false;
      }
    },
  },
});
