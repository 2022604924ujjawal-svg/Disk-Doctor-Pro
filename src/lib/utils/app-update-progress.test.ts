import { describe, expect, it } from 'vitest';

import { AppUpdateProgressUtils } from './app-update-progress';

describe('app update progress utils', () => {
  it('returns null until the total download size is known', () => {
    expect(AppUpdateProgressUtils.percent(128, null)).toBeNull();
    expect(AppUpdateProgressUtils.percent(128, 0)).toBeNull();
  });

  it('calculates determinate progress and clamps invalid bounds', () => {
    expect(AppUpdateProgressUtils.percent(25, 100)).toBe(25);
    expect(AppUpdateProgressUtils.percent(-1, 100)).toBe(0);
    expect(AppUpdateProgressUtils.percent(125, 100)).toBe(100);
  });
});
