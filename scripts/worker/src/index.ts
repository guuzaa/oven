// Serves oven's distribution endpoints from one CDN hostname.
// GitHub release assets redirect to a short-lived signed URL. That redirect
// must not be cached: only the final 200 body is stored, under the public URL,
// so a zone purge of that URL actually drops the edge copy.

const REPO = "guuzaa/oven";
const BRANCH = "master";

const API_BASE = `https://api.github.com/repos/${REPO}`;
const DOWNLOAD_BASE = `https://github.com/${REPO}/releases/download`;
const RAW_BASE = `https://raw.githubusercontent.com/${REPO}/${BRANCH}`;

const ONE_YEAR_SECONDS = 31_536_000;
const ONE_MINUTE_SECONDS = 60;
const MAX_REDIRECTS = 3;

const RELEASE_ASSET = /^\/dl\/(v[\w.\-]+)\/([\w.\-]+)$/;
const INSTALLER_SCRIPT = /^\/install\.(sh|ps1)$/;
const TAGGED_RELEASE = /^\/tags\/(v[\w.\-]+)$/;
// PowerShell's irm sends "WindowsPowerShell/5.1" or "PowerShell/7.x"; curl and
// wget send neither, and get the shell installer.
const POWERSHELL_AGENT = /powershell\//i;

const ALLOWED_HOSTS = new Set([
  "github.com",
  "api.github.com",
  "raw.githubusercontent.com",
  "objects.githubusercontent.com",
  "release-assets.githubusercontent.com",
  "github-releases.githubusercontent.com",
]);

const NO_STORE: HeadersInit = { "cache-control": "no-store" };
const USER_AGENT = "oven-dist-worker";
const PASSTHROUGH_HEADERS = ["content-type", "content-length", "content-disposition", "etag"];

interface Env {
  GITHUB_TOKEN?: string;
}

interface EdgeCache {
  match(request: Request): Promise<Response | undefined>;
  put(request: Request, response: Response): Promise<void>;
}

interface Ctx {
  waitUntil(promise: Promise<unknown>): void;
}

function edgeCache(): EdgeCache {
  return (caches as unknown as { default: EdgeCache }).default;
}

function cacheControl(ttlSeconds: number): string {
  if (ttlSeconds === ONE_YEAR_SECONDS) {
    return `public, max-age=${ttlSeconds}, immutable`;
  }
  return `public, max-age=${ttlSeconds}`;
}

function isAllowedUpstream(target: string): boolean {
  const url = new URL(target);
  return url.protocol === "https:" && ALLOWED_HOSTS.has(url.hostname);
}

function headersFor(target: string, token: string | undefined): Headers {
  const headers = new Headers();
  headers.set("user-agent", USER_AGENT);
  headers.set("accept-encoding", "identity");
  if (new URL(target).hostname === "api.github.com") {
    headers.set("accept", "application/vnd.github+json");
    if (token) {
      headers.set("authorization", `Bearer ${token}`);
    }
  }
  return headers;
}

// Follow redirects ourselves so the signed asset URL is never stored, and so
// the GitHub token cannot ride along to a different host.
async function fetchUpstream(target: string, token: string | undefined): Promise<Response> {
  let current = target;
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    if (!isAllowedUpstream(current)) {
      return new Response("upstream host is not allowed\n", { status: 502, headers: NO_STORE });
    }
    const response = await fetch(current, {
      method: "GET",
      headers: headersFor(current, token),
      redirect: "manual",
      cache: "no-store",
    });
    if (![301, 302, 303, 307, 308].includes(response.status)) {
      return response;
    }
    const location = response.headers.get("location");
    await response.body?.cancel();
    if (!location) {
      return new Response("redirect missing location\n", { status: 502, headers: NO_STORE });
    }
    current = new URL(location, current).href;
  }
  return new Response("too many redirects\n", { status: 502, headers: NO_STORE });
}

function buildResponse(upstream: Response, cacheControlValue: string): Response {
  const headers = new Headers();
  for (const name of PASSTHROUGH_HEADERS) {
    const value = upstream.headers.get(name);
    if (value) {
      headers.set(name, value);
    }
  }
  headers.set("cache-control", cacheControlValue);
  return new Response(upstream.body, { status: upstream.status, headers });
}

function mark(response: Response, cacheStatus: "HIT" | "MISS"): Response {
  const headers = new Headers(response.headers);
  headers.set("x-oven-cache", cacheStatus);
  return new Response(response.body, { status: response.status, headers });
}

function varyByAgent(response: Response): Response {
  const headers = new Headers(response.headers);
  headers.set("vary", "user-agent");
  return new Response(response.body, { status: response.status, headers });
}

// Serves /install, which picks the script for the caller, and the explicit
// /install.sh and /install.ps1. Every variant is cached under its canonical
// /install.<ext> key, so a dispatched response is never handed to the other
// platform and a purge of the canonical path drops it.
function serveInstaller(request: Request, ctx: Ctx, extension: string): Promise<Response> {
  return serve(
    request,
    ctx,
    `/install.${extension}`,
    `${RAW_BASE}/scripts/install.${extension}`,
    undefined,
    ONE_MINUTE_SECONDS,
  );
}

async function serve(
  request: Request,
  ctx: Ctx,
  keyPath: string,
  upstreamUrl: string,
  token: string | undefined,
  ttlSeconds: number,
): Promise<Response> {
  const url = new URL(request.url);
  const key = new Request(new URL(keyPath, url.origin).href, { method: "GET" });
  const hit = await edgeCache().match(key);
  if (hit) {
    if (request.method === "HEAD") {
      const headers = new Headers(hit.headers);
      await hit.body?.cancel();
      return mark(new Response(null, { status: hit.status, headers }), "HIT");
    }
    return mark(hit, "HIT");
  }

  const upstream = await fetchUpstream(upstreamUrl, token);
  if (upstream.status !== 200) {
    const headers = new Headers(NO_STORE);
    headers.set("x-oven-cache", "MISS");
    return new Response(upstream.body, { status: upstream.status, headers });
  }

  const ready = buildResponse(upstream, cacheControl(ttlSeconds));
  // Clone before returning. A client that disconnects cancels only its branch;
  // the stored branch keeps reading the upstream body.
  ctx.waitUntil(edgeCache().put(key, ready.clone()));
  return mark(ready, "MISS");
}

export default {
  async fetch(request: Request, env: Env, ctx: Ctx): Promise<Response> {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed\n", { status: 405, headers: NO_STORE });
    }

    const { pathname } = new URL(request.url);
    if (pathname === "/_health") {
      return new Response("ok\n", { headers: NO_STORE });
    }

    const asset = RELEASE_ASSET.exec(pathname);
    if (asset && asset[2] !== "." && asset[2] !== "..") {
      return serve(
        request,
        ctx,
        pathname,
        `${DOWNLOAD_BASE}/${asset[1]}/${asset[2]}`,
        undefined,
        ONE_YEAR_SECONDS,
      );
    }

    // The installers read the digest GitHub records per asset from the release
    // JSON, so the mirror has to serve it for pinned tags as well.
    const tagged = TAGGED_RELEASE.exec(pathname);
    if (tagged) {
      const upstream = `${API_BASE}/releases/tags/${tagged[1]}`;
      return serve(request, ctx, pathname, upstream, env.GITHUB_TOKEN, ONE_MINUTE_SECONDS);
    }

    if (pathname === "/install") {
      const agent = request.headers.get("user-agent") ?? "";
      return varyByAgent(await serveInstaller(request, ctx, POWERSHELL_AGENT.test(agent) ? "ps1" : "sh"));
    }

    const installer = INSTALLER_SCRIPT.exec(pathname);
    if (installer) {
      return serveInstaller(request, ctx, installer[1]);
    }

    if (pathname === "/latest") {
      const upstream = `${API_BASE}/releases/latest`;
      return serve(request, ctx, pathname, upstream, env.GITHUB_TOKEN, ONE_MINUTE_SECONDS);
    }

    return new Response(`not found: ${pathname}\n`, { status: 404, headers: NO_STORE });
  },
};
