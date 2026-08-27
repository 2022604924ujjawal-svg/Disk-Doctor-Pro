import { describe, expect, it } from 'vitest';

import { RenderBatchUtils } from './render-batch';

describe('render batch utilities', () => {
  it('returns only the visible prefix without mutating the source', () => {
    const items = [1, 2, 3, 4];

    expect(RenderBatchUtils.visibleItems(items, 2)).toEqual([1, 2]);
    expect(items).toEqual([1, 2, 3, 4]);
  });

  it('clamps remaining and next counts to valid bounds', () => {
    expect(RenderBatchUtils.remainingCount(8, 3)).toBe(5);
    expect(RenderBatchUtils.remainingCount(3, 8)).toBe(0);
    expect(RenderBatchUtils.nextVisibleCount(10, 42, 50)).toBe(42);
    expect(RenderBatchUtils.nextVisibleCount(-5, 42, 10)).toBe(10);
  });

  it('keeps paginated remaining counts stable until a loaded page is revealed', () => {
    expect(RenderBatchUtils.remainingCountAcrossPages(40, 40, 880)).toBe(880);
    expect(RenderBatchUtils.remainingCountAcrossPages(80, 40, 840)).toBe(880);
    expect(RenderBatchUtils.remainingCountAcrossPages(80, 80, 840)).toBe(840);
  });

  it('does not advance when a batch size is invalid', () => {
    expect(RenderBatchUtils.nextVisibleCount(10, 42, 0)).toBe(10);
    expect(RenderBatchUtils.nextVisibleCount(50, 42, -1)).toBe(42);
  });
});
