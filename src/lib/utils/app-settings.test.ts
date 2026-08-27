import { describe, expect, it } from 'vitest';

import { LANGUAGE_IDS } from '@/lib/models/settings';
import { AppSettingsUtils } from '@/lib/utils/app-settings';
import { BYTE_UNIT_BASES } from '@/lib/utils/format';

describe('AppSettingsUtils', () => {
  it('uses English and a 100 MB large-file threshold by default', () => {
    const settings = AppSettingsUtils.defaults();

    expect(settings.language).toBe(LANGUAGE_IDS.enUS);
    expect(settings.largeFileMinimumBytes).toBe(100 * 1024 * 1024);
    expect(settings.duplicateFileMinimumBytes).toBe(200 * 1024);
  });

  it('uses decimal preset bytes for macOS settings', () => {
    const settings = AppSettingsUtils.defaults(LANGUAGE_IDS.enUS, BYTE_UNIT_BASES.decimal);

    expect(settings.largeFileMinimumBytes).toBe(100_000_000);
    expect(settings.duplicateFileMinimumBytes).toBe(200_000);
  });

  it('normalizes saved binary thresholds to their decimal presets', () => {
    const savedSettings = AppSettingsUtils.defaults(LANGUAGE_IDS.enUS, BYTE_UNIT_BASES.binary);
    const settings = AppSettingsUtils.parse(savedSettings, BYTE_UNIT_BASES.decimal);

    expect(settings.largeFileMinimumBytes).toBe(100_000_000);
    expect(settings.duplicateFileMinimumBytes).toBe(200_000);
    expect(settings.language).toBe(savedSettings.language);
    expect(settings.theme).toBe(savedSettings.theme);
  });

  it('rejects incomplete persisted settings', () => {
    expect(() => AppSettingsUtils.parse({})).toThrow('Invalid app settings document');
  });
});
