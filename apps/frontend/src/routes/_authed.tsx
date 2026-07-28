import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { useState } from "react";

import { CreateTenantDialog } from "@/components/create-tenant-dialog";
import { TopNav } from "@/components/top-nav";
import { meQueryOptions } from "@/lib/queries";

/**
 * Auth guard for every signed-in screen.
 *
 * `beforeLoad` runs on the server during SSR, where `src/lib/api.ts` forwards
 * the incoming request's `Cookie` header to the backend, so the session is
 * visible on the very first render — no auth flash. Any failure (401 from the
 * backend, or a transport error) sends the visitor to `/login` with the
 * original path so the OAuth round trip can come back to it.
 */
export const Route = createFileRoute("/_authed")({
  beforeLoad: async ({ context, location }) => {
    try {
      const me = await context.queryClient.ensureQueryData(meQueryOptions());
      return { me };
    } catch {
      throw redirect({ to: "/login", search: { redirect_to: location.href } });
    }
  },
  component: AuthedLayout,
});

function AuthedLayout() {
  const { me } = Route.useRouteContext();
  const [createTenantOpen, setCreateTenantOpen] = useState(false);

  return (
    <div className="min-h-screen">
      <TopNav me={me} onCreateTenant={() => setCreateTenantOpen(true)} />
      <main className="mx-auto max-w-7xl px-4 py-6">
        <Outlet />
      </main>
      <CreateTenantDialog open={createTenantOpen} onOpenChange={setCreateTenantOpen} />
    </div>
  );
}
