import type {
  CleanupActionResult,
  CleanupCategory,
  CleanupResult,
  PresentedCleanupActionResult,
  PresentedCleanupResult,
  PresentedScanRuleResult,
  PresentedCleanupScanResult,
  ScanRuleResult,
  CleanupScanResult,
} from '@/lib/models/cleanup';
import type {
  DeepCleanupOperationRecord,
  OperationRecord,
  PresentedDeepCleanupOperationRecord,
  PresentedOperationRecord,
} from '@/lib/models/history';

type RuleGroup = 'ai' | 'app' | 'browser' | 'container' | 'dev' | 'system';
type MessageParameters = Readonly<Record<string, string>>;
export type CleanupRuleMessageResolver = (key: string, parameters?: MessageParameters) => string | undefined;

export class CleanupRuleTextUtils {
  /**
   * Builds the UI-owned presentation for backend rule facts. Locale resources
   * are optional: a new declarative rule remains readable before translators
   * add a curated entry.
   */
  static snapshot(snapshot: CleanupScanResult, resolveMessage: CleanupRuleMessageResolver): PresentedCleanupScanResult {
    return {
      ...snapshot,
      rules: snapshot.rules.map(rule => CleanupRuleTextUtils.rule(rule, resolveMessage)),
    };
  }

  static cleanupResult(result: CleanupResult, resolveMessage: CleanupRuleMessageResolver): PresentedCleanupResult {
    return {
      ...result,
      actions: result.actions.map(action => CleanupRuleTextUtils.action(action, resolveMessage)),
      record: CleanupRuleTextUtils.record(result.record, resolveMessage),
    };
  }

  static records(records: OperationRecord[], resolveMessage: CleanupRuleMessageResolver): PresentedOperationRecord[] {
    return records.map(record =>
      record.details.type === 'deepCleanup' ? CleanupRuleTextUtils.record(record, resolveMessage) : record
    );
  }

  private static rule(rule: ScanRuleResult, resolveMessage: CleanupRuleMessageResolver): PresentedScanRuleResult {
    const group = CleanupRuleTextUtils.group(rule.category);
    const keyPrefix = `cleanupRules.entries.${rule.ruleId}`;
    const fallbackName = CleanupRuleTextUtils.fallbackName(rule.ruleId);
    const categoryLabel = resolveMessage(`cleanupRules.categories.${group}`) ?? rule.category;
    return {
      ...rule,
      name: resolveMessage(`${keyPrefix}.name`) ?? fallbackName,
      categoryLabel,
      description: resolveMessage(`${keyPrefix}.description`) ?? '',
      impact: resolveMessage(`${keyPrefix}.impact`) ?? '',
    };
  }

  private static record(
    record: DeepCleanupOperationRecord,
    resolveMessage: CleanupRuleMessageResolver
  ): PresentedDeepCleanupOperationRecord {
    const cleanup = record.details.payload.cleanup;
    return {
      ...record,
      details: {
        ...record.details,
        payload: {
          ...record.details.payload,
          cleanup: cleanup
            ? {
                ...cleanup,
                actions: cleanup.actions.map(action => CleanupRuleTextUtils.action(action, resolveMessage)),
              }
            : null,
        },
      },
    };
  }

  private static action(
    action: CleanupActionResult,
    resolveMessage: CleanupRuleMessageResolver
  ): PresentedCleanupActionResult {
    const reasonMessage = action.reasonCode
      ? resolveMessage(`cleanupRules.actionReasons.${action.reasonCode}`, {
          processes: action.runningProcesses.join(', '),
        })
      : undefined;
    const message = reasonMessage ?? resolveMessage(`cleanupRules.actionMessages.${action.status}`) ?? action.status;
    return {
      ...action,
      name:
        resolveMessage(`cleanupRules.entries.${action.ruleId}.name`) ??
        CleanupRuleTextUtils.fallbackName(action.ruleId),
      message,
    };
  }

  private static group(category: CleanupCategory): RuleGroup {
    if (category === 'application') return 'app';
    if (category === 'development') return 'dev';
    if (category === 'ai' || category === 'browser' || category === 'container' || category === 'system') {
      return category;
    }
    return 'system';
  }

  static fallbackName(ruleId: string): string {
    const segment = ruleId.split('.').at(-1) ?? ruleId;
    return segment
      .split(/[-_]+/u)
      .filter(Boolean)
      .map(word => word.charAt(0).toLocaleUpperCase() + word.slice(1))
      .join(' ');
  }
}
