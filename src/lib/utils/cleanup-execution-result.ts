import type { CleanupResult, CleanupScanResult, CleanupSourceSelection, ScanRuleResult } from '@/lib/models/cleanup';

/**
 * Applies verified cleanup outcomes to the current review snapshot without a
 * second full scan.
 *
 * Completed actions can update the selected source set exactly. Partial
 * actions still carry verified released-byte and affected-item deltas, so the
 * aggregate rule can be reduced without pretending to know which paths remain.
 * Source details are invalidated in that case and require a later scan before
 * source-level selection is available again.
 */
export class CleanupExecutionResultUtils {
  static completedRuleIds(result: CleanupResult): Set<string> {
    const statusesByRule = new Map<string, CleanupResult['actions'][number]['status'][]>();
    for (const action of result.actions) {
      const statuses = statusesByRule.get(action.ruleId) ?? [];
      statuses.push(action.status);
      statusesByRule.set(action.ruleId, statuses);
    }
    return new Set(
      [...statusesByRule]
        .filter(([, statuses]) => statuses.length > 0 && statuses.every(status => status === 'completed'))
        .map(([ruleId]) => ruleId)
    );
  }

  static apply(
    scan: CleanupScanResult,
    result: CleanupResult,
    sourceSelections: readonly CleanupSourceSelection[]
  ): CleanupScanResult {
    if (result.dryRun) return scan;

    const completedRuleIds = this.completedRuleIds(result);
    const effectsByRule = new Map<string, { affectedItemCount: number; releasedBytes: number }>();
    for (const action of result.actions) {
      const effect = effectsByRule.get(action.ruleId) ?? { affectedItemCount: 0, releasedBytes: 0 };
      effect.affectedItemCount += action.affectedItemCount;
      effect.releasedBytes += action.releasedBytes;
      effectsByRule.set(action.ruleId, effect);
    }
    if (!completedRuleIds.size && ![...effectsByRule.values()].some(this.hasVerifiedEffect)) return scan;

    const selectionsByRule = new Map(sourceSelections.map(selection => [selection.ruleId, selection]));
    let changed = false;
    const rules = scan.rules.map(rule => {
      if (completedRuleIds.has(rule.ruleId)) {
        changed = true;
        return this.remainingRule(rule, selectionsByRule.get(rule.ruleId));
      }
      const effect = effectsByRule.get(rule.ruleId);
      if (!effect || !this.hasVerifiedEffect(effect)) return rule;
      changed = true;
      return this.remainingPartialRule(rule, effect);
    });
    if (!changed) return scan;

    return {
      ...scan,
      rules,
      safeBytes: rules
        .filter(rule => rule.selectable && rule.risk === 'safe')
        .reduce((total, rule) => total + rule.bytes, 0),
      reclaimableBytes: rules.filter(rule => rule.selectable).reduce((total, rule) => total + rule.bytes, 0),
    };
  }

  static invalidatedSourceRuleIds(result: CleanupResult): Set<string> {
    const completedRuleIds = this.completedRuleIds(result);
    return new Set(
      result.actions
        .filter(
          action =>
            !completedRuleIds.has(action.ruleId) &&
            this.hasVerifiedEffect({
              affectedItemCount: action.affectedItemCount,
              releasedBytes: action.releasedBytes,
            })
        )
        .map(action => action.ruleId)
    );
  }

  private static remainingRule(rule: ScanRuleResult, selection: CleanupSourceSelection | undefined): ScanRuleResult {
    if (!selection) return this.cleanedRule(rule);

    const selectedPaths = new Set(selection.paths);
    const sources =
      selection.mode === 'include'
        ? rule.sources.filter(source => !selectedPaths.has(source.path))
        : rule.sources.filter(source => selectedPaths.has(source.path));
    if (!sources.length) return this.cleanedRule(rule);

    return {
      ...rule,
      bytes: sources.reduce((total, source) => total + source.bytes, 0),
      fileCount: sources.reduce((total, source) => total + source.fileCount, 0),
      sources,
      sourceCount: sources.length,
      selectable: true,
      status: rule.requiresAppClose ? 'requiresClose' : 'found',
    };
  }

  private static remainingPartialRule(
    rule: ScanRuleResult,
    effect: { affectedItemCount: number; releasedBytes: number }
  ): ScanRuleResult {
    const bytes = Math.max(0, rule.bytes - effect.releasedBytes);
    const fileCount = Math.max(0, rule.fileCount - effect.affectedItemCount);
    if (bytes === 0 && fileCount === 0) return this.cleanedRule(rule);

    return {
      ...rule,
      bytes,
      fileCount,
      // Core verifies the aggregate effect, but a partial result cannot prove
      // which source paths survived. Remove stale path rows so retrying the
      // whole rule remains safe and the UI never presents deleted locations.
      sources: [],
      sourceCount: 0,
      sourcesTruncated: true,
      selectable: true,
      status: rule.requiresAppClose ? 'requiresClose' : 'found',
    };
  }

  private static hasVerifiedEffect(effect: { affectedItemCount: number; releasedBytes: number }): boolean {
    return effect.affectedItemCount > 0 || effect.releasedBytes > 0;
  }

  private static cleanedRule(rule: ScanRuleResult): ScanRuleResult {
    return {
      ...rule,
      bytes: 0,
      fileCount: 0,
      sources: [],
      sourceCount: 0,
      selectable: false,
      status: 'clean',
    };
  }
}
