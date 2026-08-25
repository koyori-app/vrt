import { describe, expect, it } from "vitest";

import { failurePresentation } from "./build-failure";

describe("failurePresentation", () => {
  it("directs Story and test failures to the project", () => {
    expect(failurePresentation("test")).toMatchObject({
      titleKey: "buildFailure.test.title",
      tone: "test",
    });
  });

  it("directs renderer and infrastructure failures to VRT", () => {
    expect(failurePresentation("vrt")).toMatchObject({
      titleKey: "buildFailure.vrt.title",
      tone: "vrt",
    });
  });

  it("keeps legacy failures explicit instead of guessing", () => {
    expect(failurePresentation(null)).toMatchObject({
      titleKey: "buildFailure.unknown.title",
      tone: "unknown",
    });
  });
});
