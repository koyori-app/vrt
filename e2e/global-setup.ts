/**
 * Shared helpers for the e2e specs.
 *
 * The schema is applied by scripts/start-backend.sh (migration crate) before the
 * webServers come up, so nothing here touches the database directly.
 */
import { expect, type APIRequestContext, type Page } from "@playwright/test";
import { PNG } from "pngjs";

import { API_URL } from "./env";

/** Short random suffix so slugs stay unique even when a server is reused. */
export function unique(prefix: string) {
  return `${prefix}-${Math.random().toString(36).slice(2, 8)}`;
}

/** Solid-colour PNG. Two different colours are enough to produce a diff. */
export function png(width: number, height: number, rgba: [number, number, number, number]): Buffer {
  const image = new PNG({ width, height });
  for (let i = 0; i < image.data.length; i += 4) {
    image.data[i] = rgba[0];
    image.data[i + 1] = rgba[1];
    image.data[i + 2] = rgba[2];
    image.data[i + 3] = rgba[3];
  }
  return PNG.sync.write(image);
}

/**
 * Seed a session without going through OAuth.
 *
 * This product has no password login, so the backend exposes
 * `POST /v1/auth/test-login` behind `TEST_LOGIN_ENABLED` (debug builds only).
 * It is called through the frontend's `/api/*` SSR proxy rather than straight at
 * the backend, so the run also covers the proxy's `Set-Cookie` forwarding —
 * the exact path a real login cookie takes in the deployed setup.
 */
export async function testLogin(page: Page, username: string) {
  const response = await page.request.post("/api/v1/auth/test-login", {
    data: { username },
  });
  expect(response.status(), await response.text()).toBe(204);
}

/** Poll the CI status endpoint until the compare worker leaves a running state. */
export async function waitForTerminalBuild(
  request: APIRequestContext,
  buildId: string,
  token: string,
  timeoutMs = 90_000,
) {
  const deadline = Date.now() + timeoutMs;
  let last: Record<string, unknown> = {};

  while (Date.now() < deadline) {
    const response = await request.get(`${API_URL}/v1/ci/builds/${buildId}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(response.status(), await response.text()).toBe(200);
    last = await response.json();
    if (last.status !== "pending" && last.status !== "queued" && last.status !== "processing") {
      return last;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }

  throw new Error(`build ${buildId} never reached a terminal status: ${JSON.stringify(last)}`);
}
