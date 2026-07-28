import { createFileRoute, Link } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { PlusIcon, SettingsIcon } from "lucide-react";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";

import { BuildStatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { $api, errorMessage, type Project } from "@/lib/api";
import { roleAtLeast, useBuilds, useMyRole, useProjects, useResolvedTenant } from "@/lib/queries";
import { formatDate, slugify } from "@/lib/utils";

export const Route = createFileRoute("/_authed/t/$tenantSlug/")({
  component: TenantDashboard,
});

function TenantDashboard() {
  const { tenantSlug } = Route.useParams();
  const { me } = Route.useRouteContext();
  const { tenant, isLoading: tenantsLoading } = useResolvedTenant(tenantSlug);
  const projects = useProjects(tenant?.id);
  const { role } = useMyRole(tenant?.id);
  const [newProjectOpen, setNewProjectOpen] = useState(false);

  if (!tenant) {
    return (
      <p className="py-16 text-center text-sm text-muted-foreground">
        {tenantsLoading ? "Loading…" : `No tenant named “${tenantSlug}”.`}
      </p>
    );
  }

  const canCreate = roleAtLeast(role, "admin");

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{tenant.name}</h1>
          <p className="text-sm text-muted-foreground">
            {projects.data?.length ?? 0} project{projects.data?.length === 1 ? "" : "s"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" asChild>
            <Link to="/t/$tenantSlug/settings" params={{ tenantSlug }}>
              <SettingsIcon className="size-3.5" />
              Settings
            </Link>
          </Button>
          {canCreate ? (
            <Button size="sm" onClick={() => setNewProjectOpen(true)}>
              <PlusIcon className="size-3.5" />
              New project
            </Button>
          ) : null}
        </div>
      </div>

      {projects.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading projects…</p>
      ) : projects.data?.length ? (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {projects.data.map((project) => (
            <ProjectCard key={project.id} tenantSlug={tenantSlug} project={project} />
          ))}
        </div>
      ) : (
        <Card>
          <CardHeader>
            <CardTitle>No projects yet</CardTitle>
            <CardDescription>
              {canCreate
                ? "Create a project, then point your CI at it to upload screenshots."
                : "Ask a tenant admin to create the first project."}
            </CardDescription>
          </CardHeader>
        </Card>
      )}

      <CreateProjectDialog
        tenantId={tenant.id}
        open={newProjectOpen}
        onOpenChange={setNewProjectOpen}
      />
    </div>
  );
}

function ProjectCard({ tenantSlug, project }: { tenantSlug: string; project: Project }) {
  // One small query per card: the build list endpoint is the only source of a
  // project's most recent status (there is no per-project summary endpoint).
  const builds = useBuilds(project.id, 1);
  const latest = builds.data?.builds[0];

  return (
    <Link
      to="/t/$tenantSlug/p/$projectSlug"
      params={{ tenantSlug, projectSlug: project.slug }}
      className="block rounded-xl transition-colors hover:bg-accent/40"
    >
      <Card className="h-full">
        <CardHeader className="flex-row items-start justify-between gap-3 space-y-0">
          <div className="min-w-0">
            <CardTitle className="truncate">{project.name}</CardTitle>
            <CardDescription className="truncate">{project.slug}</CardDescription>
          </div>
          {latest ? <BuildStatusBadge status={latest.status} /> : null}
        </CardHeader>
        <CardContent className="text-xs text-muted-foreground">
          {latest ? (
            <>
              Build #{latest.number} on {latest.branch} · {formatDate(latest.created_at)}
            </>
          ) : builds.isLoading ? (
            "Loading…"
          ) : (
            "No builds yet"
          )}
        </CardContent>
      </Card>
    </Link>
  );
}

function CreateProjectDialog({
  tenantId,
  open,
  onOpenChange,
}: {
  tenantId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugTouched, setSlugTouched] = useState(false);
  const [defaultBranch, setDefaultBranch] = useState("main");
  const queryClient = useQueryClient();

  const createProject = $api.useMutation("post", "/v1/tenants/{tenant_id}/projects", {
    onSuccess: async (project) => {
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/tenants/{tenant_id}/projects"],
      });
      toast.success(`Created ${project.name}`);
      onOpenChange(false);
      setName("");
      setSlug("");
      setSlugTouched(false);
    },
    onError: (error) => toast.error(errorMessage(error, "Could not create project")),
  });

  const effectiveSlug = slugTouched ? slug : slugify(name);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    createProject.mutate({
      params: { path: { tenant_id: tenantId } },
      body: {
        name: name.trim(),
        slug: effectiveSlug,
        default_branch: defaultBranch.trim() || null,
      },
    });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={onSubmit}>
          <DialogHeader>
            <DialogTitle>New project</DialogTitle>
            <DialogDescription>
              Screenshots are grouped per project and compared against the default branch baseline.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="project-name">Name</Label>
              <Input
                id="project-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="Web app"
                required
                autoFocus
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="project-slug">Slug</Label>
              <Input
                id="project-slug"
                value={effectiveSlug}
                onChange={(event) => {
                  setSlugTouched(true);
                  setSlug(event.target.value);
                }}
                required
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="project-branch">Default branch</Label>
              <Input
                id="project-branch"
                value={defaultBranch}
                onChange={(event) => setDefaultBranch(event.target.value)}
                placeholder="main"
              />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={createProject.isPending || !name || !effectiveSlug}>
              {createProject.isPending ? "Creating…" : "Create project"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
