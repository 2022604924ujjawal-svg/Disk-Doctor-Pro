import type { ApplicationUninstallBatchResult, ApplicationUninstallScanResult } from '@/lib/models/application';

/**
 * Applies completed primary actions to the pre-execution catalog immediately.
 *
 * A batch can complete associated-data cleanup while failing to remove the
 * application itself, so secondary component results never remove an
 * application on their own. This is an optimistic UI update only: a catalog
 * scanned after execution is authoritative and must not pass through this
 * projection.
 */
export class ApplicationUninstallResultUtils {
  static apply(
    catalog: ApplicationUninstallScanResult,
    result: ApplicationUninstallBatchResult
  ): ApplicationUninstallScanResult {
    if (result.dryRun) return catalog;

    const removedApplicationIds = new Set(
      result.results
        .filter(application =>
          application.actions.some(
            action =>
              action.status === 'completed' &&
              (action.kind === 'applicationBinary' || action.kind === 'nativeInstaller')
          )
        )
        .map(application => application.applicationId)
    );
    if (!removedApplicationIds.size) return catalog;

    const candidates = catalog.candidates.filter(candidate => !removedApplicationIds.has(candidate.applicationId));
    const readyCount = candidates.filter(
      candidate =>
        candidate.capability === 'ready' ||
        (candidate.platform === 'windowsRegistry' && candidate.capability === 'requiresElevation')
    ).length;

    return {
      ...catalog,
      candidates,
      readyCount,
      blockedCount: candidates.length - readyCount,
    };
  }
}
