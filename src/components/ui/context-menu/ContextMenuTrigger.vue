<script setup lang="ts">
import type { ContextMenuTriggerProps } from 'reka-ui';
import { ContextMenuTrigger } from 'reka-ui';

const props = defineProps<ContextMenuTriggerProps>();
</script>

<template>
  <ContextMenuTrigger data-slot="context-menu-trigger" v-bind="props">
    <slot />
  </ContextMenuTrigger>
</template>
