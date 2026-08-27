<script setup lang="ts">
import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { ICON_NAMES } from '@/lib/models/ui';

const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    description: string;
    summaryLabel?: string;
    summaryValue?: string;
    note?: string;
    cancelLabel: string;
    confirmLabel: string;
    busy?: boolean;
    loading?: boolean;
    loadingLabel?: string;
    showDetails?: boolean;
  }>(),
  {
    summaryLabel: '',
    summaryValue: '',
    note: '',
    busy: false,
    loading: false,
    loadingLabel: '',
    showDetails: false,
  }
);

const emit = defineEmits<{
  'update:open': [value: boolean];
  confirm: [];
}>();

function updateOpen(open: boolean) {
  // A destructive command continues in Core after confirmation. Keep its
  // activity dialog visible until the owning Store settles so Escape, an
  // outside click, or the close control cannot hide the only progress signal.
  if (!open && props.busy) return;
  emit('update:open', open);
}
</script>

<template>
  <Dialog :open="open" @update:open="updateOpen">
    <!-- Shared hierarchy keeps risk, target summary, and actions consistent. -->
    <MdDialogContent
      class="flex max-h-[calc(100vh-3rem)] min-h-0 w-[calc(100%-3rem)] flex-col gap-0 overflow-hidden p-0"
      :class="showDetails ? 'max-w-[620px]' : 'max-w-[480px]'"
      :show-close="!busy"
    >
      <DialogHeader class="destructive-dialog-header flex-none">
        <div class="destructive-dialog-icon" aria-hidden="true">
          <MdIcon :name="ICON_NAMES.trash" :size="20" />
        </div>
        <div class="destructive-dialog-copy">
          <DialogTitle>{{ title }}</DialogTitle>
          <DialogDescription>{{ description }}</DialogDescription>
        </div>
      </DialogHeader>

      <div v-if="summaryLabel || summaryValue" class="destructive-dialog-summary" aria-live="polite">
        <strong :title="summaryLabel">
          <span v-if="loading || busy" class="destructive-dialog-spinner" aria-hidden="true" />
          <span class="destructive-dialog-summary-label">
            {{ loading && loadingLabel ? loadingLabel : summaryLabel }}
          </span>
        </strong>
        <span>{{ summaryValue }}</span>
      </div>
      <p v-if="note" class="destructive-dialog-note">
        <MdIcon :name="ICON_NAMES.info" :size="15" />
        <span>{{ note }}</span>
      </p>
      <div v-if="showDetails && $slots.default" class="destructive-dialog-content">
        <slot />
      </div>

      <DialogFooter v-if="!showDetails" class="destructive-dialog-footer flex-none">
        <Button variant="outline" type="button" :disabled="busy" @click="updateOpen(false)">
          {{ cancelLabel }}
        </Button>
        <Button variant="destructive" type="button" :disabled="busy || loading" @click="emit('confirm')">
          <span v-if="busy" class="destructive-dialog-spinner" aria-hidden="true" />
          {{ confirmLabel }}
        </Button>
      </DialogFooter>
    </MdDialogContent>
  </Dialog>
</template>

<style scoped>
@reference "@assets/main.css";

.destructive-dialog-header {
  display: grid;
  grid-template-columns: 44px minmax(0, 1fr);
  gap: 14px;
  padding: 22px 54px 16px 22px;
}

.destructive-dialog-icon {
  display: grid;
  width: 44px;
  height: 44px;
  place-items: center;
  border-radius: 12px;
  @apply text-destructive;
  background: var(--surface-destructive-subtle);
}

.destructive-dialog-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 7px;
  padding-top: 2px;
}

.destructive-dialog-copy :deep([data-slot='dialog-title']) {
  line-height: 1.35;
}

.destructive-dialog-copy :deep([data-slot='dialog-description']) {
  line-height: 1.6;
}

.destructive-dialog-summary {
  display: flex;
  min-width: 0;
  min-height: 58px;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  margin: 0 22px 16px;
  border-width: 1px;
  border-radius: 12px;
  padding: 11px 14px;
  @apply border-border bg-muted/60;
}

.destructive-dialog-summary strong {
  display: flex;
  min-width: 0;
  flex: 1;
  align-items: center;
  gap: 9px;
}

.destructive-dialog-summary-label {
  min-width: 0;
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.destructive-dialog-spinner {
  width: 15px;
  height: 15px;
  flex: none;
  border: 2px solid currentColor;
  border-right-color: transparent;
  border-radius: 999px;
  animation: destructive-dialog-spin 0.75s linear infinite;
}

@keyframes destructive-dialog-spin {
  to {
    transform: rotate(360deg);
  }
}

.destructive-dialog-summary > span {
  flex: none;
  @apply text-muted-foreground;
}

.destructive-dialog-note {
  display: flex;
  align-items: flex-start;
  gap: 7px;
  margin: 0 22px 17px;
  @apply text-muted-foreground;
  font-size: 11px;
  line-height: 1.5;
}

.destructive-dialog-note :deep(svg) {
  flex: none;
  margin-top: 1px;
  @apply text-primary;
}

.destructive-dialog-content {
  min-height: 0;
  margin: 0 22px 16px;
  overflow: hidden;
}

.destructive-dialog-footer {
  min-height: 64px;
  align-items: center;
  border-top-width: 1px;
  padding: 11px 22px;
  @apply border-border bg-muted/25;
}

.destructive-dialog-footer :deep(button) {
  min-width: 104px;
}
</style>
