<script setup lang="ts">
import type { HTMLAttributes } from 'vue';

import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';

// The tooltip root creates a scoped-style boundary. Custom classes are
// forwarded to the button, so callers must target them with `:deep()` from
// scoped styles instead of relying on the caller's scope attribute.
defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    label: string;
    ariaLabel?: string;
    appearance?: 'result' | 'unstyled';
    destructive?: boolean;
    disabled?: boolean;
    showTooltip?: boolean;
    variant?: 'outline' | 'ghost';
    tooltipClass?: HTMLAttributes['class'];
    tooltipSide?: 'top' | 'right' | 'bottom' | 'left';
  }>(),
  {
    ariaLabel: undefined,
    appearance: 'result',
    destructive: false,
    disabled: false,
    showTooltip: true,
    variant: 'outline',
    tooltipClass: undefined,
    tooltipSide: 'top',
  }
);

const emit = defineEmits<{
  click: [event: MouseEvent];
}>();

function handleClick(event: MouseEvent) {
  if (props.disabled) {
    event.preventDefault();
    event.stopPropagation();
    return;
  }
  emit('click', event);
}
</script>

<template>
  <Tooltip :disabled="!showTooltip">
    <TooltipTrigger as-child>
      <button
        v-bind="$attrs"
        class="icon-action"
        type="button"
        :class="[appearance, appearance === 'result' ? variant : undefined, { destructive }]"
        :aria-label="ariaLabel ?? label"
        :aria-disabled="disabled || undefined"
        @click="handleClick"
      >
        <slot />
      </button>
    </TooltipTrigger>
    <TooltipContent :side="tooltipSide" :side-offset="6" :class="tooltipClass">
      {{ label }}
    </TooltipContent>
  </Tooltip>
</template>

<style scoped>
@reference "@assets/main.css";

.icon-action {
  cursor: pointer;
}

.icon-action[aria-disabled='true'] {
  cursor: not-allowed;
}

.icon-action.result {
  display: grid;
  width: 30px;
  height: 30px;
  place-items: center;
  justify-self: center;
  border-width: 1px;
  border-radius: 8px;
  padding: 0;
  @apply border-border/60 bg-transparent text-muted-foreground/75 transition-colors;
}

.icon-action.result.ghost {
  border-color: transparent;
  background: transparent;
}

.icon-action.result:not([aria-disabled='true']):hover {
  @apply border-primary/25 bg-accent/60 text-primary;
}

.icon-action.result.ghost:not([aria-disabled='true']):hover {
  border-color: transparent;
  @apply bg-muted/75 text-primary;
}

.icon-action.result.destructive:not([aria-disabled='true']):hover {
  @apply text-destructive;
  border-color: var(--border-subtle);
  background: var(--surface-destructive-subtle);
}

.icon-action.result.ghost.destructive:not([aria-disabled='true']):hover {
  border-color: transparent;
}

.icon-action.result[aria-disabled='true'] {
  opacity: 0.45;
}

.icon-action.result:focus-visible {
  @apply border-ring outline-none ring-2 ring-ring/35;
}
</style>
