<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';

import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import MdResultCheckbox from '@/components/custom/md-result-checkbox.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import type { DiskInfo } from '@/lib/models/disk';
import { ICON_NAMES } from '@/lib/models/ui';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';
import { PathUtils } from '@/lib/utils/path';

const props = defineProps<{
  disks: DiskInfo[];
  initialMountPoints: string[];
  modelValue: boolean;
  systemDisk: DiskInfo | null;
}>();
const emit = defineEmits<{
  confirm: [mountPoints: string[]];
  'update:modelValue': [open: boolean];
}>();

const { t } = useI18n({ useScope: 'global' });
const selectedMountPoints = ref<string[]>([]);
const selectedKeys = computed(() => new Set(selectedMountPoints.value.map(PathUtils.comparisonKey)));

function isSystemDisk(disk: DiskInfo): boolean {
  return Boolean(
    props.systemDisk &&
    PathUtils.comparisonKey(props.systemDisk.mountPoint) === PathUtils.comparisonKey(disk.mountPoint)
  );
}

const orderedDisks = computed(() =>
  [...props.disks].sort((left, right) => Number(isSystemDisk(right)) - Number(isSystemDisk(left)))
);
const systemDiskIsAvailable = computed(() => orderedDisks.value.some(isSystemDisk));
const selectedVolumeCount = computed(() => selectedMountPoints.value.length + Number(systemDiskIsAvailable.value));

function isSelected(disk: DiskInfo): boolean {
  return isSystemDisk(disk) || selectedKeys.value.has(PathUtils.comparisonKey(disk.mountPoint));
}

function resetSelection() {
  const availableByKey = new Map(props.disks.map(disk => [PathUtils.comparisonKey(disk.mountPoint), disk.mountPoint]));
  const systemDiskKey = props.systemDisk ? PathUtils.comparisonKey(props.systemDisk.mountPoint) : null;
  selectedMountPoints.value = [
    ...new Set(
      props.initialMountPoints
        .map(mountPoint => availableByKey.get(PathUtils.comparisonKey(mountPoint)))
        .filter((mountPoint): mountPoint is string => {
          if (!mountPoint) return false;
          return PathUtils.comparisonKey(mountPoint) !== systemDiskKey;
        })
    ),
  ];
}

function setSelected(disk: DiskInfo, selected: boolean) {
  if (isSystemDisk(disk)) return;
  const key = PathUtils.comparisonKey(disk.mountPoint);
  selectedMountPoints.value = selected
    ? [...selectedMountPoints.value.filter(value => PathUtils.comparisonKey(value) !== key), disk.mountPoint]
    : selectedMountPoints.value.filter(value => PathUtils.comparisonKey(value) !== key);
}

function confirm() {
  if (!selectedVolumeCount.value) return;
  emit('confirm', [...selectedMountPoints.value]);
  emit('update:modelValue', false);
}

watch(
  () => props.modelValue,
  open => {
    if (open) resetSelection();
  }
);
</script>

<template>
  <Dialog :open="modelValue" @update:open="emit('update:modelValue', $event)">
    <MdDialogContent
      class="flex max-h-[calc(100dvh-1.5rem)] min-h-0 flex-col gap-0 overflow-hidden p-0 sm:max-w-[600px]"
    >
      <DialogHeader class="volume-dialog-header flex-none px-5 pt-4 pr-12">
        <DialogTitle>{{ t('cleanup.scanMode.volumeDialogTitle') }}</DialogTitle>
        <DialogDescription>{{ t('cleanup.scanMode.volumeDialogDescription') }}</DialogDescription>
      </DialogHeader>

      <div class="volume-list scrollbar-stable">
        <div v-if="orderedDisks.length" class="volume-list-frame">
          <label
            v-for="(disk, index) in orderedDisks"
            :key="disk.mountPoint"
            class="volume-option"
            :class="{ 'volume-option--fixed': isSystemDisk(disk) }"
          >
            <MdResultCheckbox
              :id="`cleanup-volume-${index}`"
              :checked="isSelected(disk)"
              :disabled="isSystemDisk(disk)"
              :class="{ 'system-volume-checkbox': isSystemDisk(disk) }"
              @update:checked="setSelected(disk, $event)"
            />
            <span class="volume-icon" aria-hidden="true">
              <MdIcon :name="ICON_NAMES.hardDrive" :size="20" />
            </span>
            <span class="volume-copy">
              <span class="volume-name-row">
                <strong>{{ disk.name || disk.mountPoint }}</strong>
                <small v-if="isSystemDisk(disk)" class="system-volume-badge">
                  {{ t('cleanup.scanMode.systemVolume') }}
                </small>
              </span>
              <small>{{ disk.mountPoint }}</small>
            </span>
            <span class="volume-capacity">
              {{
                t('cleanup.scanMode.volumeCapacity', {
                  available: ByteSizeService.bytes(disk.availableBytes),
                  total: ByteSizeService.bytes(disk.totalBytes),
                })
              }}
            </span>
          </label>
        </div>
        <p v-else class="volume-empty">{{ t('cleanup.scanMode.noVolumes') }}</p>
      </div>

      <DialogFooter class="volume-dialog-footer flex-none border-t border-border/70 px-5 py-3 sm:justify-between">
        <span class="selected-volume-count">
          {{
            t(
              'cleanup.scanMode.selectedVolumeCount',
              { count: FormatUtils.integer(selectedVolumeCount) },
              selectedVolumeCount
            )
          }}
        </span>
        <span class="volume-dialog-actions">
          <Button variant="outline" type="button" @click="emit('update:modelValue', false)">
            {{ t('common.cancel') }}
          </Button>
          <Button type="button" :disabled="!selectedVolumeCount" @click="confirm">
            {{ t('cleanup.scanMode.startSelectedScan') }}
          </Button>
        </span>
      </DialogFooter>
    </MdDialogContent>
  </Dialog>
</template>

<style scoped>
@reference "@assets/main.css";

.volume-dialog-header {
  gap: 4px;
}

.volume-list {
  container-type: inline-size;
  min-height: 0;
  max-height: min(430px, 58dvh);
  overflow-y: auto;
  padding: 14px 20px 16px;
}

.volume-list-frame {
  @apply divide-y divide-border/60 overflow-hidden border border-border/70;
  border-radius: 9px;
}

.volume-option {
  @apply hover:bg-accent/55;
  display: grid;
  min-height: 66px;
  cursor: pointer;
  grid-template-columns: auto auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  padding: 9px 12px;
  transition: background-color 140ms ease;
}

.volume-option--fixed {
  cursor: default;
}

.volume-option--fixed:hover {
  background-color: transparent;
}

:deep(.system-volume-checkbox:disabled) {
  cursor: default;
  opacity: 1;
}

.volume-icon {
  @apply text-muted-foreground;
  display: grid;
  place-items: center;
}

.volume-copy {
  display: grid;
  min-width: 0;
  gap: 2px;
}

.volume-copy > small,
.volume-capacity,
.selected-volume-count {
  @apply text-muted-foreground;
  font-size: var(--font-content-secondary);
}

.volume-copy > small,
.volume-name-row strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.volume-name-row {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 7px;
}

.volume-name-row strong {
  font-size: var(--font-content-primary);
  font-weight: 600;
}

.system-volume-badge {
  @apply bg-accent text-accent-foreground;
  flex: none;
  border-radius: 999px;
  padding: 1px 7px;
  font-size: 11px;
}

.volume-capacity {
  white-space: nowrap;
}

.volume-empty {
  @apply text-muted-foreground;
  margin: 20px 0;
  text-align: center;
  font-size: var(--font-content-primary);
}

.volume-dialog-footer {
  align-items: center;
}

.volume-dialog-actions {
  display: flex;
  gap: 8px;
}

@container (max-width: 520px) {
  .volume-option {
    grid-template-columns: auto auto minmax(0, 1fr);
  }

  .volume-capacity {
    grid-column: 3;
  }
}
</style>
