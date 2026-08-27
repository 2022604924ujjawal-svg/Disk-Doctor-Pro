import { ANALYSIS_SORT_KEYS } from '@/lib/models/analysis';
import type { DirectoryEntryInfo } from '@/lib/models/analysis';
import { SORT_DIRECTIONS } from '@/lib/models/sort';
import type { SortDirection } from '@/lib/models/sort';

export type AnalysisSortKey = (typeof ANALYSIS_SORT_KEYS)[keyof typeof ANALYSIS_SORT_KEYS];
export type { SortDirection };

/** Sorts analysis entries without reading component or Store state. */
export class AnalysisEntryUtils {
  static sort(
    entries: readonly DirectoryEntryInfo[],
    key: AnalysisSortKey,
    direction: SortDirection
  ): DirectoryEntryInfo[] {
    return [...entries].sort((left, right) => {
      const comparison = AnalysisEntryUtils.compare(left, right, key);
      return direction === SORT_DIRECTIONS.ascending ? comparison : -comparison;
    });
  }

  private static compare(left: DirectoryEntryInfo, right: DirectoryEntryInfo, key: AnalysisSortKey): number {
    if (key === ANALYSIS_SORT_KEYS.name) {
      return left.name.localeCompare(right.name, undefined, { numeric: true });
    }
    if (key === ANALYSIS_SORT_KEYS.bytes) return left.bytes - right.bytes;
    if (key === ANALYSIS_SORT_KEYS.fileCount) return left.fileCount - right.fileCount;
    return (left.modifiedAtMs ?? 0) - (right.modifiedAtMs ?? 0);
  }
}
