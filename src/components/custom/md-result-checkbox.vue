<script setup lang="ts">
import { computed } from 'vue';

import { Checkbox } from '@/components/ui/checkbox';
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';

type ResultCheckboxState = boolean | 'indeterminate';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    checked?: boolean;
    disabled?: boolean;
    indeterminate?: boolean;
  }>(),
  {
    checked: false,
    disabled: false,
    indeterminate: false,
  }
);

const emit = defineEmits<{
  'update:checked': [checked: boolean];
}>();

/*
 * Result pages need the same checked, partial, disabled, and focus behavior.
 * Keeping the state adapter here prevents individual pages from falling back
 * to browser-native checkbox rendering or implementing indeterminate state
 * differently.
 */
const state = computed<ResultCheckboxState>(() => (props.indeterminate ? 'indeterminate' : props.checked));

function updateChecked(value: ResultCheckboxState) {
  emit('update:checked', value === true);
}
</script>

<template>
  <Checkbox
    v-bind="$attrs"
    class="md-result-checkbox size-[17px] rounded-[5px] border-[1.5px] shadow-none data-[state=indeterminate]:border-primary data-[state=indeterminate]:bg-primary data-[state=indeterminate]:text-primary-foreground"
    :model-value="state"
    :disabled="disabled"
    @update:model-value="updateChecked"
  >
    <MdIcon :name="state === 'indeterminate' ? ICON_NAMES.minus : ICON_NAMES.check" :size="12" :stroke-width="2.5" />
  </Checkbox>
</template>
