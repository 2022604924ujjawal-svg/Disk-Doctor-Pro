<script setup lang="ts">
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { toast } from 'vue-sonner';

import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import MdMiddleEllipsis from '@/components/custom/md-middle-ellipsis.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { APP_UPDATE_AUTOMATIC_CHECK_DELAY_MS, APP_UPDATE_STATUS_IDS } from '@/lib/models/app-update';
import type { ApplicationLeftoverCandidate, ApplicationUninstallBatchSelection } from '@/lib/models/application';
import type { ApplicationCloseMode } from '@/lib/models/application-close';
import type { DirectoryEntryInfo } from '@/lib/models/analysis';
import type { DuplicateFileEntry } from '@/lib/models/duplicate-file';
import type { LargeFileEntry } from '@/lib/models/large-file';
import { CLEANUP_OPERATION_IDS, type CleanupScanScope } from '@/lib/models/cleanup';
import { ICON_NAMES } from '@/lib/models/ui';
import {
  createSidebarLayoutState,
  PAGE_IDS,
  resizeSidebarLayout,
  toggleSidebarLayout,
} from '@/lib/models/application-shell';
import type { AppSettings } from '@/lib/models/settings';
import type { PageId } from '@/lib/models/application-shell';
import { ApplicationMenuService } from '@/lib/services/application-menu-service';
import { FileManagerService } from '@/lib/services/file-manager-service';
import { LinkService } from '@/lib/services/link-service';
import { OperatingSystemService } from '@/lib/services/operating-system-service';
import { CleanupRuleTextUtils, type CleanupRuleMessageResolver } from '@/lib/utils/cleanup-rule-text';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';
import { PathUtils } from '@/lib/utils/path';
import { useAnalysisStore } from '@/stores/analysis-store';
import { useApplicationStore } from '@/stores/application-store';
import { useAppUpdateStore } from '@/stores/app-update-store';
import { useAppStore } from '@/stores/app-store';
import { useCleanupStore } from '@/stores/cleanup-store';
import { useDuplicateFilesStore } from '@/stores/duplicate-files-store';
import { useHistoryStore } from '@/stores/history-store';
import { useLargeFilesStore } from '@/stores/large-files-store';
import { useStorageScopeStore } from '@/stores/storage-scope-store';
import { useStartupStore } from '@/stores/startup-store';
import { useSystemSettingsStore } from '@/stores/system-settings-store';

import CleanupPage from '@/pages/cleanup/index.vue';

import MdSidebar from './components/md-sidebar.vue';
import MdWindowTitlebar from './components/md-window-titlebar.vue';

// Cleanup is the startup page. Secondary pages remain separate chunks, while
// idle preloading and guarded navigation prevent their first render from
// replacing the current page with an empty async-component placeholder.
const loadAnalysisPage = () => import('@/pages/analysis/index.vue');
const loadApplicationUninstallPage = () => import('@/pages/application-uninstall/index.vue');
const loadDuplicateFilesPage = () => import('@/pages/duplicate-files/index.vue');
const loadHistoryPage = () => import('@/pages/history/index.vue');
const loadLargeFilesPage = () => import('@/pages/large-files/index.vue');
const loadSettingsPage = () => import('@/pages/settings/index.vue');
const loadStartupPage = () => import('@/pages/startup/index.vue');
const loadSystemOptimizationPage = () => import('@/pages/system-optimization/index.vue');
const pageLoaders: Partial<Record<PageId, () => Promise<unknown>>> = {
  [PAGE_IDS.analysis]: loadAnalysisPage,
  [PAGE_IDS.applicationUninstall]: loadApplicationUninstallPage,
  [PAGE_IDS.duplicateFiles]: loadDuplicateFilesPage,
  [PAGE_IDS.history]: loadHistoryPage,
  [PAGE_IDS.largeFiles]: loadLargeFilesPage,
  [PAGE_IDS.settings]: loadSettingsPage,
  [PAGE_IDS.startup]: loadStartupPage,
  [PAGE_IDS.systemOptimization]: loadSystemOptimizationPage,
};
const AnalysisPage = defineAsyncComponent(loadAnalysisPage);
const ApplicationUninstallPage = defineAsyncComponent(loadApplicationUninstallPage);
const DuplicateFilesPage = defineAsyncComponent(loadDuplicateFilesPage);
const HistoryPage = defineAsyncComponent(loadHistoryPage);
const LargeFilesPage = defineAsyncComponent(loadLargeFilesPage);
const SettingsPage = defineAsyncComponent(loadSettingsPage);
const StartupPage = defineAsyncComponent(loadStartupPage);
const SystemOptimizationPage = defineAsyncComponent(loadSystemOptimizationPage);
const MdAboutDialog = defineAsyncComponent(() => import('./components/md-about-dialog.vue'));

const { rt, t, te, tm } = useI18n({ useScope: 'global' });

const CLEANUP_RULE_ENTRY_KEY = /^cleanupRules\.entries\.(.+)\.(name|description|impact)$/u;
type CleanupRuleEntry = Partial<Record<'description' | 'impact' | 'name', Parameters<typeof rt>[0]>>;

// Rule IDs contain dots, so resolve them as exact keys inside the entries
// object rather than allowing vue-i18n to interpret them as nested paths.
const resolveCleanupRuleMessage: CleanupRuleMessageResolver = (key, parameters) => {
  const entryMatch = CLEANUP_RULE_ENTRY_KEY.exec(key);
  if (entryMatch) {
    const entries = tm('cleanupRules.entries') as Record<string, CleanupRuleEntry>;
    const message = entries[entryMatch[1]]?.[entryMatch[2] as keyof CleanupRuleEntry];
    return message === undefined ? undefined : rt(message);
  }
  return te(key) ? t(key, parameters ?? {}) : undefined;
};

const store = useAppStore();
const appUpdateStore = useAppUpdateStore();
const cleanupStore = useCleanupStore();
const analysisStore = useAnalysisStore();
const applicationStore = useApplicationStore();
const cleanupOrchestrating = ref(false);
const deepCleanupCancelling = ref(false);
const cleanupCancellationConfirmOpen = ref(false);
const cleanupCancellationRetried = ref(false);
const settingsFocusRevision = ref(0);
const historyStore = useHistoryStore();
const largeFilesStore = useLargeFilesStore();
const duplicateFilesStore = useDuplicateFilesStore();
const storageScopeStore = useStorageScopeStore();
const startupStore = useStartupStore();
const systemSettingsStore = useSystemSettingsStore();
// WebKit can leave range-based media-query utilities in their collapsed state
// after a native window is narrowed and widened again. The explicit state also
// keeps a user's toggle separate from the responsive window-width decision.
const sidebarLayout = ref(createSidebarLayoutState(window.innerWidth));
const sidebarExpanded = computed(() => sidebarLayout.value.expanded);
const UPDATE_CHECK_ERROR_TOAST_ID = 'app-update-check-error';
const LARGE_FILE_DELETE_TOAST_ID = 'large-file-delete-result';
const DUPLICATE_FILE_DELETE_TOAST_ID = 'duplicate-file-delete-result';
const DEEP_CLEANUP_TOAST_ID = 'deep-cleanup-result';
const APPLICATION_ERROR_TOAST_ID = 'application-error';
const localizedCleanupScan = computed(() =>
  cleanupStore.scan ? CleanupRuleTextUtils.snapshot(cleanupStore.scan, resolveCleanupRuleMessage) : null
);
const localizedCleanupResult = computed(() =>
  cleanupStore.result ? CleanupRuleTextUtils.cleanupResult(cleanupStore.result, resolveCleanupRuleMessage) : null
);
const localizedHistory = computed(() => CleanupRuleTextUtils.records(historyStore.records, resolveCleanupRuleMessage));
const cleanupBusy = computed(
  () =>
    cleanupOrchestrating.value ||
    cleanupStore.loading ||
    cleanupStore.closingApplications ||
    applicationStore.scanningLeftovers ||
    applicationStore.deletingLeftovers
);
const exclusiveOperationBusy = computed(
  () =>
    cleanupBusy.value ||
    analysisStore.pending ||
    analysisStore.deleting ||
    largeFilesStore.loading ||
    largeFilesStore.deleting ||
    duplicateFilesStore.loading ||
    duplicateFilesStore.deleting ||
    applicationStore.scanningUninstallCatalog ||
    applicationStore.closingUninstallApplications ||
    applicationStore.preparingUninstall ||
    applicationStore.executingUninstall ||
    startupStore.scanning ||
    startupStore.preparingChange ||
    startupStore.executingChange ||
    systemSettingsStore.scanning ||
    systemSettingsStore.preparing ||
    systemSettingsStore.executing
);
// Custom title bars keep the application chrome visually continuous. macOS
// only needs a drag region beneath the native traffic lights, while Windows
// renders explicit controls because its native decorations are disabled.
const currentPlatform = OperatingSystemService.currentPlatform();
const isMacOs = currentPlatform === 'macos';
const isWindows = currentPlatform === 'windows';
const customTitlebarPlatform = computed<'macos' | 'windows' | null>(() => {
  if (isMacOs) return 'macos';
  if (isWindows) return 'windows';
  return null;
});
const cleanupScanning = computed(
  () =>
    cleanupStore.loading &&
    [CLEANUP_OPERATION_IDS.scanning, CLEANUP_OPERATION_IDS.cancelling].includes(cleanupStore.operation)
);
const globalLoading = computed(
  () => (cleanupStore.loading && !cleanupScanning.value) || applicationStore.deletingLeftovers
);
const cleanupLoadingMessage = computed(() => {
  if (cleanupStore.operation === CLEANUP_OPERATION_IDS.cancelling) return t('loading.cancelling');
  if (cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing) return t('loading.previewing');
  if (cleanupStore.operation === CLEANUP_OPERATION_IDS.cleaning) return t('loading.cleaning');
  return t('loading.scanning');
});
const globalLoadingMessage = computed(() => {
  if (deepCleanupCancelling.value) return t('loading.cancellingCleanup');
  if (applicationStore.deletingLeftovers) return t('loading.cleaningApplicationLeftovers');
  return cleanupStore.loading ? cleanupLoadingMessage.value : t('loading.starting');
});
const globalLoadingHint = computed(() => {
  if (deepCleanupCancelling.value) return t('loading.cancellingCleanupHint');
  return cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing
    ? t('loading.previewingSafetyHint')
    : t('loading.cleaningSafetyHint');
});
const loadingClockMs = ref(Date.now());
const cleanupExecutionActive = computed(
  () =>
    cleanupStore.loading &&
    (cleanupStore.operation === CLEANUP_OPERATION_IDS.cleaning ||
      cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing)
);
const destructiveCleanupActive = computed(
  () =>
    (cleanupStore.loading && cleanupStore.operation === CLEANUP_OPERATION_IDS.cleaning) ||
    applicationStore.deletingLeftovers
);
watch(destructiveCleanupActive, active => {
  if (!active) cleanupCancellationConfirmOpen.value = false;
});
const cleanupExecutionProgress = computed(() => cleanupStore.executionProgress);
const cleanupExecutionListElement = ref<HTMLElement | null>(null);
watch(cleanupExecutionProgress, progress => {
  if (!progress || !deepCleanupCancelling.value || cleanupCancellationRetried.value) return;
  // The execution listener is registered before Core starts. A very fast click
  // can therefore send cancellation while no guard exists yet. The first
  // progress event proves that the guard is active and safely retries the
  // idempotent request before cleanup leaves validation.
  cleanupCancellationRetried.value = true;
  void cleanupStore.cancelExecution().catch(() => undefined);
});
watch(
  () => cleanupExecutionProgress.value?.currentRuleId,
  async currentRuleId => {
    if (!currentRuleId) return;
    await nextTick();
    cleanupExecutionListElement.value
      ?.querySelector<HTMLElement>('.cleanup-execution-item.is-active')
      ?.scrollIntoView({ block: 'nearest' });
  }
);
const cleanupExecutionElapsedMs = computed(() => {
  const reported = cleanupExecutionProgress.value?.elapsedMs ?? 0;
  const startedAt = cleanupStore.executionStartedAtMs;
  const live = startedAt === null ? 0 : Math.max(0, loadingClockMs.value - startedAt);
  return Math.max(reported, live);
});
const cleanupExecutionElapsedSeconds = computed(() => Math.floor(cleanupExecutionElapsedMs.value / 1000));
const cleanupExecutionItems = computed(() => {
  const progress = cleanupExecutionProgress.value;
  const completed = new Map(progress?.completedRuleResults.map(result => [result.ruleId, result]) ?? []);
  const ruleIds = cleanupStore.executionRuleIds.length ? cleanupStore.executionRuleIds : cleanupStore.selectedRuleIds;
  return ruleIds.map(ruleId => {
    const rule = localizedCleanupScan.value?.rules.find(item => item.ruleId === ruleId);
    const result = completed.get(ruleId);
    const active = !result && progress?.currentRuleId === ruleId;
    const detailIsPath = Boolean(active && progress?.currentItemPath);
    let detail = t('loading.cleanupItemWaiting');
    if (result?.status === 'previewed') {
      detail = t('loading.cleanupItemChecked');
    } else if (result?.status === 'partial') {
      detail = t('loading.cleanupItemPartial', {
        count: FormatUtils.integer(result.affectedItemCount),
        size: ByteSizeService.bytes(result.releasedBytes),
      });
    } else if (result && ['blocked', 'failed'].includes(result.status)) {
      detail = t('loading.cleanupItemSkipped');
    } else if (result) {
      detail = t('loading.cleanupItemCompleted', {
        count: FormatUtils.integer(result.affectedItemCount),
        size: ByteSizeService.bytes(result.releasedBytes),
      });
    } else if (active && progress?.stage === 'validating') {
      detail = t('loading.cleanupItemValidating');
    } else if (active && progress?.currentItemPath) {
      detail = PathUtils.display(progress.currentItemPath);
    } else if (active) {
      detail = t('loading.cleanupItemSystemProcessing');
    }
    return {
      detail,
      detailIsPath,
      name: rule?.name ?? CleanupRuleTextUtils.fallbackName(ruleId),
      ruleId,
      state: result
        ? ['completed', 'previewed'].includes(result.status)
          ? 'completed'
          : 'skipped'
        : active
          ? 'active'
          : 'pending',
    };
  });
});
const cleanupExecutionStageLabel = computed(() => {
  switch (cleanupExecutionProgress.value?.stage) {
    case 'validating':
      return t('loading.validating');
    case 'finalizing':
      return t('loading.finalizing');
    default:
      return cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing
        ? t('loading.previewing')
        : t('loading.cleaning');
  }
});
const cleanupExecutionRuleProgress = computed(() => {
  const progress = cleanupExecutionProgress.value;
  const total = Math.max(progress?.totalRuleCount ?? cleanupStore.selectedRuleIds.length, 1);
  const completed =
    progress?.stage === 'validating'
      ? (progress.validatedRuleCount ?? 0)
      : progress?.stage === 'finalizing'
        ? total
        : (progress?.completedRuleCount ?? 0);
  return { completed: Math.min(completed, total), total };
});
const cleanupExecutionActiveRuleFraction = computed(() => {
  const progress = cleanupExecutionProgress.value;
  if (!progress?.currentRuleId || progress.stage !== 'cleaning') return 0;
  const activeRule = localizedCleanupScan.value?.rules.find(rule => rule.ruleId === progress.currentRuleId);
  return activeRule?.fileCount ? Math.min(0.95, progress.currentRuleAffectedItemCount / activeRule.fileCount) : 0;
});
const cleanupExecutionPercent = computed(() => {
  const progress = cleanupExecutionProgress.value;
  const { completed, total } = cleanupExecutionRuleProgress.value;
  if (!progress) return 2;
  if (progress.stage === 'validating') return 3 + (completed / total) * 22;
  if (progress.stage === 'finalizing') return 98;
  // Whole-rule cleanup now starts its safe deletion traversal immediately.
  // Reserve the validation segment only when Core actually measured a preview
  // or source-scoped request, otherwise progress would jump to 25% at startup.
  const validationCompleted = progress.validatedRuleCount > 0;
  const cleaningStart = validationCompleted ? 25 : 3;
  const cleaningRange = validationCompleted ? 70 : 92;
  return cleaningStart + ((completed + cleanupExecutionActiveRuleFraction.value) / total) * cleaningRange;
});
const cleanupExecutionActiveItem = computed(() => cleanupExecutionItems.value.find(item => item.state === 'active'));
const cleanupExecutionTitle = computed(() => {
  if (deepCleanupCancelling.value) return globalLoadingMessage.value;
  const activeItemName = cleanupExecutionActiveItem.value?.name;
  if (!activeItemName) return cleanupExecutionStageLabel.value;
  return cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing
    ? t('loading.previewingCurrentItem', { name: activeItemName })
    : t('loading.cleaningCurrentItem', { name: activeItemName });
});
const cleanupExecutionSummary = computed(() =>
  t('loading.cleanupProgressSummary', {
    completed: FormatUtils.integer(cleanupExecutionRuleProgress.value.completed),
    total: FormatUtils.integer(cleanupExecutionRuleProgress.value.total),
  })
);
const cleanupExecutionPrimaryMetric = computed(() => {
  const progress = cleanupExecutionProgress.value;
  if (progress?.stage === 'validating') {
    return {
      label: t('loading.checkedItems'),
      value: FormatUtils.integer(progress.checkedItemCount),
    };
  }
  return {
    label: t('loading.processedItems'),
    value: FormatUtils.integer(progress?.affectedItemCount ?? 0),
  };
});
const cleanupExecutionSecondaryMetric = computed(() => {
  const progress = cleanupExecutionProgress.value;
  if (progress?.stage === 'validating') {
    return {
      label: t('loading.checkedData'),
      value: ByteSizeService.bytes(progress.checkedBytes),
    };
  }
  return {
    label:
      cleanupStore.operation === CLEANUP_OPERATION_IDS.previewing ? t('cleanup.estimated') : t('loading.releasedSpace'),
    value: ByteSizeService.bytes(progress?.releasedBytes ?? 0),
  };
});

async function openExternalLink(url: string) {
  try {
    await LinkService.open(url);
  } catch (error) {
    store.reportError(error);
  }
}
const errorMessage = computed(() => {
  switch (store.errorReason) {
    case 'resourceBusy':
      return t('errorReasons.resourceBusy.message');
    case 'accessDeniedOrBusy':
      return t('errorReasons.accessDeniedOrBusy.message');
    case 'itemChanged':
      return t('errorReasons.itemChanged.message');
    default:
      return store.errorCode ? t(`errors.${store.errorCode}`) : '';
  }
});
const errorTitle = computed(() => {
  switch (store.errorReason) {
    case 'resourceBusy':
      return t('errorReasons.resourceBusy.title');
    case 'accessDeniedOrBusy':
      return t('errorReasons.accessDeniedOrBusy.title');
    case 'itemChanged':
      return t('errorReasons.itemChanged.title');
    default:
      return store.errorCode ? t(`errorTitles.${store.errorCode}`) : t('common.operationFailed');
  }
});
watch(
  [() => store.errorCode, () => store.errorReason, errorTitle, errorMessage],
  ([errorCode, errorReason, title, message]) => {
    if (!errorCode) {
      toast.dismiss(APPLICATION_ERROR_TOAST_ID);
      return;
    }
    // Global command errors use the same renderer as operation feedback so every notification is
    // measured and stacked by one layout engine. A separate fixed panel caused overlapping toasts.
    toast.error(title, {
      id: APPLICATION_ERROR_TOAST_ID,
      description: message,
      duration: Infinity,
      onDismiss: () => {
        if (store.errorCode === errorCode && store.errorReason === errorReason) store.clearError();
      },
    });
  },
  { immediate: true }
);
const busyPages = computed<PageId[]>(() => [
  ...(cleanupBusy.value ? [PAGE_IDS.cleanup] : []),
  ...(analysisStore.pending || analysisStore.deleting ? [PAGE_IDS.analysis] : []),
  ...(largeFilesStore.loading || largeFilesStore.deleting ? [PAGE_IDS.largeFiles] : []),
  ...(duplicateFilesStore.loading || duplicateFilesStore.deleting ? [PAGE_IDS.duplicateFiles] : []),
  ...(applicationStore.scanningUninstallCatalog ||
  applicationStore.preparingUninstall ||
  applicationStore.executingUninstall
    ? [PAGE_IDS.applicationUninstall]
    : []),
  ...(startupStore.scanning || startupStore.preparingChange || startupStore.executingChange ? [PAGE_IDS.startup] : []),
  ...(systemSettingsStore.scanning || systemSettingsStore.preparing || systemSettingsStore.executing
    ? [PAGE_IDS.systemOptimization]
    : []),
  ...(historyStore.loading ? [PAGE_IDS.history] : []),
]);
const noticePages = computed<PageId[]>(() => (appUpdateStore.updateNoticeUnread ? [PAGE_IDS.settings] : []));

let navigationRequest = 0;
let diskInitialization: Promise<void> | null = null;
let historyInitialization: Promise<void> | null = null;
let unlistenOpenAbout: (() => void) | null = null;
let shellMounted = true;
let automaticUpdateTimer: ReturnType<typeof setTimeout> | null = null;
let loadingClockTimer: ReturnType<typeof setInterval> | null = null;

function initializeDisks(): Promise<void> {
  diskInitialization ??= store.initialize().then(() => storageScopeStore.initialize(store.disks));
  return diskInitialization;
}

function initializePageData(page: PageId): Promise<void> {
  if (page === PAGE_IDS.history) {
    historyInitialization ??= historyStore.load();
    return historyInitialization;
  }
  if ([PAGE_IDS.analysis, PAGE_IDS.largeFiles, PAGE_IDS.duplicateFiles].includes(page)) return initializeDisks();
  return Promise.resolve();
}

function preloadFeaturePages() {
  const preload = () => {
    void Promise.allSettled(Object.values(pageLoaders).map(loadPage => loadPage()));
    // Disk inventory is useful to two feature pages but is not required to
    // render the startup cleanup page. Begin it only after the first frame is
    // interactive, while guarded navigation still waits if users arrive first.
    void initializeDisks();
  };
  if ('requestIdleCallback' in window) {
    window.requestIdleCallback(preload, { timeout: 1200 });
  } else {
    window.setTimeout(preload, 200);
  }
}

function syncSidebarExpansion() {
  sidebarLayout.value = resizeSidebarLayout(sidebarLayout.value, window.innerWidth);
}

function toggleSidebar() {
  sidebarLayout.value = toggleSidebarLayout(sidebarLayout.value);
}

onMounted(() => {
  window.addEventListener('resize', syncSidebarExpansion);
  syncSidebarExpansion();
  cleanupStore.initialize();
  loadingClockTimer = window.setInterval(() => {
    if (cleanupExecutionActive.value) loadingClockMs.value = Date.now();
  }, 1000);
  preloadFeaturePages();
  void appUpdateStore.initialize();
  // Update checks start after the first interactive frame and never delay
  // cleanup initialization or navigation. Development builds use the same
  // path so local update endpoints and the complete startup interaction can
  // be verified before packaging.
  automaticUpdateTimer = window.setTimeout(() => {
    automaticUpdateTimer = null;
    void appUpdateStore.check(store.settings.language, false);
  }, APP_UPDATE_AUTOMATIC_CHECK_DELAY_MS);
  void ApplicationMenuService.onOpenAbout(() => {
    void openAboutSettings();
  })
    .then(unlisten => {
      if (shellMounted) {
        unlistenOpenAbout = unlisten;
      } else {
        unlisten();
      }
    })
    .catch(error => store.reportError(error));
});

onBeforeUnmount(() => {
  shellMounted = false;
  window.removeEventListener('resize', syncSidebarExpansion);
  if (automaticUpdateTimer) window.clearTimeout(automaticUpdateTimer);
  if (loadingClockTimer) window.clearInterval(loadingClockTimer);
  unlistenOpenAbout?.();
});

async function navigate(page: PageId) {
  const request = ++navigationRequest;
  try {
    await Promise.all([pageLoaders[page]?.(), initializePageData(page)]);
    if (request === navigationRequest) {
      store.navigate(page);
      if (page === PAGE_IDS.settings && appUpdateStore.updateNoticeUnread) {
        settingsFocusRevision.value += 1;
      }
    }
  } catch (error) {
    store.reportError(error);
  }
}

async function openAboutSettings() {
  await navigate(PAGE_IDS.settings);
  settingsFocusRevision.value += 1;
  appUpdateStore.showAbout();
}

async function checkForUpdates() {
  await appUpdateStore.check(store.settings.language, true);
  if (appUpdateStore.status !== APP_UPDATE_STATUS_IDS.error) return;
  toast.error(t('settings.updateCheckFailedTitle'), {
    description: appUpdateStore.checkError || t('settings.updateCheckUnknownError'),
    id: UPDATE_CHECK_ERROR_TOAST_ID,
  });
}

function saveSettings(settings: AppSettings) {
  store.saveSettings(settings);
}

function ensureOperationAvailable(): boolean {
  if (!exclusiveOperationBusy.value) return true;
  store.reportOperationBusy();
  return false;
}

function scanStartupCatalog() {
  if (startupStore.scanning) return;
  if (!ensureOperationAvailable()) return;
  return startupStore.scan();
}

async function executeStartupChange() {
  await startupStore.executeChange(t('startup.change.authorizationPromptMacos'));
  if (startupStore.lastChangeResult) await historyStore.load({ reportError: false });
}

async function clearHistoryData() {
  await historyStore.clear();
}

function analyze(path?: string, refresh = false, setHome = false) {
  // A rapid second navigation can arrive before Vue propagates the Store's
  // pending state back into the page props. Ignore that same-domain request
  // instead of presenting the cross-domain disk-safety warning.
  if (analysisStore.pending || analysisStore.deleting) return;
  if (!ensureOperationAvailable()) return;
  return analysisStore.analyze(path, refresh, setHome);
}

function deleteAnalysisEntryPermanently(entry: DirectoryEntryInfo) {
  if (!ensureOperationAvailable()) return;
  return analysisStore.deletePermanently(entry);
}

function findLargeFiles(path: string | undefined, refresh = false) {
  if (!ensureOperationAvailable()) return;
  return largeFilesStore.find(path, store.settings.largeFileMinimumBytes, refresh);
}

function updateLargeFileMinimum(minimumBytes: number) {
  if (minimumBytes === store.settings.largeFileMinimumBytes) return;
  saveSettings({ ...store.settings, largeFileMinimumBytes: minimumBytes });
}

async function deleteLargeFilesPermanently(entries: LargeFileEntry[]) {
  if (!ensureOperationAvailable()) return;
  const result = await largeFilesStore.deleteManyPermanently(entries);
  if (!result) return;
  const description = t(
    'largeFiles.deleteCompletedDescription',
    {
      count: FormatUtils.integer(result.removedPaths.length),
      size: ByteSizeService.bytes(result.releasedBytes),
      failed: FormatUtils.integer(result.failed.length),
    },
    result.removedPaths.length
  );
  const options = { description, id: LARGE_FILE_DELETE_TOAST_ID };
  if (result.failed.length) toast.warning(t('largeFiles.deleteCompletedWithWarnings'), options);
  else toast.success(t('largeFiles.deleteCompleted'), options);
}

function findDuplicateFiles(path: string) {
  if (!ensureOperationAvailable()) return;
  return duplicateFilesStore.find([path], store.settings.duplicateFileMinimumBytes);
}

function updateDuplicateFileMinimum(minimumBytes: number) {
  if (minimumBytes === store.settings.duplicateFileMinimumBytes) return;
  saveSettings({ ...store.settings, duplicateFileMinimumBytes: minimumBytes });
}

function updateDuplicateKeeperRule(keeperRule: AppSettings['duplicateKeeperRule']) {
  if (keeperRule === store.settings.duplicateKeeperRule) return;
  saveSettings({ ...store.settings, duplicateKeeperRule: keeperRule });
}

async function deleteDuplicateFilesPermanently(entries: DuplicateFileEntry[]) {
  if (!ensureOperationAvailable()) return;
  const result = await duplicateFilesStore.deletePermanently(entries);
  if (!result) return;
  const description = t(
    'duplicateFiles.deleteCompletedDescription',
    {
      count: FormatUtils.integer(result.removedPaths.length),
      size: ByteSizeService.bytes(result.releasedBytes),
      failed: FormatUtils.integer(result.failed.length),
    },
    result.removedPaths.length
  );
  const options = { description, id: DUPLICATE_FILE_DELETE_TOAST_ID };
  if (result.failed.length) toast.warning(t('duplicateFiles.deleteCompletedWithWarnings'), options);
  else toast.success(t('duplicateFiles.deleteCompleted'), options);
}

function scanApplications() {
  if (!ensureOperationAvailable()) return;
  return applicationStore.scanUninstallCatalog();
}

function prepareApplicationUninstall(selections: ApplicationUninstallBatchSelection[]) {
  if (!ensureOperationAvailable()) return;
  return applicationStore.prepareUninstall(selections);
}

function closeApplicationsBeforeCleanup(ruleIds: string[], mode: ApplicationCloseMode) {
  if (!ensureOperationAvailable()) return;
  return cleanupStore.closeApplications(ruleIds, mode);
}

function closeApplicationsBeforeUninstall(applicationIds: string[], mode: ApplicationCloseMode) {
  if (!ensureOperationAvailable()) return;
  return applicationStore.closeUninstallApplications(applicationIds, mode);
}

function executeApplicationUninstall() {
  if (!ensureOperationAvailable()) return;
  return applicationStore.executePreparedUninstall(t('applicationUninstall.authorizationPromptMacos'));
}

async function openPath(path: string) {
  await executeFileManagerAction(() => FileManagerService.reveal(path));
}

async function executeFileManagerAction(action: () => Promise<void>) {
  try {
    await action();
  } catch (error) {
    store.reportError(error);
  }
}

async function openAnalysisEntry(scanId: number, path: string) {
  await executeFileManagerAction(() => FileManagerService.openAnalysisEntry(scanId, path));
}

async function openLargeFileEntry(scanId: number, path: string) {
  await executeFileManagerAction(() => FileManagerService.openLargeFileEntry(scanId, path));
}

async function openDuplicateFileEntry(scanId: number, path: string) {
  await executeFileManagerAction(() => FileManagerService.openDuplicateFileEntry(scanId, path));
}

async function scanCleanup(scanScope: CleanupScanScope) {
  if (!ensureOperationAvailable()) return;
  cleanupOrchestrating.value = true;
  try {
    const completed = await cleanupStore.scanCandidates(scanScope);
    if (completed) await applicationStore.scanLeftovers();
  } finally {
    cleanupOrchestrating.value = false;
  }
}

async function executeCleanup(leftovers: ApplicationLeftoverCandidate[]) {
  if (!ensureOperationAvailable()) return;
  cleanupOrchestrating.value = true;
  deepCleanupCancelling.value = false;
  cleanupCancellationRetried.value = false;
  const deepCleanupOperationId = crypto.randomUUID();
  const executesCleanupRules = cleanupStore.selectedRuleIds.length > 0;
  try {
    if (executesCleanupRules) {
      const completed = await cleanupStore.execute(false, deepCleanupOperationId);
      // Do not clear the cleanup error by starting a second operation after a
      // fatal failure. Ordinary partial results may continue, while an explicit
      // user cancellation stops the complete deep-cleanup workflow.
      if (!completed || deepCleanupCancelling.value) return;
    }
    if (leftovers.length && !deepCleanupCancelling.value) {
      await applicationStore.deleteLeftoversPermanently(leftovers, deepCleanupOperationId);
      if (!applicationStore.lastResult || deepCleanupCancelling.value) return;
    }
    const cleanupResult = executesCleanupRules ? cleanupStore.result : null;
    const leftoverResult = leftovers.length ? applicationStore.lastResult : null;
    const releasedBytes = (cleanupResult?.releasedBytes ?? 0) + (leftoverResult?.releasedBytes ?? 0);
    const affectedItemCount = (cleanupResult?.affectedItemCount ?? 0) + (leftoverResult?.affectedItemCount ?? 0);
    const failedItemCount = (cleanupResult?.failedItemCount ?? 0) + (leftoverResult?.failedItemCount ?? 0);
    const description = t(
      'cleanup.completedDescription',
      {
        count: FormatUtils.integer(affectedItemCount),
        size: ByteSizeService.bytes(releasedBytes),
        failed: FormatUtils.integer(failedItemCount),
      },
      affectedItemCount
    );
    const options = { description, id: DEEP_CLEANUP_TOAST_ID };
    if (failedItemCount) toast.warning(t('cleanup.completedWithWarnings'), options);
    else toast.success(t('cleanup.completed'), options);
  } finally {
    cleanupOrchestrating.value = false;
    deepCleanupCancelling.value = false;
    cleanupCancellationRetried.value = false;
  }
}

async function cancelDeepCleanup() {
  if (deepCleanupCancelling.value || !destructiveCleanupActive.value) return;
  cleanupCancellationConfirmOpen.value = false;
  // Set the workflow flag before invoking Core. This closes the short boundary
  // between cache cleanup and leftover cleanup, where neither native operation
  // may be active yet but the second phase must still not start.
  deepCleanupCancelling.value = true;
  const requests: Promise<void>[] = [];
  if (cleanupStore.loading && cleanupStore.operation === CLEANUP_OPERATION_IDS.cleaning) {
    requests.push(cleanupStore.cancelExecution());
  }
  if (applicationStore.deletingLeftovers) {
    requests.push(applicationStore.cancelLeftoverDeletion());
  }
  await Promise.allSettled(requests);
}

function requestCancelDeepCleanup() {
  if (deepCleanupCancelling.value || !destructiveCleanupActive.value) return;
  cleanupCancellationConfirmOpen.value = true;
}
</script>

<template>
  <main
    class="app-shell"
    :class="{
      'custom-titlebar': customTitlebarPlatform,
      'macos-overlay': isMacOs,
      'windows-custom-titlebar': isWindows,
      'sidebar-expanded': sidebarExpanded,
    }"
  >
    <MdWindowTitlebar
      v-if="customTitlebarPlatform"
      :platform="customTitlebarPlatform"
      :sidebar-expanded="sidebarExpanded"
    />
    <MdSidebar
      :current-page="store.currentPage"
      :busy-pages="busyPages"
      :notice-pages="noticePages"
      :show-brand="!isWindows"
      :expanded="sidebarExpanded"
      @navigate="navigate"
      @toggle="toggleSidebar"
    />
    <div class="content-shell">
      <KeepAlive>
        <SystemOptimizationPage v-if="store.currentPage === PAGE_IDS.systemOptimization" />
        <CleanupPage
          v-else-if="store.currentPage === PAGE_IDS.cleanup"
          :disk="store.disk"
          :disks="store.disks"
          :scan="localizedCleanupScan"
          :scan-scope="cleanupStore.scanScope"
          :selected-rule-ids="cleanupStore.selectedRuleIds"
          :source-selections="cleanupStore.sourceSelections"
          :selected-bytes="cleanupStore.selectedBytes"
          :result="localizedCleanupResult"
          :leftovers="applicationStore.leftovers"
          :leftover-result="applicationStore.lastResult"
          :scanning-leftovers="applicationStore.scanningLeftovers"
          :deleting-leftovers="applicationStore.deletingLeftovers"
          :progress="cleanupStore.scanProgress"
          :loading-message="cleanupLoadingMessage"
          :operation="cleanupStore.operation"
          :busy="cleanupBusy"
          :closing-applications="cleanupStore.closingApplications"
          :close-result="cleanupStore.applicationCloseResult"
          @scan="scanCleanup"
          @toggle-source="cleanupStore.toggleSource"
          @select-all="cleanupStore.setRulesSelected"
          @execute="executeCleanup"
          @cancel="cleanupStore.cancelScan()"
          @close-applications="closeApplicationsBeforeCleanup"
          @open="openPath"
        />
        <AnalysisPage
          v-else-if="store.currentPage === PAGE_IDS.analysis"
          :result="analysisStore.result"
          :home-path="analysisStore.homePath"
          :disk="store.disk"
          :disks="store.disks"
          :progress="analysisStore.progress"
          :busy="analysisStore.pending"
          :cancelling="analysisStore.cancelling"
          :deleting="analysisStore.deleting"
          @analyze="analyze"
          @cancel="analysisStore.cancel()"
          @error="store.reportError"
          @open-entry="openAnalysisEntry"
          @reveal="openPath"
          @delete="deleteAnalysisEntryPermanently"
        />
        <LargeFilesPage
          v-else-if="store.currentPage === PAGE_IDS.largeFiles"
          :disk="store.disk"
          :disks="store.disks"
          :result="largeFilesStore.result"
          :progress="largeFilesStore.progress"
          :minimum-bytes="store.settings.largeFileMinimumBytes"
          :busy="largeFilesStore.loading"
          :cancelling="largeFilesStore.cancelling"
          :deleting="largeFilesStore.deleting"
          @find="findLargeFiles"
          @update-minimum="updateLargeFileMinimum"
          @cancel="largeFilesStore.cancel()"
          @error="store.reportError"
          @open-entry="openLargeFileEntry"
          @reveal="openPath"
          @delete-many="deleteLargeFilesPermanently"
        />
        <DuplicateFilesPage
          v-else-if="store.currentPage === PAGE_IDS.duplicateFiles"
          :disk="store.disk"
          :disks="store.disks"
          :result="duplicateFilesStore.result"
          :result-complete="duplicateFilesStore.resultComplete"
          :has-more="duplicateFilesStore.hasMore"
          :loading-more="duplicateFilesStore.loadingMore"
          :progress="duplicateFilesStore.progress"
          :busy="duplicateFilesStore.loading"
          :cancelling="duplicateFilesStore.cancelling"
          :deleting="duplicateFilesStore.deleting"
          :minimum-bytes="store.settings.duplicateFileMinimumBytes"
          :keeper-rule="store.settings.duplicateKeeperRule"
          @find="findDuplicateFiles"
          @update-minimum="updateDuplicateFileMinimum"
          @update-keeper-rule="updateDuplicateKeeperRule"
          @cancel="duplicateFilesStore.cancel()"
          @error="store.reportError"
          @open-entry="openDuplicateFileEntry"
          @reveal="openPath"
          @delete="deleteDuplicateFilesPermanently"
          @load-more="duplicateFilesStore.loadMore"
        />
        <ApplicationUninstallPage
          v-else-if="store.currentPage === PAGE_IDS.applicationUninstall"
          :catalog="applicationStore.uninstallCatalog"
          :scanning="applicationStore.scanningUninstallCatalog"
          :cancelling="applicationStore.cancellingUninstallCatalog"
          :progress="applicationStore.uninstallProgress"
          :execution-progress="applicationStore.uninstallExecutionProgress"
          :plan="applicationStore.uninstallPlan"
          :preview="applicationStore.uninstallPreview"
          :last-result="applicationStore.uninstallLastResult"
          :preparing="applicationStore.preparingUninstall"
          :executing="applicationStore.executingUninstall"
          :cancelling-execution="applicationStore.cancellingUninstall"
          :cancellation-revision="applicationStore.uninstallCancellationRevision"
          :closing-applications="applicationStore.closingUninstallApplications"
          :close-result="applicationStore.uninstallCloseResult"
          @scan="scanApplications"
          @cancel-scan="applicationStore.cancelUninstallCatalogScan()"
          @prepare="prepareApplicationUninstall"
          @cancel-plan="applicationStore.clearPreparedUninstall()"
          @execute="executeApplicationUninstall"
          @cancel-execution="applicationStore.cancelUninstallExecution()"
          @close-applications="closeApplicationsBeforeUninstall"
          @open="openPath"
        />
        <StartupPage
          v-else-if="store.currentPage === PAGE_IDS.startup"
          :catalog="startupStore.catalog"
          :scanning="startupStore.scanning"
          :cancelling="startupStore.cancelling"
          :preparing-change="startupStore.preparingChange"
          :executing-change="startupStore.executingChange"
          :cancelling-change="startupStore.cancellingChange"
          :pending-plan="startupStore.pendingPlan"
          :last-change-result="startupStore.lastChangeResult"
          @scan="scanStartupCatalog"
          @cancel="startupStore.cancelScan()"
          @prepare-change="startupStore.prepareChange($event.itemIds, $event.desiredState)"
          @cancel-change="startupStore.clearPendingPlan()"
          @cancel-change-execution="startupStore.cancelChange()"
          @execute-change="executeStartupChange"
          @open="openPath"
          @error="store.reportError"
        />
        <HistoryPage
          v-else-if="store.currentPage === PAGE_IDS.history"
          :history="localizedHistory"
          :busy="historyStore.loading"
          @clear="clearHistoryData"
        />
        <SettingsPage
          v-else-if="store.currentPage === PAGE_IDS.settings"
          :settings="store.settings"
          :focus-revision="settingsFocusRevision"
          @save="saveSettings"
          @error="store.reportError"
        />
      </KeepAlive>
    </div>

    <div v-if="globalLoading" class="loading-overlay">
      <div class="loading-drag-region" data-tauri-drag-region aria-hidden="true" />
      <section class="loading-card" :class="{ 'has-execution-details': cleanupExecutionActive }">
        <div class="loading-heading" role="status" aria-live="polite">
          <span class="loading-icon">
            <MdIcon :name="ICON_NAMES.deepCleanup" :size="27" />
          </span>
          <div>
            <h2 :title="cleanupExecutionActive ? cleanupExecutionTitle : globalLoadingMessage">
              {{ cleanupExecutionActive ? cleanupExecutionTitle : globalLoadingMessage }}
            </h2>
            <p>{{ cleanupExecutionActive && !deepCleanupCancelling ? cleanupExecutionSummary : globalLoadingHint }}</p>
          </div>
        </div>
        <template v-if="cleanupExecutionActive">
          <div
            ref="cleanupExecutionListElement"
            class="cleanup-execution-list"
            :aria-label="t('loading.cleanupItemList')"
          >
            <div
              v-for="item in cleanupExecutionItems"
              :key="item.ruleId"
              class="cleanup-execution-item"
              :class="`is-${item.state}`"
            >
              <span class="cleanup-execution-item-status" aria-hidden="true">
                <MdIcon v-if="item.state === 'completed'" :name="ICON_NAMES.check" :size="14" />
                <b v-else-if="item.state === 'skipped'">!</b>
                <i v-else-if="item.state === 'active'" class="md-operational-motion" />
                <i v-else />
              </span>
              <span class="cleanup-execution-item-content">
                <span class="cleanup-execution-item-title">
                  <strong>{{ item.name }}</strong>
                  <small
                    v-if="item.state === 'active' && cleanupExecutionElapsedSeconds >= 20"
                    class="cleanup-execution-item-slow-hint"
                  >
                    {{ t('loading.stepMayTakeMinutes') }}
                  </small>
                </span>
                <small class="cleanup-execution-item-detail" :title="item.detail">
                  <MdMiddleEllipsis v-if="item.detailIsPath" :text="item.detail" :tail-length="40" />
                  <template v-else>{{ item.detail }}</template>
                </small>
              </span>
              <small class="cleanup-execution-item-label">
                {{
                  item.state === 'completed'
                    ? t('loading.cleanupItemDone')
                    : item.state === 'skipped'
                      ? t('loading.cleanupItemSkippedLabel')
                      : item.state === 'active'
                        ? t('loading.cleanupItemActive')
                        : t('loading.cleanupItemPending')
                }}
              </small>
            </div>
          </div>
          <div
            class="cleanup-execution-progress"
            role="progressbar"
            :aria-label="cleanupExecutionStageLabel"
            :aria-valuemin="0"
            :aria-valuemax="100"
            :aria-valuenow="Math.round(cleanupExecutionPercent)"
          >
            <span :style="{ width: `${cleanupExecutionPercent}%` }" />
          </div>
          <div class="cleanup-execution-stats">
            <span>
              <small>{{ t('loading.ruleProgress') }}</small>
              <strong>
                {{
                  t('loading.ruleProgressValue', {
                    completed: FormatUtils.integer(cleanupExecutionRuleProgress.completed),
                    total: FormatUtils.integer(cleanupExecutionRuleProgress.total),
                  })
                }}
              </strong>
            </span>
            <span>
              <small>{{ t('loading.elapsed') }}</small>
              <strong>
                {{
                  t(
                    'loading.elapsedSeconds',
                    { count: FormatUtils.integer(cleanupExecutionElapsedSeconds) },
                    cleanupExecutionElapsedSeconds
                  )
                }}
              </strong>
            </span>
            <span>
              <small>{{ cleanupExecutionPrimaryMetric.label }}</small>
              <strong>{{ cleanupExecutionPrimaryMetric.value }}</strong>
            </span>
            <span>
              <small>{{ cleanupExecutionSecondaryMetric.label }}</small>
              <strong>{{ cleanupExecutionSecondaryMetric.value }}</strong>
            </span>
          </div>
        </template>
        <div v-else class="loading-activity" aria-hidden="true"><span class="md-operational-motion" /></div>
        <div v-if="destructiveCleanupActive" class="cleanup-execution-actions">
          <Button
            class="cleanup-execution-cancel"
            variant="ghost"
            size="sm"
            type="button"
            :disabled="deepCleanupCancelling"
            @click="requestCancelDeepCleanup"
          >
            {{ deepCleanupCancelling ? t('loading.cancellingCleanupAction') : t('loading.cancelCleanupAction') }}
          </Button>
        </div>
      </section>
    </div>

    <Dialog :open="cleanupCancellationConfirmOpen" @update:open="cleanupCancellationConfirmOpen = $event">
      <MdDialogContent class="w-[calc(100%-3rem)] max-w-[440px] gap-0 p-0">
        <DialogHeader class="px-6 pt-6 pr-14 pb-4">
          <DialogTitle>{{ t('loading.cancelCleanupConfirmTitle') }}</DialogTitle>
          <DialogDescription class="mt-2 leading-6">
            {{ t('loading.cancelCleanupConfirmDescription') }}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter class="border-t border-border/70 px-6 py-3.5">
          <Button variant="outline" type="button" @click="cleanupCancellationConfirmOpen = false">
            {{ t('common.cancel') }}
          </Button>
          <Button variant="destructive" type="button" @click="cancelDeepCleanup">
            {{ t('loading.stopCleanupAction') }}
          </Button>
        </DialogFooter>
      </MdDialogContent>
    </Dialog>

    <MdAboutDialog
      v-if="appUpdateStore.dialogOpen"
      :open="appUpdateStore.dialogOpen"
      :status="appUpdateStore.status"
      :action="appUpdateStore.update?.action ?? null"
      :current-version="appUpdateStore.currentVersion"
      :version="appUpdateStore.update?.version ?? ''"
      :notes="appUpdateStore.update?.notes ?? ''"
      :check-error="appUpdateStore.checkError"
      :downloaded-bytes="appUpdateStore.downloadedBytes"
      :total-bytes="appUpdateStore.totalBytes"
      :action-error="appUpdateStore.actionError"
      :failure-stage="appUpdateStore.failureStage"
      @close="appUpdateStore.dismiss()"
      @check="checkForUpdates"
      @download="appUpdateStore.download()"
      @manual-download="appUpdateStore.openManualDownload()"
      @install="appUpdateStore.installDownloaded()"
      @restart="appUpdateStore.restartApplication()"
      @open-link="openExternalLink"
    />
  </main>
</template>

<style scoped>
@reference "@assets/main.css";
.app-shell {
  --titlebar-height: 0px;
  --window-controls-width: 144px;
  --sidebar-width: var(--layout-sidebar-collapsed-width);
  --sidebar-transition-duration: 240ms;
  --sidebar-transition-easing: cubic-bezier(0.22, 1, 0.36, 1);
  display: flex;
  width: 100%;
  height: 100vh;
  overflow: hidden;
  @apply bg-sidebar text-foreground;
}
.app-shell.sidebar-expanded {
  --sidebar-width: var(--layout-sidebar-expanded-width);
}
.macos-overlay {
  --titlebar-height: 34px;
}
.windows-custom-titlebar {
  --titlebar-height: var(--layout-page-header-height);
}
.custom-titlebar :deep(.sidebar) {
  padding-top: var(--titlebar-height);
}
.content-shell {
  flex: 1;
  min-width: 0;
  height: 100vh;
  overflow: hidden;
  border-radius: 12px 0 0;
  @apply bg-background;
}
.loading-overlay {
  position: fixed;
  z-index: 40;
  inset: 0;
  display: grid;
  place-items: center;
  padding: 24px;
  background-color: var(--modal-overlay-background);
  -webkit-backdrop-filter: blur(0);
  backdrop-filter: blur(0);
}
.loading-drag-region {
  position: absolute;
  z-index: 0;
  inset: 0;
}
.loading-card {
  position: relative;
  z-index: 1;
  width: min(400px, calc(100vw - 48px));
  pointer-events: auto;
  user-select: none;
  border-width: 1px;
  border-radius: 16px;
  padding: 25px 26px 22px;
  @apply border-border bg-card text-card-foreground shadow-2xl shadow-foreground/10;
}
.loading-card.has-execution-details {
  width: min(620px, calc(100vw - 48px));
}
.loading-heading {
  display: flex;
  align-items: center;
  gap: 15px;
}
.loading-heading > div {
  min-width: 0;
  flex: 1;
}
.loading-heading h2 {
  overflow: hidden;
  margin: 0;
  @apply text-card-foreground;
  font-size: 18px;
  line-height: 1.3;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.loading-heading p {
  margin: 6px 0 0;
  @apply text-muted-foreground;
  font-size: 12px;
  line-height: 1.55;
}
.loading-icon {
  display: grid;
  position: relative;
  width: 52px;
  height: 52px;
  flex: none;
  place-items: center;
  border-radius: 14px;
  @apply text-primary;
  background: var(--surface-primary-subtle);
}
.loading-activity {
  height: 4px;
  margin-top: 20px;
  overflow: hidden;
  border-radius: 999px;
  background: var(--surface-primary-subtle);
}
.loading-activity span {
  display: block;
  width: 38%;
  height: 100%;
  border-radius: inherit;
  @apply bg-primary;
  animation: loading-activity 1.35s ease-in-out infinite;
}
.cleanup-execution-list {
  max-height: 230px;
  margin-top: 20px;
  overflow-y: auto;
  overscroll-behavior: contain;
  border-width: 1px;
  border-radius: 11px;
  @apply border-border/80 bg-muted/20;
}
.cleanup-execution-item {
  display: grid;
  min-width: 0;
  grid-template-columns: 24px minmax(0, 1fr) auto;
  align-items: center;
  gap: 9px;
  border-top-width: 1px;
  padding: 9px 11px;
  @apply border-border/60;
}
.cleanup-execution-item:first-child {
  border-top: 0;
}
.cleanup-execution-item.is-active {
  background: var(--surface-primary-subtle);
}
.cleanup-execution-item-status {
  display: grid;
  width: 22px;
  height: 22px;
  place-items: center;
  border-radius: 50%;
  @apply bg-muted text-muted-foreground;
}
.cleanup-execution-item.is-completed .cleanup-execution-item-status {
  @apply text-success;
  background: var(--surface-success-subtle);
}
.cleanup-execution-item.is-skipped .cleanup-execution-item-status {
  @apply text-warning-foreground;
  background: var(--surface-warning-subtle);
}
.cleanup-execution-item.is-active .cleanup-execution-item-status {
  @apply text-primary;
  background: var(--surface-primary-subtle);
}
.cleanup-execution-item-status i {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  @apply bg-muted-foreground/45;
}
.cleanup-execution-item-status b {
  font-size: 12px;
  line-height: 1;
}
.cleanup-execution-item.is-active .cleanup-execution-item-status i {
  width: 13px;
  height: 13px;
  border-width: 2px;
  @apply border-primary/20 border-t-primary bg-transparent;
  animation: cleanup-icon-spin 0.8s linear infinite;
}
.cleanup-execution-item-content {
  display: flex;
  min-width: 0;
  flex-direction: column;
}
.cleanup-execution-item-title {
  display: flex;
  min-width: 0;
  align-items: baseline;
  gap: 8px;
}
.cleanup-execution-item-title strong {
  overflow: hidden;
  min-width: 0;
  font-size: 12.5px;
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cleanup-execution-item-slow-hint {
  overflow: hidden;
  min-width: 0;
  max-width: 42%;
  flex: none;
  @apply text-muted-foreground;
  font-size: 9.5px;
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cleanup-execution-item-detail {
  overflow: hidden;
  margin-top: 1px;
  @apply text-muted-foreground;
  font-size: 10.5px;
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cleanup-execution-item-label {
  @apply text-muted-foreground;
  font-size: 10.5px;
  white-space: nowrap;
}
.cleanup-execution-stats small {
  @apply text-muted-foreground;
  font-size: 10.5px;
  line-height: 1.35;
}
.cleanup-execution-progress {
  height: 4px;
  margin-top: 14px;
  overflow: hidden;
  border-radius: 999px;
  background: var(--surface-primary-subtle);
}
.cleanup-execution-progress > span {
  display: block;
  min-width: 2%;
  height: 100%;
  border-radius: inherit;
  @apply bg-primary transition-[width] duration-300 ease-out;
}
.cleanup-execution-stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px;
  margin-top: 12px;
}
.cleanup-execution-stats > span {
  display: flex;
  min-width: 0;
  flex-direction: column;
  border-radius: 9px;
  padding: 9px 10px;
  @apply bg-muted/45;
}
.cleanup-execution-stats strong {
  overflow: hidden;
  margin-top: 3px;
  font-size: 13px;
  font-variant-numeric: tabular-nums;
  line-height: 1.25;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cleanup-execution-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 14px;
}
.cleanup-execution-cancel {
  pointer-events: auto;
  @apply text-muted-foreground hover:text-foreground;
}
@keyframes loading-activity {
  0% {
    transform: translateX(-110%);
  }
  50% {
    transform: translateX(165%);
  }
  100% {
    transform: translateX(280%);
  }
}
@keyframes cleanup-icon-spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
