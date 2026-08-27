<template>
  <div class="md-action-bar-container">
    <slot />
  </div>
</template>

<style scoped>
@reference "@assets/main.css";

.md-action-bar-container {
  display: flex;
  width: 100%;
  min-height: var(--layout-action-bar-height);
  align-items: center;
  border-width: 1px;
  border-radius: var(--radius);
  background: var(--card);
  color: var(--card-foreground);
  border-color: var(--border);
  box-shadow: 0 1px 3px var(--shadow-subtle);
}

@supports (color: color-mix(in oklab, black, white)) {
  .md-action-bar-container {
    background: color-mix(in oklab, var(--card) 95%, transparent);
    -webkit-backdrop-filter: blur(12px);
    backdrop-filter: blur(12px);
  }
}
</style>
