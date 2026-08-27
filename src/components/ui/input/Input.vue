<script setup lang="ts">
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

const props = withDefaults(
  defineProps<{
    class?: HTMLAttributes['class'];
    variant?: 'default' | 'embedded';
  }>(),
  { variant: 'default' },
);
const model = defineModel<string>();
</script>

<template>
  <input
    v-model="model"
    data-slot="input"
    :class="cn(
      'h-9 w-full min-w-0 rounded-md bg-transparent px-3 py-1 text-sm outline-none transition-[color,box-shadow,border-color] placeholder:text-muted-foreground disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50',
      props.variant === 'embedded'
        ? 'h-full border-0 px-0 shadow-none focus-visible:border-transparent focus-visible:ring-0 focus-visible:outline-none'
        : 'border border-input shadow-xs focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30',
      props.class,
    )"
  />
</template>
