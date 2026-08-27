import { describe, expect, it } from 'vitest';

import type { PresentedScanRuleResult } from '@/lib/models/cleanup';

import { hasCleanupRuleDetails, isAggregateOnlyCleanupRule } from './cleanup-rule-details';

function createRule(overrides: Partial<PresentedScanRuleResult> = {}): PresentedScanRuleResult {
  return {
    ruleId: 'fixture.rule',
    category: 'system',
    group: 'system',
    risk: 'safe',
    defaultSelected: true,
    recommendedSelected: true,
    bytes: 128,
    fileCount: 1,
    available: true,
    selectable: false,
    status: 'found',
    runningProcesses: [],
    requiresAppClose: false,
    sources: [],
    sourceCount: 0,
    sourcesTruncated: false,
    scanElapsedMs: 1,
    name: 'Fixture rule',
    categoryLabel: 'System',
    description: '',
    impact: '',
    ...overrides,
  };
}

describe('cleanup rule details', () => {
  it('exposes aggregate-only details without localized impact text', () => {
    const rule = createRule({ selectable: true });

    expect(isAggregateOnlyCleanupRule(rule)).toBe(true);
    expect(hasCleanupRuleDetails(rule)).toBe(true);
  });

  it('exposes application-close requirements without other details', () => {
    expect(hasCleanupRuleDetails(createRule({ requiresAppClose: true }))).toBe(true);
  });

  it('exposes source-backed and localized details independently', () => {
    const source = { path: '/fixture', bytes: 128, fileCount: 1, modifiedAtMs: null, blockReason: null } as const;

    expect(hasCleanupRuleDetails(createRule({ sources: [source], sourceCount: 1 }))).toBe(true);
    expect(hasCleanupRuleDetails(createRule({ description: 'Fixture description' }))).toBe(true);
    expect(hasCleanupRuleDetails(createRule({ impact: 'Fixture impact' }))).toBe(true);
  });

  it('keeps rules without inspectable facts collapsed', () => {
    expect(hasCleanupRuleDetails(createRule())).toBe(false);
  });
});
