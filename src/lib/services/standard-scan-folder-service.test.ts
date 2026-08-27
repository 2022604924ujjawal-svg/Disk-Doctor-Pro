import { beforeEach, describe, expect, it, vi } from 'vitest';

const pathMocks = vi.hoisted(() => ({
  audioDir: vi.fn(),
  documentDir: vi.fn(),
  downloadDir: vi.fn(),
  pictureDir: vi.fn(),
  videoDir: vi.fn(),
  filterExistingDirectories: vi.fn(),
}));

vi.mock('@tauri-apps/api/path', () => ({
  audioDir: pathMocks.audioDir,
  documentDir: pathMocks.documentDir,
  downloadDir: pathMocks.downloadDir,
  pictureDir: pathMocks.pictureDir,
  videoDir: pathMocks.videoDir,
}));

vi.mock('@/lib/services/folder-selection-service', () => ({
  FolderSelectionService: {
    filterExistingDirectories: pathMocks.filterExistingDirectories,
  },
}));

import {
  findStandardScanFolderByPath,
  STANDARD_SCAN_FOLDER_IDS,
  StandardScanFolderService,
} from '@/lib/services/standard-scan-folder-service';

describe('StandardScanFolderService', () => {
  beforeEach(() => {
    pathMocks.downloadDir.mockResolvedValue('/Users/test/Downloads');
    pathMocks.documentDir.mockResolvedValue('/Users/test/Documents');
    pathMocks.pictureDir.mockResolvedValue('/Users/test/Pictures');
    pathMocks.videoDir.mockResolvedValue('/Users/test/Movies');
    pathMocks.audioDir.mockResolvedValue('/Users/test/Music');
    pathMocks.filterExistingDirectories.mockImplementation(async (paths: string[]) => paths);
  });

  it('returns standard folders in a stable product order', async () => {
    const folders = await StandardScanFolderService.listAvailable();

    expect(folders.map(folder => folder.id)).toEqual([
      STANDARD_SCAN_FOLDER_IDS.downloads,
      STANDARD_SCAN_FOLDER_IDS.documents,
      STANDARD_SCAN_FOLDER_IDS.pictures,
      STANDARD_SCAN_FOLDER_IDS.videos,
      STANDARD_SCAN_FOLDER_IDS.music,
    ]);
  });

  it('omits unresolved or missing operating-system folders', async () => {
    pathMocks.pictureDir.mockRejectedValue(new Error('picture directory unavailable'));
    pathMocks.filterExistingDirectories.mockImplementation(async (paths: string[]) =>
      paths.filter(path => !path.endsWith('/Movies'))
    );

    const folders = await StandardScanFolderService.listAvailable();

    expect(folders.map(folder => folder.id)).toEqual([
      STANDARD_SCAN_FOLDER_IDS.downloads,
      STANDARD_SCAN_FOLDER_IDS.documents,
      STANDARD_SCAN_FOLDER_IDS.music,
    ]);
  });

  it('matches a selected standard folder by its physical path', () => {
    const folders = [
      { id: STANDARD_SCAN_FOLDER_IDS.downloads, path: '/Users/test/Downloads' },
      { id: STANDARD_SCAN_FOLDER_IDS.documents, path: '/Users/test/Documents' },
    ];

    expect(findStandardScanFolderByPath(folders, '/Users/test/Downloads')?.id).toBe(STANDARD_SCAN_FOLDER_IDS.downloads);
    expect(findStandardScanFolderByPath(folders, '/Users/test/Projects')).toBeNull();
  });

  it('uses platform-aware comparison for Windows standard folders', () => {
    const folders = [{ id: STANDARD_SCAN_FOLDER_IDS.downloads, path: 'C:\\Users\\Test\\Downloads' }];

    expect(findStandardScanFolderByPath(folders, 'c:/users/test/downloads/')?.id).toBe(
      STANDARD_SCAN_FOLDER_IDS.downloads
    );
  });
});
