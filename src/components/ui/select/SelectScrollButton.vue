<script setup lang="ts">
import { ChevronDown, ChevronUp } from '@lucide/vue';
import { SelectScrollDownButton, SelectScrollUpButton } from 'reka-ui';

defineProps<{ direction: 'up' | 'down' }>();
</script>

<template>
  <component
    :is="direction === 'up' ? SelectScrollUpButton : SelectScrollDownButton"
    class="flex cursor-default items-center justify-center py-1"
  >
    <ChevronUp v-if="direction === 'up'" class="size-4" />
    <ChevronDown v-else class="size-4" />
  </component>
</template>
