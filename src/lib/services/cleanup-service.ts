import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { EVENT_NAMES } from '@/lib/models/telemetry';
import type { ApplicationCloseBatchResult, ApplicationCloseMode } from '@/lib/models/application-close';
import type {
  CleanupExecutionProgress,
  CleanupResult,
  CleanupScanScope,
  CleanupScanResult,
  CleanupSourceSelection,
} from '@/lib/models/cleanup';
import type { TraversalProgress } from '@/lib/models/progress';

export class CleanupService {
  static scan(scanScope: CleanupScanScope): Promise<CleanupScanResult> {
    return invoke<CleanupScanResult>('scan_cleanup_candidates', { scanScope });
  }

  /**
   * The listener must be active before the command starts and released on every
   * completion path. Keeping that lifecycle here prevents stores from leaking
   * Tauri event listeners.
   */
  static async scanWithProgress(
    scanScope: CleanupScanScope,
    handler: (progress: TraversalProgress) => void
  ): Promise<CleanupScanResult> {
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await CleanupService.listenProgress(handler);
      return await CleanupService.scan(scanScope);
    } finally {
      unlisten?.();
    }
  }

  static listenProgress(handler: (progress: TraversalProgress) => void): Promise<UnlistenFn> {
    return listen<TraversalProgress>(EVENT_NAMES.cleanupScanProgress, event => handler(event.payload));
  }

  static cancelScan(): Promise<void> {
    return invoke<void>('cancel_cleanup_scan');
  }

  static cancelExecution(): Promise<void> {
    return invoke<void>('cancel_cleanup_execution');
  }

  static closeApplications(ruleIds: string[], mode: ApplicationCloseMode): Promise<ApplicationCloseBatchResult> {
    return invoke<ApplicationCloseBatchResult>('close_cleanup_applications', {
      request: { ruleIds, mode },
    });
  }

  static execute(
    ruleIds: string[],
    sourceSelections: CleanupSourceSelection[],
    dryRun: boolean,
    scanScope: CleanupScanScope,
    deepCleanupOperationId: string
  ): Promise<CleanupResult> {
    return invoke<CleanupResult>('execute_cleanup', {
      request: { ruleIds, sourceSelections, dryRun },
      scanScope,
      deepCleanupOperationId,
    });
  }

  /**
   * Register the progress listener before invoking cleanup so the first
   * preflight snapshot cannot be lost. Always release it when the command
   * settles to avoid accumulating listeners across repeated cleanup runs.
   */
  static async executeWithProgress(
    ruleIds: string[],
    sourceSelections: CleanupSourceSelection[],
    dryRun: boolean,
    scanScope: CleanupScanScope,
    deepCleanupOperationId: string,
    handler: (progress: CleanupExecutionProgress) => void
  ): Promise<CleanupResult> {
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await CleanupService.listenExecutionProgress(handler);
      return await CleanupService.execute(ruleIds, sourceSelections, dryRun, scanScope, deepCleanupOperationId);
    } finally {
      unlisten?.();
    }
  }

  static listenExecutionProgress(handler: (progress: CleanupExecutionProgress) => void): Promise<UnlistenFn> {
    return listen<CleanupExecutionProgress>(EVENT_NAMES.cleanupExecutionProgress, event => handler(event.payload));
  }
}
