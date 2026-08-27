<script setup lang="ts">
defineProps<{
  title: string;
  metricLabel: string;
  metricValue: string;
}>();
</script>

<template>
  <div class="flex min-h-12 min-w-0 flex-wrap items-center gap-x-4 gap-y-1.5 px-4 py-1.5">
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
        <strong class="text-content-primary font-semibold text-foreground">{{ title }}</strong>
        <span class="text-content-secondary text-muted-foreground">{{ metricLabel }}</span>
        <strong class="text-xl leading-none font-semibold tracking-tight text-primary">{{ metricValue }}</strong>
      </div>
    </div>

    <div v-if="$slots.actions" class="flex min-w-0 flex-none items-center justify-end">
      <slot name="actions" />
    </div>
  </div>
</template>
