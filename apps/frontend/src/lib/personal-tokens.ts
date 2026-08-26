import type { PersonalToken } from "@/lib/api";

/**
 * 期限切れかどうか。
 *
 * backend の PAT 認証は `revoked` と「`expires_at` が過去」の両方を弾く
 * （`service/src/auth.rs`）。画面もその判定に合わせる——期限切れを「使える
 * トークン」の顔で並べると、認証が通らない理由が画面から読み取れない。
 */
export function isExpired(token: PersonalToken, now: Date = new Date()): boolean {
  if (!token.expires_at) return false;
  const expires = new Date(token.expires_at);
  if (Number.isNaN(expires.getTime())) return false;
  return expires.getTime() <= now.getTime();
}

/**
 * 失効済みと、まだ失効していないトークンに分ける。
 *
 * 並び順は API が返した順のまま（作成順）を保つ——分けた結果で行が入れ替わると、
 * 直前まで見ていた並びとの対応が取れなくなる。
 */
export function partitionTokens(tokens: PersonalToken[] | undefined): {
  active: PersonalToken[];
  revoked: PersonalToken[];
} {
  const active: PersonalToken[] = [];
  const revoked: PersonalToken[] = [];
  for (const token of tokens ?? []) {
    (token.revoked ? revoked : active).push(token);
  }
  return { active, revoked };
}
