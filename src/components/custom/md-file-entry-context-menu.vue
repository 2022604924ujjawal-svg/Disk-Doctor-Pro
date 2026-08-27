<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MdIcon from '@/components/icons/md-icon.vue';
import { ContextMenu, ContextMenuContent, ContextMenuItem, ContextMenuTrigger } from '@/components/ui/context-menu';
import { ICON_NAMES } from '@/lib/models/ui';

const { t } = useI18n({ useScope: 'global' });

withDefaults(
  defineProps<{
    openDisabled?: boolean;
    deleteDisabled?: boolean;
  }>(),
  {
    openDisabled: false,
    deleteDisabled: false,
  }
);

const emit = defineEmits<{
  open: [];
  reveal: [];
  delete: [];
  menuStateChange: [open: boolean];
}>();
</script>

<template>
  <!-- The owning domain maps these presentation-only actions to its trusted entry model. -->
  <ContextMenu @update:open="emit('menuStateChange', $event)">
    <ContextMenuTrigger as-child>
      <slot />
    </ContextMenuTrigger>
    <ContextMenuContent>
      <ContextMenuItem :disabled="openDisabled" @select="emit('open')">
        <MdIcon :name="ICON_NAMES.external" :size="16" />
        {{ t('common.open') }}
      </ContextMenuItem>
      <ContextMenuItem @select="emit('reveal')">
        <MdIcon :name="ICON_NAMES.folder" :size="16" />
        {{ t('common.showInFileManager') }}
      </ContextMenuItem>
      <ContextMenuItem
        class="text-destructive focus:text-destructive"
        :disabled="deleteDisabled"
        @select="emit('delete')"
      >
        <MdIcon :name="ICON_NAMES.trash" :size="16" />
        {{ t('common.deletePermanently') }}
      </ContextMenuItem>
    </ContextMenuContent>
  </ContextMenu>
</template>
