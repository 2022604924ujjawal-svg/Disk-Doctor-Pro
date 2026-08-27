import { describe, expect, it } from 'vitest';

import { DUPLICATE_GROUP_KINDS, DUPLICATE_KEEPER_RULE_IDS } from '@/lib/models/duplicate-file';
import type { DuplicateGroup } from '@/lib/models/duplicate-file';

import { DuplicateFileSelectionUtils } from './duplicate-file-selection';

const group: DuplicateGroup = {
  id: 'group-1',
  hash: 'hash-1',
  kind: DUPLICATE_GROUP_KINDS.file,
  bytesPerFile: 1024,
  fileCountPerEntry: 1,
  reclaimableBytes: 1024,
  entries: [
    {
      name: 'report.pdf',
      path: '/docs/report.pdf',
      parentPath: '/docs',
      bytes: 1024,
      modifiedAtMs: 1,
    },
    {
      name: 'report-copy.pdf',
      path: '/archive/reports/report-copy.pdf',
      parentPath: '/archive/reports',
      bytes: 1024,
      modifiedAtMs: 2,
    },
  ],
};

describe('duplicate file selection utilities', () => {
  it('selects removable copies while preserving selections from other groups', () => {
    expect(
      DuplicateFileSelectionUtils.toggleGroupCopies(
        ['/other/group-copy.pdf'],
        group,
        DUPLICATE_KEEPER_RULE_IDS.shortestPath
      )
    ).toEqual(['/other/group-copy.pdf', '/archive/reports/report-copy.pdf']);
  });

  it('clears only the active group when its suggested selection is already applied', () => {
    expect(
      DuplicateFileSelectionUtils.toggleGroupCopies(
        ['/other/group-copy.pdf', '/archive/reports/report-copy.pdf'],
        group,
        DUPLICATE_KEEPER_RULE_IDS.shortestPath
      )
    ).toEqual(['/other/group-copy.pdf']);
  });
});
