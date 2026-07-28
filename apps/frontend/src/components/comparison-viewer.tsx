import { useState } from "react";

import { ComparisonStatusBadge, ReviewStatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { contentUrl, type Comparison } from "@/lib/api";
import { cn } from "@/lib/utils";

function Frame({
  label,
  src,
  className,
}: {
  label: string;
  src: string | null;
  className?: string;
}) {
  return (
    <figure className={cn("min-w-0 space-y-2", className)}>
      <figcaption className="text-xs font-medium text-muted-foreground">{label}</figcaption>
      {src ? (
        <img
          src={src}
          alt={label}
          className="vrt-checkerboard w-full rounded-md border border-border"
        />
      ) : (
        <div className="grid h-40 place-items-center rounded-md border border-dashed border-border text-xs text-muted-foreground">
          Not available
        </div>
      )}
    </figure>
  );
}

export function ComparisonViewer({
  comparison,
  onReview,
  reviewPending,
}: {
  comparison: Comparison;
  onReview: (action: "approve" | "reject") => void;
  reviewPending: boolean;
}) {
  const [opacity, setOpacity] = useState(50);

  const baselineSrc = comparison.baseline_entry_id
    ? contentUrl.baselineEntry(comparison.baseline_entry_id)
    : null;
  const currentSrc = comparison.screenshot_id
    ? contentUrl.screenshot(comparison.screenshot_id)
    : null;
  const diffSrc = comparison.has_diff_image ? contentUrl.comparisonDiff(comparison.id) : null;

  return (
    <div className="flex min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-3 border-b border-border p-4">
        <h2 className="min-w-0 flex-1 truncate text-sm font-medium">{comparison.name}</h2>
        <ComparisonStatusBadge status={comparison.status} />
        <ReviewStatusBadge status={comparison.review_status} />
        {comparison.diff_ratio != null ? (
          <span className="text-xs text-muted-foreground">
            {(comparison.diff_ratio * 100).toFixed(2)}% ({comparison.diff_pixel_count ?? 0} px)
          </span>
        ) : null}
        <div className="flex gap-2">
          <Button
            size="sm"
            variant="success"
            disabled={reviewPending}
            onClick={() => onReview("approve")}
          >
            Approve
          </Button>
          <Button
            size="sm"
            variant="destructive"
            disabled={reviewPending}
            onClick={() => onReview("reject")}
          >
            Reject
          </Button>
        </div>
      </div>

      {comparison.error_message ? (
        <p className="border-b border-border bg-destructive/10 px-4 py-2 text-xs text-destructive">
          {comparison.error_message}
        </p>
      ) : null}

      <Tabs defaultValue="side-by-side" className="min-h-0 flex-1 overflow-y-auto p-4">
        <TabsList>
          <TabsTrigger value="side-by-side">Side-by-side</TabsTrigger>
          <TabsTrigger value="diff">Diff</TabsTrigger>
          <TabsTrigger value="onion">Onion-skin</TabsTrigger>
        </TabsList>

        <TabsContent value="side-by-side" className="pt-4">
          <div className="grid gap-4 lg:grid-cols-2">
            <Frame label="Baseline" src={baselineSrc} />
            <Frame label="Current" src={currentSrc} />
          </div>
        </TabsContent>

        <TabsContent value="diff" className="pt-4">
          <Frame label="Diff" src={diffSrc} />
        </TabsContent>

        <TabsContent value="onion" className="space-y-4 pt-4">
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground">Baseline</span>
            <Slider
              className="max-w-sm"
              value={[opacity]}
              min={0}
              max={100}
              step={1}
              onValueChange={([value]) => setOpacity(value ?? 50)}
            />
            <span className="text-xs text-muted-foreground">Current ({opacity}%)</span>
          </div>
          {/* Current is stacked over baseline; the slider drives its opacity. */}
          <div className="relative w-full">
            {baselineSrc ? (
              <img
                src={baselineSrc}
                alt="Baseline"
                className="vrt-checkerboard w-full rounded-md border border-border"
              />
            ) : (
              <div className="grid h-40 place-items-center rounded-md border border-dashed border-border text-xs text-muted-foreground">
                No baseline
              </div>
            )}
            {currentSrc ? (
              <img
                src={currentSrc}
                alt="Current"
                style={{ opacity: opacity / 100 }}
                className="absolute inset-0 h-full w-full rounded-md object-contain object-top"
              />
            ) : null}
          </div>
        </TabsContent>
      </Tabs>
    </div>
  );
}
