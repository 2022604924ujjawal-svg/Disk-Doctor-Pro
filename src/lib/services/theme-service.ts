import { THEME_IDS } from '@/lib/models/settings';
import type { AppSettings } from '@/lib/models/settings';

type ThemeId = AppSettings['theme'];

export class ThemeService {
  static mediaQuery: MediaQueryList | null = null;
  static mediaListener: ((event: MediaQueryListEvent) => void) | null = null;

  static apply(theme: ThemeId): void {
    ThemeService.stopSystemListener();
    if (theme !== THEME_IDS.system) {
      document.documentElement.dataset.theme = theme;
      return;
    }

    ThemeService.mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
    ThemeService.applySystemPreference(ThemeService.mediaQuery.matches);
    ThemeService.mediaListener = event => ThemeService.applySystemPreference(event.matches);
    ThemeService.mediaQuery.addEventListener('change', ThemeService.mediaListener);
  }

  static applySystemPreference(isDark: boolean): void {
    document.documentElement.dataset.theme = isDark ? THEME_IDS.dark : THEME_IDS.light;
  }

  static stopSystemListener(): void {
    if (ThemeService.mediaQuery && ThemeService.mediaListener) {
      ThemeService.mediaQuery.removeEventListener('change', ThemeService.mediaListener);
    }
    ThemeService.mediaQuery = null;
    ThemeService.mediaListener = null;
  }
}
