import { createFileRoute } from "@tanstack/react-router";
import { useEffect } from "react";

type GithubSetupSearch = {
  installation_id?: number;
  setup_action?: string;
  state?: string;
};

export const Route = createFileRoute("/_authed/github/setup")({
  validateSearch: (search: Record<string, unknown>): GithubSetupSearch => {
    const installationId = Number(search.installation_id);
    return {
      installation_id:
        Number.isSafeInteger(installationId) && installationId > 0 ? installationId : undefined,
      setup_action: typeof search.setup_action === "string" ? search.setup_action : undefined,
      state: typeof search.state === "string" ? search.state : undefined,
    };
  },
  component: GithubSetupPage,
});

function GithubSetupPage() {
  const { installation_id, setup_action, state } = Route.useSearch();

  useEffect(() => {
    const fallback = "/";
    const safeState = state?.startsWith("/") && !state.startsWith("//") ? state : fallback;
    const target = new URL(safeState, window.location.origin);
    target.searchParams.set("tab", "settings");
    if (installation_id) {
      target.searchParams.set("github_installation_id", String(installation_id));
    }
    if (setup_action) {
      target.searchParams.set("github_setup_action", setup_action);
    }
    window.location.replace(`${target.pathname}${target.search}${target.hash}`);
  }, [installation_id, setup_action, state]);

  return <p className="py-16 text-center text-sm text-muted-foreground">Connecting GitHub…</p>;
}
