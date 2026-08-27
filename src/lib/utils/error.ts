export type CommandErrorCode =
  | 'invalidInput'
  | 'operationBusy'
  | 'operationCancelled'
  | 'operationFailed'
  | 'permissionDenied'
  | 'persistenceFailed'
  | 'taskJoinFailed';

export interface CommandError {
  code: CommandErrorCode;
  details: Readonly<Record<string, string>>;
  retryable: boolean;
}

export type CommandErrorReason = 'resourceBusy' | 'accessDeniedOrBusy' | 'itemChanged';

const COMMAND_ERROR_CODES: ReadonlySet<string> = new Set<CommandErrorCode>([
  'invalidInput',
  'operationBusy',
  'operationCancelled',
  'operationFailed',
  'permissionDenied',
  'persistenceFailed',
  'taskJoinFailed',
]);

const COMMAND_ERROR_REASONS: ReadonlySet<string> = new Set<CommandErrorReason>([
  'resourceBusy',
  'accessDeniedOrBusy',
  'itemChanged',
]);

/** Recognizes the stable error envelope returned by native commands. */
export function parseCommandError(error: unknown): CommandError | null {
  if (typeof error !== 'object' || error === null) return null;
  const candidate = error as Partial<CommandError>;
  if (
    typeof candidate.code !== 'string' ||
    !COMMAND_ERROR_CODES.has(candidate.code) ||
    typeof candidate.retryable !== 'boolean' ||
    typeof candidate.details !== 'object' ||
    candidate.details === null ||
    !Object.values(candidate.details).every(value => typeof value === 'string')
  ) {
    return null;
  }
  return candidate as CommandError;
}

/** Returns only failure reasons recognized by this frontend version. */
export function parseCommandErrorReason(error: CommandError | null): CommandErrorReason | null {
  const reason = error?.details.reason;
  return typeof reason === 'string' && COMMAND_ERROR_REASONS.has(reason) ? (reason as CommandErrorReason) : null;
}

/** Converts failures into a concise diagnostic string for structured logs. */
export function normalizeError(error: unknown): string {
  const commandError = parseCommandError(error);
  if (commandError) return JSON.stringify(commandError);
  return error instanceof Error ? error.message : String(error);
}
