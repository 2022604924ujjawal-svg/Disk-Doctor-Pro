import { i18n } from '@/i18n';
import { isLanguageId, LANGUAGE_IDS, LANGUAGE_OPTIONS } from '@/lib/models/settings';
import type { AppSettings } from '@/lib/models/settings';

export class LanguageService {
  static detectSystemLanguage(): AppSettings['language'] {
    if (typeof navigator === 'undefined') return LANGUAGE_IDS.enUS;
    const languages = navigator.languages?.length ? navigator.languages : [navigator.language];
    return this.resolveSupportedLanguage(languages);
  }

  static resolveSupportedLanguage(languages: readonly string[]): AppSettings['language'] {
    for (const language of languages) {
      const normalized = language.trim().toLowerCase();
      const option = LANGUAGE_OPTIONS.find(candidate =>
        candidate.browserLanguagePrefixes.some(prefix => normalized === prefix || normalized.startsWith(`${prefix}-`))
      );
      if (option) return option.id;
    }
    return LANGUAGE_IDS.enUS;
  }

  static apply(language: AppSettings['language']): void {
    const locale = isLanguageId(language) ? language : LANGUAGE_IDS.enUS;
    i18n.global.locale.value = locale;
    document.documentElement.lang = locale;
  }
}
