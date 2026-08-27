<template>
  <div class="result-table-row">
    <slot />
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.result-table-row {
  position: relative;
  padding-inline: var(--result-table-content-inline-padding, 14px);
  background: transparent;
}

.result-table-row::before {
  position: absolute;
  top: var(--result-item-background-inset, 3px);
  right: 0;
  bottom: var(--result-item-background-inset, 3px);
  left: 0;
  border-radius: var(--radius);
  content: '';
  pointer-events: none;
  @apply transition-colors duration-200;
}

.result-table-row:hover,
.result-table-row:is([data-selected='true'], [data-expanded='true']) {
  background: transparent;
}

.result-table-row:hover::before {
  @apply bg-muted/60;
}

.result-table-row[data-selected='true']::before {
  background: transparent;
}

.result-table-row[data-expanded='true']:not([data-selected='true'])::before {
  @apply bg-muted/35;
}

.result-table-row[data-selected='true']:hover::before {
  @apply bg-muted/60;
}

.result-table-row:has(:focus-visible)::before {
  box-shadow: inset 0 0 0 1px var(--focus-ring-subtle);
  box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--ring) 52%, transparent);
}

.result-table-row > :deep(*) {
  position: relative;
}
</style>
