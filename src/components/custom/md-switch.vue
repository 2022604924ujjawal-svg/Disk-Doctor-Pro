<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    modelValue?: boolean;
    disabled?: boolean;
  }>(),
  {
    modelValue: false,
    disabled: false,
  }
);

const emit = defineEmits<{
  'update:modelValue': [value: boolean];
}>();

function toggle() {
  if (!props.disabled) emit('update:modelValue', !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    class="md-switch"
    role="switch"
    :aria-checked="modelValue"
    :data-state="modelValue ? 'checked' : 'unchecked'"
    :disabled="disabled"
    @click="toggle"
  >
    <span />
  </button>
</template>

<style scoped>
.md-switch {
  position: relative;
  width: 34px;
  height: 20px;
  flex: none;
  border: 1px solid color-mix(in oklab, var(--border) 86%, var(--foreground));
  border-radius: 999px;
  padding: 0;
  background: var(--muted);
  cursor: pointer;
  transition:
    border-color 140ms ease,
    background-color 140ms ease;
}
.md-switch > span {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 14px;
  height: 14px;
  border-radius: 999px;
  background: var(--background);
  box-shadow: 0 1px 2px color-mix(in oklab, var(--foreground) 18%, transparent);
  transition: transform 140ms ease;
}
.md-switch[data-state='checked'] {
  border-color: var(--primary);
  background: var(--primary);
}
.md-switch[data-state='checked'] > span {
  transform: translateX(14px);
}
.md-switch:focus-visible {
  outline: 2px solid color-mix(in oklab, var(--primary) 45%, transparent);
  outline-offset: 2px;
}
.md-switch:disabled {
  cursor: not-allowed;
  opacity: 0.48;
}
</style>
