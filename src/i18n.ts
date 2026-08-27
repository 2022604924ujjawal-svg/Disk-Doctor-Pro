import { createI18n } from 'vue-i18n';

import { LANGUAGE_IDS, type LanguageId } from '@/lib/models/settings';
import enUS from '@/locales/en-US.json';
import jaJP from '@/locales/ja-JP.json';
import zhCN from '@/locales/zh-CN.json';
import zhTW from '@/locales/zh-TW.json';

export type MessageSchema = typeof zhCN;
export type SupportedLocale = LanguageId;

/**
 * All locale resources are imported into one message graph so every view can switch languages offline.
 * Their bounded size does not justify asynchronous loading, extra failure states, or switch latency.
 */
export const i18n = createI18n<[MessageSchema], SupportedLocale>({
  legacy: false,
  globalInjection: false,
  locale: LANGUAGE_IDS.enUS,
  fallbackLocale: LANGUAGE_IDS.enUS,
  messages: {
    [LANGUAGE_IDS.zhCN]: zhCN,
    [LANGUAGE_IDS.zhTW]: zhTW,
    [LANGUAGE_IDS.jaJP]: jaJP,
    [LANGUAGE_IDS.enUS]: enUS,
  },
});
