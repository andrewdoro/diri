// diri update mirror: a second route to the macOS update feed and archives
// for networks that cannot reach GitHub (see diri/crates/diri-updater/src/
// mirror.rs for the client side and its trust model).
//
// It proxies exactly two things from the public GitHub release, byte for byte:
//
//   GET /appcast.json                         -> releases/latest/download/appcast.json
//   GET /releases/download/<tag>/<diri-*.zip> -> releases/download/<tag>/<file>
//
// Nothing else is served, so this is not an open proxy. The mirror is not
// trusted by the app: archive URLs in the feed must name github.com, and every
// archive is checked against the feed's SHA-256 and diri's Developer ID before
// it is installed. This Worker only has to be available, not honest.

const REPO = "cristicretu/diri";
const GITHUB = `https://github.com/${REPO}/releases`;
const ASSET = /^\/releases\/download\/(v\d+\.\d+\.\d+[A-Za-z0-9.-]*)\/(diri-[A-Za-z0-9._-]+\.zip)$/;
/// The feed changes at every release; a short edge TTL keeps a new release
/// visible within minutes while absorbing a fleet's 6-hourly checks.
const FEED_TTL_SECONDS = 120;
/// A tag's assets never change once published (re-releases replace the feed
/// row's SHA-256 too, and the app re-checks it), so archives cache for long.
const ASSET_TTL_SECONDS = 30 * 24 * 60 * 60;

export interface Deps {
  fetch: (input: string, init?: RequestInit) => Promise<Response>;
  cache: Pick<Cache, "match" | "put">;
  waitUntil: (promise: Promise<unknown>) => void;
}

export async function handle(request: Request, deps: Deps): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === "/healthz") return text(200, "ok");
  const upstream = upstreamFor(url.pathname);
  if (!upstream) return text(404, "not found");
  if (request.method !== "GET" && request.method !== "HEAD") {
    return text(405, "method not allowed", { allow: "GET, HEAD" });
  }

  // Keyed on the mirror path alone: query strings never reach GitHub, so they
  // must not split (or poison) the cache either.
  const key = new Request(`${url.origin}${url.pathname}`, { method: "GET" });
  const cached = await deps.cache.match(key);
  if (cached) return request.method === "HEAD" ? withoutBody(cached) : cached;

  let response: Response;
  try {
    response = await deps.fetch(upstream.url, {
      redirect: "follow",
      headers: { "user-agent": "diri-updates-mirror" },
    });
  } catch {
    return text(502, "upstream unreachable");
  }
  if (response.status === 404) return text(404, "not found");
  if (!response.ok || !response.body) return text(502, `upstream status ${response.status}`);

  const headers = new Headers({
    "content-type": upstream.contentType,
    "cache-control": `public, max-age=${upstream.ttl}`,
    "x-diri-mirror": "github",
  });
  const length = response.headers.get("content-length");
  if (length) headers.set("content-length", length);
  const mirrored = new Response(response.body, { status: 200, headers });
  deps.waitUntil(deps.cache.put(key, mirrored.clone()));
  return request.method === "HEAD" ? withoutBody(mirrored) : mirrored;
}

function upstreamFor(path: string): { url: string; ttl: number; contentType: string } | null {
  if (path === "/appcast.json") {
    return {
      url: `${GITHUB}/latest/download/appcast.json`,
      ttl: FEED_TTL_SECONDS,
      contentType: "application/json",
    };
  }
  const asset = ASSET.exec(path);
  if (!asset) return null;
  return {
    url: `${GITHUB}/download/${asset[1]}/${asset[2]}`,
    ttl: ASSET_TTL_SECONDS,
    contentType: "application/zip",
  };
}

function text(status: number, body: string, extra: Record<string, string> = {}): Response {
  return new Response(body, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store", ...extra },
  });
}

function withoutBody(response: Response): Response {
  return new Response(null, { status: response.status, headers: response.headers });
}

export default {
  async fetch(request: Request, _env: unknown, ctx: ExecutionContext): Promise<Response> {
    return handle(request, {
      fetch: (input, init) => fetch(input, init),
      cache: caches.default,
      waitUntil: (promise) => ctx.waitUntil(promise),
    });
  },
};
