export class RenderBatchUtils {
  static visibleItems<T>(items: readonly T[], visibleCount: number): T[] {
    return items.slice(0, Math.max(0, visibleCount));
  }

  static remainingCount(totalCount: number, visibleCount: number): number {
    return Math.max(0, totalCount - Math.max(0, visibleCount));
  }

  static remainingCountAcrossPages(loadedCount: number, visibleCount: number, unloadedCount: number): number {
    return this.remainingCount(loadedCount, visibleCount) + Math.max(0, unloadedCount);
  }

  static nextVisibleCount(currentCount: number, totalCount: number, batchSize: number): number {
    if (batchSize <= 0) return Math.min(Math.max(0, currentCount), Math.max(0, totalCount));
    return Math.min(Math.max(0, totalCount), Math.max(0, currentCount) + batchSize);
  }
}
