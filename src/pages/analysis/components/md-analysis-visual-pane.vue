<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MdIcon from '@/components/icons/md-icon.vue';
import { ANALYSIS_VIEW_IDS } from '@/lib/models/analysis';
import { ICON_NAMES } from '@/lib/models/ui';
import type { AnalysisResult, AnalysisViewId, DirectoryEntryInfo } from '@/lib/models/analysis';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';

import MdAnalysisDetailsTable from './md-analysis-details-table.vue';
import MdAnalysisTreemap from './md-analysis-treemap.vue';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  result: AnalysisResult;
  entries: DirectoryEntryInfo[];
  folderCount: number;
  viewMode: AnalysisViewId;
  openDisabled: boolean;
  deleteDisabled: boolean;
}>();

const emit = defineEmits<{
  activate: [entry: DirectoryEntryInfo];
  openEntry: [entry: DirectoryEntryInfo];
  reveal: [path: string];
  delete: [entry: DirectoryEntryInfo];
  'update:viewMode': [viewMode: AnalysisViewId];
}>();
</script>

<template>
  <section class="visual-pane">
    <header class="md-workspace-toolbar">
      <p>
        {{
          t(
            'analysis.folderSpaceSummary',
            { folders: FormatUtils.integer(folderCount), size: ByteSizeService.bytes(result.totalBytes) },
            folderCount
          )
        }}
      </p>
      <div class="view-switcher" role="group" :aria-label="t('analysis.result')">
        <button
          type="button"
          :class="{ active: props.viewMode === ANALYSIS_VIEW_IDS.treemap }"
          :aria-pressed="props.viewMode === ANALYSIS_VIEW_IDS.treemap"
          @click="emit('update:viewMode', ANALYSIS_VIEW_IDS.treemap)"
        >
          <MdIcon :name="ICON_NAMES.grid" :size="15" />
          {{ t('analysis.treemap') }}
        </button>
        <button
          type="button"
          :class="{ active: props.viewMode === ANALYSIS_VIEW_IDS.details }"
          :aria-pressed="props.viewMode === ANALYSIS_VIEW_IDS.details"
          @click="emit('update:viewMode', ANALYSIS_VIEW_IDS.details)"
        >
          <MdIcon :name="ICON_NAMES.list" :size="15" />
          {{ t('analysis.details') }}
        </button>
      </div>
    </header>

    <MdAnalysisTreemap
      v-if="props.viewMode === ANALYSIS_VIEW_IDS.treemap"
      :entries="entries"
      :total-bytes="result.totalBytes"
      :open-disabled="openDisabled"
      :delete-disabled="deleteDisabled"
      @activate="emit('activate', $event)"
      @open-entry="emit('openEntry', $event)"
      @reveal="emit('reveal', $event)"
      @delete="emit('delete', $event)"
    />
    <MdAnalysisDetailsTable
      v-else
      :entries="entries"
      :open-disabled="openDisabled"
      :delete-disabled="deleteDisabled"
      @activate="emit('activate', $event)"
      @open-entry="emit('openEntry', $event)"
      @reveal="emit('reveal', $event)"
      @delete="emit('delete', $event)"
    />
  </section>
</template>

<style scoped>
@reference "@assets/main.css";

.visual-pane {
  display: flex;
  min-width: 0;
  min-height: 0;
  flex-direction: column;
  overflow: hidden;
}

.visual-pane > header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 2px 12px;
}

.visual-pane header p {
  min-width: 0;
  margin: 0;
  overflow: hidden;
  @apply text-muted-foreground;
  font-size: var(--font-content-secondary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.view-switcher {
  display: flex;
  height: var(--layout-workspace-control-height);
  flex: none;
  overflow: hidden;
  border-width: 1px;
  border-radius: 8px;
  @apply border-border;
}

.view-switcher button {
  display: flex;
  height: 100%;
  align-items: center;
  gap: 6px;
  border: 0;
  border-left-width: 1px;
  padding: 0 11px;
  @apply border-border bg-card text-card-foreground transition-colors duration-200;
  font: inherit;
  font-size: var(--font-content-secondary);
  cursor: pointer;
}

.view-switcher button:first-child {
  border-left: 0;
}

.view-switcher button:hover:not(.active) {
  @apply border-primary/40 bg-accent/65 text-accent-foreground;
}

.view-switcher button.active {
  @apply bg-accent text-accent-foreground;
}

.view-switcher button:focus-visible {
  position: relative;
  z-index: 1;
  @apply outline-none ring-2 ring-inset ring-ring/45;
}
</style>
