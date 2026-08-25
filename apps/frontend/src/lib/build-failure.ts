export type FailureOrigin = "test" | "vrt" | null | undefined;

export type FailurePresentation = {
  /** 見出しの翻訳キー。 */
  titleKey: "buildFailure.test.title" | "buildFailure.vrt.title" | "buildFailure.unknown.title";
  /** 対処の案内の翻訳キー。 */
  guidanceKey:
    "buildFailure.test.guidance" | "buildFailure.vrt.guidance" | "buildFailure.unknown.guidance";
  tone: "test" | "vrt" | "unknown";
};

/** Human-facing ownership of a failed build. Null keeps old builds readable. */
export function failurePresentation(origin: FailureOrigin): FailurePresentation {
  switch (origin) {
    case "test":
      return {
        titleKey: "buildFailure.test.title",
        guidanceKey: "buildFailure.test.guidance",
        tone: "test",
      };
    case "vrt":
      return {
        titleKey: "buildFailure.vrt.title",
        guidanceKey: "buildFailure.vrt.guidance",
        tone: "vrt",
      };
    default:
      return {
        titleKey: "buildFailure.unknown.title",
        guidanceKey: "buildFailure.unknown.guidance",
        tone: "unknown",
      };
  }
}
