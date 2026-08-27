import { describe, expect, it } from 'vitest';

import { normalizeError, parseCommandError, parseCommandErrorReason } from './error';

describe('error utilities', () => {
  it('recognizes a native command error envelope', () => {
    const error = {
      code: 'operationFailed',
      details: { operation: 'reveal_in_file_manager' },
      retryable: true,
    };

    expect(parseCommandError(error)).toEqual(error);
  });

  it('preserves native command error fields in diagnostics', () => {
    const error = {
      code: 'operationFailed',
      details: { operation: 'reveal_in_file_manager' },
      retryable: true,
    };

    expect(normalizeError(error)).toBe(
      '{"code":"operationFailed","details":{"operation":"reveal_in_file_manager"},"retryable":true}'
    );
  });

  it('rejects command envelopes with non-string detail values', () => {
    expect(
      parseCommandError({
        code: 'operationFailed',
        details: { operation: 42 },
        retryable: true,
      })
    ).toBeNull();
  });

  it('accepts only known privacy-safe failure reasons', () => {
    expect(
      parseCommandErrorReason({
        code: 'operationFailed',
        details: { operation: 'delete_analysis_entry_permanently', reason: 'resourceBusy' },
        retryable: true,
      })
    ).toBe('resourceBusy');
    expect(
      parseCommandErrorReason({
        code: 'operationFailed',
        details: { operation: 'delete_analysis_entry_permanently', reason: 'native path detail' },
        retryable: true,
      })
    ).toBeNull();
  });

  it('keeps ordinary Error messages concise', () => {
    expect(normalizeError(new Error('request failed'))).toBe('request failed');
  });
});
