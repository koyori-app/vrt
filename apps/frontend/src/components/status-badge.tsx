import type * as React from "react";
import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import type { BuildStatus, ComparisonStatus, ReviewStatus } from "@/lib/api";
import {
  buildStatusLabelKey,
  buildStatusTone,
  comparisonStatusLabelKey,
  comparisonStatusTone,
  reviewStatusLabelKey,
  reviewStatusTone,
  toneClass,
  toneDotClass,
  type Tone,
} from "@/lib/status";
import { cn } from "@/lib/utils";

/**
 * Semantic colour layer on top of the shadcn `Badge`. shadcn only ships neutral
 * variants, so the VRT status palette lives here rather than being patched into
 * the generated component — that keeps `shadcn add badge` re-runnable.
 */
export function ToneBadge({
  tone = "gray",
  dot = false,
  className,
  children,
  ...props
}: React.ComponentProps<typeof Badge> & { tone?: Tone; dot?: boolean }) {
  return (
    <Badge
      variant="outline"
      className={cn("gap-1.5 border-transparent ring-1 ring-inset", toneClass[tone], className)}
      {...props}
    >
      {dot ? <span className={cn("size-1.5 rounded-full", toneDotClass[tone])} /> : null}
      {children}
    </Badge>
  );
}

export function BuildStatusBadge({ status }: { status: BuildStatus }) {
  const { t } = useTranslation();
  return (
    <ToneBadge tone={buildStatusTone[status]} dot>
      {t(buildStatusLabelKey[status])}
    </ToneBadge>
  );
}

export function ComparisonStatusBadge({ status }: { status: ComparisonStatus }) {
  const { t } = useTranslation();
  return (
    <ToneBadge tone={comparisonStatusTone[status]} dot>
      {t(comparisonStatusLabelKey[status])}
    </ToneBadge>
  );
}

export function ReviewStatusBadge({ status }: { status: ReviewStatus }) {
  const { t } = useTranslation();
  return <ToneBadge tone={reviewStatusTone[status]}>{t(reviewStatusLabelKey[status])}</ToneBadge>;
}
