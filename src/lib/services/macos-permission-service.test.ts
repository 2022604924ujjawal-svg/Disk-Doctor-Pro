import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApplicationLeftoverScanResult } from '@/lib/models/application';
import { MACOS_PRIVACY_DESTINATION_IDS } from '@/lib/models/macos-permissions';
import { MacOsPermissionService } from '@/lib/services/macos-permission-service';

const { deleteMock, failReads, invokeMock, loadMock, platformMock, values } = vi.hoisted(() => {
  const storedValues = new Map<string, unknown>();
  const readFailure = { enabled: false };
  const deleteValue = vi.fn(async (key: string) => storedValues.delete(key));
  const store = {
    get: vi.fn(async (key: string) => {
      if (readFailure.enabled) throw new Error('store unavailable');
      return storedValues.get(key);
    }),
    set: vi.fn(async (key: string, value: unknown) => {
      storedValues.set(key, value);
    }),
    delete: deleteValue,
    save: vi.fn(async () => undefined),
  };
  return {
    deleteMock: deleteValue,
    failReads: readFailure,
    invokeMock: vi.fn(),
    loadMock: vi.fn(async () => store),
    platformMock: vi.fn(),
    values: storedValues,
  };
});

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock, isTauri: () => false }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: platformMock }));
vi.mock('@tauri-apps/plugin-store', () => ({ load: loadMock }));

function scanResult(overrides: Partial<ApplicationLeftoverScanResult> = {}): ApplicationLeftoverScanResult {
  return {
    schemaVersion: 2,
    scannedAtMs: 1_726_000_000_000,
    supported: true,
    inventoryComplete: true,
    accessLimited: false,
    candidates: [],
    totalBytes: 0,
    totalFileCount: 0,
    skippedCount: 0,
    elapsedMs: 10,
    ...overrides,
  };
}

describe('MacOsPermissionService', () => {
  beforeEach(() => {
    values.clear();
    deleteMock.mockClear();
    failReads.enabled = false;
    invokeMock.mockReset();
    platformMock.mockReset();
    platformMock.mockReturnValue('macos');
  });

  it('does not probe protected paths when no scan observation exists', async () => {
    await expect(MacOsPermissionService.loadObservation()).resolves.toEqual({
      applicationDataStatus: 'notChecked',
      observedAtMs: null,
    });
    expect(loadMock).toHaveBeenCalledWith('data/permission-observation.json', { autoSave: false });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('records limited access only from a completed application-data scan', async () => {
    await MacOsPermissionService.recordApplicationDataAccess(
      scanResult({ accessLimited: true, inventoryComplete: false })
    );

    await expect(MacOsPermissionService.loadObservation()).resolves.toEqual({
      applicationDataStatus: 'limited',
      observedAtMs: 1_726_000_000_000,
    });
  });

  it('does not treat a non-permission inventory failure as limited access', async () => {
    await MacOsPermissionService.recordApplicationDataAccess(scanResult({ inventoryComplete: false }));

    await expect(MacOsPermissionService.loadObservation()).resolves.toEqual({
      applicationDataStatus: 'available',
      observedAtMs: 1_726_000_000_000,
    });
  });

  it('ignores unsupported results instead of replacing a prior observation', async () => {
    await MacOsPermissionService.recordApplicationDataAccess(scanResult());
    const stored = values.get('observation');

    await MacOsPermissionService.recordApplicationDataAccess(scanResult({ supported: false, accessLimited: true }));

    expect(values.get('observation')).toBe(stored);
  });

  it('does not create macOS state on Windows', async () => {
    platformMock.mockReturnValue('windows');

    await MacOsPermissionService.recordApplicationDataAccess(scanResult());

    expect(values.has('observation')).toBe(false);
  });

  it('does not delete persisted state when the store cannot be read', async () => {
    values.set('observation', {
      applicationDataStatus: 'available',
      observedAtMs: 1_726_000_000_000,
    });
    failReads.enabled = true;

    await expect(MacOsPermissionService.loadObservation()).resolves.toEqual({
      applicationDataStatus: 'notChecked',
      observedAtMs: null,
    });

    expect(deleteMock).not.toHaveBeenCalled();
    expect(values.has('observation')).toBe(true);
  });

  it('opens the fixed privacy settings destination', async () => {
    invokeMock.mockResolvedValue(undefined);

    await MacOsPermissionService.openPrivacySettings(MACOS_PRIVACY_DESTINATION_IDS.fullDiskAccess);

    expect(invokeMock).toHaveBeenCalledWith('open_privacy_settings', {
      destination: MACOS_PRIVACY_DESTINATION_IDS.fullDiskAccess,
    });
  });
});
