import { BYTE_SIZE_UNITS, type ByteSizePreset, type ByteSizeUnit } from '@/lib/models/byte-size';
import type { ByteUnitBase } from '@/lib/utils/format';

const BYTE_SIZE_UNIT_EXPONENTS: Readonly<Record<ByteSizeUnit, number>> = {
  [BYTE_SIZE_UNITS.kilobytes]: 1,
  [BYTE_SIZE_UNITS.megabytes]: 2,
  [BYTE_SIZE_UNITS.gigabytes]: 3,
};

/**
 * Resolves semantic size presets without reading platform or application state.
 * Keeping the base explicit makes settings validation and compatibility mapping
 * deterministic while the service layer remains responsible for OS detection.
 */
export class ByteSizePresetUtils {
  static bytes(preset: ByteSizePreset, unitBase: ByteUnitBase): number {
    return preset.amount * unitBase ** BYTE_SIZE_UNIT_EXPONENTS[preset.unit];
  }

  static byteValues(presets: readonly ByteSizePreset[], unitBase: ByteUnitBase): number[] {
    return presets.map(preset => this.bytes(preset, unitBase));
  }
}
