<script setup lang="ts">
import { computed } from 'vue';

const props = withDefaults(
  defineProps<{
    text: string;
    tailLength?: number;
  }>(),
  {
    tailLength: 12,
  }
);

const parts = computed(() => {
  // Preserve a Unicode-safe suffix and let CSS truncate the prefix, producing
  // stable middle ellipsis without measuring the DOM.
  const characters = Array.from(props.text);
  const requestedTailLength = Math.max(0, props.tailLength);
  if (requestedTailLength === 0 || characters.length <= requestedTailLength * 2) {
    return { start: props.text, end: '' };
  }
  const tailLength = Math.min(requestedTailLength, characters.length - 1);
  return {
    start: characters.slice(0, -tailLength).join(''),
    end: characters.slice(-tailLength).join(''),
  };
});
</script>

<template>
  <span class="md-middle-ellipsis" :title="text">
    <span class="ellipsis-start">{{ parts.start }}</span>
    <span v-if="parts.end" class="ellipsis-end">{{ parts.end }}</span>
  </span>
</template>

<style scoped>
.md-middle-ellipsis {
  display: flex;
  min-width: 0;
  max-width: 100%;
  align-items: baseline;
  white-space: nowrap;
}

.ellipsis-start {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

.ellipsis-end {
  flex: none;
}
</style>
