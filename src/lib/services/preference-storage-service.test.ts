import { beforeEach, describe, expect, it, vi } from 'vitest';

import { PreferenceStorageService } from '@/lib/services/preference-storage-service';

const { delayedFirstWrite, loadMock, saveMock, values } = vi.hoisted(() => {
  const storedValues = new Map<string, unknown>();
  const delayState = { enabled: false };
  const save = vi.fn(async () => undefined);
  const store = {
    get: vi.fn(async (key: string) => storedValues.get(key)),
    set: vi.fn(async (key: string, value: unknown) => {
      if (
        delayState.enabled &&
        key === 'storageScopePreferences' &&
        typeof value === 'object' &&
        value !== null &&
        'selectedPaths' in value &&
        (value.selectedPaths as Record<string, unknown>).analysis === '/first'
      ) {
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      storedValues.set(key, value);
    }),
    delete: vi.fn(async (key: string) => storedValues.delete(key)),
    save,
  };
  return {
    delayedFirstWrite: delayState,
    loadMock: vi.fn(async () => store),
    saveMock: save,
    values: storedValues,
  };
});

vi.mock('@tauri-apps/plugin-store', () => ({ load: loadMock }));

describe('PreferenceStorageService', () => {
  beforeEach(() => {
    values.clear();
    delayedFirstWrite.enabled = false;
    saveMock.mockClear();
  });

  it('uses one unversioned settings file and persists values directly', async () => {
    const preferences = {
      selectedPaths: { analysis: '/workspace' },
      recentFolders: ['/workspace'],
    };
    await PreferenceStorageService.saveStorageScopePreferences(preferences);

    expect(loadMock).toHaveBeenCalledWith('settings.json', { autoSave: false });
    expect(values.get('storageScopePreferences')).toEqual(preferences);
    expect(saveMock).toHaveBeenCalledOnce();
  });

  it('keeps settings domains as separate keys in the same store', async () => {
    const preferences = {
      selectedPaths: {
        analysis: '/Users/example/Downloads',
      },
      recentFolders: ['/Users/example/Downloads'],
    };

    await PreferenceStorageService.saveStorageScopePreferences(preferences);

    expect(await PreferenceStorageService.loadStorageScopePreferences()).toEqual(preferences);
    expect(await PreferenceStorageService.loadSettings()).toBeNull();
  });

  it('deletes an invalid domain value without clearing other settings', async () => {
    values.set('settings', { invalid: true });
    await PreferenceStorageService.saveStorageScopePreferences({
      selectedPaths: {},
      recentFolders: [],
    });

    await PreferenceStorageService.clearSettings();

    expect(await PreferenceStorageService.loadSettings()).toBeNull();
    expect(await PreferenceStorageService.loadStorageScopePreferences()).toEqual({
      selectedPaths: {},
      recentFolders: [],
    });
  });

  it('serializes rapid writes so the latest preference reaches disk last', async () => {
    delayedFirstWrite.enabled = true;

    await Promise.all([
      PreferenceStorageService.saveStorageScopePreferences({
        selectedPaths: { analysis: '/first' },
        recentFolders: ['/first'],
      }),
      PreferenceStorageService.saveStorageScopePreferences({
        selectedPaths: { analysis: '/second' },
        recentFolders: ['/second'],
      }),
    ]);

    expect(values.get('storageScopePreferences')).toEqual({
      selectedPaths: { analysis: '/second' },
      recentFolders: ['/second'],
    });
  });
});
