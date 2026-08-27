import type { DiskInfo } from '@/lib/models/disk';
import { PathUtils } from '@/lib/utils/path';

export interface AnalysisBreadcrumb {
  label: string;
  path: string;
}

/** Builds breadcrumbs consistently for Windows volumes and Unix mounts. */
export class AnalysisBreadcrumbUtils {
  static create(path: string, activeDisk: DiskInfo | null, localDiskLabel: string): AnalysisBreadcrumb[] {
    const normalized = PathUtils.display(path);
    if (!normalized) return [];

    const driveMatch = normalized.match(/^([A-Za-z]:)[\\/]*(.*)$/);
    if (driveMatch) {
      const drive = driveMatch[1];
      const parts = driveMatch[2].split(/[\\/]+/).filter(Boolean);
      const result = [{ label: `${localDiskLabel} (${drive})`, path: `${drive}\\` }];
      let current = `${drive}\\`;
      for (const part of parts) {
        current = `${current}${part}\\`;
        result.push({ label: part, path: current });
      }
      return result;
    }

    if (!normalized.startsWith('/')) {
      return [{ label: normalized, path: normalized }];
    }

    const diskMountPoint = PathUtils.display(activeDisk?.mountPoint ?? '/').replace(/\/+$/, '');
    const mountPoint = diskMountPoint || '/';
    const isOnActiveDisk =
      normalized === mountPoint || normalized.startsWith(mountPoint === '/' ? '/' : `${mountPoint}/`);
    const rootPath = isOnActiveDisk ? mountPoint : '/';
    const rootLabel = isOnActiveDisk ? (activeDisk?.name ?? rootPath) : rootPath;
    const relativePath = normalized.slice(rootPath === '/' ? 1 : rootPath.length).replace(/^\/+/, '');
    const result = [{ label: rootLabel, path: rootPath }];
    let current = rootPath;
    for (const part of relativePath.split('/').filter(Boolean)) {
      current = current === '/' ? `/${part}` : `${current}/${part}`;
      result.push({ label: part, path: current });
    }
    return result;
  }
}
