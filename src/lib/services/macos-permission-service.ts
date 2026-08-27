import { invoke } from '@tauri-apps/api/core';
import { load, type Store } from '@tauri-apps/plugin-store';

import type { ApplicationLeftoverScanResult } from '@/lib/models/application';
import {
  MACOS_ACCESS_STATUS_IDS,
  type MacOsAccessStatus,
  type MacOsPermissionObservation,
  type MacOsPrivacyDestination,
} from '@/lib/models/macos-permissions';
import { LOG_DOMAINS, LOG_EVENTS } from '@/lib/models/telemetry';
import { LoggerService } from '@/lib/services/logger-service';
import { OperatingSystemService } from '@/lib/services/operating-system-service';

const DEFAULT_OBSERVATION: MacOsPermissionObservation = {
  applicationDataStatus: MACOS_ACCESS_STATUS_IDS.notChecked,
  observedAtMs: null,
};
const OBSERVATION_FILE_NAME = 'data/permission-observation.json';
const OBSERVATION_KEY = 'observation';

/**
 * Owns the macOS privacy-settings integration and the last scan observation.
 * Loading this state never probes protected paths, so opening Settings cannot
 * itself cause an operating-system authorization prompt.
 */
export class MacOsPermissionService {
  private static storePromise: Promise<Store> | null = null;

  static isMacOs(): boolean {
    return OperatingSystemService.isMacOs();
  }

  static defaultObservation(): MacOsPermissionObservation {
    return { ...DEFAULT_OBSERVATION };
  }

  static async loadObservation(): Promise<MacOsPermissionObservation> {
    if (!this.isMacOs()) return this.defaultObservation();
    let store: Store;
    let value: unknown;
    try {
      store = await this.store();
      value = await store.get<unknown>(OBSERVATION_KEY);
    } catch (error) {
      LoggerService.warn(LOG_DOMAINS.macosPermissions, LOG_EVENTS.permissionObservationLoadFailed, { error });
      return this.defaultObservation();
    }
    if (value === undefined) return this.defaultObservation();
    try {
      return this.parseObservation(value);
    } catch (error) {
      LoggerService.warn(LOG_DOMAINS.macosPermissions, LOG_EVENTS.permissionObservationInvalid, { error });
      try {
        await store.delete(OBSERVATION_KEY);
        await store.save();
      } catch (clearError) {
        LoggerService.warn(LOG_DOMAINS.macosPermissions, LOG_EVENTS.permissionObservationClearFailed, {
          error: clearError,
        });
      }
      return this.defaultObservation();
    }
  }

  static async recordApplicationDataAccess(result: ApplicationLeftoverScanResult): Promise<void> {
    if (!this.isMacOs() || !result.supported) return;
    const observation: MacOsPermissionObservation = {
      // Inventory completeness can also fail for reasons unrelated to TCC.
      // Only the backend's explicit access signal is evidence of a permission
      // limitation; otherwise this observation would mislead users into
      // changing macOS privacy settings for an unrelated scan failure.
      applicationDataStatus: result.accessLimited ? MACOS_ACCESS_STATUS_IDS.limited : MACOS_ACCESS_STATUS_IDS.available,
      observedAtMs: result.scannedAtMs,
    };
    try {
      const store = await this.store();
      await store.set(OBSERVATION_KEY, observation);
      await store.save();
      LoggerService.info(LOG_DOMAINS.macosPermissions, LOG_EVENTS.permissionObservationSaved, {
        status: observation.applicationDataStatus,
      });
    } catch (error) {
      LoggerService.warn(LOG_DOMAINS.macosPermissions, LOG_EVENTS.permissionObservationSaveFailed, { error });
    }
  }

  static async openPrivacySettings(destination: MacOsPrivacyDestination): Promise<void> {
    LoggerService.info(LOG_DOMAINS.macosPermissions, LOG_EVENTS.privacySettingsOpenRequested, { destination });
    await invoke<void>('open_privacy_settings', { destination });
  }

  private static store(): Promise<Store> {
    this.storePromise ??= load(OBSERVATION_FILE_NAME, { autoSave: false }).catch(error => {
      this.storePromise = null;
      throw error;
    });
    return this.storePromise;
  }

  private static parseObservation(value: unknown): MacOsPermissionObservation {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
      throw new Error('Invalid macOS permission observation');
    }
    const keys = Object.keys(value).sort();
    if (keys.length !== 2 || keys[0] !== 'applicationDataStatus' || keys[1] !== 'observedAtMs') {
      throw new Error('Invalid macOS permission observation');
    }
    const record = value as Readonly<Record<string, unknown>>;
    const status = record.applicationDataStatus;
    const observedAtMs = record.observedAtMs;
    if (
      !Object.values(MACOS_ACCESS_STATUS_IDS).includes(status as MacOsAccessStatus) ||
      (observedAtMs !== null && (typeof observedAtMs !== 'number' || observedAtMs < 0))
    ) {
      throw new Error('Invalid macOS permission observation');
    }
    return {
      applicationDataStatus: status as MacOsAccessStatus,
      observedAtMs,
    };
  }
}
