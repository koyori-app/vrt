import { createIsomorphicFn } from "@tanstack/react-start";
import { getRequestHeader } from "@tanstack/react-start/server";
import createFetchClient from "openapi-fetch";
import createQueryClient from "openapi-react-query";

import type { components, paths } from "@/generated/api";

/**
 * Base URL resolution.
 *
 * - Browser: always `/api`, so requests are same-origin and the session cookie
 *   (plus a matching `Origin` header for the backend CSRF check) flows normally.
 *   In dev the Vite proxy strips `/api`; in prod the catch-all server route in
 *   `src/routes/api.$.ts` does it.
 * - SSR: talk to the backend directly (no self-request), using `API_BASE`.
 *
 * `createIsomorphicFn` keeps the server branch (and its server-only imports)
 * out of the client bundle.
 */
const resolveBaseUrl = createIsomorphicFn()
  .server(() => process.env.API_BASE ?? "http://localhost:3500")
  .client(() => "/api");

/**
 * During SSR the browser cookie is on the incoming request, not on `document`,
 * so it has to be forwarded explicitly to the backend.
 */
const resolveForwardedHeaders = createIsomorphicFn()
  .server((): Record<string, string> => {
    const cookie = getRequestHeader("cookie");
    return cookie ? { cookie } : {};
  })
  .client((): Record<string, string> => ({}));

export const API_PREFIX = "/api";

export const client = createFetchClient<paths>({
  baseUrl: resolveBaseUrl(),
  credentials: "include",
});

// Forward the SSR cookie on every request. No-op in the browser.
client.use({
  onRequest({ request }) {
    for (const [name, value] of Object.entries(resolveForwardedHeaders())) {
      request.headers.set(name, value);
    }
    return request;
  },
});

export const $api = createQueryClient(client);

/** Browser-reachable URL for an image content endpoint (goes through the proxy). */
export const contentUrl = {
  screenshot: (id: string) => `${API_PREFIX}/v1/screenshots/${id}/content`,
  baselineEntry: (id: string) => `${API_PREFIX}/v1/baseline-entries/${id}/content`,
  comparisonDiff: (id: string) => `${API_PREFIX}/v1/comparisons/${id}/diff-content`,
};

export type Schemas = components["schemas"];
export type Me = Schemas["MeResponse"];
export type Tenant = Schemas["TenantResponse"];
export type TenantMember = Schemas["TenantMemberResponse"];
export type TenantRole = Schemas["TenantRole"];
export type Project = Schemas["ProjectResponse"];
export type Build = Schemas["BuildResponse"];
export type BuildStatus = Schemas["BuildStatus"];
export type BuildLogEntry = Schemas["BuildLogEntry"];
export type Comparison = Schemas["ComparisonResponse"];
export type ComparisonStatus = Schemas["ComparisonStatus"];
export type ReviewStatus = Schemas["ReviewStatus"];
export type PersonalToken = Schemas["PersonalTokenResponse"];
export type Scope = Schemas["Scope"];
export type GithubInstallation = Schemas["GithubInstallationResponse"];

/** Pull a human-readable message out of an `AppError` JSON body. */
export function errorMessage(error: unknown, fallback = "Request failed"): string {
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.length > 0) return message;
  }
  if (error instanceof Error) return error.message;
  return fallback;
}
