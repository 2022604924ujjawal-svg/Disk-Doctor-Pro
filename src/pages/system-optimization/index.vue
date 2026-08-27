<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { toast } from 'vue-sonner';

import MdActionBarContainer from '@/components/custom/md-action-bar-container.vue';
import MdCategoryFilter from '@/components/custom/md-category-filter.vue';
import MdEmptyState from '@/components/custom/md-empty-state.vue';
import MdOperationProgress from '@/components/custom/md-operation-progress.vue';
import MdOperationWorkspace from '@/components/custom/md-operation-workspace.vue';
import MdPageShell from '@/components/custom/md-page-shell.vue';
import MdResultFilterToolbar from '@/components/custom/md-result-filter-toolbar.vue';
import MdResultWorkspace from '@/components/custom/md-result-workspace.vue';
import MdSwitch from '@/components/custom/md-switch.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import type { SystemSettingCategory, SystemSettingItem } from '@/lib/models/system-settings';
import { ICON_NAMES } from '@/lib/models/ui';
import {
  isSystemOptimizationPresetMode,
  systemOptimizationHighRiskEnables,
  systemOptimizationModesForPlatform,
  systemOptimizationPendingChanges,
  type SystemOptimizationMode,
} from '@/lib/utils/system-settings-mode';
import { useSystemSettingsStore } from '@/stores/system-settings-store';

import MdSystemSettingRiskDialog from './components/md-system-setting-risk-dialog.vue';

const { t } = useI18n({ useScope: 'global' });
const store = useSystemSettingsStore();
const executionRequested = ref(false);
type OptimizationCategoryFilter = 'pending' | 'all' | SystemSettingCategory;

const activeCategory = ref<OptimizationCategoryFilter>('all');
const optimizationScroll = ref<HTMLElement | null>(null);
const riskDialogOpen = ref(false);
const draftNoticeShown = ref(false);

const categories: SystemSettingCategory[] = [
  'performance',
  'productivity',
  'privacy',
  'storage',
  'gaming',
  'appearance',
];
const busy = computed(() => store.scanning || store.preparing || store.executing);
const desiredOptimized = computed(() => new Set(store.desiredOptimizedIds));
const availableItems = computed(() => store.catalog?.items.filter(item => item.status !== 'unavailable') ?? []);
const pendingChanges = computed(() =>
  store.catalog ? systemOptimizationPendingChanges(store.catalog, store.desiredOptimizedIds) : []
);
const pendingById = computed(() => new Map(pendingChanges.value.map(item => [item.settingId, item.target])));
const changeCount = computed(() => pendingChanges.value.length);
const visibleItems = computed(() =>
  availableItems.value.filter(item => {
    if (activeCategory.value === 'pending' && !pendingById.value.has(item.settingId)) return false;
    if (
      activeCategory.value !== 'pending' &&
      activeCategory.value !== 'all' &&
      item.category !== activeCategory.value
    ) {
      return false;
    }
    return true;
  })
);
const categoryOptions = computed(() => [
  {
    value: 'pending',
    label: t('systemOptimization.categories.pending'),
    count: changeCount.value,
  },
  {
    value: 'all',
    label: t('systemOptimization.categories.all'),
    count: availableItems.value.length,
  },
  ...categories
    .map(category => ({
      value: category,
      label: t(`systemOptimization.categories.${category}`),
      count: availableItems.value.filter(item => item.category === category).length,
    }))
    .filter(option => option.count > 0),
]);
const itemById = computed(() => new Map(availableItems.value.map(item => [item.settingId, item])));
const pendingRequiresElevation = computed(() =>
  pendingChanges.value.some(change => itemById.value.get(change.settingId)?.requiresElevation === true)
);
const highRiskPendingItems = computed(() =>
  store.catalog ? systemOptimizationHighRiskEnables(store.catalog, pendingChanges.value) : []
);
const highRiskPendingNames = computed(() => highRiskPendingItems.value.map(item => itemMessage(item, 'name')));
const modeLabel = computed(() => modeName(store.optimizationMode));
const availableModes = computed(() =>
  store.catalog ? systemOptimizationModesForPlatform(store.catalog.platform) : []
);
watch(
  () => store.pendingPlan,
  plan => {
    if (!plan || !executionRequested.value) return;
    executionRequested.value = false;
    if (!plan.items.length) {
      store.clearPlan();
      toast.info(t('systemOptimization.feedback.nothingToApply'));
      return;
    }
    void store.execute();
  }
);

watch(
  () => store.lastResult,
  result => {
    if (!result) return;
    if (result.items.some(item => item.failureReason === 'userCancelled')) {
      toast.info(
        t(
          result.requiresRestart
            ? 'systemOptimization.feedback.cancelledRestart'
            : 'systemOptimization.feedback.cancelled',
          {
            changed: result.changedCount,
          }
        )
      );
    } else if (result.failedCount) {
      toast.warning(
        t(
          result.requiresRestart ? 'systemOptimization.feedback.partialRestart' : 'systemOptimization.feedback.partial',
          {
            changed: result.changedCount,
            failed: result.failedCount,
          }
        )
      );
    } else {
      toast.success(
        t(
          result.requiresRestart
            ? 'systemOptimization.feedback.completedRestart'
            : 'systemOptimization.feedback.completed',
          {
            count: result.changedCount,
          }
        )
      );
    }
  }
);

function riskDescription(item: SystemSettingItem): string {
  return item.riskLevel === 'high'
    ? t('systemOptimization.statuses.riskDescriptions.high')
    : t('systemOptimization.statuses.riskDescriptions.caution');
}

function itemMessage(item: SystemSettingItem, field: 'description' | 'name'): string {
  return t(`systemOptimization.items.${item.settingId.replaceAll('.', '_')}.${field}`);
}

function toggleItem(item: SystemSettingItem, optimized: boolean) {
  store.setDesiredOptimized(item.settingId, optimized);
  if (!draftNoticeShown.value) {
    draftNoticeShown.value = true;
    toast.info(t('systemOptimization.feedback.draftNotice'));
  }
}

function desiredState(item: SystemSettingItem): boolean {
  return desiredOptimized.value.has(item.settingId);
}

function pendingTarget(item: SystemSettingItem) {
  return pendingById.value.get(item.settingId);
}

function updateMode(value: unknown) {
  if (isSystemOptimizationPresetMode(value)) store.applyMode(value);
}

function updateCategory(value: string) {
  if (value !== 'pending' && value !== 'all' && !categories.includes(value as SystemSettingCategory)) return;
  activeCategory.value = value as OptimizationCategoryFilter;
  optimizationScroll.value?.scrollTo({ top: 0 });
}

function modeName(mode: SystemOptimizationMode): string {
  if (mode === 'performance') return t('systemOptimization.modes.performance.name');
  if (mode === 'privacy') return t('systemOptimization.modes.privacy.name');
  if (mode === 'manual') return t('systemOptimization.modes.manual.name');
  return t('systemOptimization.modes.smart.name');
}

function prepareOptimization() {
  if (!store.catalog) {
    void store.scan();
    return;
  }
  if (!changeCount.value) {
    toast.info(t('systemOptimization.feedback.alreadyOptimized'));
    return;
  }
  executionRequested.value = true;
  void store.prepare();
}

function runOptimization() {
  if (highRiskPendingItems.value.length) {
    riskDialogOpen.value = true;
    return;
  }
  prepareOptimization();
}

function confirmHighRiskChanges() {
  riskDialogOpen.value = false;
  prepareOptimization();
}

onMounted(() => {
  if (!store.catalog) void store.scan();
});
</script>

<template>
  <MdPageShell class="optimization-page" content-mode="workspace" :title="t('systemOptimization.title')">
    <template #actions>
      <Button variant="outline" :disabled="busy" @click="store.scan()">
        <MdIcon :name="ICON_NAMES.refresh" :size="17" />
        {{ t('systemOptimization.rescan') }}
      </Button>
    </template>

    <template v-if="store.catalog" #footer>
      <MdActionBarContainer class="optimization-action-bar">
        <button type="button" class="change-summary" :disabled="busy" @click="updateCategory('pending')">
          <small>{{ t('systemOptimization.pendingSummary') }}</small>
          <strong>{{ t('common.itemCount', { count: changeCount }, changeCount) }}</strong>
        </button>
        <div class="optimization-actions">
          <div class="mode-control">
            <span>{{ t('systemOptimization.modes.label') }}</span>
            <Select :model-value="store.optimizationMode" :disabled="busy" @update:model-value="updateMode">
              <SelectTrigger class="mode-select" :aria-label="t('systemOptimization.modes.label')">
                <SelectValue>{{ modeLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem v-for="mode in availableModes" :key="mode" :value="mode">
                  {{ modeName(mode) }}
                </SelectItem>
                <SelectItem v-if="store.optimizationMode === 'manual'" value="manual" disabled>
                  {{ t('systemOptimization.modes.manual.name') }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
          <Tooltip :disabled="!pendingRequiresElevation">
            <TooltipTrigger as-child>
              <Button class="optimize-button" :disabled="busy || !changeCount" @click="runOptimization">
                <MdIcon :name="pendingRequiresElevation ? ICON_NAMES.shield : ICON_NAMES.sparkles" :size="18" />
                {{
                  store.executing
                    ? t('systemOptimization.optimizing')
                    : store.preparing
                      ? t('systemOptimization.checking')
                      : changeCount
                        ? t('systemOptimization.applyChanges', { count: changeCount })
                        : t('systemOptimization.noChanges')
                }}
              </Button>
            </TooltipTrigger>
            <TooltipContent side="top" :side-offset="6">
              {{ t('systemOptimization.statuses.authorizationRequired') }}
            </TooltipContent>
          </Tooltip>
        </div>
      </MdActionBarContainer>
    </template>

    <MdResultWorkspace v-if="store.catalog" class="optimization-workspace">
      <template #header>
        <MdResultFilterToolbar>
          <MdCategoryFilter
            :model-value="activeCategory"
            :options="categoryOptions"
            :disabled="busy"
            :aria-label="t('systemOptimization.filterCategory')"
            @update:model-value="updateCategory"
          />
        </MdResultFilterToolbar>
      </template>

      <div ref="optimizationScroll" class="optimization-scroll scrollbar-stable-end">
        <MdEmptyState
          v-if="!visibleItems.length"
          :icon-name="ICON_NAMES.systemOptimization"
          :title="t('systemOptimization.pendingEmpty.title')"
          :description="t('systemOptimization.pendingEmpty.description')"
          compact
        />
        <section v-else class="optimization-list">
          <div v-for="item in visibleItems" :key="item.settingId" class="optimization-item">
            <span class="item-copy">
              <strong>{{ itemMessage(item, 'name') }}</strong>
              <small>{{ itemMessage(item, 'description') }}</small>
            </span>
            <span class="item-meta">
              <Tooltip v-if="item.riskLevel !== 'standard'">
                <TooltipTrigger as-child>
                  <span class="item-state is-caution" :class="{ 'is-high-risk': item.riskLevel === 'high' }">
                    {{
                      t(
                        item.riskLevel === 'high'
                          ? 'systemOptimization.statuses.highImpact'
                          : 'systemOptimization.statuses.caution'
                      )
                    }}
                  </span>
                </TooltipTrigger>
                <TooltipContent side="top" :side-offset="6">
                  {{ riskDescription(item) }}
                </TooltipContent>
              </Tooltip>
              <span v-if="pendingTarget(item)" class="item-state is-pending">
                {{
                  t(
                    pendingTarget(item) === 'optimized'
                      ? 'systemOptimization.statuses.pendingEnable'
                      : 'systemOptimization.statuses.pendingDisable'
                  )
                }}
              </span>
              <Tooltip v-if="pendingTarget(item) && item.requiresRestart">
                <TooltipTrigger as-child>
                  <span class="item-restart" :aria-label="t('systemOptimization.statuses.requiresRestart')">
                    <MdIcon :name="ICON_NAMES.refresh" :size="14" />
                  </span>
                </TooltipTrigger>
                <TooltipContent side="top" :side-offset="6">
                  {{ t('systemOptimization.statuses.requiresRestart') }}
                </TooltipContent>
              </Tooltip>
              <Tooltip v-if="item.requiresElevation">
                <TooltipTrigger as-child>
                  <span class="item-admin" :aria-label="t('systemOptimization.statuses.requiresElevation')">
                    <MdIcon :name="ICON_NAMES.shield" :size="14" />
                  </span>
                </TooltipTrigger>
                <TooltipContent side="top" :side-offset="6">
                  {{ t('systemOptimization.statuses.requiresElevation') }}
                </TooltipContent>
              </Tooltip>
            </span>
            <MdSwitch
              :model-value="desiredState(item)"
              :disabled="busy"
              :aria-label="itemMessage(item, 'name')"
              @update:model-value="toggleItem(item, $event)"
            />
          </div>
        </section>
      </div>
    </MdResultWorkspace>

    <MdOperationWorkspace v-else>
      <MdOperationProgress
        :icon-name="ICON_NAMES.systemOptimization"
        :title="t('systemOptimization.scanning')"
        :progress="null"
        :path-label="t('systemOptimization.scanning')"
        :preparing-text="t('systemOptimization.scanningDescription')"
        :hint="t('systemOptimization.scanningDescription')"
        :show-traversal-details="false"
        :show-step-progress="false"
        :cancelable="false"
        :cancel-disabled="true"
      />
    </MdOperationWorkspace>

    <MdSystemSettingRiskDialog
      v-model:open="riskDialogOpen"
      :item-names="highRiskPendingNames"
      @confirm="confirmHighRiskChanges"
    />
  </MdPageShell>
</template>

<style scoped>
@reference "@assets/main.css";
.optimization-page :deep(.md-page-content) {
  gap: 0;
}
.optimization-workspace {
  background: var(--card);
}
.optimization-action-bar {
  gap: 12px;
  padding: 2px 10px 2px 14px;
}
.change-summary {
  display: flex;
  flex: none;
  cursor: pointer;
  align-items: baseline;
  gap: 8px;
  border-radius: var(--radius-sm);
  padding: 6px 8px;
  text-align: left;
}
.change-summary small {
  color: var(--muted-foreground);
  font-size: var(--font-content-meta);
}
.change-summary strong {
  color: var(--foreground);
  font-size: var(--font-content-primary);
  white-space: nowrap;
}
.change-summary:hover:not(:disabled) strong,
.change-summary:focus-visible strong {
  color: var(--primary);
}
.change-summary:focus-visible {
  outline: 2px solid color-mix(in oklab, var(--ring) 35%, transparent);
  outline-offset: 2px;
}
.change-summary:disabled {
  cursor: default;
}
.optimization-actions {
  display: flex;
  min-width: 0;
  margin-left: auto;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
}
.mode-control {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 8px;
}
.mode-control > span {
  color: var(--muted-foreground);
  font-size: 11px;
  font-weight: 500;
  white-space: nowrap;
}
.mode-select {
  width: 202px;
  min-width: 202px;
  height: 38px;
}
.mode-select :deep([data-slot='select-value']) {
  min-width: 0;
  flex: 1;
  overflow: hidden;
  text-align: left;
  text-overflow: ellipsis;
}
.optimize-button {
  min-width: 154px;
  white-space: nowrap;
}
.optimization-scroll {
  display: flex;
  min-width: 0;
  min-height: 0;
  flex: 1;
  flex-direction: column;
  overscroll-behavior: contain;
}
.optimization-item {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto 34px;
  align-items: center;
  gap: 10px;
  min-height: 62px;
  border-bottom: 1px solid color-mix(in oklab, var(--border) 70%, transparent);
  padding: 10px 15px;
}
.optimization-item:last-child {
  border-bottom: 0;
}
.optimization-item:hover {
  background: color-mix(in oklab, var(--muted) 28%, transparent);
}
.item-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 3px;
}
.item-copy strong {
  font-size: var(--font-content-primary);
  font-weight: 600;
}
.item-copy small {
  overflow: hidden;
  color: var(--muted-foreground);
  font-size: var(--font-content-secondary);
  text-overflow: ellipsis;
  white-space: nowrap;
}
.item-meta {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 5px;
}
.item-state {
  border-radius: 999px;
  padding: 3px 7px;
  background: var(--muted);
  color: var(--muted-foreground);
  font-size: 9px;
  white-space: nowrap;
}
.item-state.is-pending {
  background: color-mix(in oklab, var(--primary) 10%, transparent);
  color: var(--primary);
}
.item-state.is-caution {
  color: var(--warning-foreground);
  background: color-mix(in oklab, var(--warning) 12%, transparent);
}
.item-state.is-high-risk {
  color: var(--destructive);
  background: color-mix(in oklab, var(--destructive) 9%, transparent);
}
.item-admin,
.item-restart {
  display: grid;
  width: 24px;
  height: 24px;
  place-items: center;
  color: var(--muted-foreground);
}
@container (max-width: 760px) {
  .mode-control > span {
    display: none;
  }
  .mode-select {
    width: 148px;
    min-width: 148px;
  }
}
@container (max-width: 520px) {
  .optimize-button {
    min-width: 0;
  }
}
</style>
