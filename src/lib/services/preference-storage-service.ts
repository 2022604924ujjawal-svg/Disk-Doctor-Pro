import { load, type Store } from '@tauri-apps/plugin-store';

import type { AppSettings } from '@/lib/models/settings';
import type { StorageScopePreferences } from '@/lib/models/storage-scope';

const SETTINGS_FILE_NAME = 'settings.json';
const SETTINGS_KEYS = {
  settings: 'settings',
  storageScopePreferences: 'storageScopePreferences',
} as const;

type SettingsKey = (typeof SETTINGS_KEYS)[keyof typeof SETTINGS_KEYS];

/**
 * Owns the single frontend settings document.
 *
 * Values remain unknown on reads so their owning domain parser validates every
 * persisted value before it enters application state.
 */
export class PreferenceStorageService {
  private static storePromise: Promise<Store> | null = null;
  private static mutationQueue: Promise<void> = Promise.resolve();

  static loadSettings(): Promise<unknown | null> {
    return this.read(SETTINGS_KEYS.settings);
  }

  static saveSettings(settings: AppSettings): Promise<void> {
    return this.write(SETTINGS_KEYS.settings, settings);
  }

  static clearSettings(): Promise<void> {
    return this.remove(SETTINGS_KEYS.settings);
  }

  static loadStorageScopePreferences(): Promise<unknown | null> {
    return this.read(SETTINGS_KEYS.storageScopePreferences);
  }

  static saveStorageScopePreferences(preferences: StorageScopePreferences): Promise<void> {
    return this.write(SETTINGS_KEYS.storageScopePreferences, preferences);
  }

  static clearStorageScopePreferences(): Promise<void> {
    return this.remove(SETTINGS_KEYS.storageScopePreferences);
  }

  private static store(): Promise<Store> {
    this.storePromise ??= load(SETTINGS_FILE_NAME, { autoSave: false }).catch(error => {
      this.storePromise = null;
      throw error;
    });
    return this.storePromise;
  }

  private static async read(key: SettingsKey): Promise<unknown | null> {
    await this.mutationQueue.catch(() => undefined);
    return (await this.store()).get<unknown>(key).then(value => value ?? null);
  }

  private static write(key: SettingsKey, value: unknown): Promise<void> {
    return this.enqueueMutation(async store => {
      await store.set(key, value);
      await store.save();
    });
  }

  private static remove(key: SettingsKey): Promise<void> {
    return this.enqueueMutation(async store => {
      await store.delete(key);
      await store.save();
    });
  }

  private static enqueueMutation(operation: (store: Store) => Promise<void>): Promise<void> {
    const mutation = this.mutationQueue.catch(() => undefined).then(async () => operation(await this.store()));
    this.mutationQueue = mutation;
    return mutation;
  }
}
