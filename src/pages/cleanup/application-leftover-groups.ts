import type { ApplicationLeftoverCandidate } from '@/lib/models/application';

export interface ApplicationLeftoverGroup {
  applicationIdentifier: string;
  applicationName: string;
  candidates: ApplicationLeftoverCandidate[];
  candidateIds: string[];
  bytes: number;
  fileCount: number;
}

export type ApplicationLeftoverGroupSelection = 'none' | 'partial' | 'all';

export function recommendedApplicationLeftoverIds(candidates: readonly ApplicationLeftoverCandidate[]): string[] {
  return candidates.filter(candidate => candidate.defaultSelected).map(candidate => candidate.candidateId);
}

export function groupApplicationLeftovers(
  candidates: readonly ApplicationLeftoverCandidate[]
): ApplicationLeftoverGroup[] {
  const grouped = new Map<string, ApplicationLeftoverGroup>();
  for (const candidate of candidates) {
    const group = grouped.get(candidate.applicationIdentifier) ?? {
      applicationIdentifier: candidate.applicationIdentifier,
      applicationName: candidate.applicationName,
      candidates: [],
      candidateIds: [],
      bytes: 0,
      fileCount: 0,
    };
    group.candidates.push(candidate);
    group.candidateIds.push(candidate.candidateId);
    group.bytes += candidate.bytes;
    group.fileCount += candidate.fileCount;
    grouped.set(candidate.applicationIdentifier, group);
  }
  return [...grouped.values()].sort(
    (left, right) => right.bytes - left.bytes || left.applicationName.localeCompare(right.applicationName)
  );
}

export function applicationLeftoverGroupSelection(
  candidateIds: readonly string[],
  selectedIds: ReadonlySet<string>
): ApplicationLeftoverGroupSelection {
  const selectedCount = candidateIds.reduce((count, candidateId) => count + Number(selectedIds.has(candidateId)), 0);
  if (!selectedCount) return 'none';
  return selectedCount === candidateIds.length ? 'all' : 'partial';
}
