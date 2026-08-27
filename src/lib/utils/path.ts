/** Normalizes path strings without reading the filesystem or platform state. */
export class PathUtils {
  static display(path: string): string {
    if (path.slice(0, 8).toLocaleUpperCase('en-US') === '\\\\?\\UNC\\') return `\\\\${path.slice(8)}`;
    if (path.slice(0, 4) === '\\\\?\\') return path.slice(4);
    return path;
  }

  static fileName(path: string): string {
    const displayPath = PathUtils.display(path);
    const normalized = displayPath.replace(/[\\/]+$/, '');
    if (!normalized) return displayPath.startsWith('/') ? '/' : displayPath;
    return normalized.split(/[\\/]/).filter(Boolean).at(-1) ?? normalized;
  }

  /**
   * Cache keys fold case only for Windows paths and retain one separator for
   * roots. Unix keys preserve case for case-sensitive volumes.
   */
  static comparisonKey(path: string): string {
    const displayPath = PathUtils.display(path);
    const isWindowsPath = /^[A-Za-z]:[\\/]?/u.test(displayPath) || displayPath.startsWith('\\\\');
    const normalized = displayPath.replaceAll('/', '\\');
    const windowsDriveRoot = /^[A-Za-z]:\\+$/u.test(normalized);
    const withoutTrailingSeparators =
      normalized === '\\' || windowsDriveRoot ? normalized.replace(/\\+$/u, '\\') : normalized.replace(/\\+$/u, '');
    return isWindowsPath ? withoutTrailingSeparators.toLocaleLowerCase('en-US') : withoutTrailingSeparators;
  }

  static isSameOrChildKey(pathKey: string, rootKey: string): boolean {
    if (pathKey === rootKey) return true;
    if (rootKey.endsWith('\\')) return pathKey.startsWith(rootKey);
    return pathKey.startsWith(`${rootKey}\\`);
  }

  /**
   * Reduces scan roots to the smallest set that covers the same filesystem
   * scope. A selected parent makes its descendants redundant; adding a parent
   * later also replaces descendants that were selected earlier.
   */
  static collapseOverlappingRoots(paths: string[]): string[] {
    return paths.map(PathUtils.display).reduce<string[]>((roots, path) => {
      const pathKey = PathUtils.comparisonKey(path);
      const alreadyCovered = roots.some(root => PathUtils.isSameOrChildKey(pathKey, PathUtils.comparisonKey(root)));
      if (alreadyCovered) return roots;

      return [...roots.filter(root => !PathUtils.isSameOrChildKey(PathUtils.comparisonKey(root), pathKey)), path];
    }, []);
  }
}
