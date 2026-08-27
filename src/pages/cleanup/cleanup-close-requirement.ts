import { CLEANUP_RULE_IDS, type CleanupSourceSelection, type ScanRuleResult } from '@/lib/models/cleanup';
import { CleanupRuleSelectionUtils } from '@/lib/utils/cleanup-rule-selection';
import { PathUtils } from '@/lib/utils/path';

export interface CleanupCloseRequirement {
  requiresAppClose: boolean;
  runningProcesses: string[];
}

/**
 * Narrows the aggregated application-optimization process list to the
 * application bundles included by the current source selection.
 */
export function selectedCleanupCloseRequirement(
  rule: ScanRuleResult,
  selectedRuleIds: readonly string[],
  sourceSelections: readonly CleanupSourceSelection[]
): CleanupCloseRequirement {
  if (rule.ruleId !== CLEANUP_RULE_IDS.macosUniversalBinaries) {
    return {
      requiresAppClose: rule.requiresAppClose,
      runningProcesses: rule.runningProcesses,
    };
  }

  const sourceSelection = sourceSelections.find(selection => selection.ruleId === rule.ruleId);
  if (!sourceSelection) {
    return {
      requiresAppClose: rule.requiresAppClose,
      runningProcesses: rule.runningProcesses,
    };
  }

  const runningProcesses = rule.sources
    .filter(
      source =>
        source.blockReason === 'requiresClose' &&
        CleanupRuleSelectionUtils.sourceSelected(rule.ruleId, source.path, selectedRuleIds, sourceSelections)
    )
    .map(source => PathUtils.fileName(source.path).replace(/\.app$/iu, ''));

  return {
    requiresAppClose: runningProcesses.length > 0,
    runningProcesses: [...new Set(runningProcesses)],
  };
}
