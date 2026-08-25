import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter as createTanStackRouter } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { routeTree } from "./routeTree.gen";

/** ルーターの既定 404。文言は i18n プロバイダ配下で解決する。 */
function NotFound() {
  const { t } = useTranslation();
  return <div className="p-8 text-sm">{t("common.notFound")}</div>;
}

export function getRouter() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        // The review UI polls builds while they are processing; keep data warm
        // but not stale enough to show an outdated status.
        staleTime: 10_000,
        retry: (failureCount, error) => {
          // Never retry auth/permission failures — the route guard handles them.
          const status = (error as { status?: number } | null)?.status;
          if (status === 401 || status === 403 || status === 404) return false;
          return failureCount < 2;
        },
      },
    },
  });

  const router = createTanStackRouter({
    routeTree,
    context: { queryClient },
    defaultPreload: "intent",
    scrollRestoration: true,
    defaultNotFoundComponent: NotFound,
    Wrap: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    ),
  });

  return router;
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof getRouter>;
  }
}
