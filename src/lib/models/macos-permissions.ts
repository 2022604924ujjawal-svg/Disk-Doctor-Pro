export const MACOS_ACCESS_STATUS_IDS = {
  notChecked: 'notChecked',
  available: 'available',
  limited: 'limited',
} as const;

export type MacOsAccessStatus = (typeof MACOS_ACCESS_STATUS_IDS)[keyof typeof MACOS_ACCESS_STATUS_IDS];

export const MACOS_PRIVACY_DESTINATION_IDS = {
  applicationData: 'applicationData',
  filesAndFolders: 'filesAndFolders',
  fullDiskAccess: 'fullDiskAccess',
} as const;

export type MacOsPrivacyDestination =
  (typeof MACOS_PRIVACY_DESTINATION_IDS)[keyof typeof MACOS_PRIVACY_DESTINATION_IDS];

export interface MacOsPermissionObservation {
  applicationDataStatus: MacOsAccessStatus;
  observedAtMs: number | null;
}
