<script setup lang="ts">
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';

withDefaults(
  defineProps<{
    ariaLabel?: string;
    compact?: boolean;
    disabled?: boolean;
    modelValue: string;
    placeholder: string;
  }>(),
  {
    ariaLabel: undefined,
    compact: false,
    disabled: false,
  }
);

const emit = defineEmits<{
  'update:modelValue': [value: string];
}>();

function updateValue(event: Event) {
  emit('update:modelValue', (event.target as HTMLInputElement).value);
}
</script>

<template>
  <label class="result-search" :class="{ 'result-search--compact': compact }">
    <MdIcon :name="ICON_NAMES.search" :size="18" />
    <input
      :value="modelValue"
      type="search"
      autocomplete="off"
      enterkeyhint="search"
      :aria-label="ariaLabel || placeholder"
      :disabled="disabled"
      :placeholder="placeholder"
      :spellcheck="false"
      @input="updateValue"
    />
  </label>
</template>

<style scoped>
@reference "@assets/main.css";

.result-search {
  display: grid;
  min-width: 220px;
  width: 320px;
  height: var(--layout-workspace-control-height);
  max-width: 320px;
  flex: none;
  grid-template-columns: 18px minmax(0, 1fr);
  align-items: center;
  gap: 8px;
  border-width: 1px;
  border-radius: 10px;
  padding: 0 11px;
  transition:
    border-color 150ms ease,
    box-shadow 150ms ease;
  @apply border-input  text-muted-foreground;
}

.result-search:focus-within {
  @apply border-ring ring-2 ring-ring/10;
}

.result-search:has(input:disabled) {
  cursor: not-allowed;
  opacity: 0.6;
}

.result-search input {
  min-width: 0;
  width: 100%;
  height: 100%;
  border: 0;
  outline: 0;
  background: transparent;
  color: var(--foreground);
  font: inherit;
  font-size: 14px;
}

.result-search input::placeholder {
  color: var(--muted-foreground);
}

.result-search input::-webkit-search-cancel-button {
  cursor: pointer;
}

.result-search input:disabled {
  cursor: not-allowed;
}

.result-search--compact {
  min-width: 168px;
  width: 168px;
  max-width: 168px;
}

@container (max-width: 760px) {
  .result-search:not(.result-search--compact) {
    width: 240px;
    max-width: 240px;
  }
}

@container (max-width: 560px) {
  .result-search:not(.result-search--compact) {
    min-width: 180px;
    width: 180px;
    max-width: 180px;
  }
}
</style>
