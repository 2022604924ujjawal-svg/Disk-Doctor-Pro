import { describe, expect, it } from 'vitest';

import { PathUtils } from '@/lib/utils/path';

describe('PathUtils.display', () => {
  it('removes Windows verbatim prefixes without depending on UNC prefix casing', () => {
    expect(PathUtils.display('\\\\?\\C:\\Users\\Developer')).toBe('C:\\Users\\Developer');
    expect(PathUtils.display('\\\\?\\unc\\Server\\Share')).toBe('\\\\Server\\Share');
  });

  it('keeps an absolute Windows drive root distinct from a drive-relative path', () => {
    expect(PathUtils.comparisonKey('C:\\')).toBe('c:\\');
    expect(PathUtils.comparisonKey('C:')).toBe('c:');
    expect(PathUtils.isSameOrChildKey(PathUtils.comparisonKey('C:\\fixture'), PathUtils.comparisonKey('C:\\'))).toBe(
      true
    );
  });
});

describe('PathUtils.collapseOverlappingRoots', () => {
  it('ignores descendants already covered by a selected parent', () => {
    expect(
      PathUtils.collapseOverlappingRoots([
        '/Users/developer/Downloads',
        '/Users/developer/Downloads/projects',
        '/Users/developer/Documents',
      ])
    ).toEqual(['/Users/developer/Downloads', '/Users/developer/Documents']);
  });

  it('replaces earlier descendants when a parent is added later', () => {
    expect(
      PathUtils.collapseOverlappingRoots([
        '/Users/developer/Downloads/projects',
        '/Users/developer/Downloads/assets',
        '/Users/developer/Downloads',
      ])
    ).toEqual(['/Users/developer/Downloads']);
  });

  it('compares Windows roots without case or separator differences', () => {
    expect(
      PathUtils.collapseOverlappingRoots([
        'C:\\Users\\Developer\\Downloads',
        'c:/users/developer/downloads/projects',
        'D:\\Archive',
      ])
    ).toEqual(['C:\\Users\\Developer\\Downloads', 'D:\\Archive']);
  });

  it('keeps siblings whose names only share a prefix', () => {
    expect(
      PathUtils.collapseOverlappingRoots(['/Users/developer/Downloads', '/Users/developer/Downloads-archive'])
    ).toEqual(['/Users/developer/Downloads', '/Users/developer/Downloads-archive']);
  });
});
