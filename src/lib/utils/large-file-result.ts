import type { LargeFilesResult } from '@/lib/models/large-file';

/** Updates large-file aggregates after the Store completes a permanent-delete operation. */
export class LargeFileResultUtils {
  static removePaths(
    result: LargeFilesResult,
    removedPaths: ReadonlySet<string>,
    releasedBytes: number
  ): LargeFilesResult {
    const entries = result.entries.filter(entry => !removedPaths.has(entry.path));
    const totalCount = Math.max(0, result.totalCount - removedPaths.size);
    return {
      ...result,
      totalBytes: Math.max(0, result.totalBytes - releasedBytes),
      totalCount,
      returnedCount: entries.length,
      truncated: entries.length < totalCount,
      entries,
    };
  }
}
