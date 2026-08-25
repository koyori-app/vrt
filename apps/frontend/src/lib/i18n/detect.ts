import { createIsomorphicFn } from "@tanstack/react-start";
import { getRequestHeader } from "@tanstack/react-start/server";

import { languageFromAcceptLanguage, normalizeLanguage, type Language } from "./languages";

/**
 * SSR が `Accept-Language` から決めた言語を、ブラウザ側で覚えておく場所。
 *
 * ハイドレーション後のローダー実行は `navigator.language` からも同じ答えを
 * 出せるはずだが、`Accept-Language` と `navigator` は必ずしも一致しない。
 * 一致しないまま画面遷移で選び直すと、設定を変えていないのに言語が入れ替わる
 * ——初回に SSR が使った値をそのまま使い続けることで、その揺れを消す。
 */
let rememberedBrowserLanguage: Language | undefined;

/** クライアントでのみ効く。サーバーはリクエストをまたいで共有されるので何もしない。 */
export const rememberBrowserLanguage = createIsomorphicFn()
  .server((_language: Language | undefined) => {})
  .client((language: Language | undefined) => {
    rememberedBrowserLanguage = language;
  });

/**
 * ユーザー設定が無いときに使う「この訪問者の言語」。
 *
 * - SSR: リクエストの `Accept-Language`
 * - ブラウザ: SSR が決めた値（[`rememberBrowserLanguage`]）、無ければ `navigator`
 */
export const detectBrowserLanguage = createIsomorphicFn()
  .server((): Language | undefined =>
    languageFromAcceptLanguage(getRequestHeader("accept-language")),
  )
  .client(
    (): Language | undefined =>
      rememberedBrowserLanguage ??
      normalizeLanguage(navigator.language) ??
      normalizeLanguage(navigator.languages?.[0]),
  );
