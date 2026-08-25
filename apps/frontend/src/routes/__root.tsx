import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, HeadContent, Outlet, Scripts } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { I18nextProvider, useTranslation } from "react-i18next";

import type { Me } from "@/lib/api";
import { Toaster } from "@/components/ui/sonner";
import { detectBrowserLanguage, rememberBrowserLanguage } from "@/lib/i18n/detect";
import { createI18n, resolveLanguage, type Language } from "@/lib/i18n";
import { meQueryOptions } from "@/lib/queries";

import appCss from "@/styles.css?url";

export interface RouterContext {
  queryClient: QueryClient;
}

/**
 * 言語のためだけの `/me` 待ちに与える上限。
 *
 * この待ちは**全ページ**の描画の前に入る。バックエンドが「接続は受けるが
 * 応答しない」状態（デプロイ中・DB ロック等）だと、`/api` クライアントには
 * タイムアウトが無いため、復旧導線であるログイン画面ごと返らなくなる。
 * 言語は外しても読めるが、ページが出ないのは外せない——上限を超えたら
 * ブラウザの言語で描く。
 */
const LANGUAGE_LOOKUP_TIMEOUT_MS = 1_500;

/**
 * 表示言語のためのユーザー設定の取得。
 *
 * すでにキャッシュにあれば通信しない（`_authed` の認可判定が先に埋めている
 * 場合はこれで足りる）。未ログインの `/me` は 401 になるが、ここでは言語の
 * ためだけに読むので「設定なし」として扱い、認可の判断は `_authed` に任せる。
 * リクエストは中断せず、結果はキャッシュに載って後続の判定に使われる。
 */
async function userLanguageSetting(queryClient: QueryClient): Promise<string | null | undefined> {
  const options = meQueryOptions();
  const cached = queryClient.getQueryData<Me>(options.queryKey);
  if (cached) return cached.language;

  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const me = await Promise.race([
      queryClient.ensureQueryData(options).catch(() => undefined),
      new Promise<undefined>((resolve) => {
        timer = setTimeout(() => resolve(undefined), LANGUAGE_LOOKUP_TIMEOUT_MS);
      }),
    ]);
    return me?.language;
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export const Route = createRootRouteWithContext<RouterContext>()({
  /**
   * 表示言語は描画より前に決める。SSR で決めた値がローダーデータとして
   * クライアントへ渡るので、ハイドレーションが別の言語で始まることはない。
   */
  loader: async ({
    context,
  }): Promise<{ language: Language; browserLanguage: Language | undefined }> => {
    const browserLanguage = detectBrowserLanguage();
    const userLanguage = await userLanguageSetting(context.queryClient);
    return { language: resolveLanguage(userLanguage, browserLanguage), browserLanguage };
  },
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { title: "VRT" },
    ],
    links: [{ rel: "stylesheet", href: appCss }],
  }),
  component: RootComponent,
});

function RootComponent() {
  const { language, browserLanguage } = Route.useLoaderData();

  // SSR が見た `Accept-Language` の結論を、以降のクライアント側解決へ引き継ぐ。
  rememberBrowserLanguage(browserLanguage);

  return (
    <RootDocument language={language}>
      <Outlet />
    </RootDocument>
  );
}

function RootDocument({
  language,
  children,
}: Readonly<{ language: Language; children: ReactNode }>) {
  // インスタンスはこのドキュメントに 1 つ。SSR ではリクエストごとに作られる。
  const [i18n] = useState(() => createI18n(language));

  useEffect(() => {
    // フルリロードやログイン状態の変化でローダーの言語が変わったら追従する
    // （設定画面からの切り替えは、その場で `changeLanguage` を呼ぶ）。
    if (i18n.language !== language) void i18n.changeLanguage(language);
  }, [i18n, language]);

  return (
    <html lang={language} className="dark">
      <head>
        <HeadContent />
      </head>
      <body className="min-h-screen bg-background text-foreground">
        <I18nextProvider i18n={i18n}>
          <LanguageAttribute />
          {children}
        </I18nextProvider>
        {/* The document is hard-coded to `dark`; there is no next-themes provider. */}
        <Toaster theme="dark" position="bottom-right" richColors closeButton />
        <Scripts />
      </body>
    </html>
  );
}

/**
 * `<html lang>` を現在の言語に合わせ続ける。
 *
 * React が描く `lang` はローダーの値なので、設定画面でその場で切り替えたときは
 * 属性だけ取り残される。スクリーンリーダーや `:lang()` が拾う値なので合わせる。
 */
function LanguageAttribute() {
  const { i18n } = useTranslation();

  useEffect(() => {
    document.documentElement.lang = i18n.language;
  }, [i18n.language]);

  return null;
}
