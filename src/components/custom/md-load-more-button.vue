<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { Button } from '@/components/ui/button';

const { t } = useI18n({ useScope: 'global' });

defineProps<{
  remainingLabel?: string;
  disabled?: boolean;
  loading?: boolean;
}>();

const emit = defineEmits<{
  loadMore: [];
}>();
</script>

<template>
  <div class="load-more-wrap">
    <Button size="sm" type="button" variant="ghost" :disabled="disabled" @click="emit('loadMore')">
      {{ loading ? t('loading.processing') : t('common.loadMore')
      }}<template v-if="!loading && remainingLabel"> · {{ remainingLabel }}</template>
    </Button>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.load-more-wrap {
  display: flex;
  min-height: 42px;
  align-items: center;
  justify-content: center;
  border-top-width: 1px;
  padding: 5px 12px;
  @apply border-border bg-muted/20;
}

.load-more-wrap :deep(button) {
  @apply text-primary;
}
</style>
