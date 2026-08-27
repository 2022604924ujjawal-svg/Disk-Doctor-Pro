<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MdCategoryFilter from '@/components/custom/md-category-filter.vue';
import type { FileCategoryId } from '@/lib/models/file-category';

const { t } = useI18n({ useScope: 'global' });

const props = withDefaults(
  defineProps<{
    disabled?: boolean;
    modelValue: FileCategoryId;
    options: Array<{ value: FileCategoryId; label: string; count?: number }>;
  }>(),
  {
    disabled: false,
  }
);

const emit = defineEmits<{
  'update:modelValue': [value: FileCategoryId];
}>();

function updateCategory(value: string) {
  const category = props.options.find(option => option.value === value)?.value;
  if (category) emit('update:modelValue', category);
}
</script>

<template>
  <MdCategoryFilter
    :model-value="modelValue"
    :options="options"
    :disabled="disabled"
    :aria-label="t('common.filterFileCategory')"
    @update:model-value="updateCategory"
  />
</template>
