import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { PAGE_IDS } from '@/lib/models/application-shell';
import type { AnalysisResult, DirectoryEntryInfo } from '@/lib/models/analysis';
import { AnalysisService } from '@/lib/services/analysis-service';
import { AnalysisCacheUtils } from '@/lib/utils/analysis-cache';

import { useAnalysisStore } from './analysis-store';
import { useAppStore } from './app-store';

const result: AnalysisResult = {
  scanId: 7,
  root: '/fixture',
  scannedAtMs: 1_000,
  totalBytes: 64,
  skippedCount: 0,
  entries: [],
};

const entry: DirectoryEntryInfo = {
  name: 'fixture.bin',
  path: '/fixture/fixture.bin',
  bytes: 64,
  isDirectory: false,
  fileCount: 1,
  directoryCount: 0,
  children: [],
};

describe('analysis store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.restoreAllMocks();
    vi.spyOn(AnalysisService, 'listenProgress').mockResolvedValue(vi.fn());
  });

  it('preserves the page selected while an analysis is running', async () => {
    let completeAnalysis: (value: AnalysisResult) => void = () => undefined;
    const analyze = vi.spyOn(AnalysisService, 'analyze').mockImplementation(
      () =>
        new Promise(resolve => {
          completeAnalysis = resolve;
        })
    );
    const appStore = useAppStore();
    const analysisStore = useAnalysisStore();

    const analysis = analysisStore.analyze('/fixture', true, true);
    await vi.waitFor(() => expect(analyze).toHaveBeenCalledOnce());
    appStore.navigate(PAGE_IDS.settings);
    completeAnalysis(result);
    await analysis;

    expect(appStore.currentPage).toBe(PAGE_IDS.settings);
    expect(analysisStore.result).toEqual(result);
  });

  it('does not navigate when showing a cached result', async () => {
    const appStore = useAppStore();
    const analysisStore = useAnalysisStore();
    const cacheKey = AnalysisCacheUtils.key('/fixture');
    analysisStore.cache = { [cacheKey]: result };
    analysisStore.cacheOrder = [cacheKey];
    appStore.navigate(PAGE_IDS.settings);
    const analyze = vi.spyOn(AnalysisService, 'analyze');

    await analysisStore.analyze('/fixture');

    expect(analyze).not.toHaveBeenCalled();
    expect(appStore.currentPage).toBe(PAGE_IDS.settings);
    expect(analysisStore.result).toEqual(result);
  });

  it('rejects deletion while an analysis is active', async () => {
    const remove = vi.spyOn(AnalysisService, 'deletePermanently');
    const analysisStore = useAnalysisStore();
    analysisStore.result = { ...result, entries: [entry] };
    analysisStore.pending = true;

    await analysisStore.deletePermanently(entry);

    expect(remove).not.toHaveBeenCalled();
    expect(analysisStore.deleting).toBe(false);
  });
});
