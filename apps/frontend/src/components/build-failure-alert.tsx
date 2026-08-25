import type { Build } from "@/lib/api";
import { failurePresentation } from "@/lib/build-failure";
import { cn } from "@/lib/utils";

export function BuildFailureAlert({
  origin,
  code,
  message,
}: {
  origin: Build["failure_origin"];
  code: Build["failure_code"];
  message: string;
}) {
  const presentation = failurePresentation(origin);

  return (
    <div
      role="alert"
      className={cn(
        "space-y-1.5 rounded-md border px-3 py-2 text-sm",
        presentation.tone === "test" && "border-amber-500/40 bg-amber-500/10",
        presentation.tone === "vrt" && "border-destructive/40 bg-destructive/10",
        presentation.tone === "unknown" && "border-border bg-muted/50",
      )}
    >
      <div className="flex flex-wrap items-center gap-2">
        <p className="font-medium">{presentation.title}</p>
        {code ? (
          <code className="rounded bg-background/70 px-1.5 py-0.5 text-xs text-muted-foreground">
            {code}
          </code>
        ) : null}
      </div>
      <p className="text-xs text-muted-foreground">{presentation.guidance}</p>
      <p className="whitespace-pre-wrap break-words">{message}</p>
    </div>
  );
}
