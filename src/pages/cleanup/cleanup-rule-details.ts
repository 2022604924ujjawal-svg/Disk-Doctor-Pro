import type { PresentedScanRuleResult } from '@/lib/models/cleanup';

export function isAggregateOnlyCleanupRule(rule: PresentedScanRuleResult): boolean {
  return rule.selectable && rule.sources.length === 0;
}

export function hasCleanupRuleDetails(rule: PresentedScanRuleResult): boolean {
  return (
    rule.sources.length > 0 ||
    isAggregateOnlyCleanupRule(rule) ||
    rule.requiresAppClose ||
    rule.runningProcesses.length > 0 ||
    Boolean(rule.description.trim() || rule.impact.trim())
  );
}
