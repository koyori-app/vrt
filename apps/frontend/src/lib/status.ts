import type { BuildStatus, ComparisonStatus, ReviewStatus } from "@/lib/api";

export type Tone = "green" | "amber" | "red" | "blue" | "gray";

export const toneClass: Record<Tone, string> = {
  green:
    "bg-emerald-100 text-emerald-800 ring-emerald-600/20 dark:bg-emerald-500/15 dark:text-emerald-300 dark:ring-emerald-400/30",
  amber:
    "bg-amber-100 text-amber-800 ring-amber-600/20 dark:bg-amber-500/15 dark:text-amber-300 dark:ring-amber-400/30",
  red: "bg-red-100 text-red-800 ring-red-600/20 dark:bg-red-500/15 dark:text-red-300 dark:ring-red-400/30",
  blue: "bg-blue-100 text-blue-800 ring-blue-600/20 dark:bg-blue-500/15 dark:text-blue-300 dark:ring-blue-400/30",
  gray: "bg-neutral-100 text-neutral-700 ring-neutral-500/20 dark:bg-neutral-500/15 dark:text-neutral-300 dark:ring-neutral-400/30",
};

export const toneDotClass: Record<Tone, string> = {
  green: "bg-emerald-500",
  amber: "bg-amber-500",
  red: "bg-red-500",
  blue: "bg-blue-500",
  gray: "bg-neutral-400",
};

export const buildStatusTone: Record<BuildStatus, Tone> = {
  pending: "gray",
  queued: "gray",
  rendering: "blue",
  processing: "blue",
  passed: "green",
  changes_detected: "amber",
  failed: "red",
  approved: "green",
  rejected: "red",
};

/**
 * 状態ラベルの翻訳キー。文言そのものではなくキーを持つのは、同じ状態が
 * 一覧・詳細・バッジの複数箇所に出るため——訳を 1 箇所に集める。
 */
export const buildStatusLabelKey = {
  pending: "buildStatus.pending",
  queued: "buildStatus.queued",
  // Storybook mode only: the server is capturing stories in headless Chromium.
  rendering: "buildStatus.rendering",
  processing: "buildStatus.processing",
  passed: "buildStatus.passed",
  changes_detected: "buildStatus.changes_detected",
  failed: "buildStatus.failed",
  approved: "buildStatus.approved",
  rejected: "buildStatus.rejected",
} as const satisfies Record<BuildStatus, string>;

export const comparisonStatusTone: Record<ComparisonStatus, Tone> = {
  pending: "gray",
  processing: "blue",
  unchanged: "green",
  changed: "amber",
  added: "blue",
  removed: "red",
  failed: "red",
};

export const comparisonStatusLabelKey = {
  pending: "comparisonStatus.pending",
  processing: "comparisonStatus.processing",
  unchanged: "comparisonStatus.unchanged",
  changed: "comparisonStatus.changed",
  added: "comparisonStatus.added",
  removed: "comparisonStatus.removed",
  failed: "comparisonStatus.failed",
} as const satisfies Record<ComparisonStatus, string>;

export const reviewStatusTone: Record<ReviewStatus, Tone> = {
  pending: "gray",
  approved: "green",
  rejected: "red",
};

export const reviewStatusLabelKey = {
  pending: "reviewStatus.pending",
  approved: "reviewStatus.approved",
  rejected: "reviewStatus.rejected",
} as const satisfies Record<ReviewStatus, string>;

/** Build states where a reviewer can still act. */
export function isReviewable(status: BuildStatus) {
  return status === "changes_detected" || status === "passed";
}
