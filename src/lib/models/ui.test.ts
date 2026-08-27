import { describe, expect, it } from 'vitest';

import { ICON_NAMES } from './ui';

describe('UI icon names', () => {
  it('provides the sparkles icon used by system optimization actions', () => {
    expect(ICON_NAMES.sparkles).toBe('sparkles');
  });

  it('provides the copy icon used by inline actions', () => {
    expect(ICON_NAMES.copy).toBe('copy');
  });
});
