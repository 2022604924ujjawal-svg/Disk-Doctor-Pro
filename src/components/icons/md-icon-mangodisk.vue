<script setup lang="ts">
import { APP_ICON_PATH, APP_NAME } from '@/lib/models/application-shell';

withDefaults(
  defineProps<{
    size?: number;
    decorative?: boolean;
  }>(),
  {
    size: 32,
    decorative: true,
  }
);
</script>

<template>
  <img
    class="mangodisk-icon"
    :src="APP_ICON_PATH"
    :width="size"
    :height="size"
    :alt="decorative ? '' : APP_NAME"
    :aria-hidden="decorative"
  />
</template>

<style scoped>
.mangodisk-icon {
  display: block;
  flex: none;
  object-fit: contain;
}
</style>
