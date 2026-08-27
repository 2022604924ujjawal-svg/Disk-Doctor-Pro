<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';

import MdIconAction from '@/components/custom/md-icon-action.vue';
import MdNativeFileIcon from '@/components/custom/md-native-file-icon.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Select, SelectContent, SelectItem, SelectTrigger } from '@/components/ui/select';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import type { DiskInfo } from '@/lib/models/disk';
import { ICON_NAMES } from '@/lib/models/ui';
import { FolderSelectionService } from '@/lib/services/folder-selection-service';
import { findStandardScanFolderByPath, type StandardScanFolder } from '@/lib/services/standard-scan-folder-service';
import { PathUtils } from '@/lib/utils/path';

const { t } = useI18n({ useScope: 'global' });

const props = withDefaults(
  defineProps<{
    modelValue: string;
    disks: DiskInfo[];
    recentFolders?: string[];
    standardFolders?: StandardScanFolder[];
    disabled?: boolean;
    allowFolder?: boolean;
  }>(),
  {
    recentFolders: () => [],
    standardFolders: () => [],
    disabled: false,
    allowFolder: true,
  }
);

const emit = defineEmits<{
  error: [error: unknown];
  'remove-folder': [path: string];
  'update:modelValue': [value: string];
}>();

const CHOOSE_FOLDER_VALUE = '__mangodisk_choose_folder__';
const selectOpen = ref(false);
const selectedPathTooltipOpen = ref(false);
const hoveredFolderPath = ref('');
const selectedDisk = computed(() => {
  const selectedKey = PathUtils.comparisonKey(props.modelValue);
  return props.disks.find(disk => PathUtils.comparisonKey(disk.mountPoint) === selectedKey) ?? null;
});
const selectedStandardFolder = computed(() =>
  selectedDisk.value ? null : findStandardScanFolderByPath(props.standardFolders, props.modelValue)
);
const selectedLabel = computed(() => {
  if (selectedDisk.value) return selectedDisk.value.name;
  if (selectedStandardFolder.value) {
    // Keep the real path for scanning and tooltips, but render the localized name through its stable ID.
    return t(`folderPicker.standardFolders.${selectedStandardFolder.value.id}`);
  }
  return PathUtils.fileName(props.modelValue);
});
const standardFolderKeys = computed(
  () => new Set(props.standardFolders.map(folder => PathUtils.comparisonKey(folder.path)))
);
const folderOptions = computed(() => {
  const selectedKey = selectedDisk.value ? '' : PathUtils.comparisonKey(props.modelValue);
  const recentFolders = props.recentFolders.filter(
    path => !standardFolderKeys.value.has(PathUtils.comparisonKey(path))
  );
  recentFolders.sort((left, right) => {
    if (PathUtils.comparisonKey(left) === selectedKey) return -1;
    if (PathUtils.comparisonKey(right) === selectedKey) return 1;
    return 0;
  });
  const recentKeys = new Set(recentFolders.map(PathUtils.comparisonKey));
  const folders = recentFolders.map(path => ({
    path,
    label: PathUtils.fileName(path),
    removable: true,
    selected: PathUtils.comparisonKey(path) === selectedKey,
  }));
  if (
    props.modelValue &&
    !selectedDisk.value &&
    !standardFolderKeys.value.has(PathUtils.comparisonKey(props.modelValue)) &&
    !recentKeys.has(PathUtils.comparisonKey(props.modelValue))
  ) {
    folders.unshift({
      path: props.modelValue,
      label: selectedLabel.value,
      removable: false,
      selected: true,
    });
  }
  return folders;
});

async function updateValue(value: unknown) {
  if (typeof value !== 'string' || !value) return;
  closeTooltips();
  if (value !== CHOOSE_FOLDER_VALUE) {
    emit('update:modelValue', PathUtils.display(value));
    return;
  }

  try {
    // Flush the closed tooltip portal before the native dialog deactivates the
    // WebView. Otherwise Floating UI can briefly retain a detached anchor.
    await nextTick();
    const selected = await FolderSelectionService.select(false, t('scanScope.chooseFolder'), props.modelValue);
    if (!selected.length) return;
    const directories = await FolderSelectionService.filterExistingDirectories(selected);
    if (directories[0]) emit('update:modelValue', PathUtils.display(directories[0]));
  } catch (error) {
    emit('error', error);
  }
}

function removeFolder(path: string) {
  closeTooltips();
  emit('remove-folder', path);
}

function updateSelectOpen(open: boolean) {
  selectOpen.value = open;
  closeTooltips();
}

function showSelectedPathTooltip() {
  if (!selectOpen.value && !selectedDisk.value) selectedPathTooltipOpen.value = true;
}

function showFolderTooltip(path: string) {
  if (selectOpen.value) hoveredFolderPath.value = path;
}

function hideFolderTooltip(path: string) {
  if (hoveredFolderPath.value === path) hoveredFolderPath.value = '';
}

function closeTooltips() {
  selectedPathTooltipOpen.value = false;
  hoveredFolderPath.value = '';
}

// Native folder dialogs and window deactivation can remove a tooltip trigger
// before Reka UI receives pointerleave. Clearing controlled state on blur keeps
// the portal from retaining a tooltip without a valid positioning anchor.
onMounted(() => {
  window.addEventListener('blur', closeTooltips);
});
onBeforeUnmount(() => window.removeEventListener('blur', closeTooltips));
watch(() => props.modelValue, closeTooltips);
</script>

<template>
  <Select
    :model-value="modelValue"
    :disabled="disabled"
    @update:model-value="updateValue"
    @update:open="updateSelectOpen"
  >
    <SelectTrigger
      class="scope-select h-9 w-full sm:w-44"
      :aria-label="t('scanScope.label')"
      @pointerenter="showSelectedPathTooltip"
      @pointerleave="selectedPathTooltipOpen = false"
      @pointerdown="closeTooltips"
    >
      <Tooltip v-if="modelValue && !selectedDisk" :open="selectedPathTooltipOpen && !selectOpen">
        <TooltipTrigger as-child>
          <span class="flex min-w-0 flex-1 items-center gap-2">
            <MdIcon class="scope-trigger-icon" :name="ICON_NAMES.folder" :size="18" />
            <span class="min-w-0 flex-1 truncate text-left">
              {{ selectedLabel || t('scanScope.label') }}
            </span>
          </span>
        </TooltipTrigger>
        <TooltipContent side="left" :side-offset="8" class="scope-path-tooltip">
          {{ modelValue }}
        </TooltipContent>
      </Tooltip>
      <span v-else class="flex min-w-0 flex-1 items-center gap-2">
        <MdIcon class="scope-trigger-icon" :name="ICON_NAMES.hardDrive" :size="18" />
        <span class="min-w-0 flex-1 truncate text-left">
          {{ selectedLabel || t('scanScope.label') }}
        </span>
      </span>
    </SelectTrigger>
    <SelectContent>
      <div v-if="standardFolders.length" class="scope-section-label">
        {{ t('folderPicker.commonFolders') }}
      </div>
      <div
        v-for="folder in standardFolders"
        :key="folder.id"
        class="scope-history-option"
        :class="{ selected: PathUtils.comparisonKey(folder.path) === PathUtils.comparisonKey(modelValue) }"
        @pointerenter="showFolderTooltip(folder.path)"
        @pointerleave="hideFolderTooltip(folder.path)"
        @pointerdown="closeTooltips"
      >
        <SelectItem :value="folder.path" :text-value="t(`folderPicker.standardFolders.${folder.id}`)">
          <Tooltip :open="hoveredFolderPath === folder.path">
            <TooltipTrigger as-child>
              <span class="flex w-full min-w-0 items-center gap-2">
                <MdNativeFileIcon
                  class="scope-native-icon"
                  :path="folder.path"
                  :name="t(`folderPicker.standardFolders.${folder.id}`)"
                  directory
                  directory-mode="path"
                  compact
                />
                <span class="min-w-0 flex-1 truncate">
                  {{ t(`folderPicker.standardFolders.${folder.id}`) }}
                </span>
              </span>
            </TooltipTrigger>
            <TooltipContent side="left" :side-offset="8" class="scope-path-tooltip">
              {{ folder.path }}
            </TooltipContent>
          </Tooltip>
        </SelectItem>
      </div>
      <div v-if="standardFolders.length && folderOptions.length" class="scope-separator" role="separator"></div>
      <div
        v-for="folder in folderOptions"
        :key="folder.path"
        class="scope-history-option"
        :class="{ selected: folder.selected }"
        @pointerenter="showFolderTooltip(folder.path)"
        @pointerleave="hideFolderTooltip(folder.path)"
        @pointerdown="closeTooltips"
      >
        <SelectItem :value="folder.path" :text-value="folder.label" class="pr-16">
          <Tooltip :open="hoveredFolderPath === folder.path">
            <TooltipTrigger as-child>
              <span class="flex w-full min-w-0 items-center gap-2">
                <MdNativeFileIcon
                  class="scope-native-icon"
                  :path="folder.path"
                  :name="folder.label"
                  directory
                  directory-mode="generic"
                  compact
                />
                <span class="min-w-0 flex-1 truncate">{{ folder.label }}</span>
              </span>
            </TooltipTrigger>
            <TooltipContent side="left" :side-offset="8" class="scope-path-tooltip">
              {{ folder.path }}
            </TooltipContent>
          </Tooltip>
        </SelectItem>
        <MdIconAction
          v-if="folder.removable"
          appearance="unstyled"
          class="scope-history-remove"
          :label="t('scanScope.removeFolder', { name: folder.label })"
          @pointerdown.stop.prevent
          @click.stop="removeFolder(folder.path)"
        >
          <MdIcon :name="ICON_NAMES.close" :size="14" />
        </MdIconAction>
      </div>
      <div v-if="standardFolders.length || folderOptions.length" class="scope-separator" role="separator"></div>
      <SelectItem v-for="disk in disks" :key="disk.mountPoint" :value="disk.mountPoint">
        <span class="flex min-w-0 items-center gap-2">
          <MdIcon class="flex-none text-muted-foreground" :name="ICON_NAMES.hardDrive" :size="16" />
          <span class="truncate">{{ disk.name }}</span>
        </span>
      </SelectItem>
      <div v-if="allowFolder" class="scope-separator" role="separator"></div>
      <SelectItem v-if="allowFolder" :value="CHOOSE_FOLDER_VALUE">
        <span class="flex min-w-0 items-center gap-2">
          <MdIcon class="flex-none text-muted-foreground" :name="ICON_NAMES.folderPlus" :size="16" />
          <span>{{ t('scanScope.chooseFolder') }}</span>
        </span>
      </SelectItem>
    </SelectContent>
  </Select>
</template>

<style scoped>
@reference "@assets/main.css";
.scope-select {
  @apply border-border/70 bg-card/35 shadow-none hover:border-border hover:bg-card/55;
}
.scope-trigger-icon {
  flex: none;
  @apply text-muted-foreground;
}
.scope-section-label {
  padding: 0.25rem 0.5rem 0.125rem;
  @apply text-muted-foreground;
  font-size: var(--font-content-secondary);
}
.scope-separator {
  height: 1px;
  margin: 0.25rem -0.25rem;
  @apply bg-border;
}
.scope-history-option {
  display: grid;
  grid-template-areas: 'option';
}
.scope-history-option > * {
  grid-area: option;
}
.scope-history-option :deep(.scope-native-icon.native-file-icon),
.scope-history-option :deep(.scope-native-icon.directory-fallback) {
  width: 20px;
  height: 20px;
}
.scope-history-option :deep(.scope-history-remove) {
  z-index: 1;
  align-self: center;
  justify-self: end;
  margin-right: 0.25rem;
  display: grid;
  width: 1.75rem;
  height: 1.75rem;
  cursor: pointer;
  place-items: center;
  border-radius: 0.25rem;
  @apply text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none;
}
.scope-history-option.selected :deep(.scope-history-remove) {
  margin-right: 1.75rem;
}
.scope-path-tooltip {
  max-width: min(34rem, calc(100vw - 2rem));
  overflow-wrap: anywhere;
  text-align: left;
  text-wrap: wrap;
}
</style>
