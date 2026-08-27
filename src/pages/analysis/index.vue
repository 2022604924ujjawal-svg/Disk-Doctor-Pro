<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed, nextTick, ref, watch } from 'vue';

import MdDelayedOperationWorkspace from '@/components/custom/md-delayed-operation-workspace.vue';
import MdStorageScopeSelect from '@/components/custom/md-storage-scope-select.vue';
import MdEmptyState from '@/components/custom/md-empty-state.vue';
import MdOperationProgress from '@/components/custom/md-operation-progress.vue';
import MdPageShell from '@/components/custom/md-page-shell.vue';
import MdDestructiveActionDialog from '@/components/custom/md-destructive-action-dialog.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { ANALYSIS_VIEW_IDS } from '@/lib/models/analysis';
import { STORAGE_SCOPE_IDS } from '@/lib/models/storage-scope';
import { ICON_NAMES } from '@/lib/models/ui';
import type { AnalysisResult, AnalysisViewId, DirectoryEntryInfo } from '@/lib/models/analysis';
import type { DiskInfo } from '@/lib/models/disk';
import type { TraversalProgress } from '@/lib/models/progress';
import { AnalysisBreadcrumbUtils } from '@/lib/utils/analysis-breadcrumb';
import { DiskUtils } from '@/lib/utils/disk';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { PathUtils } from '@/lib/utils/path';
import { useStorageScopeStore } from '@/stores/storage-scope-store';

import MdAnalysisBrowserToolbar from './components/md-analysis-browser-toolbar.vue';
import MdAnalysisFolderPane from './components/md-analysis-folder-pane.vue';
import MdAnalysisVisualPane from './components/md-analysis-visual-pane.vue';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  result: AnalysisResult | null;
  homePath: string;
  disk: DiskInfo | null;
  disks: DiskInfo[];
  progress: TraversalProgress | null;
  busy: boolean;
  cancelling: boolean;
  deleting: boolean;
}>();

const emit = defineEmits<{
  analyze: [path?: string, refresh?: boolean, setHome?: boolean];
  cancel: [];
  error: [error: unknown];
  openEntry: [scanId: number, path: string];
  reveal: [path: string];
  delete: [entry: DirectoryEntryInfo];
}>();

const storageScopeStore = useStorageScopeStore();
const scopeId = STORAGE_SCOPE_IDS.analysis;
const selectedScopePath = ref(
  PathUtils.display(storageScopeStore.selectedPath(scopeId) || props.result?.root || props.disk?.mountPoint || '')
);
const navigationHistory = ref<string[]>([]);
const navigationIndex = ref(-1);
interface PendingHistoryNavigation {
  index: number;
  target: string;
}

const pendingHistoryNavigation = ref<PendingHistoryNavigation | null>(null);
const primaryAnalysisPending = ref(false);
const confirmOpen = ref(false);
const pendingDelete = ref<DirectoryEntryInfo | null>(null);
const viewMode = ref<AnalysisViewId>(ANALYSIS_VIEW_IDS.treemap);

const entries = computed(() => [...(props.result?.entries ?? [])].sort((left, right) => right.bytes - left.bytes));
const folderCount = computed(() => entries.value.filter(entry => entry.isDirectory).length);
const fileCount = computed(() => entries.value.reduce((total, entry) => total + entry.fileCount, 0));
const activeDisk = computed(() =>
  DiskUtils.findForPath(
    props.disks,
    props.result?.root || selectedScopePath.value || props.homePath || props.disk?.mountPoint || '',
    props.disk
  )
);
// The primary action belongs to the selected analysis scope, while
// `result.root` follows whichever child folder the user is browsing. Compare
// against the stable analysis origin so folder navigation cannot incorrectly
// turn "Analyze Again" back into "Start Analysis".
const scopeIsAnalysisRoot = computed(
  () =>
    Boolean(props.homePath && selectedScopePath.value) &&
    PathUtils.comparisonKey(props.homePath) === PathUtils.comparisonKey(selectedScopePath.value)
);
const breadcrumbs = computed(() =>
  AnalysisBreadcrumbUtils.create(
    props.result?.root ?? selectedScopePath.value,
    activeDisk.value,
    t('analysis.localDisk')
  )
);
const canGoBack = computed(() => navigationIndex.value > 0);
const canGoForward = computed(
  () => navigationIndex.value >= 0 && navigationIndex.value < navigationHistory.value.length - 1
);
// Folder navigation and an explicit rescan share the same backend operation.
// Track the initiating interaction so ordinary browsing does not replace the
// page-level action label and make the right-aligned header change width.
const showPrimaryAnalysisProgress = computed(() => props.busy && primaryAnalysisPending.value);
const folderNavigationPending = computed(() => props.busy && !primaryAnalysisPending.value);

watch(
  () => props.disk?.mountPoint,
  value => {
    if (!value || selectedScopePath.value) return;
    selectedScopePath.value = PathUtils.display(value);
  },
  { immediate: true }
);

watch(
  () => props.result?.root,
  value => {
    if (!value) return;
    const normalized = PathUtils.display(value);
    if (pendingHistoryNavigation.value) {
      const pending = pendingHistoryNavigation.value;
      pendingHistoryNavigation.value = null;
      // Advance history only when the result still belongs to this navigation.
      if (PathUtils.comparisonKey(pending.target) === PathUtils.comparisonKey(normalized)) {
        navigationIndex.value = pending.index;
        return;
      }
    }
    if (navigationHistory.value[navigationIndex.value] === normalized) return;
    navigationHistory.value = [...navigationHistory.value.slice(0, navigationIndex.value + 1), normalized];
    navigationIndex.value = navigationHistory.value.length - 1;
  },
  { immediate: true }
);

watch(
  () => props.busy,
  busy => {
    if (busy) return;
    primaryAnalysisPending.value = false;
    // Let the result watcher settle before discarding a failed request so one
    // Store update cannot create a duplicate history entry.
    void nextTick(() => {
      if (!props.busy) pendingHistoryNavigation.value = null;
    });
  }
);

function analyze(path?: string, refresh = false, setHome = false) {
  const target = path?.trim() || selectedScopePath.value;
  if (!target || props.busy || props.deleting) return;
  pendingHistoryNavigation.value = null;
  emit('analyze', target, refresh, setHome);
}

function selectScope(value: unknown) {
  if (props.busy || props.deleting) return;
  const target = typeof value === 'string' ? value : '';
  if (!target) return;

  // Selection changes only configure the next explicit analysis.
  selectedScopePath.value = PathUtils.display(target);
  storageScopeStore.select(scopeId, selectedScopePath.value, props.disks);
  pendingHistoryNavigation.value = null;
}

function removeScopeFolder(path: string) {
  const removingCurrent = PathUtils.comparisonKey(path) === PathUtils.comparisonKey(selectedScopePath.value);
  storageScopeStore.removeFolder(path);
  if (!removingCurrent) return;

  const fallback = PathUtils.display(props.disk?.mountPoint || props.disks[0]?.mountPoint || '');
  selectedScopePath.value = fallback;
  if (fallback) storageScopeStore.select(scopeId, fallback, props.disks);
  pendingHistoryNavigation.value = null;
}

function startPrimaryAnalysis() {
  primaryAnalysisPending.value = true;
  analyze(selectedScopePath.value, scopeIsAnalysisRoot.value, !scopeIsAnalysisRoot.value);
  // Cache hits and rejected operations may finish without toggling `busy`.
  // Clear the local interaction state after Vue has applied any synchronous
  // Store updates so it cannot leak into a later folder navigation.
  void nextTick(() => {
    if (!props.busy) primaryAnalysisPending.value = false;
  });
}

function activateEntry(entry: DirectoryEntryInfo) {
  if (!props.deleting && entry.isDirectory) analyze(entry.path);
}

function openEntry(entry: DirectoryEntryInfo) {
  if (!props.result || props.busy || props.deleting) return;
  emit('openEntry', props.result.scanId, entry.path);
}

function requestDelete(entry: DirectoryEntryInfo) {
  if (props.busy || props.deleting) return;
  pendingDelete.value = entry;
  confirmOpen.value = true;
}

function confirmDelete() {
  if (!pendingDelete.value || props.busy || props.deleting) return;
  emit('delete', pendingDelete.value);
  confirmOpen.value = false;
  pendingDelete.value = null;
}

function navigateHistory(index: number) {
  const target = navigationHistory.value[index];
  if (!target || props.busy || props.deleting) return;
  pendingHistoryNavigation.value = { index, target };
  emit('analyze', target);
}
</script>

<template>
  <MdPageShell class="analysis-shell @container/analysis" content-mode="workspace" :title="t('analysis.title')">
    <template #actions>
      <div class="header-actions" :class="{ 'folder-navigation-pending': folderNavigationPending }">
        <MdStorageScopeSelect
          :model-value="selectedScopePath || activeDisk?.mountPoint || ''"
          :disks="disks"
          :recent-folders="storageScopeStore.recentFolders"
          :standard-folders="storageScopeStore.standardFolders"
          :disabled="busy || deleting"
          @error="emit('error', $event)"
          @remove-folder="removeScopeFolder"
          @update:model-value="selectScope"
        />
        <Button
          v-if="result"
          :variant="scopeIsAnalysisRoot ? 'outline' : 'default'"
          type="button"
          :disabled="busy || deleting || !selectedScopePath"
          :aria-label="
            showPrimaryAnalysisProgress
              ? t('loading.currentStage')
              : scopeIsAnalysisRoot
                ? t('analysis.rescan')
                : t('analysis.start')
          "
          @click="startPrimaryAnalysis"
        >
          <MdIcon
            :class="{ 'icon-spin': showPrimaryAnalysisProgress }"
            :name="showPrimaryAnalysisProgress || scopeIsAnalysisRoot ? ICON_NAMES.refresh : ICON_NAMES.analysis"
            :size="17"
          />
          <!--
            Stack every localized label in one grid cell. Hidden labels still
            reserve their intrinsic width, so neither navigation nor rescanning
            can move the adjacent scope selector.
          -->
          <span class="primary-analysis-labels" aria-hidden="true">
            <span :class="{ visible: !showPrimaryAnalysisProgress && !scopeIsAnalysisRoot }">
              {{ t('analysis.start') }}
            </span>
            <span :class="{ visible: !showPrimaryAnalysisProgress && scopeIsAnalysisRoot }">
              {{ t('analysis.rescan') }}
            </span>
            <span :class="{ visible: showPrimaryAnalysisProgress }">
              {{ t('loading.currentStage') }}
            </span>
          </span>
        </Button>
      </div>
    </template>

    <article class="browser-card">
      <MdAnalysisBrowserToolbar
        v-if="result"
        :breadcrumbs="breadcrumbs"
        :busy="busy || deleting"
        :preserve-busy-appearance="folderNavigationPending"
        :can-go-back="canGoBack"
        :can-go-forward="canGoForward"
        :home-disabled="!homePath"
        @back="navigateHistory(navigationIndex - 1)"
        @forward="navigateHistory(navigationIndex + 1)"
        @home="analyze(homePath)"
        @navigate="analyze"
      />

      <!-- The local overlay preserves workspace geometry during navigation. -->
      <MdDelayedOperationWorkspace
        class="analysis-overlay"
        :class="{ 'analysis-overlay--initial': !result }"
        :active="busy"
        mode="overlay"
        role="status"
        aria-live="polite"
      >
        <MdOperationProgress
          :icon-name="ICON_NAMES.analysis"
          :title="cancelling ? t('loading.cancelling') : t('analysis.analyzing')"
          :progress="progress"
          :path-label="t('loading.currentAnalysisDirectory')"
          :preparing-text="t('loading.preparingAnalysisDirectory')"
          :hint="t('loading.cancelHint')"
          :cancelable="true"
          :cancel-disabled="cancelling"
          @cancel="emit('cancel')"
        />
      </MdDelayedOperationWorkspace>

      <MdEmptyState
        v-if="!result"
        :icon-name="ICON_NAMES.analysis"
        :title="t('analysis.emptyTitle')"
        :description="t('analysis.emptyDescription')"
      >
        <Button
          size="lg"
          type="button"
          :disabled="busy || deleting || !selectedScopePath"
          @click="startPrimaryAnalysis"
        >
          <MdIcon :name="ICON_NAMES.analysis" :size="17" />
          {{ t('analysis.start') }}
        </Button>
      </MdEmptyState>

      <div
        v-else
        class="browser-content"
        :class="{ 'browser-content--details': viewMode === ANALYSIS_VIEW_IDS.details }"
        :inert="busy ? '' : undefined"
        :aria-busy="busy"
      >
        <MdAnalysisFolderPane
          v-if="viewMode === ANALYSIS_VIEW_IDS.treemap"
          :entries="entries"
          :total-bytes="result.totalBytes"
          :folder-count="folderCount"
          :file-count="fileCount"
          :open-disabled="busy || deleting"
          :delete-disabled="busy || deleting"
          @activate="activateEntry"
          @open-entry="openEntry"
          @reveal="emit('reveal', $event)"
          @delete="requestDelete"
        />
        <MdAnalysisVisualPane
          :result="result"
          :entries="entries"
          :folder-count="folderCount"
          :view-mode="viewMode"
          :open-disabled="busy || deleting"
          :delete-disabled="busy || deleting"
          @update:view-mode="viewMode = $event"
          @activate="activateEntry"
          @open-entry="openEntry"
          @reveal="emit('reveal', $event)"
          @delete="requestDelete"
        />
      </div>
    </article>

    <MdDestructiveActionDialog
      v-model:open="confirmOpen"
      :title="t('analysis.deleteTitle')"
      :description="t('analysis.deleteDescription')"
      :summary-label="pendingDelete?.name"
      :summary-value="pendingDelete ? ByteSizeService.bytes(pendingDelete.bytes) : ''"
      :cancel-label="t('common.cancel')"
      :confirm-label="t('analysis.deleteAction')"
      :busy="deleting"
      @confirm="confirmDelete"
    />
  </MdPageShell>
</template>

<style scoped>
@reference "@assets/main.css";

.analysis-shell {
  min-height: 0;
  overflow: hidden;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

/*
 * Native disabled controls still block concurrent analysis. During folder
 * navigation, keep their resting appearance so a short request cannot flash
 * the entire page header between full and half opacity.
 */
.header-actions.folder-navigation-pending :deep(button:disabled) {
  opacity: 1;
  transition-duration: 0s;
}

.primary-analysis-labels {
  display: grid;
}

.primary-analysis-labels > span {
  visibility: hidden;
  grid-area: 1 / 1;
}

.primary-analysis-labels > span.visible {
  visibility: visible;
}

.browser-card {
  position: relative;
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  overflow: hidden;
  overflow-anchor: none;
  border-width: 1px;
  border-radius: var(--radius);
  @apply border-border/70 bg-workspace text-foreground;
}

.analysis-overlay {
  --operation-workspace-overlay-top: calc(var(--layout-workspace-toolbar-height) + 1px);
}

.analysis-overlay--initial {
  --operation-workspace-overlay-top: 0;
}

.browser-content {
  display: grid;
  min-height: 0;
  flex: 1;
  overflow: hidden;
  overflow-anchor: none;
  contain: layout paint;
}

/*
 * Treemap mode keeps the rank pane for quick directory lookup. Details mode
 * already exposes the same entries with sorting and actions, so it owns the
 * full workspace instead of repeating that data beside another list.
 */
@container analysis (min-width: 672px) {
  .browser-content:not(.browser-content--details) {
    grid-template-columns: minmax(300px, 42%) minmax(0, 58%);
  }
}

@container analysis (max-width: 671px) {
  /* Keep one primary view on small windows instead of splitting scarce height. */
  .browser-content:not(.browser-content--details) :deep(.folder-pane) {
    display: none;
  }
}

@container analysis (min-width: 1024px) {
  .browser-content:not(.browser-content--details) {
    grid-template-columns: minmax(330px, 36%) minmax(0, 64%);
  }
}

/* Monterey's WKWebView has no container queries. At MangoDisk's supported
 * minimum desktop width, the available page area matches the two-pane mode.
 */
@supports not (container-type: inline-size) {
  @media (min-width: 900px) {
    .browser-content:not(.browser-content--details) {
      grid-template-columns: minmax(300px, 42%) minmax(0, 58%);
    }
  }
}
</style>
