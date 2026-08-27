<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { reactiveOmit } from '@vueuse/core';
import type { DialogContentEmits, DialogContentProps } from 'reka-ui';
import { DialogClose, useForwardPropsEmits } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import MdIcon from '@/components/icons/md-icon.vue';
import DialogContent from '@/components/ui/dialog/DialogContent.vue';
import { ICON_NAMES } from '@/lib/models/ui';

const { t } = useI18n({ useScope: 'global' });
const props = withDefaults(
  defineProps<DialogContentProps & { class?: HTMLAttributes['class']; showClose?: boolean }>(),
  {
    class: undefined,
    showClose: true,
  }
);
const emits = defineEmits<DialogContentEmits>();
const delegatedProps = reactiveOmit(props, 'class', 'showClose');
const forwarded = useForwardPropsEmits(delegatedProps, emits);
</script>

<template>
  <DialogContent v-bind="forwarded" :class="props.class">
    <slot />
    <DialogClose
      v-if="props.showClose"
      class="absolute top-4 right-4 grid size-9 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <MdIcon :name="ICON_NAMES.close" :size="18" />
      <span class="sr-only">{{ t('common.close') }}</span>
    </DialogClose>
  </DialogContent>
</template>
