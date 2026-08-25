import { describe, expect, it } from "vitest";

import { en } from "./en";
import { ja } from "./ja";

/**
 * キーの過不足は `ja: Resources` の型で落ちるが、型は「値が空でない」ことまでは
 * 見ない。翻訳漏れ（英語のまま貼られた・空文字）をここで拾う。
 */
function flatten(value: unknown, prefix = ""): [string, string][] {
  if (typeof value === "string") return [[prefix, value]];
  if (value && typeof value === "object") {
    return Object.entries(value).flatMap(([key, child]) =>
      flatten(child, prefix ? `${prefix}.${key}` : key),
    );
  }
  return [];
}

const enEntries = flatten(en);
const jaEntries = flatten(ja);

describe("translation resources", () => {
  it("covers exactly the same keys in both languages", () => {
    expect(jaEntries.map(([key]) => key).sort()).toEqual(enEntries.map(([key]) => key).sort());
  });

  it("has no empty strings", () => {
    expect(jaEntries.filter(([, value]) => value.trim() === "")).toEqual([]);
    expect(enEntries.filter(([, value]) => value.trim() === "")).toEqual([]);
  });

  it("keeps every interpolation placeholder that the English text declares", () => {
    const placeholders = (value: string) => (value.match(/\{\{\s*\w+\s*\}\}/g) ?? []).sort();
    const jaByKey = new Map(jaEntries);

    for (const [key, value] of enEntries) {
      expect(placeholders(jaByKey.get(key) ?? ""), `placeholders differ for ${key}`).toEqual(
        placeholders(value),
      );
    }
  });
});
