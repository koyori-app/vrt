/**
 * Production entry (`pnpm start`).
 *
 * `vite build` emits a portable web-fetch handler at `dist/server/server.js`
 * plus static client assets in `dist/client`; this version of TanStack Start
 * ships no hosting preset that turns those into a listening Node process, so we
 * do it here with srvx (the same Node<->fetch adapter Start uses internally).
 *
 * Static files are tried first, everything else — including the `/api/*`
 * catch-all proxy in `src/routes/api.$.ts` — falls through to the SSR handler.
 */
import { serve } from "srvx";
import { serveStatic } from "srvx/static";

import ssr from "./dist/server/server.js";

const server = serve({
  port: Number(process.env.PORT ?? 3000),
  hostname: process.env.HOST ?? "0.0.0.0",
  middleware: [serveStatic({ dir: "./dist/client" })],
  fetch: (request) => ssr.fetch(request),
});

await server.ready();
console.log(`frontend listening on ${server.url}`);
