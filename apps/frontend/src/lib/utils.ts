import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function shortSha(sha: string) {
  return sha.slice(0, 7);
}

/** Mirrors the slug shape the backend validates: lowercase, dash-separated. */
export function slugify(value: string) {
  return value
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

/**
 * 日時の表示。`locale` には表示中の言語（`i18n.language`）を渡す。
 *
 * 省略するとランタイムの既定ロケールになり、UI が日本語でも日時だけ
 * `8/25/2026, 4:43:52 PM` のまま残る。加えて既定ロケールは SSR（サーバーの
 * ロケール）とブラウザで食い違いうるので、明示的に渡すほうが描画も安定する。
 */
export function formatDate(value: string | null | undefined, locale?: string) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString(locale);
}
