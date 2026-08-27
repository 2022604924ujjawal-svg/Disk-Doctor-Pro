<script setup lang="ts">
import MdResultWorkspace from '@/components/custom/md-result-workspace.vue';

withDefaults(
  defineProps<{
    mode?: 'workspace' | 'overlay';
  }>(),
  {
    mode: 'workspace',
  }
);
</script>

<template>
  <MdResultWorkspace v-if="mode === 'workspace'">
    <div class="operation-workspace-stage">
      <slot />
    </div>
  </MdResultWorkspace>
  <section v-else class="operation-workspace-overlay">
    <div class="operation-workspace-stage">
      <slot />
    </div>
  </section>
</template>

<style scoped>
@reference "@assets/main.css";

.operation-workspace-stage {
  display: grid;
  min-height: 0;
  flex: 1;
  place-items: center;
}

/*
 * Workspaces with persistent local navigation keep their geometry while an
 * operation runs. The overlay uses the same semantic workspace background as
 * the page-level variant instead of creating a page-owned translucent layer.
 */
.operation-workspace-overlay {
  position: absolute;
  z-index: 5;
  top: var(--operation-workspace-overlay-top, 0);
  right: 0;
  bottom: 0;
  left: 0;
  display: flex;
  min-height: 0;
  flex-direction: column;
  @apply bg-workspace text-foreground;
}
</style>
