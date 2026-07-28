import { defineConfig, devices } from "@playwright/test";

import { API_URL, BACKEND_PORT, BASE_URL, DATABASE_URL, FRONTEND_PORT, REDIS_URL } from "./env";

const isCI = !!process.env.CI;

export default defineConfig({
  testDir: "./tests",
  // The suite is one linear VRT flow; running it twice in parallel against the
  // same database buys nothing and makes failures harder to read.
  workers: 1,
  fullyParallel: false,
  // The compare job is a background worker — a stuck build should fail the
  // assertion's own timeout, not hang the run.
  timeout: 120_000,
  expect: { timeout: 15_000 },
  retries: isCI ? 1 : 0,
  use: {
    baseURL: BASE_URL,
    trace: isCI ? "retain-on-failure" : "on-first-retry",
  },
  reporter: isCI ? [["github"], ["html", { open: "never" }]] : "list",
  // One browser. This tier is a smoke test of the product flow, not a
  // cross-browser compatibility matrix.
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      command: "bash scripts/start-backend.sh",
      cwd: ".",
      url: `${API_URL}/v1/health`,
      reuseExistingServer: !isCI,
      // First run compiles the whole workspace from scratch.
      timeout: 600_000,
      stdout: "pipe",
      stderr: "pipe",
      env: {
        DATABASE_URL,
        REDIS_URL,
        LISTEN_ADDR: `127.0.0.1:${BACKEND_PORT}`,
        APP_URL: BASE_URL,
        ALLOW_ORIGIN: BASE_URL,
        STORAGE_BACKEND: "local",
        LOCAL_UPLOAD_DIR: "./.uploads",
        // Opens POST /v1/auth/test-login. There is no password login in this
        // product (OAuth only), so this is how the specs get a session.
        // Debug builds only — a release build refuses to start with it set.
        TEST_LOGIN_ENABLED: "true",
        // Required-but-unused settings. OAuth is never exercised by the specs.
        PERSONAL_TOKEN_SECRET: "00000000000000000000000000000000",
        OAUTH_TOKEN_ENCRYPTION_KEY: "01234567890123456789012345678901",
        GITHUB_CLIENT_ID: "e2e-dummy-github-client-id",
        GITHUB_CLIENT_SECRET: "e2e-dummy-github-client-secret",
        GITLAB_CLIENT_ID: "e2e-dummy-gitlab-client-id",
        GITLAB_CLIENT_SECRET: "e2e-dummy-gitlab-client-secret",
        RUST_LOG: process.env.RUST_LOG ?? "warn",
      },
    },
    {
      // Production entry (server.mjs + srvx), not `vite dev`: the SSR `/api/*`
      // proxy in src/routes/api.$.ts only runs in this mode, and that proxy is
      // what carries the session cookie in the deployed setup.
      command: "pnpm run openapi:generate && pnpm run build && node ./server.mjs",
      cwd: "../apps/frontend",
      url: BASE_URL,
      reuseExistingServer: !isCI,
      timeout: 180_000,
      stdout: "pipe",
      stderr: "pipe",
      env: {
        API_BASE: API_URL,
        PORT: String(FRONTEND_PORT),
        HOST: "127.0.0.1",
      },
    },
  ],
});
