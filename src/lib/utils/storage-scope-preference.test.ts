import { describe, expect, it } from 'vitest';

import { MAX_RECENT_STORAGE_FOLDERS } from '@/lib/models/storage-scope';

import { StorageScopePreferenceUtils } from './storage-scope-preference';

describe('StorageScopePreferenceUtils', () => {
  it('parses the current storage scope document', () => {
    expect(
      StorageScopePreferenceUtils.parse({
        selectedPaths: {
          analysis: '/Users/example/Downloads',
        },
        recentFolders: ['C:\\Users\\example\\Downloads', '/Users/example/Downloads'],
      })
    ).toEqual({
      selectedPaths: {
        analysis: '/Users/example/Downloads',
      },
      recentFolders: ['C:\\Users\\example\\Downloads', '/Users/example/Downloads'],
    });
  });

  it('rejects obsolete or partially valid storage scope documents', () => {
    expect(() =>
      StorageScopePreferenceUtils.parse({
        selectedPaths: {
          analysis: '/Users/example/Downloads',
          unknown: '/ignored',
        },
        recentFolders: ['/Users/example/Downloads'],
      })
    ).toThrow('Invalid storage scope selection');
  });

  it('moves a selected folder to the front and caps history size', () => {
    const existing = Array.from({ length: MAX_RECENT_STORAGE_FOLDERS }, (_, index) => `/workspace/${index}`);

    expect(StorageScopePreferenceUtils.addRecentFolder(existing, '/workspace/new')).toEqual([
      '/workspace/new',
      ...existing.slice(0, MAX_RECENT_STORAGE_FOLDERS - 1),
    ]);
    expect(StorageScopePreferenceUtils.addRecentFolder(existing, '/workspace/3')[0]).toBe('/workspace/3');
  });

  it('removes equivalent Windows paths without affecting other entries', () => {
    expect(
      StorageScopePreferenceUtils.removePath(
        ['C:\\Users\\example\\Downloads', 'D:\\Projects'],
        'c:/Users/example/Downloads/'
      )
    ).toEqual(['D:\\Projects']);
  });
});
