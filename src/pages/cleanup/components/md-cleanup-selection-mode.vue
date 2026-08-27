<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import type { CleanupSelectionMode } from '@/lib/utils/cleanup-rule-selection';
import { ByteSizeService } from '@/lib/services/byte-size-service';

defineProps<{
  busy: boolean;
  mode: CleanupSelectionMode;
  recommendedBytes: number;
  totalBytes: number;
}>();

const emit = defineEmits<{
  change: [value: unknown];
}>();

const { t } = useI18n({ useScope: 'global' });
</script>

<template>
  <div class="selection-mode">
    <span>{{ t('cleanup.selectionMode.label') }}</span>
    <Select :model-value="mode" :disabled="busy" @update:model-value="emit('change', $event)">
      <SelectTrigger :aria-label="t('cleanup.selectionMode.label')">
        <SelectValue>
          {{ t(`cleanup.selectionMode.${mode}`) }}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="smart">
          {{ t('cleanup.selectionMode.smart') }} · {{ ByteSizeService.bytes(recommendedBytes) }}
        </SelectItem>
        <SelectItem value="all">
          {{ t('cleanup.selectionMode.all') }} · {{ ByteSizeService.bytes(totalBytes) }}
        </SelectItem>
        <SelectItem value="none">{{ t('cleanup.selectionMode.none') }}</SelectItem>
        <SelectItem v-if="mode === 'manual'" value="manual" disabled>
          {{ t('cleanup.selectionMode.manual') }}
        </SelectItem>
      </SelectContent>
    </Select>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.selection-mode {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 8px;
}

.selection-mode > span {
  @apply text-muted-foreground;
  flex: none;
  font-size: 11px;
}

.selection-mode :deep([data-slot='select-trigger']) {
  width: 202px;
  min-width: 202px;
  height: 38px;
}

.selection-mode :deep([data-slot='select-value']) {
  overflow: hidden;
  text-overflow: ellipsis;
}

@container (max-width: 760px) {
  .selection-mode > span {
    display: none;
  }

  .selection-mode :deep([data-slot='select-trigger']) {
    width: 148px;
    min-width: 148px;
  }
}
</style>
