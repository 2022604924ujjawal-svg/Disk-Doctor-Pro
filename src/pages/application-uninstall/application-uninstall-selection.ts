import type { ApplicationUninstallCandidate, ApplicationUninstallComponentSummary } from '@/lib/models/application';

export interface ApplicationUninstallSelection {
  applicationIds: string[];
  componentIds: Record<string, string[]>;
}

export function defaultApplicationComponentIds(candidate: ApplicationUninstallCandidate): string[] {
  return candidate.components.filter(component => component.defaultSelected).map(component => component.componentId);
}

export function selectedApplicationBytes(
  candidates: readonly ApplicationUninstallCandidate[],
  selection: ApplicationUninstallSelection
): number {
  const selectedApplications = new Set(selection.applicationIds);
  return candidates.reduce((total, candidate) => {
    if (!selectedApplications.has(candidate.applicationId)) return total;
    const selectedComponents = new Set(selection.componentIds[candidate.applicationId] ?? []);
    return (
      total +
      candidate.components
        .filter(component => selectedComponents.has(component.componentId))
        .reduce((componentTotal, component) => componentTotal + component.bytes, 0)
    );
  }, 0);
}

export function selectionIncludesUserData(
  candidates: readonly ApplicationUninstallCandidate[],
  selection: ApplicationUninstallSelection
): boolean {
  const selectedApplications = new Set(selection.applicationIds);
  return candidates.some(candidate => {
    if (!selectedApplications.has(candidate.applicationId)) return false;
    const selectedComponents = new Set(selection.componentIds[candidate.applicationId] ?? []);
    return candidate.components.some(
      component => component.risk === 'userData' && selectedComponents.has(component.componentId)
    );
  });
}

export function toggleApplicationSelection(
  selection: ApplicationUninstallSelection,
  candidate: ApplicationUninstallCandidate
): ApplicationUninstallSelection {
  if (selection.applicationIds.includes(candidate.applicationId)) {
    const componentIds = { ...selection.componentIds };
    delete componentIds[candidate.applicationId];
    return {
      applicationIds: selection.applicationIds.filter(applicationId => applicationId !== candidate.applicationId),
      componentIds,
    };
  }

  const componentIds = defaultApplicationComponentIds(candidate);
  if (!componentIds.length) return selection;
  return {
    applicationIds: [...selection.applicationIds, candidate.applicationId],
    componentIds: {
      ...selection.componentIds,
      [candidate.applicationId]: componentIds,
    },
  };
}

export function toggleApplicationComponent(
  selection: ApplicationUninstallSelection,
  candidate: ApplicationUninstallCandidate,
  component: ApplicationUninstallComponentSummary
): ApplicationUninstallSelection {
  if (component.risk === 'required') return selection;

  const applicationSelected = selection.applicationIds.includes(candidate.applicationId);
  const componentIds = new Set(
    applicationSelected
      ? (selection.componentIds[candidate.applicationId] ?? [])
      : defaultApplicationComponentIds(candidate)
  );
  if (componentIds.has(component.componentId)) componentIds.delete(component.componentId);
  else componentIds.add(component.componentId);

  return {
    applicationIds: applicationSelected
      ? selection.applicationIds
      : [...selection.applicationIds, candidate.applicationId],
    componentIds: {
      ...selection.componentIds,
      [candidate.applicationId]: [...componentIds],
    },
  };
}

export function setVisibleApplicationSelection(
  selection: ApplicationUninstallSelection,
  candidates: readonly ApplicationUninstallCandidate[],
  selected: boolean
): ApplicationUninstallSelection {
  const visibleIds = new Set(candidates.map(candidate => candidate.applicationId));
  if (!selected) {
    return {
      applicationIds: selection.applicationIds.filter(applicationId => !visibleIds.has(applicationId)),
      componentIds: Object.fromEntries(
        Object.entries(selection.componentIds).filter(([applicationId]) => !visibleIds.has(applicationId))
      ),
    };
  }

  const previouslySelected = new Set(selection.applicationIds);
  const componentIds = { ...selection.componentIds };
  const selectableIds: string[] = [];
  for (const candidate of candidates) {
    const defaults = defaultApplicationComponentIds(candidate);
    if (!defaults.length) continue;
    selectableIds.push(candidate.applicationId);
    if (!previouslySelected.has(candidate.applicationId)) {
      componentIds[candidate.applicationId] = defaults;
    }
  }
  return {
    applicationIds: [...new Set([...selection.applicationIds, ...selectableIds])],
    componentIds,
  };
}
