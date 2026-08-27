import type { ByteSizePreset } from '@/lib/models/byte-size';
import { OperatingSystemService } from '@/lib/services/operating-system-service';
import { BYTE_UNIT_BASES, FormatUtils, type ByteUnitBase } from '@/lib/utils/format';
import { ByteSizePresetUtils } from '@/lib/utils/byte-size-preset';

interface ByteSizePresetOption {
  bytes: number;
  label: string;
}

/**
 * Owns the operating system's byte-unit convention at the frontend boundary.
 *
 * Finder and macOS storage surfaces use decimal units. Windows Explorer uses
 * a 1024 base while retaining the KB/MB/GB labels. Centralizing that difference
 * keeps measured sizes and configurable size presets internally consistent.
 *
 * Measured inputs remain raw bytes and are only formatted. Semantic presets are
 * resolved to platform-accurate raw bytes before persistence or Core operations,
 * so labels and threshold behavior describe the same boundary.
 */
export class ByteSizeService {
  static bytes(bytes: number): string {
    return FormatUtils.bytes(bytes, this.currentUnitBase());
  }

  /**
   * Resolves product presets to labels and platform-accurate raw byte values.
   *
   * A 50 MB preset becomes 50,000,000 bytes on macOS and 52,428,800 bytes
   * on Windows. Core still receives only raw bytes, while the displayed label
   * remains mathematically consistent with the platform's size convention.
   */
  static presetOptions(presets: readonly ByteSizePreset[]): ByteSizePresetOption[] {
    const unitBase = this.currentUnitBase();
    return presets.map(preset => ({
      bytes: ByteSizePresetUtils.bytes(preset, unitBase),
      label: `${preset.amount} ${preset.unit}`,
    }));
  }

  static currentUnitBase(): ByteUnitBase {
    return OperatingSystemService.isMacOs() ? BYTE_UNIT_BASES.decimal : BYTE_UNIT_BASES.binary;
  }
}
