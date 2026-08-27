export interface PermanentDeleteCandidate {
  path: string;
  expectedBytes: number;
  expectedModifiedAtMs: number | null;
}

export interface PermanentDeleteFailure {
  path: string;
  message: string;
}

export interface PermanentDeleteBatchResult {
  removedPaths: string[];
  failed: PermanentDeleteFailure[];
  releasedBytes: number;
}
