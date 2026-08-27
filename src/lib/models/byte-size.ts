export const BYTE_SIZE_UNITS = {
  kilobytes: 'KB',
  megabytes: 'MB',
  gigabytes: 'GB',
} as const;

export type ByteSizeUnit = (typeof BYTE_SIZE_UNITS)[keyof typeof BYTE_SIZE_UNITS];

/**
 * Describes a user-facing size choice without committing it to a byte base.
 * The presentation adapter resolves the same semantic choice to decimal bytes
 * on macOS and binary bytes on Windows before Core receives the request.
 */
export interface ByteSizePreset {
  amount: number;
  unit: ByteSizeUnit;
}
