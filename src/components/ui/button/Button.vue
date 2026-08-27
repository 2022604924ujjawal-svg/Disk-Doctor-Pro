<script setup lang="ts">
import { Primitive, type PrimitiveProps } from 'reka-ui';
import type { HTMLAttributes } from 'vue';

import { cn } from '@/lib/utils';

import { buttonVariants, type ButtonVariants } from './button-variants';

withDefaults(
  defineProps<PrimitiveProps & {
    variant?: ButtonVariants['variant'];
    size?: ButtonVariants['size'];
    class?: HTMLAttributes['class'];
  }>(),
  { as: 'button' },
);
</script>

<template>
  <Primitive data-slot="button" :as="as" :as-child="asChild" :class="cn(buttonVariants({ variant, size }), $props.class)"><slot /></Primitive>
</template>
