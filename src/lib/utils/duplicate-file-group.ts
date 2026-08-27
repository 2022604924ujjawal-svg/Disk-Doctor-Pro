import { DUPLICATE_GROUP_KINDS, type DuplicateGroup } from '@/lib/models/duplicate-file';
import { FILE_CATEGORY_IDS, type FileCategoryId } from '@/lib/models/file-category';
import { FileTypeUtils } from '@/lib/utils/file-type';
import { PathUtils } from '@/lib/utils/path';

/** Builds concise labels that still distinguish same-name exact-content groups. */
export class DuplicateFileGroupUtils {
  /** Directory aggregates represent mixed contents and therefore belong to Other. */
  static category(group: DuplicateGroup): FileCategoryId {
    if (group.kind === DUPLICATE_GROUP_KINDS.directory) return FILE_CATEGORY_IDS.other;
    return FileTypeUtils.category(group.entries[0]?.name ?? '');
  }

  /** Counts the regular files represented by every file or directory copy. */
  static representedFileCount(group: DuplicateGroup): number {
    return group.entries.length * group.fileCountPerEntry;
  }

  static displayLabels(groups: readonly DuplicateGroup[]): ReadonlyMap<string, string> {
    // Directory aggregates are already distinguished by their child paths and
    // contents. Showing a sampled parent path in the heading adds noise without
    // helping users decide which copies to keep.
    const directoryLabels = new Map(
      groups
        .filter(group => group.kind === DUPLICATE_GROUP_KINDS.directory)
        .map(group => [group.id, group.entries[0]?.name ?? ''] as const)
    );
    const fileGroups = groups.filter(group => group.kind !== DUPLICATE_GROUP_KINDS.directory);
    const nameCounts = DuplicateFileGroupUtils.countBy(fileGroups, group => group.entries[0]?.name ?? '');
    const baseLabels = new Map(
      groups.map(group => {
        const entry = group.entries[0];
        const name = entry?.name ?? '';
        if (group.kind === DUPLICATE_GROUP_KINDS.directory || (nameCounts.get(name) ?? 0) < 2) {
          return [group.id, name] as const;
        }

        const parentName = entry ? PathUtils.fileName(entry.parentPath) : '';
        return [group.id, parentName && parentName !== name ? `${name} · ${parentName}` : name] as const;
      })
    );
    const labelCounts = DuplicateFileGroupUtils.countBy(fileGroups, group => baseLabels.get(group.id) ?? '');
    const pathLabels = new Map(
      groups.map(group => {
        const entry = group.entries[0];
        const baseLabel = directoryLabels.get(group.id) ?? baseLabels.get(group.id) ?? '';
        if (group.kind === DUPLICATE_GROUP_KINDS.directory) return [group.id, baseLabel] as const;
        if ((labelCounts.get(baseLabel) ?? 0) < 2 || !entry) return [group.id, baseLabel] as const;
        return [group.id, `${entry.name} · ${PathUtils.display(entry.parentPath)}`] as const;
      })
    );
    const pathLabelCounts = DuplicateFileGroupUtils.countBy(fileGroups, group => pathLabels.get(group.id) ?? '');

    return new Map(
      groups.map(group => {
        if (group.kind === DUPLICATE_GROUP_KINDS.directory) {
          return [group.id, directoryLabels.get(group.id) ?? ''] as const;
        }
        const label = pathLabels.get(group.id) ?? '';
        return [
          group.id,
          (pathLabelCounts.get(label) ?? 0) > 1 ? `${label} · ${group.hash.slice(0, 8)}` : label,
        ] as const;
      })
    );
  }

  private static countBy(
    groups: readonly DuplicateGroup[],
    value: (group: DuplicateGroup) => string
  ): Map<string, number> {
    const counts = new Map<string, number>();
    for (const group of groups) {
      const key = value(group);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return counts;
  }
}
