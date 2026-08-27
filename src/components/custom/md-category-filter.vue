<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';

const props = defineProps<{
  ariaLabel: string;
  disabled?: boolean;
  modelValue: string;
  options: Array<{ value: string; label: string; count?: number }>;
}>();

const emit = defineEmits<{
  'update:modelValue': [value: string];
}>();

const filter = ref<HTMLElement | null>(null);
const canScrollStart = ref(false);
const canScrollEnd = ref(false);
let resizeObserver: ResizeObserver | undefined;
let overflowFrame: number | undefined;

function updateOverflowState() {
  const element = filter.value;
  if (!element) return;
  canScrollStart.value = element.scrollLeft > 1;
  canScrollEnd.value = element.scrollLeft + element.clientWidth < element.scrollWidth - 1;
}

function scheduleOverflowUpdate() {
  if (overflowFrame !== undefined) window.cancelAnimationFrame(overflowFrame);
  overflowFrame = window.requestAnimationFrame(() => {
    overflowFrame = undefined;
    updateOverflowState();
  });
}

function revealActiveOption(behavior: 'auto' | 'smooth') {
  void nextTick(() => {
    const activeOption = filter.value?.querySelector<HTMLElement>('[data-active="true"]');
    activeOption?.scrollIntoView({ behavior, block: 'nearest', inline: 'nearest' });
    scheduleOverflowUpdate();
  });
}

function selectOption(value: string) {
  emit('update:modelValue', value);
}

watch([() => props.modelValue, () => props.options], () => revealActiveOption('smooth'), { flush: 'post' });

onMounted(() => {
  const element = filter.value;
  if (!element) return;
  // Sidebar animation changes the available toolbar width over several frames.
  // Keeping the selected option visible prevents the search field from seeming
  // to cover a category when the navigation expands.
  resizeObserver = new ResizeObserver(() => revealActiveOption('auto'));
  resizeObserver.observe(element);
  revealActiveOption('auto');
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  if (overflowFrame !== undefined) window.cancelAnimationFrame(overflowFrame);
});
</script>

<template>
  <nav
    ref="filter"
    class="category-filter scrollbar-hidden flex min-w-0 items-center gap-1 overflow-x-auto p-0.5"
    :class="{
      'category-filter--overflow-start': canScrollStart,
      'category-filter--overflow-end': canScrollEnd,
    }"
    :aria-label="ariaLabel"
    @scroll.passive="scheduleOverflowUpdate"
  >
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      class="inline-flex h-7.5 flex-none cursor-pointer items-center gap-1.5 rounded-md border border-transparent px-2.5 text-content-body text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/35 disabled:cursor-default disabled:hover:bg-transparent disabled:hover:text-muted-foreground"
      :class="{
        'border-primary/20 bg-primary/10 font-semibold text-primary': modelValue === option.value,
      }"
      :data-active="modelValue === option.value"
      :disabled="disabled"
      @click="selectOption(option.value)"
    >
      <span>{{ option.label }}</span>
      <small
        v-if="option.count !== undefined"
        class="min-w-4 px-0.5 py-0.5 text-center text-content-meta text-muted-foreground"
        :class="{ 'text-primary': modelValue === option.value }"
      >
        {{ option.count }}
      </small>
    </button>
  </nav>
</template>

<style scoped>
.category-filter {
  scroll-padding-inline: 20px;
  overscroll-behavior-inline: contain;
}

/* Edge fades communicate hidden categories without consuming toolbar space. */
.category-filter--overflow-start:not(.category-filter--overflow-end) {
  -webkit-mask-image: linear-gradient(to right, transparent, var(--foreground) 18px, var(--foreground) 100%);
  mask-image: linear-gradient(to right, transparent, var(--foreground) 18px, var(--foreground) 100%);
}

.category-filter--overflow-end:not(.category-filter--overflow-start) {
  -webkit-mask-image: linear-gradient(to right, var(--foreground) 0, var(--foreground) calc(100% - 18px), transparent);
  mask-image: linear-gradient(to right, var(--foreground) 0, var(--foreground) calc(100% - 18px), transparent);
}

.category-filter--overflow-start.category-filter--overflow-end {
  -webkit-mask-image: linear-gradient(
    to right,
    transparent,
    var(--foreground) 18px,
    var(--foreground) calc(100% - 18px),
    transparent
  );
  mask-image: linear-gradient(
    to right,
    transparent,
    var(--foreground) 18px,
    var(--foreground) calc(100% - 18px),
    transparent
  );
}
</style>
