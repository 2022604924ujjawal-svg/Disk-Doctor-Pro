export type StartupSourceKind =
  | 'registryRun'
  | 'startupFolder'
  | 'scheduledTask'
  | 'service'
  | 'packagedStartupTask'
  | 'launchAgent'
  | 'launchDaemon'
  | 'loginItem'
  | 'backgroundTask'
  | 'embeddedItem'
  | 'advancedAutoRun';

export type StartupScope = 'currentUser' | 'user' | 'allUsers' | 'machine' | 'system';
export type StartupTrigger =
  'boot' | 'userLogon' | 'scheduled' | 'event' | 'keepAlive' | 'shellLoad' | 'applicationLaunch' | 'unknown';
export type StartupConfiguredState = 'enabled' | 'disabled' | 'unknown' | 'notApplicable';
export type StartupRuntimeState = 'running' | 'stopped' | 'loaded' | 'unloaded' | 'unknown';
export type StartupControlCapability =
  'toggleable' | 'elevationRequired' | 'removeOnly' | 'systemManaged' | 'policyManaged' | 'viewOnly';
export type StartupTrustState = 'system' | 'verified' | 'invalid' | 'unsigned' | 'unknown';
export type StartupIdentityConfidence = 'exact' | 'strong' | 'probable' | 'unresolved';
export type StartupSummarySource =
  | 'serviceDescription'
  | 'taskDescription'
  | 'packageManifest'
  | 'versionInfo'
  | 'bundleMetadata'
  | 'sourceLabel'
  | 'unavailable';
export type StartupTargetKind = 'executable' | 'application' | 'script' | 'service' | 'task' | 'other' | 'unknown';
export type StartupDiagnosticCode =
  'accessDenied' | 'invalidData' | 'missingIdentity' | 'missingTarget' | 'stateUnavailable' | 'unsupportedFormat';
export type StartupCoverageStatus = 'complete' | 'partial' | 'unavailable' | 'failed' | 'cancelled';
export type StartupCoverageReason =
  | 'accessDenied'
  | 'apiUnavailable'
  | 'cancelled'
  | 'invalidData'
  | 'notImplemented'
  | 'stateUnavailable'
  | 'unsupportedOperatingSystem';
export type StartupAggregateConfiguredState = 'allEnabled' | 'partiallyEnabled' | 'allDisabled' | 'unknown';
export type StartupAggregateControlState = 'allToggleable' | 'requiresElevation' | 'partiallyManageable' | 'viewOnly';

export interface StartupTarget {
  kind: StartupTargetKind;
  path: string | null;
  executableName: string | null;
  arguments: string[];
}

export interface StartupArtifact {
  itemId: string;
  sourceId: string;
  sourceKind: StartupSourceKind;
  scope: StartupScope;
  triggers: StartupTrigger[];
  displayName: string;
  configurationPath: string | null;
  target: StartupTarget;
  ownerName: string | null;
  publisher: string | null;
  summary: string | null;
  summarySource: StartupSummarySource;
  version: string | null;
  iconPath: string | null;
  identityConfidence: StartupIdentityConfidence;
  configuredState: StartupConfiguredState;
  runtimeState: StartupRuntimeState;
  controlCapability: StartupControlCapability;
  trust: StartupTrustState;
  modifiedAtMs: number | null;
  diagnostics: StartupDiagnosticCode[];
  removableOrphan: boolean;
}

export interface StartupOwnerGroup {
  groupId: string;
  name: string;
  publisher: string | null;
  summary: string | null;
  summarySource: StartupSummarySource;
  version: string | null;
  iconPath: string | null;
  identityConfidence: StartupIdentityConfidence;
  itemIds: string[];
  sourceKinds: StartupSourceKind[];
  triggers: StartupTrigger[];
  scopes: StartupScope[];
  configuredState: StartupAggregateConfiguredState;
  controlState: StartupAggregateControlState;
  systemItem: boolean;
}

export interface StartupSourceCoverage {
  sourceId: string;
  required: boolean;
  status: StartupCoverageStatus;
  reason: StartupCoverageReason | null;
  itemCount: number;
  elapsedMs: number;
}

export interface StartupCatalogSummary {
  itemCount: number;
  groupCount: number;
  enabledCount: number;
  disabledCount: number;
  unknownStateCount: number;
  elevationRequiredCount: number;
  systemItemCount: number;
}

export interface StartupCatalog {
  schemaVersion: number;
  scanId: string;
  catalogRevision: string;
  scannedAtMs: number;
  complete: boolean;
  artifacts: StartupArtifact[];
  groups: StartupOwnerGroup[];
  coverage: StartupSourceCoverage[];
  summary: StartupCatalogSummary;
  elapsedMs: number;
}

export type StartupDesiredState = 'enabled' | 'disabled' | 'removed';
export type StartupChangeWarning = 'affectsOtherTriggers' | 'itemCurrentlyRunning';
export type StartupChangeSkipReason =
  | 'alreadyInDesiredState'
  | 'catalogExpired'
  | 'itemChanged'
  | 'itemMissing'
  | 'stateUnknown'
  | 'unsupportedCapability'
  | 'requiresElevation'
  | 'targetUnavailable';

export interface StartupChangeSelection {
  scanId: string;
  itemIds: string[];
  desiredState: StartupDesiredState;
}

export interface StartupChangePlanItem {
  itemId: string;
  displayName: string;
  sourceKind: StartupSourceKind;
  scope: StartupScope;
  previousState: StartupConfiguredState;
  desiredState: StartupDesiredState;
  warnings: StartupChangeWarning[];
  requiresElevation: boolean;
}

export interface StartupChangeSkippedItem {
  itemId: string;
  displayName: string;
  reason: StartupChangeSkipReason;
}

export interface StartupChangePlan {
  schemaVersion: number;
  planId: string;
  scanId: string;
  catalogRevision: string;
  createdAtMs: number;
  expiresAtMs: number;
  desiredState: StartupDesiredState;
  items: StartupChangePlanItem[];
  skippedItems: StartupChangeSkippedItem[];
  requiresConfirmation: boolean;
}

export type StartupChangeOutcomeStatus = 'changed' | 'unchanged' | 'failed';
export type StartupChangeFailureReason =
  'itemChanged' | 'permissionDenied' | 'userCancelled' | 'unsupported' | 'verificationFailed' | 'platformFailure';

export interface StartupChangeItemResult {
  itemId: string;
  status: StartupChangeOutcomeStatus;
  configuredState: StartupConfiguredState;
  verified: boolean;
  failureReason: StartupChangeFailureReason | null;
}

export interface StartupChangeResult {
  planId: string;
  changedCount: number;
  failedCount: number;
  items: StartupChangeItemResult[];
  catalog: StartupCatalog | null;
}
