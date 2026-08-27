import { TREEMAP_TILE_KINDS } from '@/lib/models/analysis';
import type { DirectoryEntryInfo, TreemapTile } from '@/lib/models/analysis';

interface TreemapRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

type TreemapLayoutNode =
  | {
      kind: typeof TREEMAP_TILE_KINDS.entry;
      entry: DirectoryEntryInfo;
      bytes: number;
    }
  | {
      kind: typeof TREEMAP_TILE_KINDS.remainder;
      entry: null;
      bytes: number;
      entryCount: number;
    };

export interface TreemapLayoutOptions {
  minimumVisibleShare?: number;
}

const DEFAULT_MINIMUM_VISIBLE_SHARE = 0.0075;

/** Computes percentage-based tiles without DOM, Store, or platform access. */
export class TreemapLayoutUtils {
  static layout(entries: DirectoryEntryInfo[], options: TreemapLayoutOptions = {}): TreemapTile[] {
    const candidates = entries.filter(entry => entry.bytes > 0).sort((left, right) => right.bytes - left.bytes);
    if (!candidates.length) return [];

    const total = candidates.reduce((sum, entry) => sum + entry.bytes, 0);
    const minimumVisibleShare = Math.min(1, Math.max(0, options.minimumVisibleShare ?? DEFAULT_MINIMUM_VISIBLE_SHARE));

    // Treemap area is proportional to byte share. Keeping only entries that can
    // receive a useful share avoids unreadable pixel fragments independently
    // of the current window size. Do not impose a fixed item limit: a folder
    // containing many similarly sized files still needs to show every item
    // because none of them is less meaningful than the others.
    let visibleCount = candidates.length;
    while (visibleCount > 1 && candidates[visibleCount - 1].bytes / total < minimumVisibleShare) {
      visibleCount -= 1;
    }

    const visibleEntries = candidates.slice(0, visibleCount);
    const hiddenEntries = candidates.slice(visibleCount);
    const nodes: TreemapLayoutNode[] = visibleEntries.map(entry => ({
      kind: TREEMAP_TILE_KINDS.entry,
      entry,
      bytes: entry.bytes,
    }));

    if (hiddenEntries.length) {
      nodes.push({
        kind: TREEMAP_TILE_KINDS.remainder,
        entry: null,
        bytes: hiddenEntries.reduce((sum, entry) => sum + entry.bytes, 0),
        entryCount: hiddenEntries.length,
      });
    }

    // The aggregated remainder may be larger than individual visible entries.
    // Sorting it with the other nodes lets the layout consider its real weight
    // instead of forcing it into a narrow final strip.
    nodes.sort((left, right) => right.bytes - left.bytes);
    return TreemapLayoutUtils.squarify(nodes, {
      left: 0,
      top: 0,
      width: 100,
      height: 100,
    });
  }

  /**
   * Lays out descending nodes in strips that minimize the worst tile aspect
   * ratio. The byte total is converted to the rectangle area once so every
   * emitted tile remains proportional to its source size.
   */
  private static squarify(entries: TreemapLayoutNode[], rect: TreemapRect): TreemapTile[] {
    const totalBytes = entries.reduce((sum, entry) => sum + entry.bytes, 0);
    if (totalBytes <= 0 || rect.width <= 0 || rect.height <= 0) return [];

    const scale = (rect.width * rect.height) / totalBytes;
    const areas = entries.map(entry => entry.bytes * scale);
    const tiles: TreemapTile[] = [];
    let remaining = { ...rect };
    let index = 0;

    while (index < entries.length) {
      const shortSide = Math.min(remaining.width, remaining.height);
      const rowStart = index;
      const rowAreas = [areas[index]];
      let rowArea = areas[index];
      let rowSmallestArea = areas[index];
      let rowLargestArea = areas[index];
      index += 1;

      // A row is finalized as soon as adding the next item would create a
      // worse extreme. This is the core Squarified Treemap decision and keeps
      // both labels and pointer targets usable across uneven size sets.
      while (index < entries.length) {
        const nextArea = areas[index];
        const nextRowArea = rowArea + nextArea;
        const nextSmallestArea = Math.min(rowSmallestArea, nextArea);
        const nextLargestArea = Math.max(rowLargestArea, nextArea);
        if (
          TreemapLayoutUtils.worstAspectRatio(nextRowArea, nextSmallestArea, nextLargestArea, shortSide) <=
          TreemapLayoutUtils.worstAspectRatio(rowArea, rowSmallestArea, rowLargestArea, shortSide)
        ) {
          rowAreas.push(nextArea);
          rowArea = nextRowArea;
          rowSmallestArea = nextSmallestArea;
          rowLargestArea = nextLargestArea;
          index += 1;
        } else {
          break;
        }
      }

      const thickness = shortSide > 0 ? rowArea / shortSide : 0;

      if (remaining.width >= remaining.height) {
        let top = remaining.top;
        rowAreas.forEach((area, rowIndex) => {
          const height = thickness > 0 ? area / thickness : 0;
          tiles.push({
            ...entries[rowStart + rowIndex],
            left: remaining.left,
            top,
            width: thickness,
            height,
          });
          top += height;
        });
        remaining = {
          left: remaining.left + thickness,
          top: remaining.top,
          width: Math.max(0, remaining.width - thickness),
          height: remaining.height,
        };
      } else {
        let left = remaining.left;
        rowAreas.forEach((area, rowIndex) => {
          const width = thickness > 0 ? area / thickness : 0;
          tiles.push({
            ...entries[rowStart + rowIndex],
            left,
            top: remaining.top,
            width,
            height: thickness,
          });
          left += width;
        });
        remaining = {
          left: remaining.left,
          top: remaining.top + thickness,
          width: remaining.width,
          height: Math.max(0, remaining.height - thickness),
        };
      }
    }

    return tiles;
  }

  /** Returns the largest width-to-height ratio in a prospective row. */
  private static worstAspectRatio(rowArea: number, smallestArea: number, largestArea: number, side: number): number {
    if (rowArea <= 0 || smallestArea <= 0 || side <= 0) return Number.POSITIVE_INFINITY;

    const rowAreaSquared = rowArea * rowArea;
    const sideSquared = side * side;
    return Math.max((sideSquared * largestArea) / rowAreaSquared, rowAreaSquared / (sideSquared * smallestArea));
  }
}
