<script setup lang="ts">
import { reactiveOmit } from '@vueuse/core';
import type { DialogDescriptionProps } from 'reka-ui';
import { DialogDescription } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

const props = defineProps<DialogDescriptionProps & { class?: HTMLAttributes['class'] }>();
const delegatedProps = reactiveOmit(props, 'class');
</script>

<template>
  <DialogDescription
    data-slot="dialog-description"
    v-bind="delegatedProps"
    :class="cn('text-sm text-muted-foreground', props.class)"
  >
    <slot />
  </DialogDescription>
</template>
