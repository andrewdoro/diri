// Where records come from: the Worker's admin API (remote) or a spool
// directory on this Mac (local). Both answer the same questions so every
// view works over either.

import { readdirSync, readFileSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname } from "node:path";
import { gunzipSync } from "node:zlib";
import { filterRecords, globMatcher, isIncident, mergeSort, parseNdjson, severityMatcher } from "./records.mjs";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * DIRI_TELEMETRY_URL / DIRI_TELEMETRY_ADMIN_TOKEN, else
 * ~/.config/diri-debug/config.json `{ "url": …, "token": … }`.
 * @param {Record<string, string | undefined>} env
 */
export function loadConfig(env) {
  let file = {};
  const home = env.HOME ?? homedir();
  const path = env.DIRI_DEBUG_CONFIG ?? join(home, ".config", "diri-debug", "config.json");
  if (existsSync(path)) {
    try {
      file = JSON.parse(readFileSync(path, "utf8"));
    } catch (err) {
      throw new Error(`${path}: ${err.message}`);
    }
  }
  return {
    url: (env.DIRI_TELEMETRY_URL ?? file.url ?? "").replace(/\/+$/, ""),
    token: env.DIRI_TELEMETRY_ADMIN_TOKEN ?? file.token ?? "",
  };
}

/** Runs `fn` over `items` with at most `limit` in flight, keeping order. */
async function pool(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i], i);
    }
  });
  await Promise.all(workers);
  return out;
}

export class RemoteSource {
  /** @param {{url: string, token: string}} config @param {typeof fetch} [fetchImpl] */
  constructor(config, fetchImpl = fetch) {
    if (!config.url) throw new Error("set DIRI_TELEMETRY_URL (or url in ~/.config/diri-debug/config.json), or use `diri-debug local`");
    if (!config.token) throw new Error("set DIRI_TELEMETRY_ADMIN_TOKEN (or token in ~/.config/diri-debug/config.json)");
    this.config = config;
    this.fetch = fetchImpl;
    this.kind = "remote";
  }

  async request(path, params = {}) {
    const url = new URL(`${this.config.url}${path}`);
    for (const [key, value] of Object.entries(params)) {
      if (value !== undefined && value !== null && value !== "") url.searchParams.set(key, String(value));
    }
    const res = await this.fetch(url, { headers: { authorization: `Bearer ${this.config.token}` } });
    if (!res.ok) {
      const body = await res.text().catch(() => "");
      throw new Error(`${path} → HTTP ${res.status} ${body.slice(0, 200)}`);
    }
    return res;
  }

  async get(path, params) {
    return (await this.request(path, params)).json();
  }

  /**
   * A name, Support ID, install UUID or UUID prefix → one install. Several
   * matches are an error listing them, unless one name matches exactly.
   * @param {string} who
   */
  async resolve(who) {
    if (UUID.test(who)) {
      const detail = await this.get(`/v1/admin/installs/${who.toLowerCase()}`).catch(() => null);
      if (detail) return detail.install;
    }
    const { installs } = await this.get("/v1/admin/installs", { q: who });
    if (installs.length === 1) return installs[0];
    const exact = installs.filter(
      (i) => i.name?.toLowerCase() === who.toLowerCase() || i.support_id === who.toUpperCase(),
    );
    if (exact.length === 1) return exact[0];
    if (installs.length === 0) throw new Error(`no install matches "${who}" (try \`diri-debug who ${who}\`)`);
    const list = installs.map((i) => `  ${i.support_id}  ${i.name ?? "-"}  ${i.install}`).join("\n");
    throw new Error(`"${who}" matches ${installs.length} installs; pass a Support ID:\n${list}`);
  }

  async who(q) {
    return (await this.get("/v1/admin/installs", { q })).installs;
  }

  async installDetail(install) {
    return this.get(`/v1/admin/installs/${install}`);
  }

  /**
   * Every record of `install` in [since, until], merged across batches.
   * @returns {Promise<{records: import("./records.mjs").Rec[], batches: number, bad: number}>}
   */
  async records(install, since, until) {
    const { batches } = await this.get("/v1/admin/batches", { install, since: Math.floor(since), until: Math.ceil(until) });
    let bad = 0;
    const parts = await pool(batches, 6, async (batch) => {
      const text = await this.rawBatch(batch.r2_key);
      const parsed = parseNdjson(text);
      bad += parsed.bad;
      return parsed.records;
    });
    const records = mergeSort(parts.flat()).filter((r) => r.t >= since && r.t <= until);
    return { records, batches: batches.length, bad };
  }

  async rawBatch(key) {
    const res = await this.request("/v1/admin/batch", { key });
    const bytes = Buffer.from(await res.arrayBuffer());
    // A proxy may already have decoded it; gzip starts 1f 8b.
    return (bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : bytes).toString("utf8");
  }

  async incidents(filters) {
    const { incidents } = await this.get("/v1/admin/incidents", filters);
    return incidents;
  }

  async summary(filters) {
    const { groups } = await this.get("/v1/admin/incidents/summary", filters);
    return groups;
  }

  async sessions(install) {
    return (await this.get("/v1/admin/sessions", { install })).sessions;
  }

  async find(id) {
    return (await this.get("/v1/admin/find", { id })).matches;
  }
}

export function defaultSpoolDir(env) {
  return join(env.HOME ?? homedir(), "Library", "Application Support", "Dirijor", "telemetry", "spool");
}

/** Support ID as the recorder computes it (identity.rs::support_id). */
export function supportIdOf(install) {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  const hex = install.replace(/[^0-9a-f]/gi, "").slice(0, 10);
  const bits = BigInt(`0x${hex || "0"}`);
  let code = "";
  for (let i = 7; i >= 0; i--) code += alphabet[Number((bits >> BigInt(i * 5)) & 31n)];
  return `D-${code}`;
}

/**
 * The spool on this Mac (or --dir): `*.open` files still being written and
 * sealed `*.jsonl`, from every process.
 */
export class LocalSource {
  constructor(dir) {
    this.dir = dir;
    this.kind = "local";
    if (!existsSync(dir)) throw new Error(`no spool at ${dir} (pass --dir)`);
    this._records = null;
  }

  identity() {
    const base = dirname(this.dir);
    let install = "local";
    let name = null;
    try {
      install = JSON.parse(readFileSync(join(base, "install.json"), "utf8")).install_id ?? install;
    } catch {}
    try {
      name = JSON.parse(readFileSync(join(base, "config.json"), "utf8")).name ?? null;
    } catch {}
    return {
      install,
      support_id: install === "local" ? "-" : supportIdOf(install),
      name: name ?? "this Mac",
    };
  }

  allRecords() {
    if (this._records) return this._records;
    const files = readdirSync(this.dir).filter((n) => n.endsWith(".open") || n.endsWith(".jsonl"));
    let bad = 0;
    const records = [];
    for (const name of files) {
      const parsed = parseNdjson(readFileSync(join(this.dir, name), "utf8"));
      bad += parsed.bad;
      for (const r of parsed.records) records.push(r);
    }
    this._records = { records: mergeSort(records), files: files.length, bad };
    return this._records;
  }

  async resolve() {
    return this.identity();
  }

  async who() {
    return [this.identity()];
  }

  async records(_install, since, until) {
    const all = this.allRecords();
    return { records: all.records.filter((r) => r.t >= since && r.t <= until), batches: all.files, bad: all.bad };
  }

  async incidents(filters) {
    const identity = this.identity();
    const kind = globMatcher(filters.kind);
    const sev = severityMatcher(filters.sev);
    const rows = filterRecords(this.allRecords().records, {
      since: filters.since,
      until: filters.until,
      session: filters.session,
      conv: filters.conv,
    })
      .filter((r) => isIncident(r) && kind(r.k) && sev(r.s))
      .map((r) => ({
        install: identity.install,
        name: identity.name,
        support_id: identity.support_id,
        t: r.t,
        seq: r.seq,
        proc: r.p,
        pid: r.pid,
        kind: r.k,
        sev: r.s,
        session: r.f.session ?? null,
        conv: r.f.conv ?? null,
        agent: r.f.agent ?? null,
        code: r.f.code === undefined ? null : String(r.f.code),
        signature: localSignature(r),
        fields: r.f,
      }))
      .reverse();
    return rows.slice(0, filters.limit ?? rows.length);
  }

  async summary(filters) {
    const groups = new Map();
    for (const row of await this.incidents({ ...filters, limit: undefined })) {
      const g = groups.get(row.signature) ?? { signature: row.signature, kind: row.kind, sev: row.sev, count: 0, installs: 1, first_t: row.t, last_t: row.t, versions: [] };
      g.count += 1;
      g.first_t = Math.min(g.first_t, row.t);
      g.last_t = Math.max(g.last_t, row.t);
      groups.set(row.signature, g);
    }
    return [...groups.values()].sort((a, b) => b.count - a.count || b.last_t - a.last_t);
  }

  async sessions() {
    const spans = new Map();
    for (const r of this.allRecords().records) {
      if (typeof r.f.session !== "string") continue;
      const conv = typeof r.f.conv === "string" ? r.f.conv : "";
      const key = `${r.f.session}\0${conv}`;
      const span = spans.get(key) ?? { session: r.f.session, conv, agent: null, first_t: r.t, last_t: r.t };
      if (!span.agent && typeof r.f.agent === "string") span.agent = r.f.agent;
      span.first_t = Math.min(span.first_t, r.t);
      span.last_t = Math.max(span.last_t, r.t);
      spans.set(key, span);
    }
    return [...spans.values()].sort((a, b) => b.last_t - a.last_t);
  }

  async find(id) {
    const identity = this.identity();
    return (await this.sessions())
      .filter((s) => s.session === id || (s.conv && s.conv === id))
      .map((s) => ({ ...s, install: identity.install, name: identity.name, support_id: identity.support_id }));
  }
}

/** Mirrors the Worker's signatureOf so local `top` groups the same way. */
export function localSignature(r) {
  const f = r.f;
  if (r.k === "panic" && typeof f.signature === "string" && f.signature) return `panic:${f.signature}`;
  if (r.k === "crash.report") {
    const frame = [f.signature, f.crashed_frame, f.frame, Array.isArray(f.frames) ? f.frames[0] : null].find(
      (v) => typeof v === "string" && v,
    );
    if (frame) return `crash.report:${frame}`;
  }
  return f.code !== undefined && f.code !== null ? `${r.k}:${f.code}` : r.k;
}
