import { describe, expect, it } from 'vitest';

import type { ApplicationLeftoverCandidate } from '@/lib/models/application';

import {
  applicationLeftoverGroupSelection,
  groupApplicationLeftovers,
  recommendedApplicationLeftoverIds,
} from './application-leftover-groups';

function candidate(
  candidateId: string,
  applicationIdentifier: string,
  applicationName: string,
  bytes: number,
  defaultSelected = true
): ApplicationLeftoverCandidate {
  return {
    candidateId,
    applicationIdentifier,
    applicationName,
    source: 'sandboxContainer',
    path: `/fixture/${candidateId}`,
    bytes,
    fileCount: 1,
    modifiedAtMs: 1,
    confidence: 'high',
    defaultSelected,
    evidence: ['containerMetadataVerified'],
    snapshotFingerprint: `snapshot-${candidateId}`,
  };
}

describe('application leftover groups', () => {
  it('aggregates exact application identities and sorts by reclaimable bytes', () => {
    const groups = groupApplicationLeftovers([
      candidate('container-a', 'com.example.a', 'Example A', 10),
      candidate('preferences-a', 'com.example.a', 'Example A', 5),
      candidate('container-b', 'com.example.b', 'Example B', 20),
    ]);

    expect(groups.map(group => group.applicationIdentifier)).toEqual(['com.example.b', 'com.example.a']);
    expect(groups[1]).toMatchObject({
      applicationName: 'Example A',
      candidateIds: ['container-a', 'preferences-a'],
      bytes: 15,
      fileCount: 2,
    });
  });

  it('reports none, partial, and all selection states', () => {
    const candidateIds = ['container', 'preferences'];

    expect(applicationLeftoverGroupSelection(candidateIds, new Set())).toBe('none');
    expect(applicationLeftoverGroupSelection(candidateIds, new Set(['container']))).toBe('partial');
    expect(applicationLeftoverGroupSelection(candidateIds, new Set(['container', 'preferences']))).toBe('all');
  });

  it('uses only Core-recommended candidates for the smart selection', () => {
    const candidates = [
      candidate('recommended', 'com.example.a', 'Example A', 10, true),
      candidate('manual', 'com.example.b', 'Example B', 20, false),
    ];

    expect(recommendedApplicationLeftoverIds(candidates)).toEqual(['recommended']);
  });
});
