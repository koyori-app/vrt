import { CheckIcon, XIcon } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { cn } from "@/lib/utils";
import type { Comparison } from "@/lib/api";
import { comparisonStatusLabelKey, comparisonStatusTone, toneDotClass } from "@/lib/status";

export type ComparisonFilter = "all" | "changed" | "added" | "removed" | "unchanged";

const FILTERS: ComparisonFilter[] = ["all", "changed", "added", "removed", "unchanged"];

/** 絞り込みタブの文言。`capitalize` の見た目に頼らず訳語を持つ。 */
const FILTER_LABEL_KEY = {
  all: "comparisonFilter.all",
  changed: "comparisonFilter.changed",
  added: "comparisonFilter.added",
  removed: "comparisonFilter.removed",
  unchanged: "comparisonFilter.unchanged",
} as const satisfies Record<ComparisonFilter, string>;

export function filterComparisons(comparisons: Comparison[], filter: ComparisonFilter) {
  if (filter === "all") return comparisons;
  return comparisons.filter((comparison) => comparison.status === filter);
}

export function ComparisonList({
  comparisons,
  selectedId,
  onSelect,
  filter,
  onFilterChange,
}: {
  comparisons: Comparison[];
  selectedId: string | undefined;
  onSelect: (comparison: Comparison) => void;
  filter: ComparisonFilter;
  onFilterChange: (filter: ComparisonFilter) => void;
}) {
  const { t } = useTranslation();
  const counts = useMemo(() => {
    const result: Record<ComparisonFilter, number> = {
      all: comparisons.length,
      changed: 0,
      added: 0,
      removed: 0,
      unchanged: 0,
    };
    for (const comparison of comparisons) {
      if (comparison.status in result) {
        result[comparison.status as ComparisonFilter] += 1;
      }
    }
    return result;
  }, [comparisons]);

  const visible = filterComparisons(comparisons, filter);

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap gap-1 border-b border-border p-2">
        {FILTERS.map((value) => (
          <button
            key={value}
            type="button"
            onClick={() => onFilterChange(value)}
            className={cn(
              "rounded-md px-2 py-1 text-xs transition-colors",
              value === filter
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:bg-accent",
            )}
          >
            {t(FILTER_LABEL_KEY[value])} {counts[value]}
          </button>
        ))}
      </div>

      <ul className="flex-1 overflow-y-auto">
        {visible.map((comparison) => (
          <li key={comparison.id}>
            <button
              type="button"
              onClick={() => onSelect(comparison)}
              className={cn(
                "flex w-full items-center gap-2 px-3 py-2 text-left text-sm transition-colors",
                comparison.id === selectedId ? "bg-accent" : "hover:bg-accent/50",
              )}
            >
              <span
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  toneDotClass[comparisonStatusTone[comparison.status]],
                )}
                title={t(comparisonStatusLabelKey[comparison.status])}
              />
              <span className="min-w-0 flex-1 truncate">{comparison.name}</span>
              {comparison.review_status === "approved" ? (
                <CheckIcon className="size-3.5 shrink-0 text-emerald-500" />
              ) : comparison.review_status === "rejected" ? (
                <XIcon className="size-3.5 shrink-0 text-red-500" />
              ) : null}
            </button>
          </li>
        ))}
        {!visible.length ? (
          <li className="px-3 py-6 text-center text-xs text-muted-foreground">
            {t("comparisonFilter.empty")}
          </li>
        ) : null}
      </ul>
    </div>
  );
}

/** Local filter state helper so the route stays focused on data + shortcuts. */
export function useComparisonFilter() {
  return useState<ComparisonFilter>("all");
}
