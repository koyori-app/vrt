import { createFileRoute, Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { buttonVariants } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type LoginSearch = {
  oauth_error?: string;
  redirect_to?: string;
};

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>): LoginSearch => ({
    oauth_error: typeof search.oauth_error === "string" ? search.oauth_error : undefined,
    redirect_to: typeof search.redirect_to === "string" ? search.redirect_to : undefined,
  }),
  component: LoginPage,
});

/**
 * OAuth login must be a full page navigation (plain `<a>`), not fetch: the
 * backend answers with a 302 to the provider and sets the PKCE/state cookie.
 * Going through `/api` keeps the callback cookie first-party.
 */
function providerLoginHref(provider: "github" | "gitlab", redirectTo: string) {
  return `/api/v1/auth/${provider}/login?redirect_to=${encodeURIComponent(redirectTo)}`;
}

function LoginPage() {
  const { t } = useTranslation();
  const { oauth_error, redirect_to } = Route.useSearch();
  const redirectTo = redirect_to && redirect_to.startsWith("/") ? redirect_to : "/";

  return (
    <div className="flex min-h-screen items-center justify-center px-4">
      <Card className="w-full max-w-sm">
        <CardHeader className="text-center">
          <CardTitle className="text-2xl">
            {/* shadcn の CardTitle は div なので、ページ見出しはここで実タグにする */}
            <h1>VRT</h1>
          </CardTitle>
          <CardDescription>{t("login.tagline")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {oauth_error ? (
            <div
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {t("login.failed", { reason: oauth_error })}
            </div>
          ) : null}

          <a
            href={providerLoginHref("github", redirectTo)}
            className={cn(buttonVariants({ size: "lg" }), "w-full")}
          >
            {t("login.withGithub")}
          </a>
          <a
            href={providerLoginHref("gitlab", redirectTo)}
            className={cn(buttonVariants({ variant: "outline", size: "lg" }), "w-full")}
          >
            {t("login.withGitlab")}
          </a>

          <p className="text-center text-xs text-muted-foreground">
            {t("login.consent")}{" "}
            <Link to="/" className="underline underline-offset-4">
              {t("login.backHome")}
            </Link>
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
