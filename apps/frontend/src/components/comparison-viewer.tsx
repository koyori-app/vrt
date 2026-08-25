import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation();

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
          {t("comparison.notAvailable")}
        </div>
      )}
    </figure>
  );
}

function SwipeView({
  baselineSrc,
  currentSrc,
}: {
  baselineSrc: string | null;
  currentSrc: string | null;
}) {
  const { t } = useTranslation();
  const [position, setPosition] = useState(50);
  const [dragging, setDragging] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  if (!baselineSrc || !currentSrc) {
    return (
      <div className="grid h-40 place-items-center rounded-md border border-dashed border-border text-xs text-muted-foreground">
        {t("comparison.notAvailable")}
      </div>
    );
  }

  const clamp = (value: number) => Math.min(100, Math.max(0, value));

  const positionFromEvent = (clientX: number) => {
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return position;
    return clamp(((clientX - rect.left) / rect.width) * 100);
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragging(true);
    setPosition(positionFromEvent(e.clientX));
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging) return;
    setPosition(positionFromEvent(e.clientX));
  };

  const stopDragging = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    setDragging(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 10 : 1;
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      setPosition((prev) => clamp(prev - step));
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setPosition((prev) => clamp(prev + step));
    }
  };

  return (
    <div
      ref={containerRef}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={stopDragging}
      onPointerCancel={stopDragging}
      className="vrt-checkerboard relative w-full cursor-col-resize select-none overflow-hidden rounded-md border border-border"
    >
      <img
        src={baselineSrc}
        alt={t("comparison.baseline")}
        draggable={false}
        className="w-full object-contain object-top"
      />
      <img
        src={currentSrc}
        alt={t("comparison.current")}
        draggable={false}
        style={{ clipPath: `inset(0 ${100 - position}% 0 0)` }}
        className="absolute inset-0 h-full w-full object-contain object-top"
      />

      <span className="pointer-events-none absolute left-2 top-2 rounded bg-background/80 px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
        {t("comparison.current")}
      </span>
      <span className="pointer-events-none absolute right-2 top-2 rounded bg-background/80 px-1.5 py-0.5 text-xs font-medium text-muted-foreground">
        {t("comparison.baseline")}
      </span>

      <div
        className="pointer-events-none absolute inset-y-0 w-0.5 -translate-x-1/2 bg-primary"
        style={{ left: `${position}%` }}
      >
        <div
          role="slider"
          aria-valuenow={Math.round(position)}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label={t("comparison.sliderLabel")}
          tabIndex={0}
          onKeyDown={handleKeyDown}
          className="pointer-events-auto absolute left-1/2 top-1/2 grid h-6 w-6 -translate-x-1/2 -translate-y-1/2 cursor-col-resize place-items-center rounded-full border border-border bg-background shadow"
        >
          <span className="text-xs leading-none text-muted-foreground">↔</span>
        </div>
      </div>
    </div>
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
  const { t } = useTranslation();
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
            {t("comparison.approve")}
          </Button>
          <Button
            size="sm"
            variant="destructive"
            disabled={reviewPending}
            onClick={() => onReview("reject")}
          >
            {t("comparison.reject")}
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
          <TabsTrigger value="side-by-side">{t("comparison.sideBySide")}</TabsTrigger>
          <TabsTrigger value="swipe">{t("comparison.swipe")}</TabsTrigger>
          <TabsTrigger value="diff">{t("comparison.diff")}</TabsTrigger>
          <TabsTrigger value="onion">{t("comparison.onion")}</TabsTrigger>
        </TabsList>

        <TabsContent value="side-by-side" className="pt-4">
          <div className="grid gap-4 lg:grid-cols-2">
            <Frame label={t("comparison.baseline")} src={baselineSrc} />
            <Frame label={t("comparison.current")} src={currentSrc} />
          </div>
        </TabsContent>

        <TabsContent value="swipe" className="pt-4">
          <SwipeView baselineSrc={baselineSrc} currentSrc={currentSrc} />
        </TabsContent>

        <TabsContent value="diff" className="pt-4">
          <Frame label={t("comparison.diff")} src={diffSrc} />
        </TabsContent>

        <TabsContent value="onion" className="space-y-4 pt-4">
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground">{t("comparison.baseline")}</span>
            <Slider
              className="max-w-sm"
              value={[opacity]}
              min={0}
              max={100}
              step={1}
              onValueChange={([value]) => setOpacity(value ?? 50)}
            />
            <span className="text-xs text-muted-foreground">
              {t("comparison.currentWithOpacity", { opacity })}
            </span>
          </div>
          {/* Current is stacked over baseline; the slider drives its opacity. */}
          <div className="relative w-full">
            {baselineSrc ? (
              <img
                src={baselineSrc}
                alt={t("comparison.baseline")}
                className="vrt-checkerboard w-full rounded-md border border-border"
              />
            ) : (
              <div className="grid h-40 place-items-center rounded-md border border-dashed border-border text-xs text-muted-foreground">
                {t("comparison.noBaseline")}
              </div>
            )}
            {currentSrc ? (
              <img
                src={currentSrc}
                alt={t("comparison.current")}
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
