import { describe, expect, it } from 'vitest';

import type { PresentedScanRuleResult } from '@/lib/models/cleanup';

import { cleanupApplicationCloseGroups, cleanupApplicationCloseRetry } from './cleanup-application-close';

function rule(ruleId: string, name: string, runningProcesses: string[]): PresentedScanRuleResult {
  return {
    ruleId,
    name,
    categoryLabel: 'Application',
    description: '',
    impact: '',
    category: 'application',
    group: 'application',
    risk: 'safe',
    defaultSelected: true,
    recommendedSelected: true,
    bytes: 1,
    fileCount: 1,
    available: true,
    selectable: true,
    status: 'requiresClose',
    runningProcesses,
    requiresAppClose: true,
    sources: [],
    sourceCount: 0,
    sourcesTruncated: false,
    scanElapsedMs: 1,
  };
}

describe('cleanup application close groups', () => {
  it('combines cleanup rules that belong to the same running application', () => {
    const groups = cleanupApplicationCloseGroups([
      rule('app.wps-cache', 'WPS cache', ['wps.exe', 'wpscloudsvr.exe']),
      rule('app.wps-rendering-cache', 'WPS rendering cache', ['WPS.EXE', 'promecefpluginhost.exe']),
      rule('app.sogou-input-cache', 'Sogou Input cache', ['SGTool.exe']),
    ]);

    expect(groups).toHaveLength(2);
    expect(groups[0]).toMatchObject({
      id: 'app.wps-cache:app.wps-rendering-cache',
      ruleIds: ['app.wps-cache', 'app.wps-rendering-cache'],
      processes: ['wps.exe', 'wpscloudsvr.exe', 'promecefpluginhost.exe'],
    });
  });

  it('associates grouped processes with the exact native application icon source', () => {
    const groups = cleanupApplicationCloseGroups(
      [
        rule('app.lemon-cache', 'Tencent Lemon cache', ['Tencent Lemon', 'LemonMonitor']),
        rule('app.chatgpt-cache', 'ChatGPT cache', ['ChatGPT']),
      ],
      [
        { processName: 'LemonMonitor', iconPath: '/Applications/Tencent Lemon.app' },
        { processName: 'ChatGPT', iconPath: '/Applications/ChatGPT.app' },
      ]
    );

    expect(groups).toMatchObject([
      { iconPath: '/Applications/Tencent Lemon.app' },
      { iconPath: '/Applications/ChatGPT.app' },
    ]);
  });

  it('force retries only targets that remain or failed', () => {
    const groups = cleanupApplicationCloseGroups([
      rule('app.closed-cache', 'Closed cache', ['closed-app']),
      rule('app.remaining-cache', 'Remaining cache', ['remaining-app']),
      rule('app.failed-cache', 'Failed cache', ['failed-app']),
    ]);

    const retry = cleanupApplicationCloseRetry(groups, {
      mode: 'graceful',
      matchedProcessCount: 3,
      requestedProcessCount: 2,
      remainingProcessCount: 1,
      failedTargetCount: 1,
      elapsedMs: 25,
      targets: [
        {
          targetId: 'app.closed-cache',
          status: 'completed',
          matchedProcessCount: 1,
          requestedProcessCount: 1,
          remainingProcesses: [],
        },
        {
          targetId: 'app.remaining-cache',
          status: 'completed',
          matchedProcessCount: 1,
          requestedProcessCount: 1,
          remainingProcesses: ['remaining-app'],
        },
        {
          targetId: 'app.failed-cache',
          status: 'failed',
          matchedProcessCount: 0,
          requestedProcessCount: 0,
          remainingProcesses: [],
        },
      ],
    });

    expect(retry.ruleIds).toEqual(['app.remaining-cache', 'app.failed-cache']);
    expect(retry.items.map(item => item.id)).toEqual(['app.remaining-cache', 'app.failed-cache']);
    expect(retry.items[1]?.processes).toEqual(['failed-app']);
  });
});
