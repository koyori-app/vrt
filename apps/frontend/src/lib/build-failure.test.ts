import { describe, expect, it } from "vitest";

import { failurePresentation } from "./build-failure";

describe("failurePresentation", () => {
  it("directs Story and test failures to the project", () => {
    expect(failurePresentation("test")).toMatchObject({
      title: "Story / test error",
      tone: "test",
    });
  });

  it("directs renderer and infrastructure failures to VRT", () => {
    expect(failurePresentation("vrt")).toMatchObject({
      title: "VRT execution environment error",
      tone: "vrt",
    });
  });

  it("keeps legacy failures explicit instead of guessing", () => {
    expect(failurePresentation(null)).toMatchObject({
      title: "Unclassified build error",
      tone: "unknown",
    });
  });
});
