import { describe, expect, it } from "vitest";

import type { PersonalToken } from "@/lib/api";
import { isExpired, partitionTokens } from "./personal-tokens";

function token(overrides: Partial<PersonalToken> = {}): PersonalToken {
  return {
    id: "00000000-0000-0000-0000-000000000001",
    name: "ci",
    token_last_four: "abcd",
    scopes: ["read:build"],
    expires_at: null,
    last_used_at: null,
    revoked: false,
    user_id: "00000000-0000-0000-0000-000000000002",
    created_at: "2026-08-01T00:00:00Z",
    ...overrides,
  };
}

describe("partitionTokens", () => {
  it("splits revoked tokens out of the usable ones", () => {
    const a = token({ id: "a" });
    const b = token({ id: "b", revoked: true });
    const c = token({ id: "c" });

    const { active, revoked } = partitionTokens([a, b, c]);
    expect(active.map((t) => t.id)).toEqual(["a", "c"]);
    expect(revoked.map((t) => t.id)).toEqual(["b"]);
  });

  it("keeps the order the API returned inside each group", () => {
    const first = token({ id: "1", revoked: true });
    const second = token({ id: "2", revoked: true });
    expect(partitionTokens([first, second]).revoked.map((t) => t.id)).toEqual(["1", "2"]);
  });

  it("treats a missing list as empty so the first render has nothing to show", () => {
    expect(partitionTokens(undefined)).toEqual({ active: [], revoked: [] });
    expect(partitionTokens([])).toEqual({ active: [], revoked: [] });
  });

  it("keeps an expired-but-not-revoked token in the usable group", () => {
    // 失効させる操作はまだ意味がある（期限切れの取り消しは別の状態）。
    const expired = token({ expires_at: "2020-01-01T00:00:00Z" });
    expect(partitionTokens([expired]).active).toHaveLength(1);
  });
});

describe("isExpired", () => {
  const now = new Date("2026-08-27T00:00:00Z");

  it("is false without an expiry", () => {
    expect(isExpired(token({ expires_at: null }), now)).toBe(false);
  });

  it("is true once the expiry has passed", () => {
    expect(isExpired(token({ expires_at: "2026-08-26T23:59:59Z" }), now)).toBe(true);
  });

  it("is not expired exactly at the expiry — the backend still accepts it there", () => {
    // backend は `expires < now`（`service/src/auth.rs`）。同時刻はまだ通る。
    expect(isExpired(token({ expires_at: "2026-08-27T00:00:00Z" }), now)).toBe(false);
  });

  it("is expired one millisecond past the expiry", () => {
    expect(isExpired(token({ expires_at: "2026-08-26T23:59:59.999Z" }), now)).toBe(true);
  });

  it("is false while the expiry is still ahead", () => {
    expect(isExpired(token({ expires_at: "2026-08-27T00:00:01Z" }), now)).toBe(false);
  });

  it("does not call an unparsable timestamp expired", () => {
    // 読めない値で「期限切れ」を名乗ると、生きているトークンを死んだ顔で見せる。
    expect(isExpired(token({ expires_at: "not a date" }), now)).toBe(false);
  });
});
