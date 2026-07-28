/**
 * Single source of truth for the ports and connection strings the e2e stack uses.
 *
 * Everything is deliberately off the numbers the two docker composes occupy
 * (task: 5432/6379/3400/3000, vrt: 5433/6380/3500/3000) so a running dev stack
 * never collides with an e2e run. Postgres and Valkey are the only pieces the
 * e2e run does NOT boot itself — start them with:
 *
 *     docker compose up -d db redis
 */

/** Backend under test (spawned by playwright's webServer). */
export const BACKEND_PORT = Number(process.env.E2E_BACKEND_PORT ?? 3401);
/** Frontend under test (spawned by playwright's webServer). */
export const FRONTEND_PORT = Number(process.env.E2E_FRONTEND_PORT ?? 3001);

export const API_URL = process.env.E2E_API_URL ?? `http://localhost:${BACKEND_PORT}`;
export const BASE_URL = process.env.BASE_URL ?? `http://localhost:${FRONTEND_PORT}`;

/**
 * A database of its own — `migration fresh` drops every table, so pointing this
 * at the dev database (`vrt`) would wipe local data on every run.
 * scripts/ensure-database.mjs creates it if it does not exist yet.
 */
export const DATABASE_URL =
  process.env.E2E_DATABASE_URL ?? "postgresql://vrt:vrt@localhost:5433/vrt_e2e";

export const REDIS_URL = process.env.E2E_REDIS_URL ?? "redis://localhost:6380";
