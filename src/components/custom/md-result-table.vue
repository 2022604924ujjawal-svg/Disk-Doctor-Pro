<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';

interface ResultTableScrollOptions {
  top?: number;
  left?: number;
  behavior?: 'auto' | 'smooth';
}

const scrollElement = ref<HTMLElement | null>(null);
const scrollGutter = ref(0);
let resizeObserver: ResizeObserver | null = null;
let mutationObserver: MutationObserver | null = null;

function syncScrollGutter() {
  const element = scrollElement.value;
  if (!element) return;

  // WebView engines disagree on whether a CSS scrollbar gutter contributes to
  // offsetWidth - clientWidth. Measure the rendered content inset directly so
  // the fixed header follows the same left gutter as rows on every platform.
  const firstRow = element.firstElementChild;
  scrollGutter.value = firstRow
    ? Math.max(0, firstRow.getBoundingClientRect().left - element.getBoundingClientRect().left)
    : 0;
}

function scrollTo(options: ResultTableScrollOptions) {
  scrollElement.value?.scrollTo(options);
}

onMounted(() => {
  syncScrollGutter();
  resizeObserver = new ResizeObserver(syncScrollGutter);
  if (scrollElement.value) resizeObserver.observe(scrollElement.value);
  mutationObserver = new MutationObserver(syncScrollGutter);
  if (scrollElement.value) mutationObserver.observe(scrollElement.value, { childList: true });
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  mutationObserver?.disconnect();
});

defineExpose({
  scrollTo,
});
</script>

<template>
  <div class="result-table" :style="{ '--result-table-scroll-gutter': `${scrollGutter}px` }">
    <header v-if="$slots.header" class="result-table-header md-result-header">
      <slot name="header" />
    </header>
    <div ref="scrollElement" class="result-table-scroll scrollbar-stable">
      <slot />
    </div>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.result-table {
  --result-table-content-inline-padding: 12px;
  --result-table-hierarchy-indent: 32px;

  display: flex;
  min-width: 0;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  overflow: hidden;
}

.result-table-header {
  min-width: 0;
  flex: none;
  border-bottom-width: 1px;
  padding-inline: calc(var(--result-table-scroll-gutter) + var(--result-table-content-inline-padding));
}

.result-table-scroll {
  min-width: 0;
  min-height: 0;
  flex: 1;
  overflow-x: hidden;
  overscroll-behavior: contain;
}
</style>
