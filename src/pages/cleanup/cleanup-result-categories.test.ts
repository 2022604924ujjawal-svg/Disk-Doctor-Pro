import { describe, expect, it } from 'vitest';

import type { PresentedScanRuleResult } from '@/lib/models/cleanup';

import { buildCleanupResultCategories, countSelectedCleanupGroups } from './cleanup-result-categories';

function rule(
  ruleId: string,
  category: PresentedScanRuleResult['category'],
  bytes: number,
  selectable = true,
  group: PresentedScanRuleResult['group'] = category
): PresentedScanRuleResult {
  return {
    ruleId,
    category,
    group,
    risk: 'safe',
    defaultSelected: true,
    recommendedSelected: true,
    bytes,
    fileCount: 1,
    available: true,
    selectable,
    status: selectable ? 'found' : 'clean',
    runningProcesses: [],
    requiresAppClose: false,
    sources: [],
    sourceCount: 0,
    sourcesTruncated: false,
    scanElapsedMs: 1,
    name: ruleId,
    categoryLabel: category,
    description: '',
    impact: '',
  };
}

describe('buildCleanupResultCategories', () => {
  it('keeps the master list at category level and sorts rules by size', () => {
    const categories = buildCleanupResultCategories(
      [
        rule('app.small', 'application', 10),
        rule('dev.large', 'development', 50, true, 'userCache'),
        rule('app.large', 'application', 30),
        rule('project.output', 'project', 80),
        rule('xcode.support', 'xcode', 70),
        rule('app.binary', 'applicationOptimization', 60),
        rule('app.clean', 'application', 100, false),
      ],
      ['app.large', 'app.small'],
      []
    );

    expect(categories.map(category => category.id)).toEqual([
      'userCache',
      'application',
      'applicationOptimization',
      'xcode',
      'project',
    ]);
    expect(categories[0]?.rules.map(item => item.ruleId)).toEqual(['dev.large']);
    expect(categories[1]?.rules.map(item => item.ruleId)).toEqual(['app.large', 'app.small']);
    expect(categories[1]).toMatchObject({ bytes: 40, selectedBytes: 40, selection: 'all' });
  });

  it('orders categories from common cleanup to specialized developer data', () => {
    const categories = buildCleanupResultCategories(
      [
        rule('project.output', 'project', 1),
        rule('container.cache', 'container', 1),
        rule('xcode.data', 'xcode', 1),
        rule('development.cache', 'development', 1),
        rule('ai.models', 'ai', 1),
        rule('application.binary', 'applicationOptimization', 1),
        rule('browser.cache', 'browser', 1),
        rule('application.cache', 'application', 1),
        rule('user.cache', 'userCache', 1),
        rule('system.cache', 'system', 1),
      ],
      [],
      []
    );

    expect(categories.map(category => category.id)).toEqual([
      'system',
      'userCache',
      'application',
      'browser',
      'applicationOptimization',
      'ai',
      'development',
      'xcode',
      'container',
      'project',
    ]);
  });

  it('reports partial selection when a rule or source is only partly selected', () => {
    const sourceRule = {
      ...rule('system.cache', 'system', 100),
      sources: [
        { path: '/cache/a', bytes: 40, fileCount: 1, modifiedAtMs: null, blockReason: null },
        { path: '/cache/b', bytes: 60, fileCount: 1, modifiedAtMs: null, blockReason: null },
      ],
      sourceCount: 2,
    } satisfies PresentedScanRuleResult;

    const categories = buildCleanupResultCategories(
      [sourceRule],
      ['system.cache'],
      [{ ruleId: 'system.cache', mode: 'exclude', paths: ['/cache/a'] }]
    );

    expect(categories[0]).toMatchObject({ selectedBytes: 60, selection: 'partial' });
  });

  it('counts one visible cleanup group regardless of its selected source count', () => {
    const sourceRule = {
      ...rule('system.cache', 'system', 100),
      sources: [
        { path: '/cache/a', bytes: 40, fileCount: 1, modifiedAtMs: null, blockReason: null },
        { path: '/cache/b', bytes: 60, fileCount: 1, modifiedAtMs: null, blockReason: null },
      ],
      sourceCount: 2,
    } satisfies PresentedScanRuleResult;

    expect(countSelectedCleanupGroups([sourceRule], ['system.cache'], [])).toBe(1);
    expect(
      countSelectedCleanupGroups(
        [sourceRule],
        ['system.cache'],
        [{ ruleId: 'system.cache', mode: 'include', paths: ['/cache/a'] }]
      )
    ).toBe(1);
    expect(countSelectedCleanupGroups([sourceRule], [], [])).toBe(0);
  });
});
