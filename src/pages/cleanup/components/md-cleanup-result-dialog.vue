<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed } from 'vue';
import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import type { ApplicationLeftoverResult } from '@/lib/models/application';
import { ICON_NAMES } from '@/lib/models/ui';
import type { PresentedCleanupResult } from '@/lib/models/cleanup';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  leftoverResult: ApplicationLeftoverResult | null;
  modelValue: boolean;
  result: PresentedCleanupResult | null;
}>();
const emit = defineEmits<{
  'update:modelValue': [open: boolean];
}>();

const availableResults = computed(() => [props.result, props.leftoverResult].filter(result => result !== null));
const hasResult = computed(() => availableResults.value.length > 0);
const dryRun = computed(() => availableResults.value.every(result => result.dryRun));
const cancelled = computed(
  () =>
    props.result?.record.outcome === 'cancelled' ||
    props.leftoverResult?.actions.some(action => action.status === 'cancelled') === true
);
const expectedBytes = computed(() => availableResults.value.reduce((total, result) => total + result.expectedBytes, 0));
const releasedBytes = computed(() => availableResults.value.reduce((total, result) => total + result.releasedBytes, 0));
const affectedItemCount = computed(() =>
  availableResults.value.reduce((total, result) => total + result.affectedItemCount, 0)
);
const failedItemCount = computed(() =>
  availableResults.value.reduce((total, result) => total + result.failedItemCount, 0)
);
const resultActions = computed(() => {
  const actions = (props.result?.actions ?? []).map(action => ({
    failed: Boolean(action.failedItemCount),
    key: `cleanup:${action.ruleId}`,
    message: action.message,
    name: action.name,
    releasedBytes: action.releasedBytes,
  }));
  const leftoverResult = props.leftoverResult;
  if (!leftoverResult) return actions;

  return [
    ...actions,
    {
      failed: Boolean(leftoverResult.failedItemCount),
      key: 'application-leftovers',
      message: leftoverResult.dryRun
        ? t('cleanup.previewPassed')
        : t('applicationLeftovers.executionSummary', {
            count: FormatUtils.integer(leftoverResult.affectedItemCount),
            size: ByteSizeService.bytes(leftoverResult.releasedBytes),
            failed: FormatUtils.integer(leftoverResult.failedItemCount),
          }),
      name: t('applicationLeftovers.categoryTitle'),
      releasedBytes: leftoverResult.releasedBytes,
    },
  ];
});

function updateOpen(open: boolean) {
  // A cleanup result may not exist immediately after execution starts. Sync
  // parent state only on an actual close; the open expression displays each new
  // result without a page-level watcher.
  if (!open) emit('update:modelValue', false);
}
</script>

<template>
  <Dialog :open="modelValue && hasResult" @update:open="updateOpen">
    <MdDialogContent class="flex max-h-[84vh] min-h-0 flex-col overflow-hidden p-0 sm:max-w-[620px]">
      <template v-if="hasResult">
        <DialogHeader class="flex-none px-5 pt-5 pr-12">
          <DialogTitle class="text-lg">{{
            cancelled ? t('cleanup.cancelled') : dryRun ? t('cleanup.previewCompleted') : t('cleanup.completed')
          }}</DialogTitle>
          <DialogDescription class="text-xs">{{
            cancelled ? t('cleanup.cancelledResultDescription') : t('cleanup.resultDescription')
          }}</DialogDescription>
        </DialogHeader>

        <div class="result-grid flex-none" :class="{ 'has-failures': failedItemCount }">
          <span>
            <small>{{ dryRun ? t('cleanup.estimated') : t('cleanup.actualReleased') }}</small>
            <strong>{{ ByteSizeService.bytes(dryRun ? expectedBytes : releasedBytes) }}</strong>
          </span>
          <span>
            <small>{{ t('cleanup.processedItems') }}</small>
            <strong>{{ FormatUtils.integer(affectedItemCount) }}</strong>
          </span>
          <span v-if="failedItemCount" class="failure-stat">
            <small>{{ t('cleanup.failedItems') }}</small>
            <strong>{{ FormatUtils.integer(failedItemCount) }}</strong>
          </span>
        </div>
        <div class="result-actions scrollbar-stable min-h-0 flex-1">
          <div v-for="action in resultActions" :key="action.key">
            <span :class="{ warn: action.failed }">
              <MdIcon :name="action.failed ? ICON_NAMES.info : ICON_NAMES.check" :size="13" />
            </span>
            <span>
              <strong>{{ action.name }}</strong>
              <small>{{ action.message }}</small>
            </span>
            <strong>{{ ByteSizeService.bytes(action.releasedBytes) }}</strong>
          </div>
        </div>

        <DialogFooter class="flex-none border-t border-border/70 px-5 py-3">
          <Button class="h-8" variant="outline" type="button" @click="emit('update:modelValue', false)">
            {{ t('common.close') }}
          </Button>
        </DialogFooter>
      </template>
    </MdDialogContent>
  </Dialog>
</template>

<style scoped>
@reference "@assets/main.css";

.result-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
  margin: 0 20px;
}

.result-grid.has-failures {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.result-grid > span {
  @apply border border-border/60 bg-muted/30;
  display: flex;
  min-width: 0;
  flex-direction: column;
  border-radius: 9px;
  padding: 10px 11px;
}

.result-grid small,
.result-actions small {
  @apply text-muted-foreground;
}

.result-grid small {
  font-size: 10.5px;
}

.result-grid strong {
  margin-top: 3px;
  font-size: 18px;
  font-variant-numeric: tabular-nums;
}

.result-grid .failure-stat strong {
  @apply text-warning-foreground;
}

.result-actions {
  @apply border border-border/70;
  margin: 10px 20px;
  border-radius: 9px;
}

.result-actions > div {
  @apply border-t border-border/70;
  display: grid;
  grid-template-columns: 22px minmax(0, 1fr) auto;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
}

.result-actions > div:first-child {
  border-top: 0;
}

.result-actions div > span:nth-child(2) {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

.result-actions small {
  margin-top: 1px;
  font-size: 10.5px;
  line-height: 1.35;
}

.result-actions > div > span:nth-child(2) > strong,
.result-actions > div > strong {
  font-size: 13px;
  line-height: 1.35;
}

.result-actions > div > strong {
  font-variant-numeric: tabular-nums;
}

.result-actions > div > span:first-child {
  @apply text-success;
  background: var(--surface-success-subtle);
  display: grid;
  width: 20px;
  height: 20px;
  place-items: center;
  border-radius: 50%;
}

.result-actions > div > span.warn {
  @apply text-warning-foreground;
  background: var(--surface-warning-subtle);
}
</style>
