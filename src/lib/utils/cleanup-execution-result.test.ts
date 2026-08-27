import { describe, expect, it } from 'vitest';

import type { CleanupResult, CleanupScanResult, ScanRuleResult } from '@/lib/models/cleanup';

import { CleanupExecutionResultUtils } from './cleanup-execution-result';

function rule(ruleId: string, risk: ScanRuleResult['risk'] = 'safe'): ScanRuleResult {
  return {
    ruleId,
    category: 'application',
    group: 'userCache',
    risk,
    defaultSelected: true,
    recommendedSelected: true,
    bytes: 300,
    fileCount: 3,
    available: true,
    selectable: true,
    status: 'found',
    runningProcesses: [],
    requiresAppClose: false,
    sources: [
      { path: `/${ruleId}/a`, bytes: 100, fileCount: 1, modifiedAtMs: null, blockReason: null },
      { path: `/${ruleId}/b`, bytes: 200, fileCount: 2, modifiedAtMs: null, blockReason: null },
    ],
    sourceCount: 2,
    sourcesTruncated: false,
    scanElapsedMs: 1,
  };
}

function scan(rules: ScanRuleResult[]): CleanupScanResult {
  return {
    schemaVersion: '2',
    scannedAtMs: 1,
    disk: {
      name: 'Fixture',
      mountPoint: '/',
      totalBytes: 1_000,
      availableBytes: 400,
      usedBytes: 600,
    },
    rules,
    applicationIcons: [],
    warningCount: 0,
    safeBytes: rules.filter(item => item.risk === 'safe').reduce((total, item) => total + item.bytes, 0),
    reclaimableBytes: rules.reduce((total, item) => total + item.bytes, 0),
    applicabilityElapsedMs: 0,
    applicableRuleCount: rules.length,
    filteredRuleCount: 0,
    inventoryApplicationCount: 0,
    inventoryProcessCount: 0,
    elapsedMs: 1,
  };
}

function result(status: 'completed' | 'partial'): CleanupResult {
  return {
    planId: 'plan-1',
    planHash: 'hash-1',
    expectedBytes: 300,
    releasedBytes: status === 'completed' ? 300 : 100,
    affectedItemCount: 1,
    failedItemCount: status === 'partial' ? 1 : 0,
    dryRun: false,
    actions: [
      {
        ruleId: 'completed',
        actionKind: 'delete',
        status,
        reasonCode: status === 'partial' ? 'itemsSkipped' : null,
        bytesExpected: 300,
        releasedBytes: status === 'completed' ? 300 : 100,
        affectedItemCount: 1,
        failedItemCount: status === 'partial' ? 1 : 0,
        runningProcesses: [],
      },
    ],
    record: {
      schemaVersion: 2,
      operationId: 'operation-1',
      category: 'deepCleanup',
      startedAtMs: 1,
      finishedAtMs: 2,
      outcome: status === 'completed' ? 'completed' : 'completedWithWarnings',
      dryRun: false,
      selectedItemCount: 1,
      affectedItemCount: 1,
      expectedBytes: 300,
      releasedBytes: status === 'completed' ? 300 : 100,
      releasedBytesIsEstimate: false,
      failedItemCount: status === 'partial' ? 1 : 0,
      details: {
        type: 'deepCleanup',
        payload: {
          cleanup: {
            selectedRuleIds: ['completed'],
            expectedBytes: 300,
            actions: [],
          },
          applicationLeftovers: null,
        },
      },
    },
    historySaved: true,
  };
}

describe('cleanup execution result synchronization', () => {
  it('marks a fully cleaned rule as clean and updates aggregate bytes', () => {
    const snapshot = scan([rule('completed'), rule('remaining', 'recoverable')]);
    const updated = CleanupExecutionResultUtils.apply(snapshot, result('completed'), []);

    expect(updated.rules[0]).toMatchObject({
      ruleId: 'completed',
      bytes: 0,
      fileCount: 0,
      selectable: false,
      status: 'clean',
      sources: [],
    });
    expect(updated.safeBytes).toBe(0);
    expect(updated.reclaimableBytes).toBe(300);
  });

  it('keeps only sources excluded from a completed cleanup', () => {
    const snapshot = scan([rule('completed')]);
    const updated = CleanupExecutionResultUtils.apply(snapshot, result('completed'), [
      { ruleId: 'completed', mode: 'exclude', paths: ['/completed/a'] },
    ]);

    expect(updated.rules[0]).toMatchObject({
      bytes: 100,
      fileCount: 1,
      sourceCount: 1,
      selectable: true,
      status: 'found',
    });
    expect(updated.rules[0].sources.map(source => source.path)).toEqual(['/completed/a']);
    expect(updated.reclaimableBytes).toBe(100);
  });

  it('subtracts verified effects when Core reports a partial outcome', () => {
    const snapshot = scan([rule('completed')]);
    const updated = CleanupExecutionResultUtils.apply(snapshot, result('partial'), []);

    expect(updated.rules[0]).toMatchObject({
      bytes: 200,
      fileCount: 2,
      sourceCount: 0,
      sources: [],
      sourcesTruncated: true,
      selectable: true,
      status: 'found',
    });
    expect(updated.safeBytes).toBe(200);
    expect(updated.reclaimableBytes).toBe(200);
  });

  it('keeps skipped items retryable after all measured bytes were released', () => {
    const snapshot = scan([rule('completed')]);
    const partialResult = result('partial');
    partialResult.actions[0].releasedBytes = 300;
    partialResult.actions[0].affectedItemCount = 2;

    const updated = CleanupExecutionResultUtils.apply(snapshot, partialResult, []);

    expect(updated.rules[0]).toMatchObject({
      bytes: 0,
      fileCount: 1,
      selectable: true,
      status: 'found',
    });
    expect(updated.safeBytes).toBe(0);
    expect(updated.reclaimableBytes).toBe(0);
  });

  it('aggregates verified effects when one of multiple actions is not completed', () => {
    const snapshot = scan([rule('completed')]);
    const mixedResult = result('completed');
    mixedResult.actions.push({
      ...mixedResult.actions[0],
      status: 'failed',
      reasonCode: 'verificationFailed',
      releasedBytes: 0,
      affectedItemCount: 0,
      failedItemCount: 1,
    });

    expect(CleanupExecutionResultUtils.apply(snapshot, mixedResult, []).rules[0]).toMatchObject({
      bytes: 0,
      fileCount: 2,
      sources: [],
      sourcesTruncated: true,
      selectable: true,
    });
  });

  it('retains the original snapshot when a partial outcome changed nothing', () => {
    const snapshot = scan([rule('completed')]);
    const unchangedResult = result('partial');
    unchangedResult.actions[0].releasedBytes = 0;
    unchangedResult.actions[0].affectedItemCount = 0;

    expect(CleanupExecutionResultUtils.apply(snapshot, unchangedResult, [])).toBe(snapshot);
  });

  it('invalidates source selection only for partially changed rules', () => {
    const partialResult = result('partial');
    expect(CleanupExecutionResultUtils.invalidatedSourceRuleIds(partialResult)).toEqual(new Set(['completed']));

    partialResult.actions[0].releasedBytes = 0;
    partialResult.actions[0].affectedItemCount = 0;
    expect(CleanupExecutionResultUtils.invalidatedSourceRuleIds(partialResult)).toEqual(new Set());
  });
});
