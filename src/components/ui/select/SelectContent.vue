<script setup lang="ts">
import { reactiveOmit } from '@vueuse/core';
import type { SelectContentEmits, SelectContentProps } from 'reka-ui';
import { SelectContent, SelectPortal, SelectViewport, useForwardPropsEmits } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

import SelectScrollButton from './SelectScrollButton.vue';

defineOptions({ inheritAttrs: false });
const props = withDefaults(
  defineProps<SelectContentProps & { class?: HTMLAttributes['class'] }>(),
  { position: 'popper' },
);
const emits = defineEmits<SelectContentEmits>();
const delegatedProps = reactiveOmit(props, 'class');
const forwarded = useForwardPropsEmits(delegatedProps, emits);
</script>

<template>
  <SelectPortal>
    <SelectContent
      data-slot="select-content"
      v-bind="{ ...$attrs, ...forwarded }"
      :class="cn('bg-popover text-popover-foreground data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 relative z-50 max-h-(--reka-select-content-available-height) min-w-[8rem] overflow-x-hidden overflow-y-auto rounded-md border shadow-lg data-[side=bottom]:translate-y-1 data-[side=top]:-translate-y-1', props.class)"
    >
      <SelectScrollButton direction="up" />
      <SelectViewport class="w-full min-w-(--reka-select-trigger-width) scroll-my-1 p-1"><slot /></SelectViewport>
      <SelectScrollButton direction="down" />
    </SelectContent>
  </SelectPortal>
</template>
