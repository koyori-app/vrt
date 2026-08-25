import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { client, type BuildLogEntry, type BuildStatus } from "@/lib/api";
import { cn } from "@/lib/utils";

/** Poll interval while the build is still running. */
const LOG_POLL_MS = 2_000;
/** How close to the bottom still counts as "following" the tail. */
const FOLLOW_THRESHOLD_PX = 40;

/**
 * A build can still produce logs while it is pending / queued / rendering / processing.
 * Once it reaches any other state the render + compare jobs are done, so polling
 * stops (one final fetch flushes the lines written alongside the transition).
 */
function isRunning(status: BuildStatus): boolean {
  return (
    status === "pending" || status === "queued" || status === "rendering" || status === "processing"
  );
}

function levelClass(level: string): string {
  if (level === "error") return "text-red-400";
  if (level === "warn") return "text-yellow-400";
  return "text-zinc-300";
}

/**
 * GitHub-Actions-style progress log for a build.
 *
 * Lines are fetched incrementally with an `?after=<cursor>` query so each poll
 * only transfers new rows, which the component appends to what it already has
 * (openapi-react-query would replace the page, so the raw client is used here to
 * accumulate instead). The view auto-scrolls to the tail unless the user scrolls
 * up; returning to the bottom re-enables following.
 */
export function BuildLogPanel({ buildId, status }: { buildId: string; status: BuildStatus }) {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<BuildLogEntry[]>([]);
  const [error, setError] = useState<string | undefined>(undefined);
  // Cursor lives in a ref so the poll callback never closes over a stale value
  // and the effect does not need to re-subscribe on every new line.
  const cursorRef = useRef(0);
  const followingRef = useRef(true);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const fetchNewLines = useCallback(async () => {
    const { data, error: fetchError } = await client.GET("/v1/builds/{build_id}/logs", {
      params: { path: { build_id: buildId }, query: { after: cursorRef.current } },
    });
    if (fetchError) {
      setError(t("buildLog.loadFailed"));
      return;
    }
    setError(undefined);
    cursorRef.current = data.last_id;
    if (data.entries.length > 0) {
      setEntries((prev) => [...prev, ...data.entries]);
    }
  }, [buildId, t]);

  // Reset when switching to a different build.
  useEffect(() => {
    cursorRef.current = 0;
    followingRef.current = true;
    setEntries([]);
    setError(undefined);
  }, [buildId]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;

    // Always fetch once (also flushes the final lines when we land here after
    // the build has already reached a terminal state).
    void fetchNewLines();

    if (isRunning(status)) {
      timer = setInterval(() => {
        if (!cancelled) void fetchNewLines();
      }, LOG_POLL_MS);
    }

    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, [status, fetchNewLines]);

  // Stick to the tail after new lines arrive, unless the user scrolled up.
  useEffect(() => {
    if (!followingRef.current) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [entries]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    followingRef.current = distance <= FOLLOW_THRESHOLD_PX;
  }, []);

  return (
    <section className="space-y-2">
      <div className="flex items-center gap-2">
        <h2 className="text-sm font-semibold tracking-tight">{t("buildLog.title")}</h2>
        {isRunning(status) ? (
          <span className="text-xs text-muted-foreground">{t("buildLog.live")}</span>
        ) : null}
      </div>
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="h-64 overflow-auto rounded-lg border border-border bg-zinc-950 p-3 font-mono text-xs leading-relaxed text-zinc-300"
      >
        {entries.length === 0 ? (
          <p className="text-zinc-500">
            {error ?? (isRunning(status) ? t("buildLog.waiting") : t("buildLog.empty"))}
          </p>
        ) : (
          entries.map((entry) => (
            <div key={entry.id} className={cn("whitespace-pre-wrap", levelClass(entry.level))}>
              <span className="select-none text-zinc-600">{entry.level.padEnd(5)} </span>
              {entry.message}
            </div>
          ))
        )}
        {error && entries.length > 0 ? <p className="mt-1 text-red-400">{error}</p> : null}
      </div>
    </section>
  );
}
