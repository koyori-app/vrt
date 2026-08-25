/**
 * 対応言語と、その解決規則。
 *
 * UI 文言の実体は `resources/` にあり、ここは「どの言語で描くか」だけを決める。
 * 解決の優先順位は
 *
 * 1. ユーザー設定（`GET /v1/users/me` の `language`。DB に保存され端末をまたぐ）
 * 2. リクエスト/ブラウザの言語（SSR は `Accept-Language`、CSR は `navigator`）
 * 3. `DEFAULT_LANGUAGE`
 *
 * で、1 が `null`（未設定）のときだけ 2 へ落ちる。
 */

/** 画面を描ける言語。バックエンドの `Language` スキーマと同じ集合。 */
export const SUPPORTED_LANGUAGES = ["en", "ja"] as const;

export type Language = (typeof SUPPORTED_LANGUAGES)[number];

/** 判定材料が何も無いときの言語。 */
export const DEFAULT_LANGUAGE: Language = "en";

/** 言語切替 UI に出す表記。**その言語自身の綴り**で出す（探しやすさのため）。 */
export const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  ja: "日本語",
};

export function isLanguage(value: unknown): value is Language {
  return typeof value === "string" && (SUPPORTED_LANGUAGES as readonly string[]).includes(value);
}

/**
 * BCP 47 の言語タグを対応言語へ丸める（`ja-JP` → `ja`）。
 *
 * 対応外なら `undefined`。呼び出し側が次の候補へ進めるように、既定値へ
 * 勝手に倒さない。
 */
export function normalizeLanguage(tag: string | null | undefined): Language | undefined {
  if (!tag) return undefined;
  const primary = tag.trim().toLowerCase().split("-")[0] ?? "";
  return isLanguage(primary) ? primary : undefined;
}

/**
 * `Accept-Language` ヘッダから最も優先度の高い対応言語を選ぶ。
 *
 * `ja,en-US;q=0.9` のような並びを q 値の降順で見る。q の無い項目は 1 と
 * みなす（RFC 9110）。対応言語が一つも無ければ `undefined`。
 */
export function languageFromAcceptLanguage(
  header: string | null | undefined,
): Language | undefined {
  if (!header) return undefined;

  const candidates = header
    .split(",")
    .map((part) => {
      const [tag = "", ...params] = part.split(";").map((piece) => piece.trim());
      const q = params
        .map((param) => /^q=([0-9.]+)$/i.exec(param))
        .find((match): match is RegExpExecArray => match !== null);
      const quality = q ? Number.parseFloat(q[1] ?? "") : 1;
      return { tag, quality: Number.isFinite(quality) ? quality : 0 };
    })
    .filter((candidate) => candidate.tag.length > 0 && candidate.quality > 0)
    .sort((a, b) => b.quality - a.quality);

  for (const candidate of candidates) {
    const language = normalizeLanguage(candidate.tag);
    if (language) return language;
  }
  return undefined;
}

/** 上の優先順位をそのまま関数にしたもの。 */
export function resolveLanguage(
  userSetting: string | null | undefined,
  browserLanguage: string | null | undefined,
): Language {
  return normalizeLanguage(userSetting) ?? normalizeLanguage(browserLanguage) ?? DEFAULT_LANGUAGE;
}
