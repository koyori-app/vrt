import i18next, { type i18n as I18nInstance } from "i18next";

import { DEFAULT_LANGUAGE, type Language } from "./languages";
import { en } from "./resources/en";
import { ja } from "./resources/ja";

export const resources = {
  en: { translation: en },
  ja: { translation: ja },
} as const;

/**
 * リクエストごとに i18next のインスタンスを作る。
 *
 * SSR ではリクエストが並行するので、モジュール直下の 1 個を共有して
 * `changeLanguage` を呼ぶ形は他リクエストの言語を書き換えうる。`initReactI18next`
 * （＝react-i18next の既定インスタンス登録）も同じ理由で使わず、
 * `I18nextProvider` で明示的に配る。
 */
export function createI18n(language: Language): I18nInstance {
  const instance = i18next.createInstance();
  void instance.init({
    lng: language,
    fallbackLng: DEFAULT_LANGUAGE,
    resources,
    // 文言中の値は React が既にエスケープする。
    interpolation: { escapeValue: false },
    // 辞書は同梱していて非同期読み込みが無いので、Suspense に載せない。
    react: { useSuspense: false },
  });
  return instance;
}

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: { translation: typeof en };
  }
}

export * from "./languages";
