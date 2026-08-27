import { LARGE_FILE_SORT_KEYS } from '@/lib/models/large-file';
import { SORT_DIRECTIONS } from '@/lib/models/sort';
import type { SortDirection } from '@/lib/models/sort';
import type { LargeFileEntry } from '@/lib/models/large-file';

export type LargeFileSortKey = (typeof LARGE_FILE_SORT_KEYS)[keyof typeof LARGE_FILE_SORT_KEYS];

export interface LargeFileSelectionState {
  checked: boolean;
  indeterminate: boolean;
}

/**
 * Centralizes pure large-file sorting and selection behavior so every view
 * applies the same result semantics.
 */
export class LargeFileEntryUtils {
  static sorted(
    entries: readonly LargeFileEntry[],
    sortKey: LargeFileSortKey,
    direction: SortDirection
  ): LargeFileEntry[] {
    const directionFactor = direction === SORT_DIRECTIONS.ascending ? 1 : -1;
    return [...entries].sort((left, right) => {
      let comparison: number;
      if (sortKey === LARGE_FILE_SORT_KEYS.name) {
        comparison = left.name.localeCompare(right.name, undefined, { numeric: true });
      } else if (sortKey === LARGE_FILE_SORT_KEYS.modified) {
        comparison = (left.modifiedAtMs ?? 0) - (right.modifiedAtMs ?? 0);
      } else {
        comparison = left.bytes - right.bytes;
      }
      return comparison * directionFactor;
    });
  }

  static selectedEntries(entries: readonly LargeFileEntry[], selectedPaths: readonly string[]): LargeFileEntry[] {
    const selected = new Set(selectedPaths);
    return entries.filter(entry => selected.has(entry.path));
  }

  static selectionState(
    entries: readonly LargeFileEntry[],
    selectedPaths: ReadonlySet<string>
  ): LargeFileSelectionState {
    const selectedCount = entries.reduce((count, entry) => count + Number(selectedPaths.has(entry.path)), 0);
    return {
      checked: entries.length > 0 && selectedCount === entries.length,
      indeterminate: selectedCount > 0 && selectedCount < entries.length,
    };
  }

  static updateSelection(
    selectedPaths: readonly string[],
    targetPaths: readonly string[],
    selected: boolean
  ): string[] {
    const next = new Set(selectedPaths);
    targetPaths.forEach(path => {
      if (selected) next.add(path);
      else next.delete(path);
    });
    return [...next];
  }
}
