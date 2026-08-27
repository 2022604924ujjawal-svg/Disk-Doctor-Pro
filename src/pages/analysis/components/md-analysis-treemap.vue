<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { computed, ref } from 'vue';

import MdFileEntryContextMenu from '@/components/custom/md-file-entry-context-menu.vue';
import MdNativeFileIcon from '@/components/custom/md-native-file-icon.vue';
import MdIcon from '@/components/icons/md-icon.vue';
import { ICON_NAMES } from '@/lib/models/ui';
import { TREEMAP_TILE_KINDS } from '@/lib/models/analysis';
import type { DirectoryEntryInfo, TreemapTile } from '@/lib/models/analysis';
import { ByteSizeService } from '@/lib/services/byte-size-service';
import { FormatUtils } from '@/lib/utils/format';
import { TreemapLayoutUtils } from '@/lib/utils/treemap-layout';

const { t } = useI18n({ useScope: 'global' });

const props = defineProps<{
  entries: DirectoryEntryInfo[];
  totalBytes: number;
  openDisabled: boolean;
  deleteDisabled: boolean;
}>();

const emit = defineEmits<{
  activate: [entry: DirectoryEntryInfo];
  openEntry: [entry: DirectoryEntryInfo];
  reveal: [path: string];
  delete: [entry: DirectoryEntryInfo];
}>();

const tiles = computed(() => TreemapLayoutUtils.layout(props.entries));
const hoveredTile = ref<TreemapTile | null>(null);
const contextMenuOpen = ref(false);
const tooltipPosition = ref({
  left: 0,
  top: 0,
  opensLeft: false,
  opensUp: false,
});

function tileStyle(tile: TreemapTile, tileIndex: number) {
  /*
   * Color only separates adjacent regions; it does not encode file type or
   * risk. Cycling theme chart colors in stable layout order keeps every skin
   * legible without deriving style data from private paths. Aggregate tiles
   * stay neutral so they cannot be mistaken for real directories.
   */
  const paletteColor = tile.kind === TREEMAP_TILE_KINDS.entry ? `var(--chart-${(tileIndex % 5) + 1})` : 'var(--muted)';
  return {
    left: `${tile.left}%`,
    top: `${tile.top}%`,
    width: `${tile.width}%`,
    height: `${tile.height}%`,
    '--treemap-tile-color': paletteColor,
  };
}

function tileClass(tile: TreemapTile) {
  const area = tile.width * tile.height;
  return {
    prominent: tile.kind === TREEMAP_TILE_KINDS.entry && area >= 1_800,
    compact: area < 650,
    tiny: area < 240,
    remainder: tile.kind === TREEMAP_TILE_KINDS.remainder,
  };
}

function tilePercentage(tile: TreemapTile) {
  return Math.round(FormatUtils.percent(tile.bytes, props.totalBytes));
}

function shouldLoadTileIcon(tile: TreemapTile) {
  return tile.width * tile.height >= 240;
}

function updateTooltipPosition(event: PointerEvent) {
  const offset = 14;
  const opensLeft = event.clientX > window.innerWidth / 2;
  const opensUp = event.clientY > window.innerHeight / 2;
  tooltipPosition.value = {
    left: event.clientX + (opensLeft ? -offset : offset),
    top: event.clientY + (opensUp ? -offset : offset),
    opensLeft,
    opensUp,
  };
}

function showTooltip(tile: TreemapTile, event: PointerEvent) {
  if (contextMenuOpen.value) return;
  hoveredTile.value = tile;
  updateTooltipPosition(event);
}

function hideTooltip() {
  hoveredTile.value = null;
}

function setContextMenuOpen(open: boolean) {
  contextMenuOpen.value = open;
  // A context menu is the active interaction surface. Remove the cursor
  // tooltip immediately instead of relying on portal z-index ordering, which
  // would leave two overlapping surfaces visible during menu animation.
  if (open) hideTooltip();
}

function tooltipStyle() {
  const position = tooltipPosition.value;
  return {
    left: `${position.left}px`,
    top: `${position.top}px`,
    transform: `translate(${position.opensLeft ? '-100%' : '0'}, ${position.opensUp ? '-100%' : '0'})`,
  };
}
</script>

<template>
  <div class="treemap">
    <template
      v-for="(tile, tileIndex) in tiles"
      :key="tile.kind === TREEMAP_TILE_KINDS.entry ? tile.entry.path : tile.kind"
    >
      <MdFileEntryContextMenu
        v-if="tile.kind === TREEMAP_TILE_KINDS.entry"
        :open-disabled="openDisabled"
        :delete-disabled="deleteDisabled"
        @menu-state-change="setContextMenuOpen"
        @open="emit('openEntry', tile.entry)"
        @reveal="emit('reveal', tile.entry.path)"
        @delete="emit('delete', tile.entry)"
      >
        <button
          type="button"
          class="treemap-tile"
          :class="tileClass(tile)"
          :style="tileStyle(tile, tileIndex)"
          :aria-label="`${tile.entry.name} · ${ByteSizeService.bytes(tile.bytes)}`"
          @pointerenter="showTooltip(tile, $event)"
          @pointermove="updateTooltipPosition"
          @pointerleave="hideTooltip"
          @contextmenu="hideTooltip"
          @click="emit('activate', tile.entry)"
          @dblclick="!tile.entry.isDirectory && emit('openEntry', tile.entry)"
          @keydown.enter="!tile.entry.isDirectory && emit('openEntry', tile.entry)"
        >
          <MdNativeFileIcon
            v-if="shouldLoadTileIcon(tile)"
            :path="tile.entry.path"
            :name="tile.entry.name"
            :directory="tile.entry.isDirectory"
            directory-mode="generic"
          />
          <span class="tile-copy">
            <strong class="md-result-primary">{{ tile.entry.name }}</strong>
            <small>{{ ByteSizeService.bytes(tile.bytes) }}</small>
          </span>
          <em>{{ tilePercentage(tile) }}%</em>
        </button>
      </MdFileEntryContextMenu>

      <div
        v-else
        class="treemap-tile"
        :class="tileClass(tile)"
        :style="tileStyle(tile, tileIndex)"
        role="group"
        :aria-label="
          t(
            'analysis.treemapRemainderHint',
            {
              count: FormatUtils.integer(tile.entryCount),
              size: ByteSizeService.bytes(tile.bytes),
            },
            tile.entryCount
          )
        "
        @pointerenter="showTooltip(tile, $event)"
        @pointermove="updateTooltipPosition"
        @pointerleave="hideTooltip"
      >
        <span class="tile-icon remainder-icon">
          <MdIcon :name="ICON_NAMES.list" :size="24" />
        </span>
        <span class="tile-copy">
          <strong class="md-result-primary">
            {{ t('analysis.treemapRemainder', { count: FormatUtils.integer(tile.entryCount) }, tile.entryCount) }}
          </strong>
          <small>{{ ByteSizeService.bytes(tile.bytes) }}</small>
        </span>
        <em>{{ tilePercentage(tile) }}%</em>
      </div>
    </template>
  </div>

  <Teleport to="body">
    <div
      v-if="hoveredTile && !contextMenuOpen"
      class="treemap-pointer-tooltip"
      :style="tooltipStyle()"
      aria-hidden="true"
    >
      <MdNativeFileIcon
        v-if="hoveredTile.kind === TREEMAP_TILE_KINDS.entry"
        :path="hoveredTile.entry.path"
        :name="hoveredTile.entry.name"
        :directory="hoveredTile.entry.isDirectory"
        directory-mode="generic"
      />
      <span v-else class="tooltip-remainder-icon">
        <MdIcon :name="ICON_NAMES.list" :size="22" />
      </span>

      <span class="tooltip-copy">
        <strong v-if="hoveredTile.kind === TREEMAP_TILE_KINDS.entry" class="md-result-primary">
          {{ hoveredTile.entry.name }}
        </strong>
        <strong v-else class="md-result-primary">
          {{
            t(
              'analysis.treemapRemainder',
              { count: FormatUtils.integer(hoveredTile.entryCount) },
              hoveredTile.entryCount
            )
          }}
        </strong>
        <small>
          {{ ByteSizeService.bytes(hoveredTile.bytes) }} · {{ tilePercentage(hoveredTile) }}%
          <template v-if="hoveredTile.kind === TREEMAP_TILE_KINDS.entry">
            ·
            {{
              t(
                'common.fileCount',
                { count: FormatUtils.integer(hoveredTile.entry.fileCount) },
                hoveredTile.entry.fileCount
              )
            }}
          </template>
        </small>
      </span>
    </div>
  </Teleport>
</template>

<style scoped>
@reference "@assets/main.css";

.treemap {
  position: relative;
  min-height: 0;
  max-height: 100%;
  flex: 1;
  overflow: hidden;
  margin: 0 12px 12px;
  border-radius: var(--radius);
  @apply bg-card;
  contain: layout paint;
  isolation: isolate;
}

.treemap-tile {
  --treemap-card-overlay-opacity: 0.78;

  position: absolute;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  overflow: hidden;
  border-width: 2px;
  border-radius: 9px;
  padding: 12px;
  border-color: var(--card);
  background: var(--treemap-tile-color, var(--secondary));
  @apply text-card-foreground transition-[color,background-color,border-color,box-shadow] duration-200;
  box-shadow: inset 0 0 0 1px var(--border);
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.treemap-tile::before {
  position: absolute;
  inset: 0;
  background: var(--card);
  content: '';
  opacity: var(--treemap-card-overlay-opacity);
  pointer-events: none;
  transition: opacity 0.2s ease;
}

.treemap-tile > * {
  /*
   * Keep the tile as the only pointer target. Nested file-type icons carry
   * native titles for other screens; bypassing them here prevents a second
   * browser tooltip from appearing over the cursor-following tooltip.
   */
  pointer-events: none;
  z-index: 1;
}

.treemap-tile:not(.remainder):is(:hover, [data-state='open']) {
  --treemap-card-overlay-opacity: 0.66;

  z-index: 2;
  border-color: var(--border-primary-subtle);
  box-shadow:
    inset 0 0 0 1px var(--border-primary-subtle),
    0 4px 10px var(--shadow-subtle);
}

.treemap-tile:focus-visible {
  z-index: 2;
  outline: 2px solid var(--focus-ring-subtle);
  outline-offset: -2px;
}

.treemap-tile.prominent {
  --treemap-card-overlay-opacity: 0.72;
}

.treemap-tile.remainder {
  --treemap-card-overlay-opacity: 0.3;

  background: var(--muted);
  @apply text-muted-foreground;
  cursor: default;
}

.remainder-icon {
  @apply bg-muted text-muted-foreground;
}

.tile-icon {
  display: grid;
  width: 40px;
  height: 40px;
  flex: none;
  place-items: center;
  border-radius: 8px;
  @apply text-warning-foreground;
  background: var(--surface-warning-subtle);
}

.tile-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 5px;
}

.tile-copy strong {
  overflow: hidden;
  @apply text-card-foreground;
  font-size: 15px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tile-copy small {
  font-size: 13px;
}

.treemap-tile > em {
  position: absolute;
  right: 16px;
  bottom: 13px;
  @apply text-primary;
  font-size: 13px;
  font-style: normal;
}

.treemap-tile.compact {
  --treemap-card-overlay-opacity: 0.82;

  gap: 7px;
  padding: 8px;
}

.treemap-tile.compact .tile-icon {
  width: 32px;
  height: 32px;
}

.treemap-tile.compact .tile-copy strong {
  font-size: 12px;
}

.treemap-tile.compact .tile-copy small,
.treemap-tile.compact > em,
.treemap-tile.tiny .tile-copy strong {
  font-size: 10px;
}

.treemap-tile.compact > em {
  right: 10px;
  bottom: 8px;
}

.treemap-tile.tiny .tile-icon,
.treemap-tile.tiny :deep(.file-type-icon),
.treemap-tile.tiny :deep(.native-file-icon),
.treemap-tile.tiny :deep(.directory-fallback),
.treemap-tile.tiny .tile-copy small,
.treemap-tile.tiny > em {
  display: none;
}

.treemap-tile.tiny {
  --treemap-card-overlay-opacity: 0.86;
}

.treemap-tile.remainder:is(.compact, .tiny) {
  --treemap-card-overlay-opacity: 0.3;
}

.treemap-pointer-tooltip {
  position: fixed;
  z-index: 80;
  display: flex;
  width: max-content;
  max-width: min(360px, calc(100vw - 24px));
  align-items: center;
  gap: 11px;
  border-radius: 10px;
  padding: 10px 13px;
  @apply bg-foreground text-background shadow-2xl shadow-background/25;
  pointer-events: none;
  will-change: left, top, transform;
}

.tooltip-remainder-icon {
  display: grid;
  width: 36px;
  height: 36px;
  flex: none;
  place-items: center;
  border-radius: 8px;
}

.tooltip-remainder-icon {
  border: 1px solid currentColor;
  background: transparent;
  @apply text-background;
}

.tooltip-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 3px;
}

.tooltip-copy strong {
  overflow-wrap: anywhere;
  font-size: 14px;
  line-height: 1.25;
}

.tooltip-copy small {
  opacity: 0.75;
  font-size: 12px;
  line-height: 1.3;
}
</style>
