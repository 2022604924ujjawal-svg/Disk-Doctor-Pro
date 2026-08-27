<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed, ref } from 'vue';

import MdNativeFileIcon from '@/components/custom/md-native-file-icon.vue';
import MdFileEntryContextMenu from '@/components/custom/md-file-entry-context-menu.vue';
import MdIconAction from '@/components/custom/md-icon-action.vue';
import MdResultTable from '@/components/custom/md-result-table.vue';
import MdResultTableRow from '@/components/custom/md-result-table-row.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { ANALYSIS_SORT_KEYS } from '@/lib/models/analysis';
import { ICON_NAMES } from '@/lib/models/ui';
import type { DirectoryEntryInfo } from '@/lib/models/analysis';
import { SORT_DIRECTIONS } from '@/lib/models/sort';
import { AnalysisEntryUtils, type AnalysisSortKey, type SortDirection } from '@/lib/utils/analysis-entry';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';

const { locale, t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  entries: DirectoryEntryInfo[];
  openDisabled: boolean;
  deleteDisabled: boolean;
}>();

const emit = defineEmits<{
  activate: [entry: DirectoryEntryInfo];
  openEntry: [entry: DirectoryEntryInfo];
  reveal: [path: string];
  delete: [entry: DirectoryEntryInfo];
}>();

const sortKey = ref<AnalysisSortKey>(ANALYSIS_SORT_KEYS.bytes);
const sortDirection = ref<SortDirection>(SORT_DIRECTIONS.descending);

const sortedEntries = computed(() => AnalysisEntryUtils.sort(props.entries, sortKey.value, sortDirection.value));

function changeSort(key: AnalysisSortKey) {
  if (sortKey.value === key) {
    sortDirection.value =
      sortDirection.value === SORT_DIRECTIONS.ascending ? SORT_DIRECTIONS.descending : SORT_DIRECTIONS.ascending;
    return;
  }
  sortKey.value = key;
  sortDirection.value = key === ANALYSIS_SORT_KEYS.name ? SORT_DIRECTIONS.ascending : SORT_DIRECTIONS.descending;
}

function sortIndicator(key: AnalysisSortKey) {
  if (sortKey.value !== key) return ICON_NAMES.arrowUpDown;
  return sortDirection.value === SORT_DIRECTIONS.ascending ? ICON_NAMES.arrowUp : ICON_NAMES.arrowDown;
}

function sortActionLabel(key: AnalysisSortKey) {
  return sortKey.value === key && sortDirection.value === SORT_DIRECTIONS.ascending
    ? t('analysis.sortDescending')
    : t('analysis.sortAscending');
}

function sortControlLabel(key: AnalysisSortKey, column: string) {
  return t('analysis.sortColumn', {
    column,
    direction: sortActionLabel(key),
  });
}
</script>

<template>
  <MdResultTable class="details-view">
    <template #header>
      <div
        class="details-head-grid grid-cols-[minmax(178px,1fr)_90px_72px] @5xl/analysis:grid-cols-[minmax(188px,1fr)_100px_85px_110px]"
      >
        <button
          type="button"
          class="md-result-sort flex h-full items-center justify-start gap-1"
          :data-active="sortKey === ANALYSIS_SORT_KEYS.name"
          :aria-label="sortControlLabel(ANALYSIS_SORT_KEYS.name, t('analysis.name'))"
          @click="changeSort(ANALYSIS_SORT_KEYS.name)"
        >
          {{ t('analysis.name') }}
          <MdIcon :name="sortIndicator(ANALYSIS_SORT_KEYS.name)" :size="13" />
        </button>
        <button
          type="button"
          class="details-number md-result-sort flex h-full items-center justify-end gap-1"
          :data-active="sortKey === ANALYSIS_SORT_KEYS.bytes"
          :aria-label="sortControlLabel(ANALYSIS_SORT_KEYS.bytes, t('analysis.size'))"
          @click="changeSort(ANALYSIS_SORT_KEYS.bytes)"
        >
          {{ t('analysis.size') }}
          <MdIcon :name="sortIndicator(ANALYSIS_SORT_KEYS.bytes)" :size="13" />
        </button>
        <button
          type="button"
          class="details-number md-result-sort flex h-full items-center justify-end gap-1"
          :data-active="sortKey === ANALYSIS_SORT_KEYS.fileCount"
          :aria-label="sortControlLabel(ANALYSIS_SORT_KEYS.fileCount, t('analysis.fileCount'))"
          @click="changeSort(ANALYSIS_SORT_KEYS.fileCount)"
        >
          {{ t('analysis.fileCount') }}
          <MdIcon :name="sortIndicator(ANALYSIS_SORT_KEYS.fileCount)" :size="13" />
        </button>
        <button
          type="button"
          class="details-modified md-result-sort hidden h-full items-center justify-end gap-1 @5xl/analysis:flex"
          :data-active="sortKey === ANALYSIS_SORT_KEYS.modified"
          :aria-label="sortControlLabel(ANALYSIS_SORT_KEYS.modified, t('analysis.modified'))"
          @click="changeSort(ANALYSIS_SORT_KEYS.modified)"
        >
          {{ t('analysis.modified') }}
          <MdIcon :name="sortIndicator(ANALYSIS_SORT_KEYS.modified)" :size="13" />
        </button>
      </div>
    </template>

    <MdFileEntryContextMenu
      v-for="entry in sortedEntries"
      :key="entry.path"
      :open-disabled="openDisabled"
      :delete-disabled="deleteDisabled"
      @open="emit('openEntry', entry)"
      @reveal="emit('reveal', entry.path)"
      @delete="emit('delete', entry)"
    >
      <MdResultTableRow
        class="details-row grid-cols-[minmax(178px,1fr)_90px_72px] @5xl/analysis:grid-cols-[minmax(188px,1fr)_100px_85px_110px]"
      >
        <span class="details-primary">
          <button
            class="details-name"
            type="button"
            :title="entry.path"
            @click="emit('activate', entry)"
            @dblclick="!entry.isDirectory && emit('openEntry', entry)"
            @keydown.enter="!entry.isDirectory && emit('openEntry', entry)"
          >
            <MdNativeFileIcon
              :path="entry.path"
              :name="entry.name"
              :directory="entry.isDirectory"
              directory-mode="generic"
              compact
            />
            <strong class="md-result-primary">{{ entry.name }}</strong>
          </button>
          <span class="details-actions">
            <MdIconAction
              variant="ghost"
              :label="t('common.open')"
              :disabled="openDisabled"
              @click="emit('openEntry', entry)"
            >
              <MdIcon :name="ICON_NAMES.external" :size="16" />
            </MdIconAction>
            <MdIconAction variant="ghost" :label="t('common.showInFileManager')" @click="emit('reveal', entry.path)">
              <MdIcon :name="ICON_NAMES.folder" :size="16" />
            </MdIconAction>
          </span>
        </span>
        <strong class="details-number md-result-primary">{{ ByteSizeService.bytes(entry.bytes) }}</strong>
        <span class="details-number">{{ FormatUtils.integer(entry.fileCount) }}</span>
        <span class="details-modified hidden @5xl/analysis:block">{{
          FormatUtils.dateTime(entry.modifiedAtMs, locale)
        }}</span>
      </MdResultTableRow>
    </MdFileEntryContextMenu>
  </MdResultTable>
</template>

<style scoped>
@reference "@assets/main.css";

.details-view {
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  overflow: hidden;
  margin: 0 16px 12px;
  border-width: 1px;
  border-radius: 9px;
  @apply border-border/70;
}

.details-view :deep(.result-table-header) {
  @apply border-border/70 bg-muted/35;
}

.details-head-grid,
.details-row {
  display: grid;
  align-items: center;
  gap: 12px;
}

.details-head-grid {
  height: var(--layout-result-header-height);
  font-size: var(--font-content-meta);
}

.details-head-grid button {
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.details-row {
  min-height: var(--layout-result-row-height);
  @apply text-card-foreground;
  font-size: var(--font-content-body);
}

.details-number,
.details-modified {
  font-variant-numeric: tabular-nums;
  text-align: right;
}

.details-primary {
  position: relative;
  display: flex;
  min-width: 0;
  align-items: center;
}

.details-name {
  display: flex;
  min-width: 0;
  flex: 1;
  align-items: center;
  gap: 11px;
  border: 0;
  padding: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.details-name strong {
  min-width: 0;
  flex: 1;
  overflow: hidden;
  font-size: var(--font-content-primary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.details-actions {
  position: absolute;
  right: 0;
  display: flex;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.14s ease;
}

.details-row:is(:hover, :has(:focus-visible)) .details-actions {
  opacity: 1;
  pointer-events: auto;
}

.details-row:is(:hover, :has(:focus-visible)) .details-name strong {
  padding-right: 64px;
}
</style>
