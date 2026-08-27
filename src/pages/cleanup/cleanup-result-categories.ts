import type { CleanupResultGroup, CleanupSourceSelection, PresentedScanRuleResult } from '@/lib/models/cleanup';
import type { CleanupRuleSelectionLevel } from '@/lib/utils/cleanup-rule-selection';
import { CleanupRuleSelectionUtils } from '@/lib/utils/cleanup-rule-selection';

const CATEGORY_ORDER: readonly CleanupResultGroup[] = [
  'system',
  'userCache',
  'application',
  'browser',
  'applicationOptimization',
  'ai',
  'development',
  'xcode',
  'container',
  'project',
];

export interface CleanupResultCategory {
  id: CleanupResultGroup;
  rules: PresentedScanRuleResult[];
  bytes: number;
  selectedBytes: number;
  selection: CleanupRuleSelectionLevel;
}

/**
 * Counts the cleanup choices shown in the result list instead of internal
 * source directories. This keeps the fixed action bar aligned with what the
 * user selected even when one rule owns hundreds of cache locations.
 */
export function countSelectedCleanupGroups(
  rules: readonly PresentedScanRuleResult[],
  selectedRuleIds: readonly string[],
  sourceSelections: readonly CleanupSourceSelection[]
): number {
  return CleanupRuleSelectionUtils.selectedRules(rules, selectedRuleIds).filter(
    rule => CleanupRuleSelectionUtils.selectedBytesForRule(rule, selectedRuleIds, sourceSelections) > 0
  ).length;
}

/**
 * Builds the small, semantic category navigation used by the cleanup result.
 * Risk remains a property of each rule; using risk as navigation would expose
 * dozens of implementation rules in the master column and obscure the
 * product-level cleanup categories.
 */
export function buildCleanupResultCategories(
  rules: readonly PresentedScanRuleResult[],
  selectedRuleIds: readonly string[],
  sourceSelections: readonly CleanupSourceSelection[]
): CleanupResultCategory[] {
  return CATEGORY_ORDER.flatMap(group => {
    const categoryRules = rules
      .filter(rule => rule.selectable && rule.group === group)
      .sort((left, right) => right.bytes - left.bytes || left.name.localeCompare(right.name));
    if (!categoryRules.length) return [];

    const selectionLevels = categoryRules.map(rule =>
      CleanupRuleSelectionUtils.ruleSelectionLevel(rule, selectedRuleIds, sourceSelections)
    );
    const selectedBytes = CleanupRuleSelectionUtils.selectedBytes(categoryRules, selectedRuleIds, sourceSelections);

    return [
      {
        id: group,
        rules: categoryRules,
        bytes: categoryRules.reduce((total, rule) => total + rule.bytes, 0),
        selectedBytes,
        selection: selectionLevels.every(level => level === 'all')
          ? 'all'
          : selectionLevels.every(level => level === 'none')
            ? 'none'
            : 'partial',
      },
    ];
  });
}
