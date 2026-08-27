import googleChromeIconUrl from '@/assets/brands/google-chrome.svg?url';
import microsoft365IconUrl from '@/assets/brands/microsoft-365.svg?url';
import microsoftIconUrl from '@/assets/brands/microsoft.svg?url';
import type { StartupArtifact, StartupOwnerGroup } from '@/lib/models/startup';

const MICROSOFT_OFFICE_LICENSING_ID = 'com.microsoft.office.licensingv2.helper';
const MICROSOFT_AUTOUPDATE_ID = 'com.microsoft.autoupdate.helper';
const GOOGLE_UPDATER_ID_PREFIX = 'com.google.googleupdater';

function addIdentityToken(tokens: Set<string>, value: string | null | undefined) {
  const normalized = value?.trim().toLowerCase();
  if (!normalized) return;

  tokens.add(normalized.replace(/\.plist$/, ''));
  const leaf = normalized.split(/[\\/]/).at(-1);
  if (leaf) tokens.add(leaf.replace(/\.plist$/, ''));
}

function startupIdentityTokens(group: StartupOwnerGroup, artifacts: readonly StartupArtifact[]): ReadonlySet<string> {
  const tokens = new Set<string>();
  addIdentityToken(tokens, group.name);
  for (const artifact of artifacts) {
    addIdentityToken(tokens, artifact.displayName);
    addIdentityToken(tokens, artifact.configurationPath);
    addIdentityToken(tokens, artifact.target.path);
    addIdentityToken(tokens, artifact.target.executableName);
  }
  return tokens;
}

/**
 * Returns a bundled brand only for stable vendor identifiers. A native application icon remains
 * more specific and is therefore preferred whenever the operating system can resolve one.
 */
export function startupGroupIconUrl(
  group: StartupOwnerGroup,
  artifacts: readonly StartupArtifact[],
  nativeIconUrl: string
): string {
  if (nativeIconUrl) return nativeIconUrl;

  const identities = startupIdentityTokens(group, artifacts);
  if (identities.has(MICROSOFT_OFFICE_LICENSING_ID)) return microsoft365IconUrl;
  if (
    [...identities].some(
      identity => identity === GOOGLE_UPDATER_ID_PREFIX || identity.startsWith(`${GOOGLE_UPDATER_ID_PREFIX}.`)
    )
  ) {
    return googleChromeIconUrl;
  }
  if (identities.has(MICROSOFT_AUTOUPDATE_ID)) return microsoftIconUrl;
  return '';
}
