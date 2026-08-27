<template>
  <div class="result-table-hierarchy">
    <div class="hierarchy-items">
      <slot />
    </div>
    <div v-if="$slots.footer" class="hierarchy-footer">
      <slot name="footer" />
    </div>
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.result-table-hierarchy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: var(--result-item-gap, 6px);
  margin-inline-start: var(--result-table-hierarchy-indent, 34px);
  padding-block: var(--result-item-background-inset, 3px);
}

.hierarchy-items {
  position: relative;
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 0px;
  padding-inline-start: 10px;
}

/* Pagination and notices use the footer slot so the connector ends at the
 * final result row instead of implying that controls are hierarchy items. */
.hierarchy-items::before {
  position: absolute;
  top: calc(0px - var(--result-item-background-inset, 3px));
  bottom: 0;
  inset-inline-start: 0;
  width: 2px;
  border-radius: 999px;
  content: '';
  @apply bg-border;
}

.hierarchy-footer {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: var(--result-item-gap, 6px);
  padding-inline-start: 10px;
}
</style>
