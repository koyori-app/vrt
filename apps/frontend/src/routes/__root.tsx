import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, HeadContent, Outlet, Scripts } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { I18nextProvider, useTranslation } from "react-i18next";

import { Toaster } from "@/components/ui/sonner";
import { detectBrowserLanguage, rememberBrowserLanguage } from "@/lib/i18n/detect";
import { createI18n, resolveLanguage, type Language } from "@/lib/i18n";
import { meQueryOptions } from "@/lib/queries";

import appCss from "@/styles.css?url";

export interface RouterContext {
  queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  /**
   * 表示言語は描画より前に決める。SSR で決めた値がローダーデータとして
   * クライアントへ渡るので、ハイドレーションが別の言語で始まることはない。
   *
   * `/me` は未ログインだと 401 になる。ここでは言語のためだけに読むので、
   * 失敗は「ユーザー設定なし」として扱い、認可の判断は `_authed` に任せる。
   */
  loader: async ({
    context,
  }): Promise<{ language: Language; browserLanguage: Language | undefined }> => {
    let userLanguage: string | null | undefined;
    try {
      userLanguage = (await context.queryClient.ensureQueryData(meQueryOptions())).language;
    } catch {
      userLanguage = undefined;
    }
    const browserLanguage = detectBrowserLanguage();
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
