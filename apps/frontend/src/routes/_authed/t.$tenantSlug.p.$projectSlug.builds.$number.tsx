import { createFileRoute, Link } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowLeftIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import {
  ComparisonList,
  filterComparisons,
  useComparisonFilter,
} from "@/components/comparison-list";
import { ComparisonViewer } from "@/components/comparison-viewer";
import { CommitLink } from "@/components/commit-link";
import { BuildLogPanel } from "@/components/build-log-panel";
import { BuildStatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { $api, errorMessage, type Build, type Comparison } from "@/lib/api";
import { useResolvedProject, useResolvedTenant } from "@/lib/queries";
import { formatDate } from "@/lib/utils";

export const Route = createFileRoute("/_authed/t/$tenantSlug/p/$projectSlug/builds/$number")({
  component: BuildReviewPage,
});

/** Poll interval while the compare job is still running. */
const PROCESSING_POLL_MS = 3_000;

function BuildReviewPage() {
  const { tenantSlug, projectSlug, number } = Route.useParams();
  const { tenant } = useResolvedTenant(tenantSlug);
  const { project } = useResolvedProject(tenant?.id, projectSlug);

  // The URL carries the project-scoped build number, which the backend resolves
  // directly — no list scan, no pagination ceiling.
  const buildQuery = $api.useQuery(
    "get",
    "/v1/projects/{project_id}/builds/{number}",
    { params: { path: { project_id: project?.id ?? "", number: Number(number) } } },
    {
      enabled: !!project?.id && Number.isFinite(Number(number)),
      refetchInterval: (query) => {
        const status = query.state.data?.status;
        return status === "processing" || status === "pending" ? PROCESSING_POLL_MS : false;
      },
    },
  );

  const build = buildQuery.data;

  if (!build) {
    return (
      <p className="py-16 text-center text-sm text-muted-foreground">
        {buildQuery.isLoading || !project ? "Loading…" : `No build #${number} in this project.`}
      </p>
    );
  }

  return (
    <div className="space-y-4">
      <Link
        to="/t/$tenantSlug/p/$projectSlug"
        params={{ tenantSlug, projectSlug }}
        className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeftIcon className="size-3.5" />
        {project?.name ?? projectSlug}
      </Link>

      <BuildReview buildId={build.id} initialBuild={build} githubRepo={project?.github_repo} />
    </div>
  );
}

function BuildReview({
  buildId,
  initialBuild,
  githubRepo,
}: {
  buildId: string;
  initialBuild: Build;
  githubRepo: string | null | undefined;
}) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useComparisonFilter();
  const [selectedId, setSelectedId] = useState<string | undefined>(undefined);

  const buildQuery = $api.useQuery("get", "/v1/builds/{build_id}", {
    params: { path: { build_id: buildId } },
  });
  const build = buildQuery.data ?? initialBuild;

  const comparisonsQuery = $api.useQuery(
    "get",
    "/v1/builds/{build_id}/comparisons",
    { params: { path: { build_id: buildId } } },
    {
      refetchInterval:
        build.status === "processing" || build.status === "pending" ? PROCESSING_POLL_MS : false,
    },
  );

  const comparisons = useMemo(
    () => comparisonsQuery.data?.comparisons ?? [],
    [comparisonsQuery.data],
  );
  const visible = useMemo(() => filterComparisons(comparisons, filter), [comparisons, filter]);

  // Keep a valid selection as the filter or the polled data changes.
  useEffect(() => {
    if (visible.length === 0) {
      setSelectedId(undefined);
      return;
    }
    if (!visible.some((comparison) => comparison.id === selectedId)) {
      setSelectedId(visible[0]?.id);
    }
  }, [visible, selectedId]);

  const selected = comparisons.find((comparison) => comparison.id === selectedId);

  const invalidateBuild = useCallback(async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["get", "/v1/builds/{build_id}"] }),
      queryClient.invalidateQueries({ queryKey: ["get", "/v1/builds/{build_id}/comparisons"] }),
      queryClient.invalidateQueries({ queryKey: ["get", "/v1/projects/{project_id}/builds"] }),
    ]);
  }, [queryClient]);

  const reviewComparison = $api.useMutation("post", "/v1/comparisons/{comparison_id}/review", {
    onSuccess: invalidateBuild,
    onError: (error) => toast.error(errorMessage(error, "Review failed")),
  });

  const approveBuild = $api.useMutation("post", "/v1/builds/{build_id}/approve", {
    onSuccess: async () => {
      await invalidateBuild();
      toast.success("Build approved — screenshots promoted to the new baseline");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not approve build")),
  });

  const rejectBuild = $api.useMutation("post", "/v1/builds/{build_id}/reject", {
    onSuccess: async () => {
      await invalidateBuild();
      toast.success("Build rejected");
    },
    onError: (error) => toast.error(errorMessage(error, "Could not reject build")),
  });

  const review = useCallback(
    (comparison: Comparison | undefined, action: "approve" | "reject") => {
      if (!comparison) return;
      reviewComparison.mutate({
        params: { path: { comparison_id: comparison.id } },
        body: { action },
      });
    },
    [reviewComparison],
  );

  const move = useCallback(
    (delta: number) => {
      if (visible.length === 0) return;
      const index = visible.findIndex((comparison) => comparison.id === selectedId);
      const next = visible[Math.min(Math.max(index + delta, 0), visible.length - 1)];
      if (next) setSelectedId(next.id);
    },
    [visible, selectedId],
  );

  // j/k to walk the list, a/x to review — ignored while typing in a field.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      if (target?.isContentEditable) return;
      // Radix overlays (Select/DropdownMenu/Dialog) portal their content and own
      // the keyboard while open — typeahead in a Select would otherwise be eaten
      // by j/k/a/x. Bail whenever any popper layer is mounted or focus sits in one.
      if (
        typeof document !== "undefined" &&
        document.querySelector("[data-radix-popper-content-wrapper]")
      ) {
        return;
      }
      if (
        target?.closest('[role="dialog"],[role="menu"],[role="listbox"],[aria-expanded="true"]')
      ) {
        return;
      }

      switch (event.key) {
        case "j":
          event.preventDefault();
          move(1);
          break;
        case "k":
          event.preventDefault();
          move(-1);
          break;
        case "a":
          event.preventDefault();
          review(
            comparisons.find((comparison) => comparison.id === selectedId),
            "approve",
          );
          break;
        case "x":
          event.preventDefault();
          review(
            comparisons.find((comparison) => comparison.id === selectedId),
            "reject",
          );
          break;
        default:
          break;
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [comparisons, move, review, selectedId]);

  const pendingReviews = comparisons.filter(
    (comparison) => comparison.review_status === "pending" && comparison.status !== "unchanged",
  ).length;

  function onApproveBuild() {
    if (pendingReviews > 0) {
      const ok = confirm(
        `${pendingReviews} comparison(s) are still unreviewed. Approve the whole build anyway?`,
      );
      if (!ok) return;
      // `force` is what lets the backend approve past unreviewed comparisons.
      approveBuild.mutate({ params: { path: { build_id: buildId } }, body: { force: true } });
      return;
    }
    approveBuild.mutate({ params: { path: { build_id: buildId } }, body: { force: false } });
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-semibold tracking-tight">Build #{build.number}</h1>
        <BuildStatusBadge status={build.status} />
        <span className="text-sm text-muted-foreground">
          {build.branch} ·{" "}
          <CommitLink githubRepo={githubRepo} commitSha={build.commit_sha} className="font-mono" />
          {build.pull_request_number ? ` · PR #${build.pull_request_number}` : ""}
        </span>
        <span className="text-xs text-muted-foreground">{formatDate(build.created_at)}</span>
        <div className="flex-1" />
        {build.mode === "storybook" && build.storybook_uploaded ? (
          <Button variant="outline" asChild>
            {/* Opens the actual uploaded Storybook (Chromatic's "View Storybook").
                Served by the backend under the /api proxy; the manager UI loads its
                assets with relative paths so it resolves under the prefixed base. */}
            <a
              href={`/api/v1/builds/${build.id}/storybook/`}
              target="_blank"
              rel="noopener noreferrer"
            >
              Open Storybook
            </a>
          </Button>
        ) : null}
        <Button variant="success" disabled={approveBuild.isPending} onClick={onApproveBuild}>
          Approve build
        </Button>
        <Button
          variant="destructive"
          disabled={rejectBuild.isPending}
          onClick={() => {
            if (!confirm("Reject this build?")) return;
            rejectBuild.mutate({ params: { path: { build_id: buildId } } });
          }}
        >
          Reject build
        </Button>
      </div>

      <p className="text-xs text-muted-foreground">
        {build.total_count} comparisons · {build.changed_count} changed · {build.added_count} added
        · {build.removed_count} removed · {build.unchanged_count} unchanged
        {pendingReviews > 0 ? ` · ${pendingReviews} awaiting review` : ""}
      </p>
      {build.error_message ? (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {build.error_message}
        </p>
      ) : null}

      <BuildLogPanel buildId={buildId} status={build.status} />

      <div className="grid min-h-[70vh] grid-cols-1 gap-0 overflow-hidden rounded-xl border border-border lg:grid-cols-[280px_1fr]">
        <aside className="min-h-0 border-b border-border lg:border-b-0 lg:border-r">
          <ComparisonList
            comparisons={comparisons}
            selectedId={selectedId}
            onSelect={(comparison) => setSelectedId(comparison.id)}
            filter={filter}
            onFilterChange={setFilter}
          />
          <p className="border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
            <kbd className="rounded border border-border px-1">j</kbd>/
            <kbd className="rounded border border-border px-1">k</kbd> navigate ·{" "}
            <kbd className="rounded border border-border px-1">a</kbd> approve ·{" "}
            <kbd className="rounded border border-border px-1">x</kbd> reject
          </p>
        </aside>

        <section className="min-h-0">
          {selected ? (
            <ComparisonViewer
              comparison={selected}
              reviewPending={reviewComparison.isPending}
              onReview={(action) => review(selected, action)}
            />
          ) : (
            <p className="grid h-full place-items-center p-8 text-sm text-muted-foreground">
              {comparisonsQuery.isLoading ? "Loading comparisons…" : "No comparison selected."}
            </p>
          )}
        </section>
      </div>
    </div>
  );
}
