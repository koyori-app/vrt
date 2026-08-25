import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowRightIcon, CopyIcon, ExternalLinkIcon, SearchIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { Trans, useTranslation } from "react-i18next";
import { toast } from "sonner";

import { buildGraph, BuildGraph } from "@/components/build-graph";
import { CommitLink } from "@/components/commit-link";
import { BuildStatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
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
import { rememberSetupReturnPath } from "@/lib/github-setup";
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

type ProjectSearch = {
  tab?: "builds" | "settings" | "ci";
  github_installation_id?: number;
  github_setup_state?: string;
  github_setup_action?: string;
};

export const Route = createFileRoute("/_authed/t/$tenantSlug/p/$projectSlug/")({
  validateSearch: (search: Record<string, unknown>): ProjectSearch => {
    const installationId = Number(search.github_installation_id);
    const tab = search.tab;
    return {
      tab: tab === "builds" || tab === "settings" || tab === "ci" ? tab : undefined,
      github_installation_id:
        Number.isSafeInteger(installationId) && installationId > 0 ? installationId : undefined,
      github_setup_state:
        typeof search.github_setup_state === "string" ? search.github_setup_state : undefined,
      github_setup_action:
        typeof search.github_setup_action === "string" ? search.github_setup_action : undefined,
    };
  },
  component: ProjectPage,
});

function ProjectPage() {
  const { t } = useTranslation();
  const { tenantSlug, projectSlug } = Route.useParams();
  const {
    tab,
    github_installation_id: setupInstallationId,
    github_setup_state: setupState,
  } = Route.useSearch();
  const { me } = Route.useRouteContext();
  const { tenant } = useResolvedTenant(tenantSlug);
  const { project, isLoading } = useResolvedProject(tenant?.id, projectSlug);
  const { role } = useMyRole(tenant?.id);

  if (!tenant || !project) {
    return (
      <p className="py-16 text-center text-sm text-muted-foreground">
        {isLoading || !tenant ? t("common.loading") : t("project.missing", { slug: projectSlug })}
      </p>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{project.name}</h1>
        <p className="text-sm text-muted-foreground">
          {tenantSlug}/{project.slug} ·{" "}
          {t("project.defaultBranchIs", { branch: project.default_branch })}
        </p>
      </div>

      <Tabs defaultValue={tab ?? "builds"} className="space-y-4">
        <TabsList>
          <TabsTrigger value="builds">{t("project.tabs.builds")}</TabsTrigger>
          <TabsTrigger value="settings">{t("project.tabs.settings")}</TabsTrigger>
          <TabsTrigger value="ci">{t("project.tabs.ci")}</TabsTrigger>
        </TabsList>

        <TabsContent value="builds">
          <BuildsTable
            projectId={project.id}
            githubRepo={project.github_repo}
            defaultBranch={project.default_branch}
            tenantSlug={tenantSlug}
            projectSlug={project.slug}
          />
        </TabsContent>

        <TabsContent value="settings" className="space-y-4">
          <ProjectSettings project={project} canEdit={roleAtLeast(role, "admin")} />
          <GithubLink
            project={project}
            tenantId={tenant.id}
            tenantSlug={tenantSlug}
            projectSlug={project.slug}
            setupInstallationId={setupInstallationId}
            setupState={setupState}
            canEdit={roleAtLeast(role, "admin")}
          />
        </TabsContent>

        <TabsContent value="ci">
          <CiUsage tenantSlug={tenantSlug} projectSlug={project.slug} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

/** Page size for the builds table (the API caps limit at 100). */
const BUILD_LIMIT = 50;

function BuildsTable({
  projectId,
  githubRepo,
  defaultBranch,
  tenantSlug,
  projectSlug,
}: {
  projectId: string;
  githubRepo: string | null | undefined;
  defaultBranch: string;
  tenantSlug: string;
  projectSlug: string;
}) {
  const { t, i18n } = useTranslation();
  const [offset, setOffset] = useState(0);
  const builds = useBuilds(projectId, BUILD_LIMIT, offset);
  const navigate = useNavigate();
  const rows = builds.data?.builds;
  const total = builds.data?.total ?? 0;
  const graph = useMemo(() => buildGraph(rows ?? [], defaultBranch), [rows, defaultBranch]);

  // Retention pruning (or switching projects) can shrink the list below the
  // current offset; snap back to the last page that still has rows.
  useEffect(() => {
    if (builds.data && offset > 0 && offset >= builds.data.total) {
      const lastPage = Math.max(0, Math.ceil(builds.data.total / BUILD_LIMIT) - 1);
      setOffset(lastPage * BUILD_LIMIT);
    }
  }, [builds.data, offset]);

  return (
    <Card>
      <CardContent>
        <Table>
          <TableHeader>
            <TableRow>
              {/* The graph column is decorative and sized by its body cells. */}
              <TableHead className="p-0">
                <span className="sr-only">{t("builds.columns.graph")}</span>
              </TableHead>
              <TableHead className="w-20">{t("builds.columns.build")}</TableHead>
              <TableHead>{t("builds.columns.branch")}</TableHead>
              <TableHead className="w-28">{t("builds.columns.commit")}</TableHead>
              <TableHead className="w-44">{t("builds.columns.status")}</TableHead>
              <TableHead>{t("builds.columns.comparisons")}</TableHead>
              <TableHead className="w-48">{t("builds.columns.created")}</TableHead>
              <TableHead className="w-16">
                <span className="sr-only">{t("builds.columns.open")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {graph.rows.map(({ build, cells }) => {
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
                  <TableCell className="relative p-0">
                    {/* The SVG is absolutely positioned, so this spacer is what
                        actually reserves the column's width. */}
                    <div style={{ width: graph.width }} />
                    <BuildGraph cells={cells} />
                  </TableCell>
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
                    {t("builds.rowCounts", {
                      total: build.total_count,
                      changed: build.changed_count,
                      added: build.added_count,
                      removed: build.removed_count,
                    })}
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {formatDate(build.created_at, i18n.language)}
                  </TableCell>
                  <TableCell className="text-right">
                    <span className="inline-flex items-center gap-1 text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
                      {t("builds.view")}
                      <ArrowRightIcon className="size-3" />
                    </span>
                  </TableCell>
                </TableRow>
              );
            })}
            {!builds.data?.builds.length ? (
              <TableRow>
                <TableCell colSpan={8} className="text-sm text-muted-foreground">
                  {builds.isLoading ? t("common.loading") : t("builds.empty")}
                </TableCell>
              </TableRow>
            ) : null}
          </TableBody>
        </Table>
        {total > BUILD_LIMIT ? (
          <div className="mt-4 flex items-center justify-between gap-3">
            <p className="text-xs text-muted-foreground">
              {t("builds.range", {
                from: offset + 1,
                to: Math.min(offset + BUILD_LIMIT, total),
                total,
              })}
            </p>
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={offset === 0 || builds.isFetching}
                onClick={() => setOffset(Math.max(0, offset - BUILD_LIMIT))}
              >
                {t("builds.newer")}
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={offset + BUILD_LIMIT >= total || builds.isFetching}
                onClick={() => setOffset(offset + BUILD_LIMIT)}
              >
                {t("builds.older")}
              </Button>
            </div>
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}

function ProjectSettings({ project, canEdit }: { project: Project; canEdit: boolean }) {
  const { t } = useTranslation();
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
  const [reducedMotion, setReducedMotion] = useState(project.emulate_reduced_motion);

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
    setReducedMotion(project.emulate_reduced_motion);
  }, [project]);

  const update = $api.useMutation("patch", "/v1/projects/{project_id}", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/tenants/{tenant_id}/projects"],
      });
      toast.success(t("projectSettings.updated"));
    },
    onError: (error) => toast.error(errorMessage(error, t("projectSettings.updateFailed"))),
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
        emulate_reduced_motion: reducedMotion,
      },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("projectSettings.title")}</CardTitle>
        <CardDescription>{t("projectSettings.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={onSubmit} className="grid max-w-xl gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="p-name">{t("project.name")}</Label>
            <Input
              id="p-name"
              value={name}
              disabled={!canEdit}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-branch">{t("project.defaultBranch")}</Label>
            <Input
              id="p-branch"
              value={defaultBranch}
              disabled={!canEdit}
              onChange={(event) => setDefaultBranch(event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="p-threshold">{t("projectSettings.diffThreshold")}</Label>
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
            <Label htmlFor="p-ratio">{t("projectSettings.failRatio")}</Label>
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
            <Label htmlFor="p-viewport-w">{t("projectSettings.viewportWidth")}</Label>
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
            <Label htmlFor="p-viewport-h">{t("projectSettings.viewportHeight")}</Label>
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
            <Label htmlFor="p-retention">{t("projectSettings.retention")}</Label>
            <Input
              id="p-retention"
              type="number"
              step="1"
              min="1"
              placeholder={t("projectSettings.retentionPlaceholder")}
              value={retention}
              disabled={!canEdit}
              onChange={(event) => setRetention(event.target.value)}
            />
            <p className="text-xs text-muted-foreground">{t("projectSettings.retentionHint")}</p>
          </div>
          <div className="space-y-2 sm:col-span-2">
            {/* The cost has to be readable before the box is ticked: enabling this
                replaces the baseline once. Same warning as the README section. */}
            <label className="flex items-start gap-3 text-sm">
              <Checkbox
                className="mt-0.5"
                checked={reducedMotion}
                disabled={!canEdit}
                onCheckedChange={(checked) => setReducedMotion(checked === true)}
              />
              <span>
                <span className="font-medium">{t("projectSettings.reducedMotion")}</span>
                <span className="block text-xs text-muted-foreground">
                  {t("projectSettings.reducedMotionHint")}
                </span>
              </span>
            </label>
          </div>
          <div className="sm:col-span-2">
            <Button type="submit" disabled={!canEdit || update.isPending}>
              {update.isPending ? t("common.saving") : t("projectSettings.save")}
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
  tenantSlug,
  projectSlug,
  setupInstallationId,
  setupState,
  canEdit,
}: {
  project: Project;
  tenantId: string;
  tenantSlug: string;
  projectSlug: string;
  setupInstallationId?: number;
  setupState?: string;
  canEdit: boolean;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const app = $api.useQuery("get", "/v1/github/app", {});
  const installations = $api.useQuery("get", "/v1/github/installations", {
    params: { query: { tenant_id: tenantId } },
  });
  const [installationId, setInstallationId] = useState(
    project.github_installation_id ? String(project.github_installation_id) : "",
  );
  const [repo, setRepo] = useState(project.github_repo ?? "");
  const [repoSearch, setRepoSearch] = useState("");
  const claimedSetupId = useRef<number | undefined>(undefined);

  const repositories = $api.useQuery(
    "get",
    "/v1/github/installations/{installation_id}/repositories",
    {
      params: {
        path: { installation_id: Number(installationId || 0) },
        query: { tenant_id: tenantId },
      },
    },
    // repository 名（private 含む）は admin にしか出さない。
    { enabled: canEdit && installationId !== "" },
  );

  useEffect(() => {
    setInstallationId(project.github_installation_id ? String(project.github_installation_id) : "");
    setRepo(project.github_repo ?? "");
  }, [project]);

  const update = $api.useMutation("patch", "/v1/projects/{project_id}/github", {
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/tenants/{tenant_id}/projects"],
      });
      toast.success(t("github.updated"));
    },
    onError: (error) => toast.error(errorMessage(error, t("github.updateFailed"))),
  });

  const claim = $api.useMutation("post", "/v1/github/installations/{installation_id}/claim", {
    retry: (failureCount, error) => failureCount < 10 && errorMessage(error, "") === "not-found",
    retryDelay: 1_000,
    onSuccess: async (installation) => {
      setInstallationId(String(installation.installation_id));
      await queryClient.invalidateQueries({
        queryKey: ["get", "/v1/github/installations"],
      });
      toast.success(t("github.installed", { account: installation.account_login }));
    },
    onError: (error) => toast.error(errorMessage(error, t("github.connectFailed"))),
  });

  // claim はサーバ発行の one-time state が揃っているときだけ走る。
  // installation_id だけの URL を踏まされても、state が無ければ何も起きない。
  useEffect(() => {
    if (!canEdit || !setupInstallationId || !setupState) return;
    if (claimedSetupId.current === setupInstallationId) return;
    claimedSetupId.current = setupInstallationId;
    claim.mutate({
      params: { path: { installation_id: setupInstallationId } },
      body: { tenant_id: tenantId, state: setupState },
    });
  }, [canEdit, claim, setupInstallationId, setupState, tenantId]);

  const normalizedSearch = repoSearch.trim().toLocaleLowerCase();
  const filteredRepositories = repositories.data?.repositories.filter((repository) =>
    repository.full_name.toLocaleLowerCase().includes(normalizedSearch),
  );

  // install_url が設定済みで、かつ URL として解釈できるときだけ導線を出す。
  let installBaseUrl: URL | undefined;
  if (app.data?.install_url) {
    try {
      installBaseUrl = new URL(app.data.install_url);
    } catch {
      installBaseUrl = undefined;
    }
  }

  const setupStateMutation = $api.useMutation("post", "/v1/github/setup/state", {
    onError: (error) => toast.error(errorMessage(error, t("github.startInstallFailed"))),
  });

  function startInstall() {
    if (!installBaseUrl) return;
    setupStateMutation.mutate(
      { body: { tenant_id: tenantId } },
      {
        onSuccess: ({ state }) => {
          rememberSetupReturnPath(
            state,
            `/t/${encodeURIComponent(tenantSlug)}/p/${encodeURIComponent(projectSlug)}?tab=settings`,
          );
          const url = new URL(installBaseUrl);
          url.searchParams.set("state", state);
          window.location.assign(url.toString());
        },
      },
    );
  }

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
        <div className="flex flex-wrap items-center justify-between gap-3">
          <CardTitle>GitHub</CardTitle>
          {installBaseUrl && canEdit ? (
            <Button
              variant="outline"
              size="sm"
              onClick={startInstall}
              disabled={setupStateMutation.isPending}
            >
              {t("github.install")}
              <ExternalLinkIcon className="size-4" />
            </Button>
          ) : null}
        </div>
        <CardDescription>{t("github.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={onSubmit} className="grid max-w-xl gap-4">
          {!app.isLoading && !installBaseUrl ? (
            <p className="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-700 dark:text-amber-300">
              {t("github.installUrlMissing")}
            </p>
          ) : null}
          {claim.isPending ? (
            <p className="text-sm text-muted-foreground">{t("github.connecting")}</p>
          ) : null}
          <div className="space-y-2">
            <Label htmlFor="gh-installation">{t("github.organization")}</Label>
            <Select
              // Radix reserves the empty string, so "none" is the sentinel for
              // "no installation linked" and is mapped back to "" in state.
              value={installationId === "" ? NO_INSTALLATION : installationId}
              disabled={!canEdit}
              onValueChange={(value) => {
                const next = value === NO_INSTALLATION ? "" : value;
                setInstallationId(next);
                setRepo("");
                setRepoSearch("");
              }}
            >
              <SelectTrigger id="gh-installation" className="w-full">
                <SelectValue placeholder={t("github.chooseOrganization")} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NO_INSTALLATION}>{t("github.none")}</SelectItem>
                {installations.data?.installations.map((installation) => (
                  <SelectItem key={installation.id} value={String(installation.installation_id)}>
                    {installation.account_login}
                    {installation.account_type === "Organization"
                      ? ` · ${t("github.organizationTag")}`
                      : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {installationId ? (
            <div className="space-y-2">
              <Label htmlFor="gh-repo-search">{t("github.repository")}</Label>
              <div className="relative">
                <SearchIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  id="gh-repo-search"
                  value={repoSearch}
                  disabled={!canEdit || repositories.isLoading}
                  className="pl-9"
                  placeholder={t("github.searchRepositories")}
                  onChange={(event) => setRepoSearch(event.target.value)}
                />
              </div>
              <Select value={repo || undefined} disabled={!canEdit} onValueChange={setRepo}>
                <SelectTrigger className="w-full">
                  <SelectValue
                    placeholder={
                      repositories.isLoading
                        ? t("github.loadingRepositories")
                        : t("github.chooseRepository")
                    }
                  />
                </SelectTrigger>
                <SelectContent>
                  {filteredRepositories?.map((repository) => (
                    <SelectItem key={repository.id} value={repository.full_name}>
                      {repository.full_name}
                      {repository.private ? ` · ${t("github.private")}` : ""}
                      {repository.archived ? ` · ${t("github.archived")}` : ""}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {!repositories.isLoading && filteredRepositories?.length === 0 ? (
                <p className="text-xs text-muted-foreground">{t("github.noRepositories")}</p>
              ) : null}
              {repositories.isError ? (
                <p className="text-xs text-destructive">
                  {errorMessage(repositories.error, t("github.loadRepositoriesFailed"))}
                </p>
              ) : null}
            </div>
          ) : null}
          <div>
            <Button
              type="submit"
              disabled={!canEdit || update.isPending || (!!installationId && !repo)}
            >
              {update.isPending ? t("common.saving") : t("github.save")}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

type CiMode = "screenshots" | "storybook";

const CI_MODE_DESCRIPTION_KEY = {
  screenshots: "ci.modes.screenshotsDescription",
  storybook: "ci.modes.storybookDescription",
} as const satisfies Record<CiMode, string>;

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

# 4. poll until the build leaves "queued"/"rendering"/"processing"
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

# 4. poll until the build leaves "queued"/"processing"
curl -sS "$VRT_URL/v1/ci/builds/$BUILD" -H "Authorization: Bearer $VRT_TOKEN"`;
}

function CiUsage({ tenantSlug, projectSlug }: { tenantSlug: string; projectSlug: string }) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<CiMode>("screenshots");
  const snippet = ciSnippet(mode, tenantSlug, projectSlug);

  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-3 space-y-0">
        <div>
          <CardTitle>{t("ci.title")}</CardTitle>
          <CardDescription>
            <Trans i18nKey="ci.description" components={{ code: <code /> }} />
          </CardDescription>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={async () => {
            await navigator.clipboard.writeText(snippet);
            toast.success(t("ci.copied"));
          }}
        >
          <CopyIcon className="size-3.5" />
          {t("tokens.copy")}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="ci-mode">{t("ci.buildMode")}</Label>
          <Select value={mode} onValueChange={(value) => setMode(value as CiMode)}>
            <SelectTrigger id="ci-mode" className="w-full max-w-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="screenshots">{t("ci.modes.screenshots")}</SelectItem>
              <SelectItem value="storybook">{t("ci.modes.storybook")}</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">{t(CI_MODE_DESCRIPTION_KEY[mode])}</p>
        </div>
        <pre className="overflow-x-auto rounded-md bg-muted p-4 text-xs leading-relaxed">
          {snippet}
        </pre>
      </CardContent>
    </Card>
  );
}
