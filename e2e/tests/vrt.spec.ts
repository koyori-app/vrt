/**
 * The VRT smoke tier: one linear pass through the product's reason to exist.
 *
 *   sign in → create tenant → create project → issue a PAT (all through the UI)
 *   → CI uploads screenshots with that PAT (API, exactly as a real CI job would)
 *   → the compare worker produces comparisons
 *   → the review UI shows them and approving the build promotes the baseline
 *
 * Breadth lives in the backend integration tests (apps/backend/tests). What is
 * only checkable here is that the browser, the SSR proxy, the session cookie,
 * the PAT and the background worker all line up in one running system.
 */
import { expect, test } from "@playwright/test";
import { Client } from "pg";

import { API_URL, DATABASE_URL } from "../env";
import { png, testLogin, unique, waitForTerminalBuild } from "../global-setup";

test("login page renders both OAuth providers", async ({ page }) => {
  await page.goto("/login");

  await expect(page.getByRole("heading", { name: "VRT" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Sign in with GitHub" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Sign in with GitLab" })).toBeVisible();
});

test("unauthenticated visitors are bounced to the login page", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
});

test("full VRT flow: tenant, project, PAT, CI upload, review, approve", async ({ page }) => {
  const username = unique("e2e");
  const tenantName = unique("E2E Co");
  const tenantSlug = tenantName.toLowerCase().replaceAll(" ", "-");
  const projectName = unique("Web");
  const projectSlug = projectName.toLowerCase().replaceAll(" ", "-");

  // ── sign in ───────────────────────────────────────────────────────────
  await testLogin(page, username);

  // A brand-new user owns nothing, so the dashboard is the empty state.
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Create your first tenant" })).toBeVisible();

  // ── create a tenant through the UI ────────────────────────────────────
  await page.getByRole("button", { name: "New tenant" }).click();
  await page.locator("#tenant-name").fill(tenantName);
  // The slug field mirrors the name until it is touched; assert the derived value.
  await expect(page.locator("#tenant-slug")).toHaveValue(tenantSlug);
  await page.getByRole("button", { name: "Create tenant" }).click();

  await expect(page).toHaveURL(new RegExp(`/t/${tenantSlug}$`));
  await expect(page.getByRole("heading", { name: tenantName })).toBeVisible();

  // ── create a project through the UI ───────────────────────────────────
  // "New project" only renders for admin+, so seeing it also proves `my_role`
  // came back on the tenant list.
  await page.getByRole("button", { name: "New project" }).click();
  await page.locator("#project-name").fill(projectName);
  await expect(page.locator("#project-slug")).toHaveValue(projectSlug);
  await page.getByRole("button", { name: "Create project" }).click();

  await expect(page.getByRole("link", { name: new RegExp(projectName) })).toBeVisible();

  // ── issue a PAT through the UI ────────────────────────────────────────
  await page.goto("/settings/tokens");
  await page.getByRole("button", { name: "New token" }).click();
  await page.locator("#token-name").fill("e2e-ci");
  // Default scopes are read:build + write:build — exactly what a CI job needs.
  await page.getByRole("button", { name: "Create token" }).click();

  const tokenDialog = page.getByRole("dialog");
  await expect(tokenDialog.getByRole("heading", { name: "Copy your token" })).toBeVisible();
  // The raw secret is shown exactly once, in this dialog.
  const token = (await tokenDialog.locator("code").first().innerText()).trim();
  expect(token).not.toEqual("");
  await page.getByRole("button", { name: "Done" }).click();

  // ── CI: create a build, upload two screenshots, finalize ──────────────
  // Straight at the backend with a Bearer token: that is the real CI path, and
  // it does not go through the browser's session at all.
  const auth = { Authorization: `Bearer ${token}` };
  const request = page.request;

  const created = await request.post(
    `${API_URL}/v1/ci/projects/${tenantSlug}/${projectSlug}/builds`,
    {
      headers: auth,
      data: { branch: "main", commit_sha: "0123456789abcdef0123456789abcdef01234567" },
    },
  );
  expect(created.status(), await created.text()).toBe(201);
  const build = await created.json();
  expect(build.number).toBe(1);
  expect(build.status).toBe("pending");

  for (const [name, colour] of [
    ["home", [220, 40, 40, 255]],
    ["settings", [40, 80, 220, 255]],
  ] as const) {
    const uploaded = await request.post(`${API_URL}/v1/ci/builds/${build.id}/screenshots`, {
      headers: auth,
      multipart: {
        // `name` must precede `file`; the handler reads the fields in order.
        name,
        file: {
          name: `${name}.png`,
          mimeType: "image/png",
          buffer: png(64, 48, [...colour]),
        },
      },
    });
    expect(uploaded.status(), await uploaded.text()).toBe(201);
  }

  const finalized = await request.post(`${API_URL}/v1/ci/builds/${build.id}/finalize`, {
    headers: auth,
  });
  expect(finalized.status(), await finalized.text()).toBe(200);

  // No baseline exists yet, so both screenshots come back as `added` and the
  // build lands in changes_detected (a human has to look at it).
  const compared = await waitForTerminalBuild(request, build.id, token);
  expect(compared.status).toBe("changes_detected");
  expect(compared.added_count).toBe(2);
  expect(compared.total_count).toBe(2);

  // ── review UI ─────────────────────────────────────────────────────────
  await page.goto(`/t/${tenantSlug}/p/${projectSlug}/builds/1`);
  await expect(page.getByRole("heading", { name: "Build #1" })).toBeVisible();
  await expect(page.getByText("Changes detected")).toBeVisible();

  const comparisons = page.locator("aside ul li button");
  await expect(comparisons).toHaveCount(2);
  await expect(comparisons.filter({ hasText: "home" })).toBeVisible();
  await expect(comparisons.filter({ hasText: "settings" })).toBeVisible();

  // ── approve ───────────────────────────────────────────────────────────
  // Both comparisons are still unreviewed, so the page asks for confirmation
  // before force-approving the whole build.
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Approve build" }).click();

  // "Approved" also appears on individual comparisons, so assert on the state
  // that can only be true for the build: it is no longer awaiting review.
  await expect(page.getByText("Changes detected")).toHaveCount(0);
  await expect(page.getByText("Approved", { exact: true }).first()).toBeVisible();

  // The API agrees, and the screenshots were promoted to the baseline.
  const after = await request.get(`${API_URL}/v1/ci/builds/${build.id}`, { headers: auth });
  expect(after.status()).toBe(200);
  expect((await after.json()).status).toBe("approved");

  // ── failed comparison bulk approval ──────────────────────────────────
  // Produce another reviewed build, then model the compare worker's failure
  // state directly. The browser assertion below is intentionally about the UI
  // contract: acknowledge the baseline consequence, then send the explicit
  // acceptance bit that the approval API requires.
  const failedBuildResponse = await request.post(
    `${API_URL}/v1/ci/projects/${tenantSlug}/${projectSlug}/builds`,
    {
      headers: auth,
      data: { branch: "main", commit_sha: "1123456789abcdef0123456789abcdef01234567" },
    },
  );
  expect(failedBuildResponse.status(), await failedBuildResponse.text()).toBe(201);
  const failedBuild = await failedBuildResponse.json();

  for (const [name, colour] of [
    ["home", [20, 200, 80, 255]],
    ["settings", [40, 80, 220, 255]],
  ] as const) {
    const uploaded = await request.post(
      `${API_URL}/v1/ci/builds/${failedBuild.id}/screenshots`,
      {
        headers: auth,
        multipart: {
          name,
          file: {
            name: `${name}.png`,
            mimeType: "image/png",
            buffer: png(64, 48, [...colour]),
          },
        },
      },
    );
    expect(uploaded.status(), await uploaded.text()).toBe(201);
  }

  const failedFinalized = await request.post(
    `${API_URL}/v1/ci/builds/${failedBuild.id}/finalize`,
    { headers: auth },
  );
  expect(failedFinalized.status(), await failedFinalized.text()).toBe(200);
  expect((await waitForTerminalBuild(request, failedBuild.id, token)).status).toBe(
    "changes_detected",
  );

  const database = new Client({ connectionString: DATABASE_URL });
  await database.connect();
  try {
    const marked = await database.query(
      "UPDATE comparisons SET status = 'failed', error_message = 'forced e2e comparison failure' WHERE build_id = $1 AND name = 'home'",
      [failedBuild.id],
    );
    expect(marked.rowCount).toBe(1);
  } finally {
    await database.end();
  }

  await page.goto(`/t/${tenantSlug}/p/${projectSlug}/builds/2`);
  await expect(page.getByText("Failed", { exact: true })).toBeVisible();

  const dialogs: string[] = [];
  page.on("dialog", async (dialog) => {
    dialogs.push(dialog.message());
    await dialog.accept();
  });
  const approvalRequestPromise = page.waitForRequest(
    (candidate) =>
      candidate.method() === "POST" && candidate.url().endsWith(`/builds/${failedBuild.id}/approve`),
  );
  await page.getByRole("button", { name: "Approve build" }).click();
  const approvalRequest = await approvalRequestPromise;

  await expect.poll(() => dialogs.length).toBe(2);
  expect(dialogs[1]).toContain("screenshots from these failed comparisons will become the baseline");
  expect(dialogs[1]).toContain("review each failed comparison individually first");
  expect(approvalRequest.postDataJSON()).toMatchObject({ force: true, accept_failures: true });
  await expect(page.getByText("Approved", { exact: true }).first()).toBeVisible();
});
