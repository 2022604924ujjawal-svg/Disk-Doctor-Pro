import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { EVENT_NAMES } from '@/lib/models/telemetry';
import type { AnalysisDeleteResult, AnalysisResult } from '@/lib/models/analysis';
import type { TraversalProgress } from '@/lib/models/progress';

export class AnalysisService {
  static analyze(path?: string, refresh = false): Promise<AnalysisResult> {
    return invoke<AnalysisResult>('analyze_path', {
      path: path?.trim() || null,
      refresh,
    });
  }

  static listenProgress(handler: (progress: TraversalProgress) => void): Promise<UnlistenFn> {
    return listen<TraversalProgress>(EVENT_NAMES.analysisProgress, event => handler(event.payload));
  }

  static cancel(): Promise<void> {
    return invoke<void>('cancel_analysis');
  }

  static deletePermanently(scanId: number, selectedPath: string): Promise<AnalysisDeleteResult> {
    return invoke<AnalysisDeleteResult>('delete_analysis_entry_permanently', { scanId, selectedPath });
  }
}
