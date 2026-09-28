// /v1/admin/*: read-only investigation API for `diri-debug`.
//
// Every route requires `Authorization: Bearer <ADMIN_TOKEN>`. Responses are
// JSON except /v1/admin/batch, which streams the stored gzip object untouched
// (the CLI gunzips it). Times are epoch milliseconds throughout.

import { DAY_MS, type Env, error, json } from "./env";

const encoder = new TextEncoder();

async function digest(value: string): Promise<ArrayBuffer> {
  return crypto.subtle.digest("SHA-256", encoder.encode(value));
}

/** Constant-time comparison: both sides are hashed to equal length first. */
export async function authorized(request: Request, env: Env): Promise<boolean> {
  const secret = env.ADMIN_TOKEN;
  if (!secret || secret.length < 16) return false;
  const header = request.headers.get("authorization") ?? "";
  const presented = header.startsWith("Bearer ") ? header.slice(7) : "";
  const [a, b] = await Promise.all([digest(presented), digest(secret)]);
  return crypto.subtle.timingSafeEqual(a, b) && presented.length > 0;
}

const UUID_PREFIX = /^[0-9a-f-]{4,36}$/;
const SUPPORT_ID = /^D-?[0-9A-Z]{4,8}$/i;
const ID = /^[A-Za-z0-9_.:-]{1,96}$/;

function int(value: string | null, fallback: number | null = null): number | null {
  if (value === null || value === "") return fallback;
  const n = Number(value);
  return Number.isSafeInteger(n) ? n : fallback;
}

function limitParam(url: URL, fallback: number, max: number): number {
  const n = int(url.searchParams.get("limit"), fallback) ?? fallback;
  return Math.max(1, Math.min(n, max));
}

/** `session.*` → LIKE 'session.%'. Escapes LIKE's own wildcards. */
export function globToLike(glob: string): string {
  return glob.replace(/[\\%_]/g, (c) => `\\${c}`).replace(/\*/g, "%").replace(/\?/g, "_");
}

function installParam(url: URL): string | null {
  const install = url.searchParams.get("install")?.toLowerCase() ?? null;
  return install && /^[0-9a-f-]{36}$/.test(install) ? install : null;
}

interface Filters {
  where: string[];
  binds: unknown[];
}

function incidentFilters(url: URL): Filters {
  const where: string[] = [];
  const binds: unknown[] = [];
  const add = (clause: string, value: unknown) => {
    binds.push(value);
    where.push(clause.replace("?", `?${binds.length}`));
  };
  const install = installParam(url);
  if (install) add("install = ?", install);
  const kind = url.searchParams.get("kind");
  if (kind) {
    if (/[*?]/.test(kind)) add("kind LIKE ? ESCAPE '\\'", globToLike(kind));
    else add("kind = ?", kind);
  }
  const sev = url.searchParams.get("sev");
  if (sev === "error" || sev === "incident") add("sev = ?", sev);
  const version = url.searchParams.get("version");
  if (version) add("app_version = ?", version);
  const session = url.searchParams.get("session");
  if (session) add("session = ?", session);
  const conv = url.searchParams.get("conv");
  if (conv) add("conv = ?", conv);
  const signature = url.searchParams.get("signature");
  if (signature) add("signature = ?", signature);
  const since = int(url.searchParams.get("since"));
  if (since !== null) add("t >= ?", since);
  const until = int(url.searchParams.get("until"));
  if (until !== null) add("t <= ?", until);
  return { where, binds };
}

function whereSql(filters: Filters): string {
  return filters.where.length ? `WHERE ${filters.where.join(" AND ")}` : "";
}

async function installs(url: URL, env: Env): Promise<Response> {
  const q = (url.searchParams.get("q") ?? "").trim();
  const limit = limitParam(url, 25, 200);
  if (!q) {
    const { results } = await env.DB.prepare("SELECT * FROM installs ORDER BY last_seen DESC LIMIT ?1")
      .bind(limit)
      .all();
    return json({ installs: results });
  }
  // installs is one row per Mac; a scan is cheaper than an index that costs
  // a row write on every batch.
  const clauses = ["name LIKE ?1 ESCAPE '\\'"];
  const binds: unknown[] = [`%${globToLike(q)}%`];
  if (SUPPORT_ID.test(q)) {
    const code = q.toUpperCase().replace(/^D-?/, "");
    binds.push(`D-${code}%`);
    clauses.push(`support_id LIKE ?${binds.length}`);
  }
  if (UUID_PREFIX.test(q.toLowerCase())) {
    binds.push(`${q.toLowerCase()}%`);
    clauses.push(`install LIKE ?${binds.length}`);
  }
  binds.push(limit);
  const { results } = await env.DB.prepare(
    `SELECT * FROM installs WHERE ${clauses.join(" OR ")} ORDER BY last_seen DESC LIMIT ?${binds.length}`,
  )
    .bind(...binds)
    .all();
  return json({ installs: results });
}

async function installDetail(install: string, env: Env): Promise<Response> {
  const [row, batches, incidents, sessions, versions] = await env.DB.batch([
    env.DB.prepare("SELECT * FROM installs WHERE install = ?1").bind(install),
    env.DB.prepare(
      "SELECT COUNT(*) AS n, MIN(t_min) AS t_min, MAX(t_max) AS t_max, SUM(bytes) AS bytes, SUM(lines) AS lines FROM batches WHERE install = ?1",
    ).bind(install),
    env.DB.prepare(
      "SELECT sev, COUNT(*) AS n, MAX(t) AS last_t FROM incidents WHERE install = ?1 GROUP BY sev",
    ).bind(install),
    env.DB.prepare("SELECT COUNT(DISTINCT session) AS n FROM sessions WHERE install = ?1").bind(install),
    env.DB.prepare(
      "SELECT app_version, MIN(received_at) AS first_seen, MAX(received_at) AS last_seen FROM batches WHERE install = ?1 GROUP BY app_version ORDER BY last_seen DESC",
    ).bind(install),
  ]);
  const found = row.results[0];
  if (!found) return error(404, "not_found");
  return json({
    install: found,
    batches: batches.results[0],
    incidents: incidents.results,
    sessions: (sessions.results[0] as { n: number }).n,
    versions: versions.results,
  });
}

async function incidents(url: URL, env: Env): Promise<Response> {
  const filters = incidentFilters(url);
  const limit = limitParam(url, 100, 1000);
  filters.binds.push(limit);
  const { results } = await env.DB.prepare(
    `SELECT id, install, t, seq, proc, pid, kind, sev, session, conv, agent, code, signature, app_version, fields
     FROM incidents ${whereSql(filters)} ORDER BY t DESC LIMIT ?${filters.binds.length}`,
  )
    .bind(...filters.binds)
    .all<Record<string, unknown>>();
  for (const row of results) {
    try {
      row.fields = JSON.parse(String(row.fields));
    } catch {
      // Left as the stored string.
    }
  }
  return json({ incidents: results });
}

async function incidentSummary(url: URL, env: Env): Promise<Response> {
  const filters = incidentFilters(url);
  const limit = limitParam(url, 50, 500);
  filters.binds.push(limit);
  const { results } = await env.DB.prepare(
    `SELECT signature, MIN(kind) AS kind, MAX(sev) AS sev, COUNT(*) AS count,
            COUNT(DISTINCT install) AS installs, MIN(t) AS first_t, MAX(t) AS last_t,
            json_group_array(DISTINCT app_version) AS versions
     FROM incidents ${whereSql(filters)}
     GROUP BY signature ORDER BY count DESC, last_t DESC LIMIT ?${filters.binds.length}`,
  )
    .bind(...filters.binds)
    .all<Record<string, unknown>>();
  for (const row of results) {
    const versions = JSON.parse(String(row.versions)) as (string | null)[];
    row.versions = versions.filter((v) => v !== null);
  }
  return json({ groups: results });
}

async function batches(url: URL, env: Env): Promise<Response> {
  const install = installParam(url);
  if (!install) return error(400, "install_required");
  const since = int(url.searchParams.get("since"), 0);
  const until = int(url.searchParams.get("until"), Number.MAX_SAFE_INTEGER);
  const limit = limitParam(url, 500, 5000);
  // Records' own clock decides overlap. The received_at bound (a day of
  // slack for client clock skew) lets the index skip older batches.
  const { results } = await env.DB.prepare(
    `SELECT r2_key, received_at, sent_at, t_min, t_max, procs, lines, bad_lines, bytes, raw_bytes, app_version
     FROM batches
     WHERE install = ?1 AND received_at >= ?2 AND t_max >= ?3 AND t_min <= ?4
     ORDER BY t_min ASC LIMIT ?5`,
  )
    .bind(install, (since ?? 0) - DAY_MS, since, until, limit)
    .all();
  return json({ batches: results });
}

async function batchBody(url: URL, env: Env): Promise<Response> {
  const key = url.searchParams.get("key") ?? "";
  if (!/^v1\/[0-9a-f-]{36}\/\d{4}-\d{2}-\d{2}\/\d+-[0-9a-f]+\.ndjson\.gz$/.test(key)) {
    return error(400, "bad_key");
  }
  const object = await env.BATCHES.get(key);
  if (!object) return error(404, "not_found");
  // Deliberately no Content-Encoding: the client receives the gzip bytes as
  // stored and decompresses them itself.
  return new Response(object.body, {
    headers: {
      "content-type": "application/gzip",
      "content-length": String(object.size),
      "x-diri-install": object.customMetadata?.install ?? "",
    },
  });
}

async function sessions(url: URL, env: Env): Promise<Response> {
  const install = installParam(url);
  if (!install) return error(400, "install_required");
  const limit = limitParam(url, 200, 2000);
  const { results } = await env.DB.prepare(
    `SELECT session, conv, agent, first_t, last_t FROM sessions
     WHERE install = ?1 ORDER BY last_t DESC LIMIT ?2`,
  )
    .bind(install, limit)
    .all();
  return json({ sessions: results });
}

async function find(url: URL, env: Env): Promise<Response> {
  const id = url.searchParams.get("id") ?? "";
  if (!ID.test(id)) return error(400, "bad_id");
  const { results } = await env.DB.prepare(
    `SELECT s.install, s.session, s.conv, s.agent, s.first_t, s.last_t,
            i.support_id, i.name, i.app_version
     FROM sessions s JOIN installs i ON i.install = s.install
     WHERE s.session = ?1 OR (s.conv = ?1 AND s.conv != '')
     ORDER BY s.last_t DESC LIMIT 100`,
  )
    .bind(id)
    .all();
  return json({ matches: results });
}

export async function handleAdmin(request: Request, env: Env, url: URL): Promise<Response> {
  if (!(await authorized(request, env))) return error(401, "unauthorized");
  if (request.method !== "GET") return error(405, "method_not_allowed");
  const path = url.pathname.replace(/\/+$/, "");
  switch (path) {
    case "/v1/admin/installs":
      return installs(url, env);
    case "/v1/admin/incidents":
      return incidents(url, env);
    case "/v1/admin/incidents/summary":
      return incidentSummary(url, env);
    case "/v1/admin/batches":
      return batches(url, env);
    case "/v1/admin/batch":
      return batchBody(url, env);
    case "/v1/admin/sessions":
      return sessions(url, env);
    case "/v1/admin/find":
      return find(url, env);
  }
  const detail = /^\/v1\/admin\/installs\/([0-9a-fA-F-]{36})$/.exec(path);
  if (detail) return installDetail(detail[1].toLowerCase(), env);
  return error(404, "not_found");
}
