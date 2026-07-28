import { createFileRoute } from "@tanstack/react-router";

/**
 * Production SSR proxy: forwards every `/api/*` request to the Rust backend
 * with the `/api` prefix stripped.
 *
 * Ported from task's `server/middlewares/api-proxy.ts` (Elysia) onto TanStack
 * Start's server-route API: `createFileRoute(path)({ server: { handlers } })`,
 * where each handler receives `{ request, params, context }` and returns a
 * `Response`.
 *
 * In dev this file is never reached — Vite's `server.proxy` (see vite.config.ts)
 * intercepts `/api` first as connect middleware, before the Start handler runs.
 *
 * Semantics preserved from the original:
 * - hop-by-hop headers are stripped in both directions
 * - `Cookie` is forwarded upstream, and every upstream `Set-Cookie` is
 *   forwarded back individually via `Headers.getSetCookie()`
 * - request bodies stream through with a hard byte cap (413 when exceeded)
 * - status code / status text / response body pass through untouched
 */

/** 25MB, matching the backend's per-screenshot multipart limit. */
export const MAX_PROXY_BODY_BYTES = Number(process.env.UPLOAD_MAX_SIZE_MB ?? 25) * 1024 * 1024;

/**
 * 200MB, matching the backend's Storybook bundle limit
 * (`service::render::MAX_BUNDLE_BYTES`). The bundle route is the only endpoint
 * that legitimately carries hundreds of megabytes, so the cap is path-aware
 * rather than globally raised: everything else stays at the 25MB screenshot cap.
 */
export const MAX_BUNDLE_PROXY_BODY_BYTES =
  Number(process.env.BUNDLE_MAX_SIZE_MB ?? 200) * 1024 * 1024;

const BUNDLE_PATH = /^\/api\/v1\/ci\/builds\/[^/]+\/storybook\/?$/;

/** Per-path body cap. Bodies stream through, so this only bounds the byte count. */
export function maxBodyBytesForPath(pathname: string): number {
  return BUNDLE_PATH.test(pathname) ? MAX_BUNDLE_PROXY_BODY_BYTES : MAX_PROXY_BODY_BYTES;
}

const API_BASE = process.env.API_BASE ?? "http://localhost:3500";

const HOP_BY_HOP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailers",
  "transfer-encoding",
  "upgrade",
  "host",
  // Node's fetch decompresses the upstream body, so a copied
  // content-encoding/content-length would describe bytes we no longer send.
  "content-encoding",
  "content-length",
  // `Expect: 100-continue` is a hop-by-hop negotiation between the client and
  // *this* proxy; Node's HTTP server already answered it before the handler ran.
  // Forwarding it makes undici throw `NotSupportedError: expect header not
  // supported`, which surfaces as a failed upstream fetch. curl sends this
  // automatically for bodies over 1KB, so every multi-MB upload hit it.
  "expect",
]);

class BodyTooLargeError extends Error {
  constructor() {
    super("Payload Too Large");
    this.name = "BodyTooLargeError";
  }
}

function buildBackendUrl(request: Request): string {
  const url = new URL(request.url);
  return `${API_BASE}${url.pathname.replace(/^\/api/, "")}${url.search}`;
}

function copyHeaders(source: Headers): Headers {
  const headers = new Headers();
  source.forEach((value, key) => {
    const lowerKey = key.toLowerCase();
    if (HOP_BY_HOP.has(lowerKey)) return;
    // Set-Cookie must be appended one-by-one, never collapsed into one value.
    if (lowerKey === "set-cookie") return;
    headers.set(key, value);
  });
  for (const cookie of source.getSetCookie()) {
    headers.append("Set-Cookie", cookie);
  }
  return headers;
}

function rejectIfContentLengthTooLarge(request: Request, maxBytes: number): Response | null {
  const contentLength = request.headers.get("content-length");
  if (!contentLength) return null;
  const length = Number(contentLength);
  if (!Number.isFinite(length) || length < 0) return null;
  if (length > maxBytes) return new Response("Payload Too Large", { status: 413 });
  return null;
}

export type LimitedReadableStream = {
  stream: ReadableStream<Uint8Array>;
  readonly exceeded: boolean;
};

export function limitReadableStream(
  body: ReadableStream<Uint8Array>,
  maxBytes: number,
): LimitedReadableStream {
  let consumed = 0;
  let exceeded = false;
  const reader = body.getReader();

  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        controller.close();
        return;
      }
      consumed += value.byteLength;
      if (consumed > maxBytes) {
        exceeded = true;
        await reader.cancel();
        controller.error(new BodyTooLargeError());
        return;
      }
      controller.enqueue(value);
    },
    cancel(reason) {
      return reader.cancel(reason);
    },
  });

  return {
    stream,
    get exceeded() {
      return exceeded;
    },
  };
}

async function proxyToBackend(request: Request): Promise<Response> {
  const maxBodyBytes = maxBodyBytesForPath(new URL(request.url).pathname);
  const rejected = rejectIfContentLengthTooLarge(request, maxBodyBytes);
  if (rejected) return rejected;

  const backendUrl = buildBackendUrl(request);
  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  const limited =
    hasBody && request.body ? limitReadableStream(request.body, maxBodyBytes) : undefined;

  try {
    const backendResponse = await fetch(backendUrl, {
      method: request.method,
      headers: copyHeaders(request.headers),
      body: limited?.stream,
      redirect: "manual",
      // Node's fetch requires `duplex` when streaming a request body.
      ...(limited ? { duplex: "half" } : {}),
    } as RequestInit);

    return new Response(backendResponse.body, {
      status: backendResponse.status,
      statusText: backendResponse.statusText,
      headers: copyHeaders(backendResponse.headers),
    });
  } catch (error) {
    const cause = error instanceof Error ? error.cause : undefined;
    if (
      limited?.exceeded ||
      error instanceof BodyTooLargeError ||
      cause instanceof BodyTooLargeError
    ) {
      return new Response("Payload Too Large", { status: 413 });
    }
    // 上流への fetch が失敗した（接続拒否・接続リセット・上流が過大ボディで
    // 中断した等）。ここで投げ返すと Start のエラー境界が
    // `{"status":500,"unhandled":true,"message":"HTTPError"}` を返してしまい、
    // 障害がプロキシ側なのか API 側なのか区別がつかなくなる。502 に正規化する。
    console.error("[api-proxy] upstream request failed", {
      url: backendUrl,
      method: request.method,
      error,
    });
    return new Response(JSON.stringify({ message: "upstream request failed" }), {
      status: 502,
      headers: { "content-type": "application/json" },
    });
  }
}

const handler = ({ request }: { request: Request }) => proxyToBackend(request);

export const Route = createFileRoute("/api/$")({
  server: {
    handlers: {
      GET: handler,
      HEAD: handler,
      POST: handler,
      PUT: handler,
      PATCH: handler,
      DELETE: handler,
      OPTIONS: handler,
    },
  },
});
