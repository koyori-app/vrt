# e2e

Playwright smoke tier for the whole stack. Its own package with its own
lockfile — it is not part of the pnpm workspace, so `pnpm install` at the repo
root never drags a browser download along.

## Running

Postgres and Valkey are the only pieces this does not boot itself:

```bash
docker compose up -d db redis          # from the repo root
cd e2e
pnpm install
pnpm exec playwright install chromium --with-deps   # first time only
pnpm exec playwright test
```

`playwright.config.ts` starts the backend (`scripts/start-backend.sh`) and the
frontend (production `server.mjs`, not `vite dev`) itself.

Ports and connection strings all live in `env.ts`, deliberately off the numbers
either docker-compose stack uses:

| piece    | e2e            | vrt compose | task compose |
| -------- | -------------- | ----------- | ------------ |
| frontend | 3001           | 3000        | 3000         |
| backend  | 3401           | 3500        | 3400         |
| database | `vrt_e2e` @ 5433 | `vrt` @ 5433 | 5432       |

The e2e run uses a **separate database** (`vrt_e2e`, created on demand by
`scripts/ensure-database.mjs`) because `migration fresh` drops every table.

## Authentication

The product has no password login — OAuth is the only real sign-in path, and an
e2e run cannot drive GitHub. So the backend exposes a test-only endpoint:

```
POST /v1/auth/test-login  { "username": "..." }
```

It creates the user if needed and issues a session. It is gated twice:

1. `TEST_LOGIN_ENABLED` (default `false`) — otherwise the route answers `404`
2. `common::settings::load_settings` refuses to start a **release** build with
   the flag set, so it cannot ship in the distributed image

**Never set `TEST_LOGIN_ENABLED` outside of this test harness.** It is a full
authentication bypass.
