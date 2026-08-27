<script setup lang="ts">
import { computed } from 'vue';

import MdIconMynauiTerminalSolid from '@/components/icons/md-icon-mynaui-terminal-solid.vue';
import MdIconWindowsExecutable from '@/components/icons/md-icon-windows-executable.vue';
import type { ApplicationUninstallPlatform } from '@/lib/models/application';
import { OperatingSystemService } from '@/lib/services/operating-system-service';

const props = withDefaults(
  defineProps<{
    src?: string;
    platform?: ApplicationUninstallPlatform;
    size?: number;
    artworkSize?: number;
  }>(),
  {
    src: '',
    platform: undefined,
    size: 36,
    artworkSize: 0,
  }
);
const emit = defineEmits<{
  error: [];
}>();

const resolvedPlatform = computed<ApplicationUninstallPlatform>(() => {
  if (props.platform) return props.platform;
  return OperatingSystemService.isWindows() ? 'windowsRegistry' : 'macosBundle';
});

const resolvedArtworkSize = computed(() => {
  if (props.artworkSize > 0) return props.artworkSize;
  // Windows icon resources usually fill their canvas while macOS ICNS artwork includes optical
  // padding. Normalizing only the artwork keeps alignment slots identical across platforms.
  return resolvedPlatform.value === 'windowsRegistry' ? Math.round(props.size * 0.85) : props.size;
});
</script>

<template>
  <span
    class="md-application-icon"
    :class="{
      resolved: Boolean(src),
      'fallback-container': !src,
    }"
    :style="{ width: `${size}px`, height: `${size}px` }"
  >
    <img
      v-if="src"
      :src="src"
      alt=""
      :style="{ width: `${resolvedArtworkSize}px`, height: `${resolvedArtworkSize}px` }"
      @error="emit('error')"
    />
    <span v-else-if="resolvedPlatform === 'windowsRegistry'" class="fallback-icon" aria-hidden="true">
      <MdIconWindowsExecutable :size="Math.round(size * 0.72)" />
    </span>
    <span v-else class="fallback-icon" aria-hidden="true">
      <MdIconMynauiTerminalSolid :size="Math.round(size * 0.92)" />
    </span>
  </span>
</template>

<style scoped>
@reference "@assets/main.css";

.md-application-icon {
  @apply text-primary;
  background: var(--surface-primary-subtle);
  display: grid;
  flex: none;
  overflow: hidden;
  place-items: center;
  border-radius: calc(var(--radius) - 2px);
}

.md-application-icon.resolved {
  background: transparent;
}

.md-application-icon.fallback-container {
  background: transparent;
}

img {
  object-fit: contain;
}

.fallback-icon {
  display: grid;
  width: 100%;
  height: 100%;
  place-items: center;
}
</style>
