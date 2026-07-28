import { createFileRoute, redirect } from "@tanstack/react-router";
import { useState } from "react";

import { CreateTenantDialog } from "@/components/create-tenant-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { tenantsQueryOptions } from "@/lib/queries";

export const Route = createFileRoute("/_authed/")({
  // Resolved during SSR so the common case (has a tenant) never renders a
  // flash of the empty state before bouncing to the dashboard.
  loader: async ({ context }) => {
    const tenants = await context.queryClient.ensureQueryData(tenantsQueryOptions());
    const first = tenants[0];
    if (first) {
      throw redirect({ to: "/t/$tenantSlug", params: { tenantSlug: first.slug } });
    }
  },
  component: IndexPage,
});

function IndexPage() {
  const [open, setOpen] = useState(false);

  return (
    <div className="mx-auto max-w-lg py-16">
      <Card>
        <CardHeader>
          <CardTitle>Create your first tenant</CardTitle>
          <CardDescription>
            Projects, builds and GitHub installations all live inside a tenant. You need one before
            you can upload screenshots.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Button onClick={() => setOpen(true)}>New tenant</Button>
        </CardContent>
      </Card>
      <CreateTenantDialog open={open} onOpenChange={setOpen} />
    </div>
  );
}
