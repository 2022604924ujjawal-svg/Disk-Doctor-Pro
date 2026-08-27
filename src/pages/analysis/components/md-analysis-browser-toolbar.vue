<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { onMounted, ref, watch } from 'vue';

import MdIconAction from '@/components/custom/md-icon-action.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';
import type { AnalysisBreadcrumb } from '@/lib/utils/analysis-breadcrumb';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  breadcrumbs: AnalysisBreadcrumb[];
  busy: boolean;
  preserveBusyAppearance: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  homeDisabled: boolean;
}>();

const emit = defineEmits<{
  back: [];
  forward: [];
  home: [];
  navigate: [path: string];
}>();

const breadcrumbsElement = ref<HTMLElement | null>(null);

function scrollBreadcrumbsToEnd() {
  const element = breadcrumbsElement.value;
  if (!element) return;
  element.scrollLeft = element.scrollWidth;
}

watch(
  () => props.breadcrumbs,
  // A post-flush watcher already runs after Vue updates the breadcrumb DOM.
  // Scrolling here avoids painting the old position for one frame before a
  // second `nextTick` moves the current folder into view.
  scrollBreadcrumbsToEnd,
  { flush: 'post' }
);

onMounted(scrollBreadcrumbsToEnd);
</script>

<template>
  <div class="browser-toolbar md-workspace-toolbar">
    <div class="history-actions">
      <MdIconAction
        appearance="unstyled"
        class="history-action"
        :label="t('analysis.back')"
        :disabled="busy || !canGoBack"
        :data-busy-disabled="preserveBusyAppearance && busy && canGoBack"
        @click="emit('back')"
      >
        <MdIcon :name="ICON_NAMES.chevronLeft" :size="16" />
      </MdIconAction>
      <MdIconAction
        appearance="unstyled"
        class="history-action"
        :label="t('analysis.forward')"
        :disabled="busy || !canGoForward"
        :data-busy-disabled="preserveBusyAppearance && busy && canGoForward"
        @click="emit('forward')"
      >
        <MdIcon :name="ICON_NAMES.chevronRight" :size="16" />
      </MdIconAction>
      <MdIconAction
        appearance="unstyled"
        class="history-action"
        :label="t('analysis.home')"
        :disabled="busy || homeDisabled"
        :data-busy-disabled="preserveBusyAppearance && busy && !homeDisabled"
        @click="emit('home')"
      >
        <MdIcon :name="ICON_NAMES.home" :size="16" />
      </MdIconAction>
    </div>
    <nav ref="breadcrumbsElement" class="breadcrumbs scrollbar-hidden" :aria-label="t('analysis.pathLabel')">
      <template v-for="(segment, index) in breadcrumbs" :key="`${segment.path}-${index}`">
        <button
          type="button"
          :disabled="busy || !segment.path || index === breadcrumbs.length - 1"
          :aria-current="index === breadcrumbs.length - 1 ? 'page' : undefined"
          :title="segment.path"
          @click="emit('navigate', segment.path)"
        >
          {{ segment.label }}
        </button>
        <MdIcon v-if="index < breadcrumbs.length - 1" :name="ICON_NAMES.chevronRight" :size="14" />
      </template>
    </nav>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.browser-toolbar {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  align-items: center;
  gap: 10px;
  border-bottom-width: 1px;
  padding: 3px 10px;
  @apply border-border;
}

.history-actions {
  display: flex;
  gap: 5px;
}

.history-actions :deep(.history-action) {
  display: grid;
  width: 28px;
  height: 28px;
  place-items: center;
  border-width: 1px;
  border-radius: 7px;
  @apply border-border bg-card text-muted-foreground transition-colors duration-200;
  cursor: pointer;
}

.history-actions :deep(.history-action:hover:not([aria-disabled='true'])) {
  @apply border-primary/40 bg-accent/65 text-accent-foreground;
}

.history-actions :deep(.history-action:focus-visible) {
  @apply border-ring outline-none ring-2 ring-ring/35;
}

.history-actions :deep(.history-action[aria-disabled='true']) {
  cursor: not-allowed;
  opacity: 0.45;
}

/*
 * Preserve the pre-navigation visual state while the shared action blocks
 * repeated requests. Controls unavailable before navigation remain dimmed
 * because they do not receive this data attribute.
 */
.history-actions :deep(.history-action[data-busy-disabled='true']) {
  opacity: 1;
}

.breadcrumbs {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 4px;
  overflow-x: auto;
  font-size: var(--font-content-body);
}

.breadcrumbs > svg {
  flex: none;
  @apply text-muted-foreground;
}

.breadcrumbs button {
  min-width: 0;
  max-width: 280px;
  flex: none;
  overflow: hidden;
  border: 0;
  border-radius: 6px;
  padding: 4px 6px;
  background: transparent;
  @apply text-muted-foreground transition-colors duration-200;
  font: inherit;
  text-overflow: ellipsis;
  white-space: nowrap;
  cursor: pointer;
}

.breadcrumbs button:hover:not(:disabled) {
  @apply bg-accent/65 text-accent-foreground;
}

.breadcrumbs button:focus-visible {
  @apply outline-none ring-2 ring-ring/35;
}

.breadcrumbs button:disabled {
  opacity: 1;
}

.breadcrumbs button[aria-current='page'] {
  @apply text-card-foreground;
}
</style>
