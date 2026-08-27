import type zhCN from '@/locales/zh-CN.json';

type AppMessageSchema = typeof zhCN;

declare module 'vue-i18n' {
  /**
   * The primary locale defines the message schema used for key completion and
   * compile-time parity checks. Vue I18n requires interface augmentation here.
   */
  // eslint-disable-next-line @typescript-eslint/no-empty-object-type
  export interface DefineLocaleMessage extends AppMessageSchema {}
}

export {};
