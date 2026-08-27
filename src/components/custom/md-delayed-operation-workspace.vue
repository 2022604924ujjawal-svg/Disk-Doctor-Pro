<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue';

import { OPERATION_PROGRESS_DELAY_MS } from '@/lib/models/ui';

import MdOperationWorkspace from './md-operation-workspace.vue';

const props = withDefaults(
  defineProps<{
    active: boolean;
    delay?: number;
    mode?: 'workspace' | 'overlay';
  }>(),
  {
    delay: OPERATION_PROGRESS_DELAY_MS,
    mode: 'workspace',
  }
);

const visible = ref(false);
let timer: ReturnType<typeof setTimeout> | null = null;

function clearTimer() {
  if (!timer) return;
  clearTimeout(timer);
  timer = null;
}

watch(
  () => props.active,
  active => {
    clearTimer();
    visible.value = false;
    if (!active) return;

    // Fast cache hits should leave the current workspace untouched. Longer
    // operations still expose progress and cancellation after the delay.
    timer = setTimeout(() => {
      visible.value = true;
      timer = null;
    }, props.delay);
  },
  { immediate: true }
);

onBeforeUnmount(clearTimer);
</script>

<template>
  <MdOperationWorkspace v-if="visible" :mode="mode">
    <slot />
  </MdOperationWorkspace>
</template>
