import type {
  ApplicationLeftoverActionResult,
  ApplicationUninstallActionResult,
  ApplicationUninstallInstallerKind,
  ApplicationUninstallPlatform,
} from './application';
import type { CleanupActionResult, PresentedCleanupActionResult } from './cleanup';
import type { SystemSettingChangeFailureReason } from './system-settings';

export type OperationCategory =
  | 'deepCleanup'
  | 'largeFileCleanup'
  | 'duplicateFileCleanup'
  | 'applicationUninstall'
  | 'startupManagement'
  | 'systemOptimization';
export type OperationOutcome = 'completed' | 'completedWithWarnings' | 'cancelled';

interface OperationRecordBase {
  schemaVersion: number;
  operationId: string;
  category: OperationCategory;
  startedAtMs: number;
  finishedAtMs: number;
  outcome: OperationOutcome;
  dryRun: boolean;
  selectedItemCount: number;
  affectedItemCount: number;
  expectedBytes: number;
  releasedBytes: number | null;
  releasedBytesIsEstimate: boolean;
  failedItemCount: number;
}

export interface CleanupOperationDetails {
  selectedRuleIds: string[];
  expectedBytes: number;
  actions: CleanupActionResult[];
}

export interface ApplicationLeftoverOperationDetails {
  candidateIds: string[];
  expectedBytes: number;
  actions: ApplicationLeftoverActionResult[];
}

export interface DeepCleanupOperationRecord extends OperationRecordBase {
  category: 'deepCleanup';
  details: {
    type: 'deepCleanup';
    payload: {
      cleanup: CleanupOperationDetails | null;
      applicationLeftovers: ApplicationLeftoverOperationDetails | null;
    };
  };
}

export interface FileCleanupOperationDetails {
  items: Array<{
    path: string;
    status: 'deleted' | 'failed';
  }>;
  omittedItemCount: number;
}

export interface LargeFileCleanupOperationRecord extends OperationRecordBase {
  category: 'largeFileCleanup';
  details: {
    type: 'largeFileCleanup';
    payload: FileCleanupOperationDetails;
  };
}

export interface DuplicateFileCleanupOperationRecord extends OperationRecordBase {
  category: 'duplicateFileCleanup';
  details: {
    type: 'duplicateFileCleanup';
    payload: FileCleanupOperationDetails;
  };
}

export interface ApplicationUninstallApplicationDetails {
  restartRequired: boolean;
  planId: string;
  applicationId: string;
  applicationName: string;
  applicationIdentifier: string;
  applicationVersion: string | null;
  applicationPublisher: string | null;
  applicationPlatform: ApplicationUninstallPlatform;
  installerKind: ApplicationUninstallInstallerKind | null;
  componentIds: string[];
  actions: ApplicationUninstallActionResult[];
}

export interface ApplicationUninstallOperationDetails {
  batchId: string;
  applications: ApplicationUninstallApplicationDetails[];
  restartRequired: boolean;
}

export interface ApplicationUninstallOperationRecord extends OperationRecordBase {
  category: 'applicationUninstall';
  details: {
    type: 'applicationUninstall';
    payload: ApplicationUninstallOperationDetails;
  };
}

export interface StartupManagementOperationRecord extends OperationRecordBase {
  category: 'startupManagement';
  details: {
    type: 'startupManagement';
    payload: {
      planId: string | null;
      items: Array<{
        itemId: string;
        displayName: string;
        previousState: 'enabled' | 'disabled' | 'unknown';
        desiredState: 'enabled' | 'disabled' | 'removed' | 'unknown';
        status: 'changed' | 'unchanged' | 'failed';
        failureReason: string | null;
      }>;
    };
  };
}

export interface SystemOptimizationOperationRecord extends OperationRecordBase {
  category: 'systemOptimization';
  details: {
    type: 'systemOptimization';
    payload: {
      planId: string;
      restoration: boolean;
      items: Array<{
        settingId: string;
        status: 'changed' | 'unchanged' | 'failed';
        failureReason: SystemSettingChangeFailureReason | null;
        desiredOptimized: boolean | null;
      }>;
    };
  };
}

export type OperationRecord =
  | DeepCleanupOperationRecord
  | LargeFileCleanupOperationRecord
  | DuplicateFileCleanupOperationRecord
  | ApplicationUninstallOperationRecord
  | StartupManagementOperationRecord
  | SystemOptimizationOperationRecord;

export type PresentedDeepCleanupOperationRecord = Omit<DeepCleanupOperationRecord, 'details'> & {
  details: {
    type: 'deepCleanup';
    payload: {
      cleanup:
        | (Omit<CleanupOperationDetails, 'actions'> & {
            actions: PresentedCleanupActionResult[];
          })
        | null;
      applicationLeftovers: ApplicationLeftoverOperationDetails | null;
    };
  };
};

export type PresentedOperationRecord =
  | PresentedDeepCleanupOperationRecord
  | LargeFileCleanupOperationRecord
  | DuplicateFileCleanupOperationRecord
  | ApplicationUninstallOperationRecord
  | StartupManagementOperationRecord
  | SystemOptimizationOperationRecord;
