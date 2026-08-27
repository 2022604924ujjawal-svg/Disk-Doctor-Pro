<script setup lang="ts">
import { reactiveOmit } from '@vueuse/core';
import type { DialogContentEmits, DialogContentProps } from 'reka-ui';
import {
  DialogContent,
  DialogOverlay,
  DialogPortal,
  useForwardPropsEmits,
} from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

const props = defineProps<DialogContentProps & { class?: HTMLAttributes['class'] }>();
const emits = defineEmits<DialogContentEmits>();
const delegatedProps = reactiveOmit(props, 'class');
const forwarded = useForwardPropsEmits(delegatedProps, emits);
</script>

<template>
  <DialogPortal>
    <DialogOverlay
      data-slot="dialog-overlay"
      class="fixed inset-0 z-50 bg-background/65 backdrop-blur-sm data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0"
    />
    <DialogContent
      data-slot="dialog-content"
      v-bind="forwarded"
      :class="cn('fixed top-1/2 left-1/2 z-50 grid w-full max-w-[680px] -translate-x-1/2 -translate-y-1/2 gap-4 rounded-2xl border border-border bg-card text-card-foreground shadow-2xl shadow-foreground/15 duration-200 data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95', props.class)"
    >
      <slot />
    </DialogContent>
  </DialogPortal>
</template>
