<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import MdFileEntryContextMenu from '@/components/custom/md-file-entry-context-menu.vue';
import MdNativeFileIcon from '@/components/custom/md-native-file-icon.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';
import type { DirectoryEntryInfo } from '@/lib/models/analysis';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';

const { t } = useI18n({ useScope: 'global' });

defineProps<{
  entries: DirectoryEntryInfo[];
  totalBytes: number;
  folderCount: number;
  fileCount: number;
  openDisabled: boolean;
  deleteDisabled: boolean;
}>();

const emit = defineEmits<{
  activate: [entry: DirectoryEntryInfo];
  openEntry: [entry: DirectoryEntryInfo];
  reveal: [path: string];
  delete: [entry: DirectoryEntryInfo];
}>();
</script>

<template>
  <aside class="folder-pane border-b @2xl/analysis:border-r @2xl/analysis:border-b-0">
    <header class="md-workspace-toolbar">
      <p>
        {{
          t(
            'analysis.folderFileSummary',
            { folders: FormatUtils.integer(folderCount), files: FormatUtils.integer(fileCount) },
            fileCount
          )
        }}
      </p>
    </header>
    <div class="folder-list scrollbar-stable">
      <MdFileEntryContextMenu
        v-for="entry in entries"
        :key="entry.path"
        :open-disabled="openDisabled"
        :delete-disabled="deleteDisabled"
        @open="emit('openEntry', entry)"
        @reveal="emit('reveal', entry.path)"
        @delete="emit('delete', entry)"
      >
        <div class="folder-row">
          <button
            class="folder-entry"
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
            <span class="item-copy">
              <strong class="md-result-primary">{{ entry.name }}</strong>
              <small>
                {{ t('common.fileCount', { count: FormatUtils.integer(entry.fileCount) }, entry.fileCount) }}
              </small>
            </span>
            <span class="item-metrics">
              <span>
                <strong class="md-result-primary">{{ ByteSizeService.bytes(entry.bytes) }}</strong>
                <small>{{ Math.round(FormatUtils.percent(entry.bytes, totalBytes)) }}%</small>
              </span>
              <i>
                <em :style="{ width: `${FormatUtils.percent(entry.bytes, totalBytes)}%` }" />
              </i>
            </span>
            <span class="chevron">
              <MdIcon v-if="entry.isDirectory" :name="ICON_NAMES.chevronRight" :size="18" />
            </span>
          </button>
        </div>
      </MdFileEntryContextMenu>
    </div>
  </aside>
</template>

<style scoped>
@reference "@assets/main.css";

.folder-pane {
  display: flex;
  min-width: 0;
  min-height: 0;
  flex-direction: column;
  overflow: hidden;
  @apply border-border;
}

.folder-pane > header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 2px 12px;
}

.folder-pane header p {
  min-width: 0;
  margin: 0;
  overflow: hidden;
  @apply text-muted-foreground;
  font-size: var(--font-content-secondary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.folder-list {
  min-height: 0;
  flex: 1;
  overflow-x: hidden;
  padding: 0 8px 8px;
}

.folder-row {
  display: grid;
  width: 100%;
  grid-template-columns: minmax(0, 1fr);
  border-radius: var(--radius);
  padding: 4px 6px;
}

.folder-row:hover,
.folder-row:has(.folder-entry:focus-visible) {
  @apply bg-muted/60;
}

.folder-entry {
  display: grid;
  min-width: 0;
  min-height: var(--layout-result-row-height);
  grid-template-columns: 38px minmax(90px, 1fr) minmax(118px, 0.85fr) 18px;
  align-items: center;
  gap: 8px;
  border: 0;
  padding: 2px 4px;
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.folder-entry:focus-visible {
  outline: none;
}

.item-copy,
.item-metrics {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

.item-copy {
  gap: 3px;
}

.item-copy strong,
.item-metrics strong {
  @apply text-card-foreground;
}

.item-copy strong {
  overflow: hidden;
  font-size: var(--font-content-primary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.item-copy small,
.item-metrics small {
  @apply text-muted-foreground;
  font-size: var(--font-content-secondary);
}

.item-metrics {
  gap: 5px;
}

.item-metrics > span {
  display: flex;
  justify-content: space-between;
}

.item-metrics strong {
  font-size: var(--font-content-body);
}

.item-metrics > i {
  height: 5px;
  overflow: hidden;
  border-radius: 99px;
  background: var(--surface-primary-subtle);
}

.item-metrics em {
  display: block;
  height: 100%;
  border-radius: 99px;
  @apply bg-primary;
}

.chevron {
  @apply text-muted-foreground;
}

@supports not (container-type: inline-size) {
  @media (min-width: 900px) {
    .folder-pane {
      border-right-width: 1px;
      border-bottom-width: 0;
    }
  }
}
</style>
