<script setup lang="ts">
import { reactiveOmit } from '@vueuse/core';
import { ChevronDown } from '@lucide/vue';
import type { SelectTriggerProps } from 'reka-ui';
import { SelectIcon, SelectTrigger, useForwardProps } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

const props = withDefaults(
  defineProps<SelectTriggerProps & { class?: HTMLAttributes['class']; size?: 'sm' | 'default' }>(),
  { size: 'default' },
);
const delegatedProps = reactiveOmit(props, 'class', 'size');
const forwardedProps = useForwardProps(delegatedProps);
</script>

<template>
  <SelectTrigger
    data-slot="select-trigger"
    :data-size="size"
    v-bind="forwardedProps"
    :class="cn('border-input data-[placeholder]:text-muted-foreground focus-visible:border-ring focus-visible:ring-ring/20 data-[state=open]:border-ring data-[state=open]:ring-ring/15 hover:border-primary/40 flex w-fit items-center justify-between gap-2 rounded-md border bg-background px-3 py-2 text-sm whitespace-nowrap shadow-xs transition-[color,box-shadow,border-color,background-color] outline-none hover:bg-accent/35 focus-visible:ring-3 data-[state=open]:ring-3 disabled:cursor-not-allowed disabled:opacity-50 data-[size=default]:h-10 data-[size=sm]:h-8 [&_svg]:pointer-events-none [&_svg]:shrink-0', props.class)"
  >
    <slot />
    <SelectIcon as-child><ChevronDown class="size-4 opacity-55" /></SelectIcon>
  </SelectTrigger>
</template>
