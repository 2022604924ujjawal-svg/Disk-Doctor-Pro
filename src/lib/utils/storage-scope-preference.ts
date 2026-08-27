import {
  MAX_RECENT_STORAGE_FOLDERS,
  STORAGE_SCOPE_IDS,
  type StorageScopeId,
  type StorageScopePreferences,
} from '@/lib/models/storage-scope';

import { PathUtils } from './path';

const STORAGE_SCOPE_ID_VALUES = new Set<string>(Object.values(STORAGE_SCOPE_IDS));

export class StorageScopePreferenceUtils {
  static parse(value: unknown): StorageScopePreferences {
    if (!this.hasExactKeys(value, ['selectedPaths', 'recentFolders'])) {
      throw new Error('Invalid storage scope preferences');
    }
    if (!this.isRecord(value.selectedPaths) || !Array.isArray(value.recentFolders)) {
      throw new Error('Invalid storage scope preferences');
    }

    const selectedPaths: Partial<Record<StorageScopeId, string>> = {};
    for (const [scopeId, path] of Object.entries(value.selectedPaths)) {
      if (!STORAGE_SCOPE_ID_VALUES.has(scopeId) || typeof path !== 'string' || !path.trim()) {
        throw new Error('Invalid storage scope selection');
      }
      selectedPaths[scopeId as StorageScopeId] = PathUtils.display(path.trim());
    }
    if (
      value.recentFolders.length > MAX_RECENT_STORAGE_FOLDERS ||
      value.recentFolders.some(path => typeof path !== 'string' || !path.trim())
    ) {
      throw new Error('Invalid recent storage folders');
    }
    const recentFolders = this.uniquePaths(value.recentFolders);
    if (recentFolders.length !== value.recentFolders.length) {
      throw new Error('Duplicate recent storage folders');
    }

    return {
      selectedPaths,
      recentFolders,
    };
  }

  static addRecentFolder(folders: readonly string[], path: string): string[] {
    return this.uniquePaths([path, ...folders]);
  }

  static removePath(paths: readonly string[], path: string): string[] {
    const removedKey = PathUtils.comparisonKey(path);
    return paths.filter(item => PathUtils.comparisonKey(item) !== removedKey);
  }

  static empty(): StorageScopePreferences {
    return { selectedPaths: {}, recentFolders: [] };
  }

  private static uniquePaths(values: readonly unknown[]): string[] {
    const keys = new Set<string>();
    const paths: string[] = [];
    for (const value of values) {
      if (typeof value !== 'string' || !value.trim()) continue;
      const path = PathUtils.display(value.trim());
      const key = PathUtils.comparisonKey(path);
      if (!key || keys.has(key)) continue;
      keys.add(key);
      paths.push(path);
      if (paths.length === MAX_RECENT_STORAGE_FOLDERS) break;
    }
    return paths;
  }

  private static isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }

  private static hasExactKeys<const Keys extends readonly string[]>(
    value: unknown,
    expectedKeys: Keys
  ): value is Record<Keys[number], unknown> {
    if (!this.isRecord(value)) return false;
    const actualKeys = Object.keys(value);
    return actualKeys.length === expectedKeys.length && expectedKeys.every(key => actualKeys.includes(key));
  }
}
