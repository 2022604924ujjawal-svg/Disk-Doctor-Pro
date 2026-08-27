export const ANALYSIS_RESULT_CACHE_LIMIT = 80;

export const ANALYSIS_VIEW_IDS = {
  treemap: 'treemap',
  details: 'details',
} as const;

export type AnalysisViewId = (typeof ANALYSIS_VIEW_IDS)[keyof typeof ANALYSIS_VIEW_IDS];

export const ANALYSIS_SORT_KEYS = {
  name: 'name',
  bytes: 'bytes',
  fileCount: 'fileCount',
  modified: 'modified',
} as const;

export const TREEMAP_TILE_KINDS = {
  entry: 'entry',
  remainder: 'remainder',
} as const;

export interface DirectoryEntryInfo {
  name: string;
  path: string;
  bytes: number;
  fileCount: number;
  isDirectory: boolean;
  modifiedAtMs: number | null;
  contentFingerprint: string | null;
}

export interface AnalysisResult {
  scanId: number;
  root: string;
  scannedAtMs: number;
  totalBytes: number;
  skippedCount: number;
  entries: DirectoryEntryInfo[];
}

export interface AnalysisDeleteResult {
  removedPath: string;
  releasedBytes: number;
  removedFileCount: number;
}

interface TreemapTileRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export type TreemapTile = TreemapTileRect &
  (
    | {
        kind: typeof TREEMAP_TILE_KINDS.entry;
        entry: DirectoryEntryInfo;
        bytes: number;
      }
    | {
        kind: typeof TREEMAP_TILE_KINDS.remainder;
        entry: null;
        bytes: number;
        entryCount: number;
      }
  );
