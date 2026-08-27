<script setup lang="ts">
defineProps<{
  path: string;
  size: number;
}>();
</script>

<template>
  <!--
    Simple Icons use filled silhouettes while the application's Lucide and Tabler icons use strokes.
    The expanded viewBox adds consistent optical padding so brand marks do not dominate list rows.
  -->
  <svg
    :width="size"
    :height="size"
    viewBox="-1.5 -1.5 27 27"
    fill="currentColor"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path :d="path" />
  </svg>
</template>
