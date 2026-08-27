export type ApplicationCloseMode = 'graceful' | 'force';
export type ApplicationCloseTargetStatus = 'completed' | 'failed';

export interface ApplicationCloseItem {
  id: string;
  name: string;
  processes: string[];
  /** Native application source resolved from trusted inventory metadata. */
  iconPath?: string;
}

export interface ApplicationCloseTargetResult {
  targetId: string;
  status: ApplicationCloseTargetStatus;
  matchedProcessCount: number;
  requestedProcessCount: number;
  remainingProcesses: string[];
}

export interface ApplicationCloseBatchResult {
  mode: ApplicationCloseMode;
  matchedProcessCount: number;
  requestedProcessCount: number;
  remainingProcessCount: number;
  failedTargetCount: number;
  targets: ApplicationCloseTargetResult[];
  elapsedMs: number;
}
