<script setup lang="ts">
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuTrigger,
} from 'reka-ui';
import { useI18n } from 'vue-i18n';

import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { ICON_NAMES } from '@/lib/models/ui';

const props = withDefaults(
  defineProps<{
    action?: 'start' | 'rescan';
    busy: boolean;
  }>(),
  {
    action: 'start',
  }
);

const emit = defineEmits<{
  primary: [];
  standard: [];
  'select-volumes': [];
}>();

const { t } = useI18n({ useScope: 'global' });
</script>

<template>
  <div
    class="scan-split-button"
    :class="{ 'scan-split-button--compact': props.action === 'rescan' }"
    role="group"
    :aria-label="t('cleanup.scanMode.label')"
  >
    <Button
      class="scan-primary-action rounded-r-none shadow-none"
      :variant="props.action === 'rescan' ? 'outline' : 'default'"
      :size="props.action === 'start' ? 'lg' : 'default'"
      type="button"
      :disabled="busy"
      @click="emit('primary')"
    >
      <MdIcon :name="props.action === 'start' ? ICON_NAMES.deepCleanup : ICON_NAMES.refresh" :size="16" />
      {{ t(props.action === 'start' ? 'overview.startScan' : 'overview.rescan') }}
    </Button>

    <DropdownMenuRoot>
      <DropdownMenuTrigger as-child>
        <Button
          class="scan-menu-trigger rounded-l-none px-0 shadow-none"
          :variant="props.action === 'rescan' ? 'outline' : 'default'"
          :size="props.action === 'start' ? 'lg' : 'default'"
          type="button"
          :disabled="busy"
          :aria-label="t('cleanup.scanMode.label')"
        >
          <MdIcon :name="ICON_NAMES.chevronDown" :size="16" />
        </Button>
      </DropdownMenuTrigger>

      <DropdownMenuPortal>
        <DropdownMenuContent
          align="end"
          :side-offset="6"
          class="scan-mode-menu z-50 w-80 max-w-[calc(100vw-32px)] overflow-hidden rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-xl data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
        >
          <DropdownMenuItem class="scan-mode-item" @select="emit('standard')">
            <MdIcon :name="ICON_NAMES.scan" :size="16" />
            <span class="scan-mode-copy">
              <strong>{{ t('cleanup.scanMode.standard') }}</strong>
              <small>{{ t('cleanup.scanMode.standardDescription') }}</small>
            </span>
          </DropdownMenuItem>
          <DropdownMenuItem class="scan-mode-item" @select="emit('select-volumes')">
            <MdIcon :name="ICON_NAMES.hardDrive" :size="16" />
            <span class="scan-mode-copy">
              <strong>{{ t('cleanup.scanMode.selectVolumes') }}</strong>
              <small>{{ t('cleanup.scanMode.selectVolumesDescription') }}</small>
            </span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenuPortal>
    </DropdownMenuRoot>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.scan-split-button {
  display: inline-flex;
  border-radius: var(--radius-lg);
}

.scan-split-button:not(.scan-split-button--compact) {
  @apply shadow-md shadow-primary/20;
}

.scan-primary-action,
.scan-menu-trigger {
  box-shadow: none;
}

.scan-primary-action {
  padding-right: 16px;
}

.scan-menu-trigger {
  width: 42px;
  border-left: 1px solid var(--primary-foreground);
  border-left: 1px solid color-mix(in srgb, var(--primary-foreground) 24%, transparent);
}

.scan-split-button--compact .scan-primary-action {
  border-right-width: 0;
  padding-right: 12px;
}

.scan-split-button--compact .scan-menu-trigger {
  width: 36px;
  border-left-width: 0;
}

.scan-primary-action:hover,
.scan-menu-trigger:hover {
  transform: none;
}

.scan-mode-item {
  @apply focus:bg-accent focus:text-accent-foreground;
  position: relative;
  display: flex;
  cursor: default;
  user-select: none;
  align-items: flex-start;
  gap: 10px;
  border-radius: calc(var(--radius) - 2px);
  padding: 10px;
  font-size: var(--font-content-secondary);
  outline: none;
  transition:
    color 0.15s ease,
    background-color 0.15s ease;
}

.scan-mode-item[data-disabled] {
  pointer-events: none;
  opacity: 0.5;
}

.scan-mode-item > :deep(svg) {
  margin-top: 2px;
  flex: none;
}

.scan-mode-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 2px;
}

.scan-mode-copy strong {
  color: var(--popover-foreground);
  font-size: var(--font-content-primary);
  font-weight: 600;
  line-height: 1.4;
}

.scan-mode-copy small {
  color: var(--muted-foreground);
  font-size: var(--font-content-secondary);
  line-height: 1.45;
}
</style>
