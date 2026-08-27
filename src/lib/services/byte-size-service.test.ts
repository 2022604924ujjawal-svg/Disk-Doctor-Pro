import { beforeEach, describe, expect, it, vi } from 'vitest';

import { LARGE_FILE_MINIMUM_PRESETS } from '@/lib/models/large-file';
import { ByteSizeService } from '@/lib/services/byte-size-service';

const { platformMock } = vi.hoisted(() => ({ platformMock: vi.fn() }));

vi.mock('@tauri-apps/plugin-os', () => ({ platform: platformMock }));

describe('ByteSizeService', () => {
  beforeEach(() => {
    platformMock.mockReset();
  });

  it('uses decimal units for measured macOS sizes', () => {
    platformMock.mockReturnValue('macos');

    expect(ByteSizeService.bytes(10_842_048)).toBe('10.8 MB');
    expect(ByteSizeService.bytes(53_400_000_000)).toBe('53.4 GB');
    expect(ByteSizeService.bytes(100 * 1024 * 1024)).toBe('105 MB');
  });

  it('uses binary units for measured Windows sizes', () => {
    platformMock.mockReturnValue('windows');

    expect(ByteSizeService.bytes(10_842_048)).toBe('10.3 MB');
    expect(ByteSizeService.bytes(50 * 1024 * 1024 * 1024)).toBe('50.0 GB');
    expect(ByteSizeService.bytes(100 * 1024 * 1024)).toBe('100 MB');
  });

  it('resolves semantic presets to decimal raw bytes on macOS', () => {
    platformMock.mockReturnValue('macos');

    expect(ByteSizeService.presetOptions(LARGE_FILE_MINIMUM_PRESETS)).toEqual([
      { bytes: 50_000_000, label: '50 MB' },
      { bytes: 100_000_000, label: '100 MB' },
      { bytes: 500_000_000, label: '500 MB' },
      { bytes: 1_000_000_000, label: '1 GB' },
      { bytes: 5_000_000_000, label: '5 GB' },
    ]);
  });

  it('resolves semantic presets to binary raw bytes on Windows', () => {
    platformMock.mockReturnValue('windows');

    expect(ByteSizeService.presetOptions(LARGE_FILE_MINIMUM_PRESETS)).toEqual([
      { bytes: 50 * 1024 * 1024, label: '50 MB' },
      { bytes: 100 * 1024 * 1024, label: '100 MB' },
      { bytes: 500 * 1024 * 1024, label: '500 MB' },
      { bytes: 1024 * 1024 * 1024, label: '1 GB' },
      { bytes: 5 * 1024 * 1024 * 1024, label: '5 GB' },
    ]);
  });
});
