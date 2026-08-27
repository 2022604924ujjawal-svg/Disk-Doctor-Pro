<script setup lang="ts">
import { reactiveOmit } from '@vueuse/core';
import type { ContextMenuItemEmits, ContextMenuItemProps } from 'reka-ui';
import { ContextMenuItem, useForwardPropsEmits } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

const props = defineProps<ContextMenuItemProps & { class?: HTMLAttributes['class'] }>();
const emits = defineEmits<ContextMenuItemEmits>();
const delegatedProps = reactiveOmit(props, 'class');
const forwarded = useForwardPropsEmits(delegatedProps, emits);
</script>

<template>
  <ContextMenuItem
    data-slot="context-menu-item"
    v-bind="forwarded"
    :class="cn('relative flex cursor-default select-none items-center gap-2 rounded-md px-2.5 py-2 text-sm outline-none transition-colors focus:bg-accent focus:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50', props.class)"
  >
    <slot />
  </ContextMenuItem>
</template>
