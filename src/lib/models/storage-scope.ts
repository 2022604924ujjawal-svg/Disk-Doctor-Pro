export const STORAGE_SCOPE_IDS = {
  analysis: 'analysis',
  largeFiles: 'large-files',
  duplicateFiles: 'duplicate-files',
} as const;

export type StorageScopeId = (typeof STORAGE_SCOPE_IDS)[keyof typeof STORAGE_SCOPE_IDS];

export const MAX_RECENT_STORAGE_FOLDERS = 8;

export interface StorageScopePreferences {
  selectedPaths: Partial<Record<StorageScopeId, string>>;
  recentFolders: string[];
}
