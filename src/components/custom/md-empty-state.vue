<script setup lang="ts">
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';

type EmptyStateIconName = (typeof ICON_NAMES)[keyof typeof ICON_NAMES];

withDefaults(
  defineProps<{
    iconName: EmptyStateIconName;
    title: string;
    description: string;
    compact?: boolean;
  }>(),
  {
    compact: false,
  }
);
</script>

<template>
  <section
    class="md-empty-state flex min-h-0 flex-1 flex-col items-center justify-center text-muted-foreground"
    :class="{ 'md-empty-state--compact': compact }"
  >
    <span class="empty-state-icon grid place-items-center text-primary">
      <MdIcon :name="iconName" :size="compact ? 28 : 36" />
    </span>
    <h2 class="empty-state-title text-content-empty-title text-card-foreground">{{ title }}</h2>
    <p class="empty-state-description max-w-xl text-center text-content-body leading-relaxed">
      {{ description }}
    </p>
    <div v-if="$slots.default" class="empty-state-actions">
      <slot />
    </div>
  </section>
</template>

<style scoped>
@reference "@assets/main.css";

.md-empty-state {
  gap: 10px;
  padding: 24px 24px clamp(52px, 9vh, 88px);
}

.empty-state-icon {
  width: 52px;
  height: 52px;
  margin-bottom: 2px;
}

.empty-state-title,
.empty-state-description {
  margin: 0;
}

.empty-state-description {
  max-width: 520px;
}

.empty-state-actions {
  display: flex;
  align-items: center;
  justify-content: center;
  margin-top: 8px;
}

.md-empty-state--compact {
  min-height: 240px;
  padding-bottom: 32px;
}

.md-empty-state--compact .empty-state-icon {
  width: 44px;
  height: 44px;
  color: var(--muted-foreground);
}
</style>
