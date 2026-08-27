import type { DiskInfo } from './disk';

export const CLEANUP_OPERATION_IDS = {
  idle: 'idle',
  scanning: 'scanning',
  cancelling: 'cancelling',
  previewing: 'previewing',
  cleaning: 'cleaning',
} as const;

export const CLEANUP_SCAN_SCOPE_MODES = {
  standard: 'standard',
  selectedVolumes: 'selectedVolumes',
} as const;

export type CleanupScanScope =
  | { mode: typeof CLEANUP_SCAN_SCOPE_MODES.standard }
  | {
      mode: typeof CLEANUP_SCAN_SCOPE_MODES.selectedVolumes;
      volumeMountPoints: string[];
    };

export const STANDARD_CLEANUP_SCAN_SCOPE: CleanupScanScope = {
  mode: CLEANUP_SCAN_SCOPE_MODES.standard,
};

export const CLEANUP_RULE_CATEGORY_IDS = {
  ai: 'ai',
  application: 'application',
  browser: 'browser',
  container: 'container',
  development: 'development',
  project: 'project',
  xcode: 'xcode',
  applicationOptimization: 'applicationOptimization',
  system: 'system',
} as const;

export const CLEANUP_RESULT_GROUP_IDS = {
  system: 'system',
  userCache: 'userCache',
  application: 'application',
  browser: 'browser',
  development: 'development',
  project: 'project',
  xcode: 'xcode',
  applicationOptimization: 'applicationOptimization',
  ai: 'ai',
  container: 'container',
} as const;

export const CLEANUP_RULE_IDS = {
  aiModelCoquiTts: 'special.ai-model-coqui-tts',
  aiModelGpt4All: 'special.ai-model-gpt4all',
  aiModelHuggingFace: 'special.ai-model-hugging-face',
  aiModelJan: 'special.ai-model-jan',
  aiModelKeras: 'special.ai-model-keras',
  aiModelLmStudio: 'special.ai-model-lm-studio',
  aiModelModelScope: 'special.ai-model-modelscope',
  aiModelOllama: 'special.ai-model-ollama',
  aiModelOpenAiClip: 'special.ai-model-openai-clip',
  aiModelPytorch: 'special.ai-model-pytorch',
  aiCachePytorchHubRepositories: 'special.ai-cache-pytorch-hub-repositories',
  aiModelTensorFlowHub: 'special.ai-model-tensorflow-hub',
  aiModelWhisper: 'special.ai-model-whisper',
  dockerBuildCache: 'special.docker-build-cache',
  macosUniversalBinaries: 'special.macos-universal-binaries',
  windowsRecycleBin: 'special.windows-recycle-bin',
} as const;

export type CleanupOperationId = (typeof CLEANUP_OPERATION_IDS)[keyof typeof CLEANUP_OPERATION_IDS];
export type CleanupExecutionStage = 'validating' | 'cleaning' | 'finalizing';
export type RiskLevel = 'safe' | 'recoverable';
export type CleanupCategory =
  | 'system'
  | 'browser'
  | 'application'
  | 'development'
  | 'project'
  | 'xcode'
  | 'applicationOptimization'
  | 'ai'
  | 'container';
export type CleanupResultGroup = (typeof CLEANUP_RESULT_GROUP_IDS)[keyof typeof CLEANUP_RESULT_GROUP_IDS];
export type ScanItemStatus = 'found' | 'clean' | 'notApplicable' | 'requiresClose' | 'reviewOnly' | 'limited';

export interface CleanupSourceDetail {
  path: string;
  bytes: number;
  fileCount: number;
  modifiedAtMs: number | null;
  blockReason: 'requiresClose' | 'incompleteMeasurement' | null;
}

export type CleanupSourceSelectionMode = 'include' | 'exclude';

export interface CleanupSourceSelection {
  ruleId: string;
  mode: CleanupSourceSelectionMode;
  paths: string[];
}

export interface ScanRuleResult {
  ruleId: string;
  category: CleanupCategory;
  group: CleanupResultGroup;
  risk: RiskLevel;
  defaultSelected: boolean;
  recommendedSelected: boolean;
  bytes: number;
  fileCount: number;
  available: boolean;
  selectable: boolean;
  status: ScanItemStatus;
  runningProcesses: string[];
  requiresAppClose: boolean;
  sources: CleanupSourceDetail[];
  sourceCount: number;
  sourcesTruncated: boolean;
  scanElapsedMs: number;
}

/** Native icon source associated with one running process identity. */
export interface CleanupApplicationIcon {
  processName: string;
  iconPath: string;
}

export interface CleanupRulePresentation {
  name: string;
  categoryLabel: string;
  description: string;
  impact: string;
}

export type PresentedScanRuleResult = ScanRuleResult & CleanupRulePresentation;

export interface CleanupScanResult {
  schemaVersion: string;
  scannedAtMs: number;
  disk: DiskInfo;
  rules: ScanRuleResult[];
  applicationIcons: CleanupApplicationIcon[];
  warningCount: number;
  safeBytes: number;
  reclaimableBytes: number;
  applicabilityElapsedMs: number;
  applicableRuleCount: number;
  filteredRuleCount: number;
  inventoryApplicationCount: number;
  inventoryProcessCount: number;
  elapsedMs: number;
}

export type PresentedCleanupScanResult = Omit<CleanupScanResult, 'rules'> & {
  rules: PresentedScanRuleResult[];
};

export interface CleanupActionResult {
  ruleId: string;
  actionKind: 'delete' | 'command' | 'optimize';
  status: 'blocked' | 'previewed' | 'completed' | 'partial' | 'failed';
  reasonCode:
    | 'runningProcesses'
    | 'itemsSkipped'
    | 'requiredToolUnavailable'
    | 'preflightFailed'
    | 'executionFailed'
    | 'verificationFailed'
    | 'cleanerUnavailable'
    | 'cancelled'
    | null;
  bytesExpected: number;
  releasedBytes: number;
  affectedItemCount: number;
  failedItemCount: number;
  runningProcesses: string[];
}

export type PresentedCleanupActionResult = CleanupActionResult & {
  name: string;
  message: string;
};

export interface CleanupResult {
  planId: string;
  planHash: string;
  expectedBytes: number;
  releasedBytes: number;
  affectedItemCount: number;
  failedItemCount: number;
  dryRun: boolean;
  actions: CleanupActionResult[];
  record: import('./history').DeepCleanupOperationRecord;
  historySaved: boolean;
}

export interface CleanupExecutionProgress {
  stage: CleanupExecutionStage;
  plannedRuleIds: string[];
  currentRuleId: string | null;
  currentItemPath: string | null;
  currentRuleAffectedItemCount: number;
  currentRuleReleasedBytes: number;
  completedRuleResults: CleanupExecutionRuleResult[];
  validatedRuleCount: number;
  completedRuleCount: number;
  totalRuleCount: number;
  checkedItemCount: number;
  checkedBytes: number;
  affectedItemCount: number;
  releasedBytes: number;
  elapsedMs: number;
}

export interface CleanupExecutionRuleResult {
  ruleId: string;
  status: CleanupActionResult['status'];
  affectedItemCount: number;
  releasedBytes: number;
}

export type PresentedCleanupResult = Omit<CleanupResult, 'actions' | 'record'> & {
  actions: PresentedCleanupActionResult[];
  record: import('./history').PresentedDeepCleanupOperationRecord;
};
