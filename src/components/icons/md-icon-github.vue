<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), {
  size: 20,
});
</script>

<template>
  <span class="github-icon" :style="{ width: `${size}px`, height: `${size}px` }" aria-hidden="true" />
</template>

<style scoped>
.github-icon {
  display: inline-block;
  flex: none;
  background-color: currentColor;
  mask: url('/github.svg') center / contain no-repeat;
}
</style>
