<template>
  <div class="flex h-full min-h-0 w-full min-w-0 flex-1 items-center gap-2 px-3">
    <div class="result-filter-toolbar-main flex min-w-0 flex-1 items-center overflow-hidden">
      <slot />
    </div>
    <div v-if="$slots.aside" class="flex flex-none items-center justify-end gap-2.5 whitespace-nowrap">
      <slot name="aside" />
    </div>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.result-filter-toolbar-main :slotted(*) {
  min-width: 0;
  flex: 1;
}
</style>
