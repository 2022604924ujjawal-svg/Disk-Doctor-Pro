import { describe, expect, it } from 'vitest';

import { TREEMAP_TILE_KINDS } from '@/lib/models/analysis';
import type { DirectoryEntryInfo } from '@/lib/models/analysis';

import { TreemapLayoutUtils } from './treemap-layout';

function entry(name: string, bytes: number): DirectoryEntryInfo {
  return {
    name,
    path: `/fixture/${name}`,
    bytes,
    fileCount: 1,
    isDirectory: false,
    modifiedAtMs: null,
    contentFingerprint: null,
  };
}

describe('TreemapLayoutUtils', () => {
  it('aggregates entries below the useful visible share', () => {
    const tiles = TreemapLayoutUtils.layout([entry('large', 990), entry('small-a', 5), entry('small-b', 5)]);

    expect(tiles).toHaveLength(2);
    expect(tiles.find(tile => tile.kind === TREEMAP_TILE_KINDS.entry)?.entry.name).toBe('large');
    expect(tiles.find(tile => tile.kind === TREEMAP_TILE_KINDS.remainder)).toMatchObject({
      bytes: 10,
      entryCount: 2,
    });
    expect(tiles.reduce((sum, tile) => sum + tile.bytes, 0)).toBe(1_000);
  });

  it('preserves many similarly sized entries instead of creating a dominant remainder', () => {
    const entries = Array.from({ length: 55 }, (_, index) => entry(`track-${index}`, 100));
    const tiles = TreemapLayoutUtils.layout(entries);
    const visibleTiles = tiles.filter(tile => tile.kind === TREEMAP_TILE_KINDS.entry);
    const remainder = tiles.find(tile => tile.kind === TREEMAP_TILE_KINDS.remainder);

    expect(visibleTiles).toHaveLength(55);
    expect(remainder).toBeUndefined();
  });

  it('retains the largest positive entry when every share is below the threshold', () => {
    const tiles = TreemapLayoutUtils.layout([entry('largest', 4), entry('small-a', 3), entry('small-b', 3)], {
      minimumVisibleShare: 0.9,
    });

    expect(tiles).toHaveLength(2);
    expect(tiles.find(tile => tile.kind === TREEMAP_TILE_KINDS.entry)?.entry.name).toBe('largest');
    expect(tiles.find(tile => tile.kind === TREEMAP_TILE_KINDS.remainder)).toMatchObject({
      bytes: 6,
      entryCount: 2,
    });
  });

  it('ignores zero-byte entries and fills the complete treemap area', () => {
    const tiles = TreemapLayoutUtils.layout([entry('a', 60), entry('b', 40), entry('empty', 0)], {
      minimumVisibleShare: 0,
    });
    const totalArea = tiles.reduce((sum, tile) => sum + tile.width * tile.height, 0);

    expect(tiles).toHaveLength(2);
    expect(totalArea).toBeCloseTo(10_000);
  });

  it('keeps every tile area proportional to its byte share', () => {
    const tiles = TreemapLayoutUtils.layout([entry('large', 60), entry('medium', 25), entry('small', 15)], {
      minimumVisibleShare: 0,
    });

    for (const tile of tiles) {
      expect(tile.width * tile.height).toBeCloseTo(tile.bytes * 100);
    }
  });

  it('avoids narrow tiles for an uneven but groupable size set', () => {
    const tiles = TreemapLayoutUtils.layout(
      [entry('large', 60), entry('small-a', 10), entry('small-b', 10), entry('small-c', 10), entry('small-d', 10)],
      { minimumVisibleShare: 0 }
    );
    const worstAspectRatio = Math.max(
      ...tiles.map(tile => Math.max(tile.width / tile.height, tile.height / tile.width))
    );

    // The previous balanced binary partition produced a 2.5:1 extreme for
    // this distribution. Squarifying keeps every tile at or below 5:3.
    expect(worstAspectRatio).toBeCloseTo(5 / 3);
  });

  it('keeps every tile finite and inside the percentage coordinate space', () => {
    const tiles = TreemapLayoutUtils.layout(
      Array.from({ length: 55 }, (_, index) => entry(`track-${index}`, 100 - index)),
      { minimumVisibleShare: 0 }
    );

    for (const tile of tiles) {
      expect([tile.left, tile.top, tile.width, tile.height].every(Number.isFinite)).toBe(true);
      expect(tile.left).toBeGreaterThanOrEqual(0);
      expect(tile.top).toBeGreaterThanOrEqual(0);
      expect(tile.left + tile.width).toBeLessThanOrEqual(100 + Number.EPSILON * 100);
      expect(tile.top + tile.height).toBeLessThanOrEqual(100 + Number.EPSILON * 100);
    }
  });
});
