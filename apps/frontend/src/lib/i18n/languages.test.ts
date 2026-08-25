import { describe, expect, it } from "vitest";

import {
  DEFAULT_LANGUAGE,
  languageFromAcceptLanguage,
  normalizeLanguage,
  resolveLanguage,
} from "./languages";

describe("normalizeLanguage", () => {
  it("keeps supported tags and drops the region", () => {
    expect(normalizeLanguage("ja")).toBe("ja");
    expect(normalizeLanguage("ja-JP")).toBe("ja");
    expect(normalizeLanguage("EN-us")).toBe("en");
  });

  it("returns undefined for unsupported or empty tags so the caller can fall through", () => {
    expect(normalizeLanguage("fr")).toBeUndefined();
    expect(normalizeLanguage("")).toBeUndefined();
    expect(normalizeLanguage(null)).toBeUndefined();
    expect(normalizeLanguage(undefined)).toBeUndefined();
  });
});

describe("languageFromAcceptLanguage", () => {
  it("picks the highest quality supported language, not the first one", () => {
    expect(languageFromAcceptLanguage("fr;q=1.0,ja;q=0.8,en;q=0.5")).toBe("ja");
  });

  it("treats a missing q as 1", () => {
    expect(languageFromAcceptLanguage("ja,en;q=0.9")).toBe("ja");
    expect(languageFromAcceptLanguage("en;q=0.9,ja")).toBe("ja");
  });

  it("skips q=0 entries, which mean “not acceptable”", () => {
    expect(languageFromAcceptLanguage("ja;q=0,en;q=0.1")).toBe("en");
  });

  it("returns undefined when nothing is supported", () => {
    expect(languageFromAcceptLanguage("fr-CA,de;q=0.7")).toBeUndefined();
    expect(languageFromAcceptLanguage("")).toBeUndefined();
    expect(languageFromAcceptLanguage(null)).toBeUndefined();
  });
});

describe("resolveLanguage", () => {
  it("prefers the stored user setting over the browser", () => {
    expect(resolveLanguage("en", "ja")).toBe("en");
  });

  it("falls back to the browser only when the setting is unset", () => {
    expect(resolveLanguage(null, "ja")).toBe("ja");
    expect(resolveLanguage(undefined, "ja")).toBe("ja");
  });

  it("ignores a stored value the app can no longer render", () => {
    expect(resolveLanguage("fr", "ja")).toBe("ja");
  });

  it("ends at the default when nothing is usable", () => {
    expect(resolveLanguage(null, null)).toBe(DEFAULT_LANGUAGE);
  });
});
