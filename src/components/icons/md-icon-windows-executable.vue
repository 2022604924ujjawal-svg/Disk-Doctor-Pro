<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), {
  size: 40,
});
</script>

<template>
  <svg :width="size" :height="size" viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path d="M65.7 179.6h897v32.2h-897z" fill="var(--windows-executable-titlebar)" />
    <path d="M65.7 211.8h897V244h-897z" fill="var(--windows-executable-chrome)" />
    <path d="M65.7 244h897v605.2h-897z" fill="var(--windows-executable-surface)" />
    <path d="M65.7 244h154.2v605.2H65.7z" fill="var(--windows-executable-chrome)" />
    <path d="M254.1 282.1h395.3v513.4H254.1z" fill="var(--windows-executable-accent)" />
    <path
      d="M665 299.6h276.2v16.6H665zm0 34h276.2v16.6H665zm0 34h276.2v16.6H665zm0 33.9h276.2v16.6H665zm0 34h276.2v16.6H665zm0 34h276.2V520H665zm0 33.9h276.2V554H665zm0 34h276.2V588H665zm0 34h276.2v16.6H665zm0 33.9h276.2v16.6H665zm0 34h156.2v16.6H665zM82.3 299.6h34.2v16.6H82.3zm51.7 0h68.3v16.6H134zm-51.7 63.7h34.2v16.6H82.3zm51.7 0h68.3v16.6H134zM82.3 427h34.2v16.6H82.3zm51.7 0h68.3v16.6H134zm-51.7 63.7h34.2v16.6H82.3zm51.7 0h68.3v16.6H134zm-51.7 63.7h34.2V571H82.3zm51.7 0h68.3V571H134z"
      fill="var(--windows-executable-detail)"
    />
  </svg>
</template>
