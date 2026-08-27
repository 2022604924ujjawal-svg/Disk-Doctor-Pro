<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed, ref, watch } from 'vue';
import MdApplicationClosePanel from '@/components/custom/md-application-close-panel.vue';
import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import type { PresentedScanRuleResult } from '@/lib/models/cleanup';
import type { CleanupApplicationIcon } from '@/lib/models/cleanup';
import type {
  ApplicationCloseBatchResult,
  ApplicationCloseItem,
  ApplicationCloseMode,
} from '@/lib/models/application-close';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';
import { cleanupApplicationCloseGroups, cleanupApplicationCloseRetry } from '../cleanup-application-close';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  busy: boolean;
  leftoverApplicationCount: number;
  leftoverBytes: number;
  leftoverItemCount: number;
  modelValue: boolean;
  rules: PresentedScanRuleResult[];
  selectedBytes: number;
  selectedItemCount: number;
  closingApplications: boolean;
  closeResult: ApplicationCloseBatchResult | null;
  applicationIcons: CleanupApplicationIcon[];
}>();
const emit = defineEmits<{
  closeApplications: [ruleIds: string[], mode: ApplicationCloseMode];
  execute: [];
  'update:modelValue': [open: boolean];
}>();

const closePhase = ref<'selection' | 'force'>('selection');
const selectedCloseGroupIds = ref<string[]>([]);
const remainingRuleIds = ref<string[]>([]);
const remainingCloseItems = ref<ApplicationCloseItem[]>([]);

const requiresAppClose = computed(() => props.rules.some(rule => rule.requiresAppClose));
const closeGroups = computed(() =>
  cleanupApplicationCloseGroups(
    props.rules.filter(rule => !rule.ruleId.startsWith('special.')),
    props.applicationIcons
  )
);
const selectedCloseGroups = computed(() => {
  const selected = new Set(selectedCloseGroupIds.value);
  return closeGroups.value.filter(group => selected.has(group.id));
});
const closeRuleIds = computed(() => [...new Set(selectedCloseGroups.value.flatMap(group => group.ruleIds))]);
const interactionBusy = computed(() => props.busy || props.closingApplications);
const planItems = computed(() => {
  const items = props.rules.map(rule => ({
    bytes: rule.bytes,
    description: rule.impact,
    key: `rule:${rule.ruleId}`,
    name: rule.name,
  }));

  if (props.leftoverItemCount) {
    const summary = t(
      'applicationLeftovers.planSummary',
      {
        applications: FormatUtils.integer(props.leftoverApplicationCount),
        locations: FormatUtils.integer(props.leftoverItemCount),
      },
      props.leftoverApplicationCount
    );
    items.push({
      bytes: props.leftoverBytes,
      description: `${summary} · ${t('applicationLeftovers.planImpact')}`,
      key: 'application-leftovers',
      name: t('applicationLeftovers.categoryTitle'),
    });
  }

  return items.sort((left, right) => right.bytes - left.bytes);
});

watch(
  () => props.modelValue,
  open => {
    if (!open) return;
    closePhase.value = 'selection';
    selectedCloseGroupIds.value = [];
    remainingRuleIds.value = [];
    remainingCloseItems.value = [];
  }
);

watch(
  () => props.closeResult,
  result => {
    if (!props.modelValue || !result) return;
    const retry = cleanupApplicationCloseRetry(selectedCloseGroups.value, result);
    remainingRuleIds.value = retry.ruleIds;
    remainingCloseItems.value = retry.items;
    if (!remainingRuleIds.value.length) {
      emit('execute');
      return;
    }
    closePhase.value = 'force';
  }
);

function closeApplications(mode: ApplicationCloseMode) {
  const ruleIds = mode === 'force' ? remainingRuleIds.value : closeRuleIds.value;
  if (props.closingApplications || !ruleIds.length) return;
  emit('closeApplications', ruleIds, mode);
}

function executeSelection() {
  if (!selectedCloseGroupIds.value.length) {
    emit('execute');
    return;
  }
  void closeApplications('graceful');
}
</script>

<template>
  <Dialog :open="modelValue" @update:open="emit('update:modelValue', $event)">
    <MdDialogContent
      class="flex max-h-[calc(100dvh-1.5rem)] min-h-0 flex-col gap-0 overflow-hidden p-0 sm:max-h-[86dvh] sm:max-w-[720px]"
    >
      <DialogHeader class="plan-header flex-none px-5 pt-4 pr-12">
        <DialogTitle>{{ t('cleanup.planDialogTitle') }}</DialogTitle>
        <DialogDescription class="plan-summary">
          <span>
            {{ t('cleanup.selectedItemCount', { count: FormatUtils.integer(selectedItemCount) }, selectedItemCount) }}
          </span>
          <span aria-hidden="true">·</span>
          <span>{{ t('cleanup.estimated') }}</span>
          <strong>{{ ByteSizeService.bytes(selectedBytes) }}</strong>
        </DialogDescription>
      </DialogHeader>

      <div class="plan-scroll-region scrollbar-stable">
        <p v-if="requiresAppClose && closePhase === 'selection'" class="process-warning">
          {{ t('cleanup.closeAppsBeforeCleanup') }}
        </p>
        <div v-if="closeGroups.length && closePhase === 'selection'" class="application-close-container">
          <MdApplicationClosePanel
            v-model:selected-ids="selectedCloseGroupIds"
            :items="closeGroups"
            :disabled="interactionBusy"
          />
        </div>
        <div v-else-if="closePhase === 'force'" class="application-close-container">
          <p class="force-close-warning">
            <strong>{{ t('applicationClose.normalCloseFailed') }}</strong>
            <span>{{ t('applicationClose.forceWarning') }}</span>
          </p>
          <MdApplicationClosePanel :items="remainingCloseItems" :selectable="false" />
        </div>
        <div class="modal-rules">
          <div v-for="item in planItems" :key="item.key">
            <span class="plan-item-copy">
              <strong>{{ item.name }}</strong>
              <small :title="item.description">{{ item.description }}</small>
            </span>
            <strong class="plan-item-size">{{ ByteSizeService.bytes(item.bytes) }}</strong>
          </div>
        </div>
      </div>

      <DialogFooter v-if="closePhase === 'selection'" class="flex-none border-t border-border/70 px-5 py-3">
        <Button variant="outline" type="button" :disabled="interactionBusy" @click="emit('update:modelValue', false)">
          {{ t('cleanup.adjustSelection') }}
        </Button>
        <Button type="button" :disabled="interactionBusy" @click="executeSelection">
          {{
            closingApplications
              ? t('applicationClose.closing')
              : selectedCloseGroupIds.length
                ? t(
                    'applicationClose.closeSelectedAndContinue',
                    { count: FormatUtils.integer(selectedCloseGroupIds.length) },
                    selectedCloseGroupIds.length
                  )
                : t('cleanup.execute')
          }}
        </Button>
      </DialogFooter>
      <DialogFooter v-else class="flex-none border-t border-border/70 px-5 py-3">
        <Button type="button" variant="outline" :disabled="interactionBusy" @click="emit('execute')">
          {{ t('applicationClose.skipAndContinue') }}
        </Button>
        <Button type="button" variant="destructive" :disabled="interactionBusy" @click="closeApplications('force')">
          {{ closingApplications ? t('applicationClose.closing') : t('applicationClose.forceAndContinue') }}
        </Button>
      </DialogFooter>
    </MdDialogContent>
  </Dialog>
</template>

<style scoped>
@reference "@assets/main.css";

.plan-header {
  gap: 4px;
}

.plan-summary {
  display: flex;
  align-items: baseline;
  gap: 6px;
  font-size: var(--font-content-secondary);
}

.plan-summary strong {
  @apply text-primary;
  font-size: 17px;
  font-weight: 600;
}

.plan-scroll-region {
  min-height: 0;
  flex: 1 1 auto;
  overflow-y: auto;
  overscroll-behavior: contain;
}

.modal-rules {
  @apply border border-border/70;
  margin: 7px 20px 9px;
  border-radius: 9px;
}

.process-warning {
  margin: 7px 20px 0;
  border-radius: 7px;
  padding: 5px 9px;
  @apply text-warning-foreground;
  background: var(--surface-warning-subtle);
  font-size: var(--font-content-secondary);
}

.application-close-container {
  margin: 7px 20px 0;
}

.force-close-warning {
  display: flex;
  flex-direction: column;
  gap: 3px;
  margin: 0 0 8px;
  border-radius: 8px;
  padding: 8px 10px;
  @apply text-destructive;
  background: var(--surface-destructive-subtle);
  font-size: var(--font-content-secondary);
}

.modal-rules > div {
  @apply border-t border-border/70;
  display: grid;
  min-height: 52px;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: 16px;
  padding: 7px 12px;
}

.modal-rules > div:first-child {
  border-top: 0;
}

.plan-item-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 2px;
}

.plan-item-copy > strong,
.plan-item-size {
  font-size: 13px;
  font-weight: 500;
  line-height: 1.35;
}

.plan-item-copy small {
  @apply text-muted-foreground;
  overflow: hidden;
  font-size: 10.5px;
  line-height: 1.45;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.plan-item-size {
  white-space: nowrap;
}
</style>
