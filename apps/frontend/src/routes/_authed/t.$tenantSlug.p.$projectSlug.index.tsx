import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowRightIcon, CopyIcon } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { CommitLink } from "@/components/commit-link";
import { BuildStatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { $api, errorMessage, type Project } from "@/lib/api";
import {
  roleAtLeast,
  useBuilds,
  useMyRole,
  useResolvedProject,
  useResolvedTenant,
} from "@/lib/queries";
import { formatDate } from "@/lib/utils";

/** Radix Select treats "" as "clear", so the GitHub link form needs a sentinel. */
const NO_INSTALLATION = "none";

export const Route = createFileRoute("/_authed/t/$tenantSlug/p/$projectSlug/")({
  component: ProjectPage,
});

function ProjectPage() {
  const { tenantSlug, projectSlug } = Route.useParams();
  const { me } = Route.useRouteContext();
  const { tenant } = useResolvedTenant(tenantSlug);
  const { project, isLoading } = useResolvedProject(tenant?.id, projectSlug);
  const { role } = useMyRole(tenant?.id);

  if (!tenant || !project) {
    return (
      <p className="py-16 text-center text-sm text-muted-foreground">
        {isLoading || !tenant ? "Loading…" : `No project named “${projectSlug}”.`}
      </p>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{project.name}</h1>
        <p className="text-sm text-muted-foreground">
          {tenantSlug}/{project.slug} · default branch {project.default_branch}
        </p>
      </div>

      <Tabs defaultValue="builds" className="space-y-4">
        <TabsList>
          <TabsTrigger value="builds">Builds</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
          <TabsTrigger value="ci">CI usage</TabsTrigger>
        </TabsList>

        <TabsContent value="builds">
          <BuildsTable
            projectId={project.id}
            githubRepo={project.github_repo}
            tenantSlug={tenantSlug}
            projectSlug={project.slug}
          />
        </TabsContent>

        <TabsContent value="settings" className="space-y-4">
          <ProjectSettings project={project} canEdit={roleAtLeast(role, "admin")} />
          <GithubLink project={project} tenantId={tenant.id} canEdit={roleAtLeast(role, "admin")} />
        </TabsContent>

        <TabsContent value="ci">
          <CiUsage tenantSlug={tenantSlug} projectSlug={project.slug} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

function BuildsTable({
  projectId,
  githubRepo,
  tenantSlug,
  projectSlug,
}: {
  projectId: string;
  githubRepo: string | null | undefined;
  tenantSlug: string;
  projectSlug: string;
}) {
  const builds = useBuilds(projectId);
  const navigate = useNavigate();

  return (
    <Card>
      <CardContent>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="w-20">Build</TableHead>
              <TableHead>Branch</TableHead>
              <TableHead className="w-28">Commit</TableHead>
              <TableHead className="w-44">Status</TableHead>
              <TableHead>Comparisons</TableHead>
              <TableHead className="w-48">Created</TableHead>
              <TableHead className="w-16">
                <span className="sr-only">Open</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {builds.data?.builds.map((build) => {
              const to = "/t/$tenantSlug/p/$projectSlug/builds/$number" as const;
              const params = { tenantSlug, projectSlug, number: String(build.number) };
              return (
                <TableRow
                  key={build.id}
                  className="group cursor-pointer hover:bg-muted/50"
                  onClick={() => {
                    // Don't hijack the click when the user is selecting text in a cell.
                    if (window.getSelection()?.toString()) return;
                    void navigate({ to, params });
                  }}
                >
                  <TableCell>
                    {/* The real, keyboard-focusable navigation. The row onClick above is
                        a mouse-only enhancement that points at the same destination. */}
                    <Link
                      to={to}
                      params={params}
                      className="font-medium underline-offset-4 hover:underline"
                    >
                      #{build.number}
                    </Link>
                  </TableCell>
                  <TableCell className="truncate">{build.branch}</TableCell>
                  <TableCell className="text-xs">
                    <CommitLink
                      githubRepo={githubRepo}
                      commitSha={build.commit_sha}
                      className="font-mono"
                    />
                  </TableCell>
                  <TableCell>
                    <BuildStatusBadge status={build.status} />
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {build.total_count} total · {build.changed_count} changed · {build.added_count}{" "}
                    added · {build.removed_count} removed
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {formatDate(build.created_at)}
                  </TableCell>
                  <TableCell className="text-right">
                    <span className="inline-flex items-center gap-1 text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
                      view
                      <ArrowRightIcon className="size-3" />
                    </span>
                  </TableCell>
                </TableRow>
              );
            })}
            {!builds.data?.builds.length ? (
              <TableRow>
                <TableCell colSpan={7} className="text-sm text-muted-foreground">
                  {builds.isLoading ? "Loading…" : "No builds yet. Upload screenshots from CI."}
                </TableCell>
              </TableRow>
            ) : null}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

function ProjectSettings({ project, canEdit }: { project: Project; canEdit: boolean }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(project.name);
  const [defaultBranch, setDefaultBranch] = useState(project.default_branch);
  const [threshold, setThreshold] = useState(String(project.diff_threshold));
  const [ratioFail, setRatioFail] = useState(String(project.diff_ratio_fail));
  const [viewportWidth, setViewportWidth] = useState(String(project.viewport_width));
  const [viewportHeight, setViewportHeight] = useState(String(project.viewport_height));
  const [retention, setRetention] = useState(
    project.build_retention_limit == null ? "" : String(project.build_retention_limit),
  );

  useEffect(() => {
    setName(project.name);
    setDefaultBranch(project.default_branch);
    setThreshold(String(project.diff_threshold));
    setRatioFail(String(project.diff_ratio_fail));
    setViewportWidth(String(project.viewport_width));
    setViewportHeight(String(project.viewport_height));
    setRetention(
      project.build_retention_limit == null ? "" : String(project.build_retention_limit),
    );
  }, [project]);

  const update = $api.useMutation("patch", "/v1/projects/{project_id}", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/tenants/{tenant_id}/projects"],
      });
      toast.success("Project updated");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not update project")),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    update.mutate({
      params: { path: { project_id: project.id } },
      body: {
        name: name.trim(),
        default_branch: defaultBranch.trim(),
        diff_threshold: Number(threshold),
        diff_ratio_fail: Number(ratioFail),
        viewport_width: Number(viewportWidth),
        viewport_height: Number(viewportHeight),
        build_retention_limit: retention.trim() === "" ? null : Number(retention),
      },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Project settings</CardTitle>
        <CardDescription>
          The pixel threshold controls per-pixel colour tolerance; the fail ratio is the share of
          changed pixels that marks a comparison as changed. The viewport is the window size VRT
          renders Storybook stories at (storybook-mode builds only).
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={onSubmit} className="grid max-w-xl gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="p-name">Name</Label>
            <Input
              id="p-name"
              value={name}
              disabled={!canEdit}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-branch">Default branch</Label>
            <Input
              id="p-branch"
              value={defaultBranch}
              disabled={!canEdit}
              onChange={(event) => setDefaultBranch(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-threshold">Diff threshold (0–1)</Label>
            <Input
              id="p-threshold"
              type="number"
              step="0.01"
              min="0"
              max="1"
              value={threshold}
              disabled={!canEdit}
              onChange={(event) => setThreshold(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-ratio">Fail ratio (0–1)</Label>
            <Input
              id="p-ratio"
              type="number"
              step="0.0001"
              min="0"
              max="1"
              value={ratioFail}
              disabled={!canEdit}
              onChange={(event) => setRatioFail(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-viewport-w">Storybook viewport width (px)</Label>
            <Input
              id="p-viewport-w"
              type="number"
              step="1"
              min="64"
              max="10000"
              value={viewportWidth}
              disabled={!canEdit}
              onChange={(event) => setViewportWidth(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-viewport-h">Storybook viewport height (px)</Label>
            <Input
              id="p-viewport-h"
              type="number"
              step="1"
              min="64"
              max="10000"
              value={viewportHeight}
              disabled={!canEdit}
              onChange={(event) => setViewportHeight(event.target.value)}
            />
          </div>
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="p-retention">Build retention (blank = unlimited)</Label>
            <Input
              id="p-retention"
              type="number"
              step="1"
              min="1"
              placeholder="Unlimited"
              value={retention}
              disabled={!canEdit}
              onChange={(event) => setRetention(event.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Keep only this many completed builds per project. Older builds (and their screenshots)
              are deleted automatically; builds referenced by the current baseline are always kept.
            </p>
          </div>
          <div className="sm:col-span-2">
            <Button type="submit" disabled={!canEdit || update.isPending}>
              {update.isPending ? "Saving…" : "Save changes"}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function GithubLink({
  project,
  tenantId,
  canEdit,
}: {
  project: Project;
  tenantId: string;
  canEdit: boolean;
}) {
  const queryClient = useQueryClient();
  const installations = $api.useQuery("get", "/v1/github/installations", {
    params: { query: { tenant_id: tenantId } },
  });
  const [installationId, setInstallationId] = useState(
    project.github_installation_id ? String(project.github_installation_id) : "",
  );
  const [repo, setRepo] = useState(project.github_repo ?? "");

  useEffect(() => {
    setInstallationId(project.github_installation_id ? String(project.github_installation_id) : "");
    setRepo(project.github_repo ?? "");
  }, [project]);

  const update = $api.useMutation("patch", "/v1/projects/{project_id}/github", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/tenants/{tenant_id}/projects"],
      });
      toast.success("GitHub link updated");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not update GitHub link")),
  });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    update.mutate({
      params: { path: { project_id: project.id } },
      body: {
        installation_id: installationId ? Number(installationId) : null,
        github_repo: repo.trim() ? repo.trim() : null,
      },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>GitHub</CardTitle>
        <CardDescription>
          Pick a linked installation and the repository builds belong to; VRT then posts a commit
          status for every build.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={onSubmit} className="grid max-w-xl gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="gh-installation">Installation</Label>
            <Select
              // Radix reserves the empty string, so "none" is the sentinel for
              // "no installation linked" and is mapped back to "" in state.
              value={installationId === "" ? NO_INSTALLATION : installationId}
              disabled={!canEdit}
              onValueChange={(value) => setInstallationId(value === NO_INSTALLATION ? "" : value)}
            >
              <SelectTrigger id="gh-installation" className="w-full">
                <SelectValue placeholder="None" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NO_INSTALLATION}>None</SelectItem>
                {installations.data?.installations.map((installation) => (
                  <SelectItem key={installation.id} value={String(installation.installation_id)}>
                    {installation.account_login} (#{installation.installation_id})
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="gh-repo">Repository</Label>
            <Input
              id="gh-repo"
              value={repo}
              disabled={!canEdit}
              placeholder="owner/repo"
              onChange={(event) => setRepo(event.target.value)}
            />
          </div>
          <div className="sm:col-span-2">
            <Button type="submit" disabled={!canEdit || update.isPending}>
              {update.isPending ? "Saving…" : "Save GitHub link"}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

type CiMode = "screenshots" | "storybook";

const CI_MODE_DESCRIPTION: Record<CiMode, string> = {
  screenshots: "Your CI captures the PNGs and uploads them one by one.",
  storybook:
    "Your CI uploads a built Storybook (a zip of storybook-static) and VRT renders every story " +
    "server-side in headless Chromium. Rendering happens between finalize and the comparison, so " +
    "the build passes through the “Rendering” state first.",
};

function ciSnippet(mode: CiMode, tenantSlug: string, projectSlug: string) {
  const createBuild = (body: string) => `# 1. create a build (PAT needs write:build)
BUILD=$(curl -sS -X POST \\
  "$VRT_URL/v1/ci/projects/${tenantSlug}/${projectSlug}/builds" \\
  -H "Authorization: Bearer $VRT_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '${body}' | jq -r .id)`;

  if (mode === "storybook") {
    return `${createBuild(
      `{"branch":"'"$GIT_BRANCH"'","commit_sha":"'"$GIT_SHA"'","mode":"storybook"}`,
    )}

# 2. zip the built Storybook and upload it (one bundle per build, max 200MB)
pnpm build-storybook
(cd storybook-static && zip -qr ../storybook-static.zip .)
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/storybook" \\
  -H "Authorization: Bearer $VRT_TOKEN" \\
  -F "file=@./storybook-static.zip"

# 3. finalize — VRT renders every story, then compares against the baseline
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/finalize" \\
  -H "Authorization: Bearer $VRT_TOKEN"

# 4. poll until the build leaves "rendering"/"processing"
curl -sS "$VRT_URL/v1/ci/builds/$BUILD" -H "Authorization: Bearer $VRT_TOKEN"`;
  }

  return `${createBuild(`{"branch":"'"$GIT_BRANCH"'","commit_sha":"'"$GIT_SHA"'"}`)}

# 2. upload one screenshot per snapshot
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/screenshots" \\
  -H "Authorization: Bearer $VRT_TOKEN" \\
  -F "name=home-page" \\
  -F "file=@./screenshots/home-page.png"

# 3. finalize — this queues the comparison job
curl -sS -X POST "$VRT_URL/v1/ci/builds/$BUILD/finalize" \\
  -H "Authorization: Bearer $VRT_TOKEN"

# 4. poll until the build leaves "processing"
curl -sS "$VRT_URL/v1/ci/builds/$BUILD" -H "Authorization: Bearer $VRT_TOKEN"`;
}

function CiUsage({ tenantSlug, projectSlug }: { tenantSlug: string; projectSlug: string }) {
  const [mode, setMode] = useState<CiMode>("screenshots");
  const snippet = ciSnippet(mode, tenantSlug, projectSlug);

  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0">
        <div>
          <CardTitle>CI usage</CardTitle>
          <CardDescription>
            Create a personal access token with the <code>write:build</code> scope first.
          </CardDescription>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={async () => {
            await navigator.clipboard.writeText(snippet);
            toast.success("Snippet copied");
          }}
        >
          <CopyIcon className="size-3.5" />
          Copy
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="ci-mode">Build mode</Label>
          <Select value={mode} onValueChange={(value) => setMode(value as CiMode)}>
            <SelectTrigger id="ci-mode" className="w-full max-w-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="screenshots">screenshots — CI uploads PNGs</SelectItem>
              <SelectItem value="storybook">storybook — VRT renders the stories</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">{CI_MODE_DESCRIPTION[mode]}</p>
        </div>
        <pre className="overflow-x-auto rounded-md bg-muted p-4 text-xs leading-relaxed">
          {snippet}
        </pre>
      </CardContent>
    </Card>
  );
}
