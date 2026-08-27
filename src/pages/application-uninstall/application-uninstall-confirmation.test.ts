import { describe, expect, it } from 'vitest';

import { shouldNotifyUninstallCancellation } from './application-uninstall-confirmation';

describe('application uninstall confirmation', () => {
  it('notifies once when an open confirmation is cancelled', () => {
    expect(shouldNotifyUninstallCancellation(true, false)).toBe(true);
    expect(shouldNotifyUninstallCancellation(false, false)).toBe(false);
  });

  it('does not notify while the confirmation remains open', () => {
    expect(shouldNotifyUninstallCancellation(true, true)).toBe(false);
  });
});
