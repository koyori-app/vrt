# VRT frontend

TanStack Start (React 19) UI for the VRT service. Every piece of data comes from the Rust
backend's OpenAPI document: types are generated with `openapi-typescript` and consumed through a
typed `openapi-fetch` client wrapped in `openapi-react-query`, so there are no hand-written fetch
wrappers or response types anywhere in `src/`.

```
backend (utoipa)  ──cargo run --bin export_openapi──▶  openapi.json
openapi.json      ──openapi-typescript──────────────▶  src/generated/api.d.ts
api.d.ts          ──openapi-fetch + openapi-react-query▶  $api.useQuery("get", "/v1/…")
```

`openapi.json` is committed; `src/generated/` is gitignored (root `.gitignore`) and rebuilt from it.

## Requirements

- Node 24+, pnpm 11 (this app is a member of the `/home/yupix/vrt` pnpm workspace, and
  `sharedWorkspaceLockfile: false` means it keeps its own `pnpm-lock.yaml`)
- A Rust toolchain, only for `pnpm openapi:export`

## Backend environment (required for local dev)

Run the backend so that the browser origin of the dev server is accepted by the CSRF middleware
(`crates/handler/src/middlewares/csrf.rs` rejects state-changing requests whose `Origin` is neither
`ALLOW_ORIGIN` nor the API's own host):

| Variable       | Dev value               | Why                                                               |
| -------------- | ----------------------- | ----------------------------------------------------------------- |
| `ALLOW_ORIGIN` | `http://localhost:3000` | Must equal the frontend origin, or every POST/PATCH/DELETE is 403. |
| `APP_URL`      | `http://localhost:3000` | OAuth `redirect_uri` is built from it — keeps the session cookie first-party via the proxy. |
| `LISTEN_ADDR`  | `0.0.0.0:3400` (default) | `docker compose` publishes the container's 3400 on host **3500**. |

The repo's `docker-compose.yml` already sets all three (and publishes the backend on
`127.0.0.1:3500`), so `docker compose up -d` is enough. Real OAuth logins additionally need real
`GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` (and/or the GitLab pair) — the compose file ships dummy
values that get you as far as the provider's error page.

## Development

```bash
pnpm install            # from the repo root (workspace) or from this directory
pnpm openapi            # export from the backend + regenerate src/generated/api.d.ts
pnpm dev                # http://localhost:3000
```

`vite.config.ts` proxies `/api/*` to `VITE_API_BASE` (default `http://localhost:3500`) and strips
the `/api` prefix. `changeOrigin` is deliberately **off** so the browser's real `Origin`
(`http://localhost:3000`) reaches the backend's CSRF check.

Point the proxy elsewhere with a `.env` (see `.env.example`):

```bash
VITE_API_BASE=http://localhost:3400   # e.g. a `cargo run` backend using the default LISTEN_ADDR
```

Other scripts:

| Script                   | Does                                                    |
| ------------------------ | ------------------------------------------------------- |
| `pnpm typecheck`         | `tsc --noEmit`                                          |
| `pnpm fmt` / `fmt:check` | Prettier over `src` (the repo's lint-staged hook runs this) |
| `pnpm openapi:export`    | Backend → `openapi.json`                                |
| `pnpm openapi:generate`  | `openapi.json` → `src/generated/api.d.ts`               |

## Production

```bash
pnpm build              # dist/client (assets) + dist/server/server.js (fetch handler)
API_BASE=http://backend:3400 PORT=3000 pnpm start
```

`pnpm start` runs `server.mjs`, a ~20-line srvx host that serves `dist/client` statically and hands
everything else to the SSR handler. This version of TanStack Start emits a portable web-fetch
handler rather than a ready-to-run Node server, so the host lives in the repo instead of in a
hosting preset.

In production there is no Vite proxy, so `/api/*` is served by the catch-all **server route** in
`src/routes/api.$.ts`, a port of task's `server/middlewares/api-proxy.ts`:

- forwards every method to `API_BASE`, stripping the `/api` prefix
- strips hop-by-hop headers both ways (plus `content-encoding`/`content-length`, which Node's
  `fetch` invalidates by decompressing)
- forwards `Cookie` upstream and every upstream `Set-Cookie` back individually via
  `Headers.getSetCookie()`
- streams request bodies with a hard 25MB cap (`UPLOAD_MAX_SIZE_MB`), answering `413` past it —
  this is the path screenshot uploads and image `GET`s take
- passes status codes and bodies through untouched

| Variable             | Default                 | Meaning                                          |
| -------------------- | ----------------------- | ------------------------------------------------ |
| `API_BASE`           | `http://localhost:3500` | Backend origin for the SSR proxy and SSR queries. |
| `PORT` / `HOST`      | `3000` / `0.0.0.0`      | Where `pnpm start` listens.                       |
| `UPLOAD_MAX_SIZE_MB` | `25`                    | Proxy body cap.                                   |

## Notes on data access

- `src/lib/api.ts` builds one `openapi-fetch` client whose base URL is `/api` in the browser and
  `API_BASE` during SSR (`createIsomorphicFn`), and forwards the incoming request's `Cookie` header
  when running on the server — that is what makes the `_authed` guard work on the first render.
- `src/lib/queries.ts` holds the shared query options. Backend routes are id-based while URLs are
  slug-based, so `useResolvedTenant` / `useResolvedProject` resolve a slug against the tenant and
  project lists (the same idea as task's `useResolvedTenantId`).
- Images are plain `<img src="/api/v1/screenshots/{id}/content">`, so the session cookie rides along
  through the proxy.

### Known backend gaps the UI works around

- Builds are addressed by uuid only, so `/…/builds/$number` finds the build by scanning the
  project's build list. A `GET /v1/projects/{id}/builds/{number}` endpoint would remove that hop.
- `TenantMemberResponse` carries `user_id` but no username/display name, so the members table shows
  raw ids.
