import type { DiskInfo } from '@/lib/models/disk';
import { PathUtils } from '@/lib/utils/path';

/** Matches paths to already loaded disks without performing native I/O. */
export class DiskUtils {
  static findForPath(disks: DiskInfo[], path: string, fallback: DiskInfo | null = null): DiskInfo | null {
    const normalizedPath = PathUtils.comparisonKey(path);
    if (!normalizedPath) return fallback;

    // Prefer specific mounts and enforce path boundaries so similarly prefixed
    // volumes cannot match one another.
    return (
      [...disks]
        .sort((left, right) => PathUtils.display(right.mountPoint).length - PathUtils.display(left.mountPoint).length)
        .find(disk => {
          const mountPoint = PathUtils.comparisonKey(disk.mountPoint);
          return PathUtils.isSameOrChildKey(normalizedPath, mountPoint);
        }) ?? fallback
    );
  }
}
