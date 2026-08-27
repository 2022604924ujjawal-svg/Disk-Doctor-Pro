import type { CleanupSourceSelection, RiskLevel, ScanRuleResult } from '@/lib/models/cleanup';

export interface CleanupRuleSelectionState {
  allSelected: boolean;
  selectedCount: number;
  selectableRuleIds: string[];
  someSelected: boolean;
}

export type CleanupRuleSelectionLevel = 'all' | 'partial' | 'none';
export type CleanupSelectionMode = 'smart' | 'all' | 'none' | 'manual';

/**
 * Derives cleanup filtering and selection from input values only, keeping
 * pages, grouped results, and bulk actions on one selection policy.
 */
export class CleanupRuleSelectionUtils {
  static defaultSelectedRuleIds(rules: readonly ScanRuleResult[]): string[] {
    return this.recommendedRuleIds(rules);
  }

  static recommendedRuleIds(rules: readonly ScanRuleResult[]): string[] {
    return rules.filter(rule => rule.recommendedSelected && rule.selectable).map(rule => rule.ruleId);
  }

  static selectableRuleIds(rules: readonly ScanRuleResult[]): string[] {
    return rules.filter(rule => rule.selectable).map(rule => rule.ruleId);
  }

  static foundBytes(rules: readonly ScanRuleResult[]): number {
    return rules.reduce((total, rule) => total + (rule.selectable ? rule.bytes : 0), 0);
  }

  static recommendedBytes(rules: readonly ScanRuleResult[]): number {
    return this.selectedBytes(rules, this.recommendedRuleIds(rules), []);
  }

  static selectionMode(
    rules: readonly ScanRuleResult[],
    selectedRuleIds: readonly string[],
    sourceSelections: readonly CleanupSourceSelection[]
  ): CleanupSelectionMode {
    if (sourceSelections.length) return 'manual';
    const selected = new Set(selectedRuleIds);
    const selectable = this.selectableRuleIds(rules);
    if (!selected.size) return 'none';

    const matches = (ruleIds: readonly string[]) =>
      selected.size === ruleIds.length && ruleIds.every(ruleId => selected.has(ruleId));
    if (matches(this.recommendedRuleIds(rules))) return 'smart';
    if (matches(selectable)) return 'all';
    return 'manual';
  }

  static selectableByRisk(rules: readonly ScanRuleResult[], risk: RiskLevel): ScanRuleResult[] {
    return rules.filter(rule => rule.risk === risk && rule.selectable);
  }

  static selectedRules(rules: readonly ScanRuleResult[], selectedRuleIds: readonly string[]): ScanRuleResult[] {
    const selectedIds = new Set(selectedRuleIds);
    return rules.filter(rule => selectedIds.has(rule.ruleId));
  }

  static ruleSelectionLevel(
    rule: ScanRuleResult,
    selectedRuleIds: readonly string[],
    sourceSelections: readonly CleanupSourceSelection[]
  ): CleanupRuleSelectionLevel {
    if (!selectedRuleIds.includes(rule.ruleId)) return 'none';
    const selection = sourceSelections.find(item => item.ruleId === rule.ruleId);
    if (!selection) return 'all';
    if (selection.mode === 'exclude') {
      return selection.paths.length ? 'partial' : 'all';
    }
    const selectableSources = rule.sources.filter(source => !source.blockReason);
    if (
      !rule.sourcesTruncated &&
      selectableSources.length > 0 &&
      selectableSources.every(source => selection.paths.includes(source.path))
    ) {
      return 'all';
    }
    return selection.paths.length ? 'partial' : 'none';
  }

  static sourceSelected(
    ruleId: string,
    sourcePath: string,
    selectedRuleIds: readonly string[],
    sourceSelections: readonly CleanupSourceSelection[]
  ): boolean {
    if (!selectedRuleIds.includes(ruleId)) return false;
    const selection = sourceSelections.find(item => item.ruleId === ruleId);
    if (!selection) return true;
    const contains = selection.paths.includes(sourcePath);
    return selection.mode === 'include' ? contains : !contains;
  }

  static selectedBytes(
    rules: readonly ScanRuleResult[],
    selectedRuleIds: readonly string[],
    sourceSelections: readonly CleanupSourceSelection[]
  ): number {
    const selectedIds = new Set(selectedRuleIds);
    const selectionsByRule = new Map(sourceSelections.map(selection => [selection.ruleId, selection]));
    return rules.reduce((total, rule) => {
      if (!selectedIds.has(rule.ruleId)) return total;
      const selection = selectionsByRule.get(rule.ruleId);
      if (!selection) return total + rule.bytes;
      const selectedPaths = new Set(selection.paths);
      const overriddenBytes = rule.sources.reduce(
        (bytes, source) => bytes + (selectedPaths.has(source.path) ? source.bytes : 0),
        0
      );
      return total + (selection.mode === 'include' ? overriddenBytes : Math.max(0, rule.bytes - overriddenBytes));
    }, 0);
  }

  static selectedBytesForRule(
    rule: ScanRuleResult,
    selectedRuleIds: readonly string[],
    sourceSelections: readonly CleanupSourceSelection[]
  ): number {
    return this.selectedBytes([rule], selectedRuleIds, sourceSelections);
  }

  static selectionState(
    rules: readonly ScanRuleResult[],
    selectedRuleIds: readonly string[]
  ): CleanupRuleSelectionState {
    const selectableRuleIds = rules.filter(rule => rule.selectable).map(rule => rule.ruleId);
    const selectedIds = new Set(selectedRuleIds);
    const selectedCount = selectableRuleIds.reduce((count, ruleId) => count + Number(selectedIds.has(ruleId)), 0);

    return {
      allSelected: selectableRuleIds.length > 0 && selectedCount === selectableRuleIds.length,
      selectedCount,
      selectableRuleIds,
      someSelected: selectedCount > 0 && selectedCount < selectableRuleIds.length,
    };
  }
}
