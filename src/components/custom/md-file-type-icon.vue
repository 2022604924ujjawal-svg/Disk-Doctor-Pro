<script setup lang="ts">
import { computed } from 'vue';

import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';
import { FileTypeUtils, type FileVisualKind } from '@/lib/utils/file-type';

const props = withDefaults(
  defineProps<{
    name: string;
    compact?: boolean;
  }>(),
  {
    compact: false,
  }
);

const descriptor = computed(() => FileTypeUtils.descriptor(props.name));

const iconNames: Readonly<Record<FileVisualKind, (typeof ICON_NAMES)[keyof typeof ICON_NAMES]>> = {
  pdf: ICON_NAMES.fileText,
  document: ICON_NAMES.fileText,
  spreadsheet: ICON_NAMES.fileSpreadsheet,
  presentation: ICON_NAMES.filePresentation,
  text: ICON_NAMES.fileText,
  code: ICON_NAMES.fileCode,
  data: ICON_NAMES.database,
  audio: ICON_NAMES.fileAudio,
  video: ICON_NAMES.fileVideo,
  image: ICON_NAMES.fileImage,
  archive: ICON_NAMES.fileArchive,
  installer: ICON_NAMES.package,
  'disk-image': ICON_NAMES.disc,
  binary: ICON_NAMES.fileSettings,
  'ai-model': ICON_NAMES.aiModel,
  other: ICON_NAMES.file,
};

const iconName = computed(() => iconNames[descriptor.value.kind]);
const iconSize = computed(() => (props.compact ? 22 : 24));
</script>

<template>
  <span
    class="file-type-icon grid flex-none place-items-center text-accent-foreground"
    :class="[descriptor.kind, compact ? 'size-[30px]' : 'size-[34px]']"
    :title="name"
    aria-hidden="true"
  >
    <MdIcon :name="iconName" :size="iconSize" />
  </span>
</template>

<style scoped>
@reference "@assets/main.css";

/* File formats share one geometry and differ only through semantic color. */
.file-type-icon.pdf {
  @apply text-destructive;
}
.file-type-icon.document {
  @apply text-primary;
}
.file-type-icon.spreadsheet {
  @apply text-success;
}
.file-type-icon.presentation,
.file-type-icon.archive {
  @apply text-warning-foreground;
}
.file-type-icon.code,
.file-type-icon.video {
  @apply text-file-code;
}
.file-type-icon.audio {
  @apply text-file-audio;
}
.file-type-icon.image {
  @apply text-file-image;
}
.file-type-icon.data {
  @apply text-file-data;
}
.file-type-icon.installer,
.file-type-icon.disk-image {
  @apply text-file-package;
}
.file-type-icon.binary {
  @apply text-file-binary;
}
</style>
