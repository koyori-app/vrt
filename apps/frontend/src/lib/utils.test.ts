import { describe, expect, it } from "vitest";

import { formatDate } from "./utils";

describe("formatDate", () => {
  const iso = "2026-08-25T07:43:52Z";

  it("formats in the language the UI is showing, not the runtime default", () => {
    const ja = formatDate(iso, "ja");
    const en = formatDate(iso, "en");
    expect(ja).not.toBe(en);
    // ja は年/月/日、en(-US) は月/日/年。並びで見分ける。
    expect(ja).toMatch(/^2026\//);
    expect(en).toMatch(/^8\/25\/2026/);
  });

  it("keeps the placeholder for missing or unparsable values", () => {
    expect(formatDate(null, "ja")).toBe("-");
    expect(formatDate(undefined, "ja")).toBe("-");
    expect(formatDate("not a date", "ja")).toBe("-");
  });
});
