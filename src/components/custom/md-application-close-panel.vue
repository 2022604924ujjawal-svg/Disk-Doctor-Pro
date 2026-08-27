<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';

import MdApplicationIcon from '@/components/custom/md-application-icon.vue';
import MdResultCheckbox from '@/components/custom/md-result-checkbox.vue';
import type { ApplicationCloseItem } from '@/lib/models/application-close';
import { ApplicationIconService } from '@/lib/services/application-icon-service';
import { FormatUtils } from '@/lib/utils/format';

const { t } = useI18n({ useScope: 'global' });

const props = withDefaults(
  defineProps<{
    items: ApplicationCloseItem[];
    selectedIds?: string[];
    disabled?: boolean;
    selectable?: boolean;
  }>(),
  {
    selectedIds: () => [],
    disabled: false,
    selectable: true,
  }
);
const emit = defineEmits<{
  'update:selectedIds': [ids: string[]];
}>();

const selected = computed(() => new Set(props.selectedIds));
const allSelected = computed(() => props.items.length > 0 && props.items.every(item => selected.value.has(item.id)));
const iconUrls = ref<ReadonlyMap<string, string>>(new Map());
let iconRequestVersion = 0;

watch(
  () => props.items.map(item => item.iconPath).filter((path): path is string => Boolean(path)),
  paths => {
    const requestVersion = ++iconRequestVersion;
    if (!paths.length) {
      iconUrls.value = new Map();
      return;
    }
    void ApplicationIconService.resolveIncrementally(paths, icons => {
      if (requestVersion === iconRequestVersion) iconUrls.value = icons;
    });
  },
  { immediate: true }
);

function setSelected(id: string, checked: boolean) {
  if (props.disabled || !props.selectable) return;
  const next = new Set(props.selectedIds);
  if (checked) next.add(id);
  else next.delete(id);
  emit('update:selectedIds', [...next]);
}

function toggleAll() {
  if (props.disabled || !props.selectable) return;
  emit('update:selectedIds', allSelected.value ? [] : props.items.map(item => item.id));
}

function handleIconError(iconPath?: string) {
  if (!iconPath) return;
  const icons = new Map(iconUrls.value);
  icons.delete(iconPath);
  iconUrls.value = icons;
}
</script>

<template>
  <section class="application-close-panel" :aria-label="t('applicationClose.title')">
    <header class="application-close-header">
      <strong>{{ t('applicationClose.title') }}</strong>
      <button v-if="selectable" type="button" :disabled="disabled" @click="toggleAll">
        {{ t(allSelected ? 'applicationClose.clearAll' : 'applicationClose.selectAll') }}
      </button>
    </header>

    <div class="application-close-list">
      <label v-for="item in items" :key="item.id" class="application-close-row">
        <MdResultCheckbox
          v-if="selectable"
          :checked="selected.has(item.id)"
          :disabled="disabled"
          @update:checked="setSelected(item.id, $event)"
        />
        <span v-else class="application-close-state" aria-hidden="true">!</span>
        <MdApplicationIcon
          :src="item.iconPath ? iconUrls.get(item.iconPath) : undefined"
          :size="30"
          :artwork-size="26"
          @error="handleIconError(item.iconPath)"
        />
        <span class="application-close-copy">
          <strong>{{ item.name }}</strong>
          <small :title="item.processes.join(', ')">
            {{
              t(
                'applicationClose.processCount',
                { count: FormatUtils.integer(item.processes.length) },
                item.processes.length
              )
            }}
            · {{ item.processes.join(', ') }}
          </small>
        </span>
      </label>
    </div>
  </section>
</template>

<style scoped>
@reference "@assets/main.css";

.application-close-panel {
  overflow: hidden;
  border-width: 1px;
  border-radius: 9px;
  @apply border-border;
}

.application-close-list {
  min-height: 0;
}

.application-close-header {
  display: flex;
  min-height: 40px;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 6px 10px;
  @apply bg-muted/45;
}

.application-close-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 2px;
}

.application-close-header strong,
.application-close-copy strong {
  font-size: 13px;
  font-weight: 600;
}

.application-close-header small,
.application-close-copy small {
  @apply text-muted-foreground;
  font-size: 10.5px;
  line-height: 1.45;
}

.application-close-header button {
  flex: none;
  border: 0;
  padding: 5px;
  @apply text-primary;
  background: transparent;
  font-size: 12px;
}

.application-close-header button:disabled {
  opacity: 0.5;
}

.application-close-row {
  display: grid;
  min-height: 54px;
  grid-template-columns: 18px 30px minmax(0, 1fr) auto;
  align-items: center;
  gap: 8px;
  border-top-width: 1px;
  padding: 6px 10px;
  @apply border-border/70;
}

.application-close-row:has([data-state='checked']) {
  background: var(--surface-primary-subtle);
}

.application-close-copy small {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.application-close-state {
  display: grid;
  width: 18px;
  height: 18px;
  place-items: center;
  border-radius: 999px;
  @apply text-destructive;
  background: var(--surface-destructive-subtle);
  font-size: 11px;
  font-weight: 700;
}

@container (max-width: 540px) {
  .application-close-row {
    grid-template-columns: 18px 30px minmax(0, 1fr);
  }
}
</style>
