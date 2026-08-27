<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MdIconAction from '@/components/custom/md-icon-action.vue';
import MdResultCheckbox from '@/components/custom/md-result-checkbox.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';
import { ByteSizeService } from '@/lib/services/byte-size-service';

withDefaults(
  defineProps<{
    title: string;
    selectedBytes: number;
    totalBytes: number;
    selection: 'all' | 'partial' | 'none';
    disabled?: boolean;
    description?: string;
  }>(),
  { description: undefined, disabled: false }
);
const emit = defineEmits<{
  'update:selected': [selected: boolean];
}>();
const { t } = useI18n({ useScope: 'global' });
</script>

<template>
  <header class="detail-header">
    <span class="detail-heading">
      <strong class="detail-title">{{ title }}</strong>
      <MdIconAction
        v-if="description"
        appearance="unstyled"
        class="detail-help"
        :label="description"
        tooltip-side="bottom"
        tooltip-class="max-w-72 leading-relaxed"
      >
        <MdIcon :name="ICON_NAMES.info" :size="15" />
      </MdIconAction>
    </span>
    <span class="detail-size">
      <small>{{ t('cleanup.selected') }} / {{ t('cleanup.cleanableFound') }}</small>
      <strong>{{ ByteSizeService.bytes(selectedBytes) }}</strong>
      <i>/ {{ ByteSizeService.bytes(totalBytes) }}</i>
    </span>
    <label class="category-selection">
      <MdResultCheckbox
        :checked="selection === 'all'"
        :indeterminate="selection === 'partial'"
        :disabled="disabled"
        @update:checked="emit('update:selected', $event)"
      />
      <span>{{ t('cleanup.selectAll') }}</span>
    </label>
  </header>
</template>

<style scoped>
@reference "@assets/main.css";

.detail-header {
  @apply border-border;
  display: grid;
  min-height: 46px;
  flex: none;
  grid-template-columns: minmax(0, 1fr) auto auto;
  align-items: center;
  gap: 12px;
  border-bottom-width: 1px;
  padding: 5px 12px;
}

.detail-heading {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 6px;
}

.detail-title {
  min-width: 0;
  overflow: hidden;
  font-size: var(--font-content-primary);
  font-weight: 600;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.detail-heading :deep(.detail-help) {
  display: inline-flex;
  width: 24px;
  height: 24px;
  flex: none;
  align-items: center;
  justify-content: center;
  border: 0;
  border-radius: 6px;
  padding: 0;
  background: transparent;
  @apply text-muted-foreground transition-colors hover:bg-muted hover:text-foreground;
  cursor: help;
}

.detail-heading :deep(.detail-help:focus-visible) {
  outline: 2px solid var(--focus-ring-subtle);
  outline: 2px solid color-mix(in oklab, var(--ring) 45%, transparent);
  outline-offset: 1px;
}

.detail-size {
  display: flex;
  align-items: baseline;
  gap: 6px;
  white-space: nowrap;
}

.detail-size small {
  @apply text-muted-foreground;
  font-size: 10px;
}

.detail-size strong {
  @apply text-primary;
  font-size: 15px;
}

.detail-size i {
  @apply text-muted-foreground;
  font-size: 11px;
  font-style: normal;
}

.category-selection {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12px;
  cursor: pointer;
}

@container cleanup (max-width: 760px) {
  .detail-header {
    grid-template-columns: minmax(0, 1fr) auto;
    padding-inline: 10px;
  }

  .detail-size,
  .category-selection span {
    display: none;
  }
}
</style>
