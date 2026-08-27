import { version } from '@tauri-apps/plugin-os';

import type { AppDistribution } from '@/lib/models/app-update';
import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import { InstallationIdentityService } from '@/lib/services/installation-identity-service';
import { LoggerService } from '@/lib/services/logger-service';
import { normalizeError } from '@/lib/utils/error';

const UPDATE_HEADER_NAMES = {
  distribution: 'x-mangodisk-distribution',
  installId: 'x-mangodisk-install-id',
  locale: 'x-mangodisk-locale',
  osVersion: 'x-mangodisk-os-version',
  timezone: 'x-mangodisk-timezone',
} as const;

/**
 * Builds the metadata shared by update checks and update downloads.
 *
 * Metadata collection is best-effort because statistics must never prevent
 * the signed updater from checking or installing a release. Host names,
 * hardware identifiers, user names, and filesystem data are intentionally
 * outside this boundary.
 */
export class AppUpdateMetadataService {
  static async createHeaders(language: string, distribution: AppDistribution): Promise<Record<string, string>> {
    const headers: Record<string, string> = {
      'Accept-Language': language,
      [UPDATE_HEADER_NAMES.distribution]: distribution,
      [UPDATE_HEADER_NAMES.locale]: language,
    };

    let installId: string;
    try {
      installId = await InstallationIdentityService.getOrCreateInstallId();
    } catch (error) {
      LoggerService.warn(LOG_DOMAINS.appUpdate, LOG_EVENTS.updateMetadataUnavailable, {
        source: 'installation_identity',
        diagnostic: normalizeError(error),
      });
      return headers;
    }

    headers[UPDATE_HEADER_NAMES.installId] = installId;
    this.appendOsVersion(headers);
    this.appendTimezone(headers);
    return headers;
  }

  private static appendOsVersion(headers: Record<string, string>): void {
    try {
      const osVersion = version().trim();
      if (osVersion) headers[UPDATE_HEADER_NAMES.osVersion] = osVersion;
    } catch (error) {
      this.logOptionalMetadataFailure('os_version', error);
    }
  }

  private static appendTimezone(headers: Record<string, string>): void {
    try {
      const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone?.trim();
      if (timezone) headers[UPDATE_HEADER_NAMES.timezone] = timezone;
    } catch (error) {
      this.logOptionalMetadataFailure('timezone', error);
    }
  }

  private static logOptionalMetadataFailure(source: string, error: unknown): void {
    LoggerService.warn(LOG_DOMAINS.appUpdate, LOG_EVENTS.updateMetadataUnavailable, {
      source,
      diagnostic: normalizeError(error),
    });
  }
}
