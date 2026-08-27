export const TRAVERSAL_STAGE_IDS = {
  analyzing: 'analyzing',
  validatingFiles: 'validatingFiles',
  hashingFiles: 'hashingFiles',
  discoveringApplications: 'discoveringApplications',
  checkingProcesses: 'checkingProcesses',
  validatingApplications: 'validatingApplications',
  inspectingApplications: 'inspectingApplications',
} as const;

export interface TraversalProgress {
  operationId: number;
  currentStage: (typeof TRAVERSAL_STAGE_IDS)[keyof typeof TRAVERSAL_STAGE_IDS];
  currentPath: string;
  itemsScanned: number;
  bytesScanned: number;
  completedSteps: number;
  totalSteps: number;
  foundItems: number;
  foundBytes: number;
  elapsedMs: number;
}
