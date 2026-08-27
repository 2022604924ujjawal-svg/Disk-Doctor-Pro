export type FileIconItemKind = 'file' | 'directory';
export type FileIconMode = 'automatic' | 'generic' | 'path';

export interface FileIconRequest {
  path: string;
  kind: FileIconItemKind;
  mode: FileIconMode;
}

export interface FileIconAssignment {
  path: string;
  kind: FileIconItemKind;
  mode: FileIconMode;
  iconKey: string;
}

export interface FileIconAsset {
  iconKey: string;
  dataUrl: string;
}

export interface FileIconBatch {
  assignments: FileIconAssignment[];
  assets: FileIconAsset[];
}
