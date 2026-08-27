import { describe, expect, it } from 'vitest';

import type { ScanRuleResult } from '@/lib/models/cleanup';

import { CleanupRuleSelectionUtils } from './cleanup-rule-selection';

const rule: ScanRuleResult = {
  ruleId: 'app.cache',
  category: 'application',
  group: 'userCache',
  risk: 'safe',
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
    { path: '/cache/a', bytes: 100, fileCount: 1, modifiedAtMs: null, blockReason: null },
    { path: '/cache/b', bytes: 200, fileCount: 2, modifiedAtMs: null, blockReason: null },
  ],
  sourceCount: 2,
  sourcesTruncated: false,
  scanElapsedMs: 1,
};

describe('cleanup source selection', () => {
  it('selects interactive recommendations and lets preflight handle running applications', () => {
    const recoverableRecommendation = {
      ...rule,
      ruleId: 'dev.rebuildable-cache',
      risk: 'recoverable' as const,
      defaultSelected: false,
      recommendedSelected: true,
    };
    const blockedRecommendation = {
      ...recoverableRecommendation,
      ruleId: 'app.running-cache',
      status: 'requiresClose' as const,
      runningProcesses: ['Example'],
      requiresAppClose: true,
    };

    expect(
      CleanupRuleSelectionUtils.defaultSelectedRuleIds([rule, recoverableRecommendation, blockedRecommendation])
    ).toEqual(['app.cache', 'dev.rebuildable-cache', 'app.running-cache']);
  });

  it('calculates include and exclude selections without changing whole-rule semantics', () => {
    expect(CleanupRuleSelectionUtils.selectedBytes([rule], ['app.cache'], [])).toBe(300);
    expect(
      CleanupRuleSelectionUtils.selectedBytes(
        [rule],
        ['app.cache'],
        [{ ruleId: 'app.cache', mode: 'include', paths: ['/cache/a'] }]
      )
    ).toBe(100);
    expect(
      CleanupRuleSelectionUtils.selectedBytes(
        [rule],
        ['app.cache'],
        [{ ruleId: 'app.cache', mode: 'exclude', paths: ['/cache/a'] }]
      )
    ).toBe(200);
  });

  it('derives tri-state rule and source selection consistently', () => {
    const partial = [{ ruleId: 'app.cache', mode: 'include' as const, paths: ['/cache/a'] }];

    expect(CleanupRuleSelectionUtils.ruleSelectionLevel(rule, ['app.cache'], partial)).toBe('partial');
    expect(CleanupRuleSelectionUtils.sourceSelected('app.cache', '/cache/a', ['app.cache'], partial)).toBe(true);
    expect(CleanupRuleSelectionUtils.sourceSelected('app.cache', '/cache/b', ['app.cache'], partial)).toBe(false);
    expect(CleanupRuleSelectionUtils.ruleSelectionLevel(rule, [], partial)).toBe('none');
  });

  it('treats all selectable sources as fully selected when blocked sources are excluded', () => {
    const ruleWithBlockedSource = {
      ...rule,
      sources: [
        rule.sources[0],
        {
          ...rule.sources[1],
          blockReason: 'requiresClose' as const,
        },
      ],
    };
    const selectableOnly = [{ ruleId: rule.ruleId, mode: 'include' as const, paths: ['/cache/a'] }];

    expect(CleanupRuleSelectionUtils.ruleSelectionLevel(ruleWithBlockedSource, [rule.ruleId], selectableOnly)).toBe(
      'all'
    );
  });

  it('distinguishes smart, all, none, and manual selection modes', () => {
    const optionalRule = {
      ...rule,
      ruleId: 'app.optional-cache',
      recommendedSelected: false,
      bytes: 700,
    };
    const rules = [rule, optionalRule];

    expect(CleanupRuleSelectionUtils.selectionMode(rules, ['app.cache'], [])).toBe('smart');
    expect(CleanupRuleSelectionUtils.selectionMode(rules, ['app.cache', 'app.optional-cache'], [])).toBe('all');
    expect(CleanupRuleSelectionUtils.selectionMode(rules, [], [])).toBe('none');
    expect(CleanupRuleSelectionUtils.selectionMode(rules, ['app.optional-cache'], [])).toBe('manual');
    expect(
      CleanupRuleSelectionUtils.selectionMode(
        rules,
        ['app.cache'],
        [{ ruleId: 'app.cache', mode: 'include', paths: ['/cache/a'] }]
      )
    ).toBe('manual');
    expect(CleanupRuleSelectionUtils.foundBytes(rules)).toBe(1_000);
    expect(CleanupRuleSelectionUtils.recommendedBytes(rules)).toBe(300);
  });
});
