import { DUPLICATE_KEEPER_RULE_IDS } from '@/lib/models/duplicate-file';
import type { DuplicateFileEntry, DuplicateGroup, DuplicateKeeperRuleId } from '@/lib/models/duplicate-file';

export class DuplicateFileSelectionUtils {
  /**
   * Selects removable copies while preserving one suggested keeper per group.
   * Changing the policy can recompute selections without filesystem I/O.
   */
  static keeper(entries: readonly DuplicateFileEntry[], rule: DuplicateKeeperRuleId): DuplicateFileEntry | undefined {
    return [...entries].sort((left, right) => this.compare(left, right, rule))[0];
  }

  static suggestedPaths(groups: readonly DuplicateGroup[], rule: DuplicateKeeperRuleId): string[] {
    return groups.flatMap(group => {
      const keeper = this.keeper(group.entries, rule);
      return group.entries.filter(entry => entry.path !== keeper?.path).map(entry => entry.path);
    });
  }

  static selectedEntries(groups: readonly DuplicateGroup[], selectedPaths: readonly string[]): DuplicateFileEntry[] {
    const selected = new Set(selectedPaths);
    return groups.flatMap(group => group.entries).filter(entry => selected.has(entry.path));
  }

  static updateEntrySelection(
    selectedPaths: readonly string[],
    entry: DuplicateFileEntry,
    group: DuplicateGroup,
    selected: boolean
  ): string[] {
    const next = new Set(selectedPaths);
    if (selected) {
      // Every interaction must leave at least one copy in the group.
      const hasOtherKeeper = group.entries.some(item => item.path !== entry.path && !next.has(item.path));
      if (!hasOtherKeeper) return [...selectedPaths];
      next.add(entry.path);
    } else {
      next.delete(entry.path);
    }
    return [...next];
  }

  static selectGroupCopies(
    selectedPaths: readonly string[],
    group: DuplicateGroup,
    rule: DuplicateKeeperRuleId
  ): string[] {
    const keeper = this.keeper(group.entries, rule);
    const next = new Set(selectedPaths);
    group.entries.forEach(entry => {
      if (entry.path === keeper?.path) next.delete(entry.path);
      else next.add(entry.path);
    });
    return [...next];
  }

  static toggleGroupCopies(
    selectedPaths: readonly string[],
    group: DuplicateGroup,
    rule: DuplicateKeeperRuleId
  ): string[] {
    const keeper = this.keeper(group.entries, rule);
    const selected = new Set(selectedPaths);
    const selectionApplied = group.entries.every(entry => selected.has(entry.path) === (entry.path !== keeper?.path));
    if (!selectionApplied) return this.selectGroupCopies(selectedPaths, group, rule);

    // Clearing only this group preserves selections made in every other
    // duplicate group and makes the group action behave as a true toggle.
    group.entries.forEach(entry => selected.delete(entry.path));
    return [...selected];
  }

  private static compare(left: DuplicateFileEntry, right: DuplicateFileEntry, rule: DuplicateKeeperRuleId): number {
    if (rule === DUPLICATE_KEEPER_RULE_IDS.shortestName) {
      return (
        left.name.length - right.name.length ||
        left.name.localeCompare(right.name, undefined, { numeric: true }) ||
        this.compareShortestPath(left, right)
      );
    }

    if (rule === DUPLICATE_KEEPER_RULE_IDS.oldestModified) {
      return this.compareModifiedTime(left, right, false) || this.compareShortestPath(left, right);
    }

    if (rule === DUPLICATE_KEEPER_RULE_IDS.newestModified) {
      return this.compareModifiedTime(left, right, true) || this.compareShortestPath(left, right);
    }

    /*
     * Preserve the shortest path by default, then use modification time and
     * the full path as deterministic tie-breakers.
     */
    return (
      left.path.length - right.path.length ||
      this.compareModifiedTime(left, right, false) ||
      left.path.localeCompare(right.path, undefined, { numeric: true })
    );
  }

  private static compareShortestPath(left: DuplicateFileEntry, right: DuplicateFileEntry): number {
    return left.path.length - right.path.length || left.path.localeCompare(right.path, undefined, { numeric: true });
  }

  /**
   * Files without modification times sort after known values. Stable path
   * ordering resolves groups where every timestamp is unavailable.
   */
  private static compareModifiedTime(
    left: DuplicateFileEntry,
    right: DuplicateFileEntry,
    newestFirst: boolean
  ): number {
    if (left.modifiedAtMs === null && right.modifiedAtMs === null) return 0;
    if (left.modifiedAtMs === null) return 1;
    if (right.modifiedAtMs === null) return -1;
    return newestFirst ? right.modifiedAtMs - left.modifiedAtMs : left.modifiedAtMs - right.modifiedAtMs;
  }
}
