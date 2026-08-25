export type FailureOrigin = "test" | "vrt" | null | undefined;

export type FailurePresentation = {
  title: string;
  guidance: string;
  tone: "test" | "vrt" | "unknown";
};

/** Human-facing ownership of a failed build. Null keeps old builds readable. */
export function failurePresentation(origin: FailureOrigin): FailurePresentation {
  switch (origin) {
    case "test":
      return {
        title: "Story / test error",
        guidance: "The Story, play test, or uploaded Storybook bundle needs to be fixed.",
        tone: "test",
      };
    case "vrt":
      return {
        title: "VRT execution environment error",
        guidance:
          "The renderer or VRT infrastructure failed. Retry the build; if it happens again, check the build log.",
        tone: "vrt",
      };
    default:
      return {
        title: "Unclassified build error",
        guidance: "This older build does not contain enough information to identify the owner.",
        tone: "unknown",
      };
  }
}
