import { describe, expect, it } from "vitest";

import { createI18n } from "./index";

describe("createI18n", () => {
  it("renders the requested language", () => {
    expect(createI18n("ja").t("nav.signOut")).toBe("ログアウト");
    expect(createI18n("en").t("nav.signOut")).toBe("Sign out");
  });

  it("interpolates values", () => {
    expect(createI18n("ja").t("build.title", { number: 12 })).toBe("ビルド #12");
  });

  it("keeps instances independent so one request cannot change another's language", () => {
    const japanese = createI18n("ja");
    const english = createI18n("en");
    expect(japanese.t("common.cancel")).toBe("キャンセル");
    expect(english.t("common.cancel")).toBe("Cancel");
  });
});
