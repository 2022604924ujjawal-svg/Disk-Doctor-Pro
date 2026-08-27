<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MdDialogContent from '@/components/custom/md-dialog-content.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { Button } from '@/components/ui/button';
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { ICON_NAMES } from '@/lib/models/ui';

defineProps<{
  open: boolean;
  itemNames: string[];
}>();

const emit = defineEmits<{
  'update:open': [value: boolean];
  confirm: [];
}>();

const { t } = useI18n({ useScope: 'global' });
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <MdDialogContent class="risk-dialog gap-0 p-0 sm:max-w-[480px]">
      <DialogHeader class="risk-dialog-header">
        <div class="risk-dialog-icon" aria-hidden="true">
          <MdIcon :name="ICON_NAMES.shield" :size="21" />
        </div>
        <div class="risk-dialog-copy">
          <DialogTitle>{{ t('systemOptimization.riskDialog.title') }}</DialogTitle>
          <DialogDescription>{{ t('systemOptimization.riskDialog.description') }}</DialogDescription>
        </div>
      </DialogHeader>

      <ul class="risk-dialog-items">
        <li v-for="itemName in itemNames" :key="itemName">{{ itemName }}</li>
      </ul>

      <p class="risk-dialog-note">
        <MdIcon :name="ICON_NAMES.info" :size="15" />
        <span>{{ t('systemOptimization.riskDialog.note') }}</span>
      </p>

      <DialogFooter class="risk-dialog-footer">
        <Button variant="outline" type="button" @click="emit('update:open', false)">
          {{ t('common.cancel') }}
        </Button>
        <Button type="button" @click="emit('confirm')">
          {{ t('systemOptimization.riskDialog.confirm') }}
        </Button>
      </DialogFooter>
    </MdDialogContent>
  </Dialog>
</template>

<style scoped>
@reference "@assets/main.css";

.risk-dialog-header {
  display: grid;
  grid-template-columns: 44px minmax(0, 1fr);
  gap: 14px;
  padding: 22px 54px 15px 22px;
}

.risk-dialog-icon {
  display: grid;
  width: 44px;
  height: 44px;
  place-items: center;
  border-radius: 12px;
  color: var(--warning-foreground);
  background: color-mix(in oklab, var(--warning) 14%, transparent);
}

.risk-dialog-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 7px;
  padding-top: 2px;
}

.risk-dialog-copy :deep([data-slot='dialog-description']) {
  line-height: 1.55;
}

.risk-dialog-items {
  display: grid;
  max-height: 180px;
  gap: 7px;
  margin: 0 22px 13px;
  overflow-y: auto;
  border-width: 1px;
  border-radius: 10px;
  padding: 11px 14px 11px 30px;
  @apply border-border bg-muted/40;
  font-size: 12px;
}

.risk-dialog-note {
  display: flex;
  align-items: flex-start;
  gap: 7px;
  margin: 0 22px 17px;
  color: var(--muted-foreground);
  font-size: 11px;
  line-height: 1.5;
}

.risk-dialog-note :deep(svg) {
  flex: none;
  margin-top: 1px;
  color: var(--primary);
}

.risk-dialog-footer {
  min-height: 64px;
  align-items: center;
  border-top-width: 1px;
  padding: 11px 22px;
  @apply border-border bg-muted/25;
}

.risk-dialog-footer :deep(button) {
  min-width: 104px;
}
</style>
