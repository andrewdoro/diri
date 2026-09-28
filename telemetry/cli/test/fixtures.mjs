// Deterministic fixture: julia's evening. A Claude tab resumed a
// conversation Claude never wrote and dropped to zsh; the Engine's memory
// crept up all evening. Written as a spool (for `local`) and served as
// gzip batches by a stub admin API (for the remote commands).

import { createServer } from "node:http";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";

export const INSTALL = "0a40e747-fa0c-4e9a-b755-c195ab079cda";
export const SUPPORT_ID = "D-190EEHZT";
export const TOKEN = "fixture-token";
export const CONV = "0a40e747-fa0c-4e9a-b755-c195ab079cda";
export const OTHER_CONV = "5b1d2e3f-0000-4000-8000-000000000001";
export const SESSION = "s_26bf32debd4c";
/** 2026-09-27 23:40:00 UTC; tests run with TZ=UTC. */
export const T0 = Date.UTC(2026, 8, 27, 23, 40);
export const NOW = T0 + 30 * 60_000;
const MIN = 60_000;

const seqs = new Map();
function rec(t, p, pid, k, s, f = {}) {
  const key = `${p}:${pid}`;
  const seq = (seqs.get(key) ?? 0) + 1;
  seqs.set(key, seq);
  return { t, seq, p, pid, k, s, f };
}

export function buildRecords() {
  seqs.clear();
  const out = [];
  const start = T0 - 6 * 60 * MIN;
  out.push(rec(start, "engine", 812, "process.start", "info", { version: "0.9.0", os: "macos", arch: "aarch64", debug_build: false }));
  out.push(rec(start + 500, "app", 900, "process.start", "info", { version: "0.9.0", os: "macos", arch: "aarch64", debug_build: false }));
  for (let i = 0; i <= 370; i += 1) {
    const t = start + i * MIN + 1000;
    // Engine: a floor that keeps rising (leak). App: flat with noise.
    out.push(rec(t, "engine", 812, "health", "info", { uptime_s: i * 60, rss_mb: 200 + i * 0.6 + (i % 7), footprint_mb: 180 + i * 0.5, cpu_pct: 1.5, threads: 30, fds: 40 + (i % 3), fd_limit: 10240, sessions_live: 3 }));
    out.push(rec(t + 200, "app", 900, "health", "info", { uptime_s: i * 60, rss_mb: 300 + (i % 11), footprint_mb: 280, cpu_pct: 4 + (i % 5), threads: 50, fds: 60, fd_limit: 10240 }));
    out.push(rec(t + 400, "app", 900, "metrics", "info", { window_s: 60, counters: { frames: 3600 }, timings: { frame_ms: { n: 3600, avg: 6, p50: 5, p90: 9, p99: 12 + (i === 365 ? 180 : 0), max: 40 }, "rpc.session_attach": { n: 2, avg: 3, p50: 3, p90: 4, p99: 5, max: 5 } } }));
  }
  out.push(rec(T0 - 4 * 60 * MIN, "engine", 812, "session.spawn", "info", { session: "s_older", agent: "codex", mode: "new", conv: OTHER_CONV }));
  out.push(rec(T0 - 3 * MIN, "engine", 812, "rpc.slow", "warn", { method: "session.list", ms: 850 }));
  out.push(rec(T0 - 2 * MIN, "engine", 812, "session.spawn", "info", { session: SESSION, agent: "claude-code", mode: "resume", conv: CONV }));
  out.push(rec(T0 - 2 * MIN + 400, "holder", 4242, "holder.spawn", "info", { session: SESSION, pid: 4243 }));
  out.push(rec(T0 - 2 * MIN + 2500, "holder", 4242, "session.exit", "incident", { session: SESSION, code: "exit_1", exit: 1, ms: 2100 }));
  out.push(rec(T0 - 2 * MIN + 2600, "engine", 812, "session.resume_failed", "incident", { session: SESSION, agent: "claude-code", conv: CONV, code: "no_conversation" }));
  out.push(rec(T0 - MIN + 10_000, "app", 900, "terminal.mouse_mode_stuck", "warn", { session: SESSION, mode: 1003, window: "w1" }));
  out.push(rec(T0 + 5 * MIN, "app", 900, "panic", "incident", { message: "index out of bounds", location: "crates/diri-term/src/grid.rs:88:9", thread: "main", signature: "diri_term::grid::Grid::resize", frames: ["diri_term::grid::Grid::resize", "diri_app::root::render"] }));
  return out.sort((a, b) => a.t - b.t);
}

const line = (r) => JSON.stringify(r);

/** Writes the fixture as a spool: per-process files, some sealed, some open, plus one junk line. */
export function writeSpool() {
  const state = mkdtempSync(join(tmpdir(), "diri-debug-"));
  const telemetry = join(state, "telemetry");
  const dir = join(telemetry, "spool");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(telemetry, "install.json"), JSON.stringify({ install_id: INSTALL, created_ms: T0 - 86_400_000 }));
  writeFileSync(join(telemetry, "config.json"), JSON.stringify({ upload: true, name: "julia" }));
  writeFileSync(join(dir, "offsets.json"), "{}");
  const records = buildRecords();
  const byProc = new Map();
  for (const r of records) {
    const key = `${r.p}-${r.pid}`;
    if (!byProc.has(key)) byProc.set(key, []);
    byProc.get(key).push(r);
  }
  for (const [key, list] of byProc) {
    const half = Math.floor(list.length / 2);
    const start = list[0].t;
    writeFileSync(join(dir, `${key}-${start}-0000.jsonl`), `${list.slice(0, half).map(line).join("\n")}\n`);
    // The open file ends mid-write, like a live process's.
    writeFileSync(join(dir, `${key}-${start}-0001.open`), `${list.slice(half).map(line).join("\n")}\n{"t":17905`);
  }
  return dir;
}

/** Records split into batches as the uploader would (several per window, processes interleaved). */
function buildBatches() {
  const records = buildRecords();
  const batches = [];
  const size = 400;
  for (let i = 0; i < records.length; i += size) {
    const chunk = records.slice(i, i + size);
    const sentAt = chunk[chunk.length - 1].t + 1000;
    const header = { v: 1, type: "batch", install: INSTALL, support_id: SUPPORT_ID, name: "julia", app_version: "0.9.0", sent_at: sentAt, lines: chunk.length };
    // Re-sending the same batch must not duplicate records in a timeline.
    const body = [header, ...chunk].map(line).join("\n");
    batches.push({
      r2_key: `v1/${INSTALL}/2026-09-27/${sentAt}-${String(i).padStart(8, "0")}.ndjson.gz`,
      t_min: chunk[0].t,
      t_max: chunk[chunk.length - 1].t,
      received_at: sentAt,
      body: gzipSync(`${body}\n`),
      records: chunk,
    });
  }
  // A duplicate delivery of the last batch.
  const last = batches[batches.length - 1];
  batches.push({ ...last, r2_key: last.r2_key.replace(/-(\d+)\.ndjson/, "-9$1.ndjson") });
  return batches;
}

const INSTALL_ROW = {
  install: INSTALL,
  support_id: SUPPORT_ID,
  name: "julia",
  app_version: "0.9.0",
  os: "macos",
  os_version: "27.0",
  arch: "aarch64",
  first_seen: T0 - 86_400_000,
  last_seen: NOW - 60_000,
};

/**
 * A stub of the Worker's admin API over the fixture. Records every request
 * so tests can assert what the CLI asked for.
 */
export async function startStub() {
  const batches = buildBatches();
  const records = buildRecords();
  const incidents = records
    .filter((r) => r.s === "error" || r.s === "incident")
    .map((r, i) => ({
      id: i + 1,
      install: INSTALL,
      name: "julia",
      support_id: SUPPORT_ID,
      t: r.t,
      seq: r.seq,
      proc: r.p,
      pid: r.pid,
      kind: r.k,
      sev: r.s,
      session: r.f.session ?? null,
      conv: r.f.conv ?? null,
      agent: r.f.agent ?? null,
      code: r.f.code ?? null,
      signature: r.k === "panic" ? `panic:${r.f.signature}` : r.f.code ? `${r.k}:${r.f.code}` : r.k,
      app_version: "0.9.0",
      fields: r.f,
    }))
    .reverse();
  const sessions = [
    { session: SESSION, conv: CONV, agent: "claude-code", first_t: T0 - 2 * MIN, last_t: T0 - 2 * MIN + 2600 },
    { session: SESSION, conv: "", agent: null, first_t: T0 - 2 * MIN + 400, last_t: T0 - MIN + 10_000 },
    { session: "s_older", conv: OTHER_CONV, agent: "codex", first_t: T0 - 4 * 60 * MIN, last_t: T0 - 4 * 60 * MIN },
  ];
  const requests = [];
  const server = createServer((req, res) => {
    const url = new URL(req.url, "http://stub");
    requests.push(`${url.pathname}${url.search}`);
    const send = (status, body) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return send(401, { error: "unauthorized" });
    const q = url.searchParams;
    const num = (k, d) => (q.has(k) ? Number(q.get(k)) : d);
    switch (url.pathname) {
      case "/v1/admin/installs": {
        const term = (q.get("q") ?? "").toLowerCase();
        const hit = !term || "julia".includes(term) || SUPPORT_ID.toLowerCase().startsWith(term) || INSTALL.startsWith(term);
        return send(200, { installs: hit ? [INSTALL_ROW] : [] });
      }
      case `/v1/admin/installs/${INSTALL}`:
        return send(200, { install: INSTALL_ROW });
      case "/v1/admin/incidents": {
        const rows = incidents.filter(
          (r) =>
            r.t >= num("since", 0) &&
            r.t <= num("until", Infinity) &&
            (!q.get("sev") || r.sev === q.get("sev")) &&
            (!q.get("session") || r.session === q.get("session")),
        );
        return send(200, { incidents: rows.slice(0, num("limit", 100)) });
      }
      case "/v1/admin/incidents/summary": {
        const groups = new Map();
        for (const r of incidents.filter((r) => r.t >= num("since", 0))) {
          const g = groups.get(r.signature) ?? { signature: r.signature, kind: r.kind, sev: r.sev, count: 0, installs: 1, first_t: r.t, last_t: r.t, versions: ["0.9.0"] };
          g.count += 1;
          g.first_t = Math.min(g.first_t, r.t);
          g.last_t = Math.max(g.last_t, r.t);
          groups.set(r.signature, g);
        }
        return send(200, { groups: [...groups.values()].sort((a, b) => b.count - a.count) });
      }
      case "/v1/admin/batches": {
        const since = num("since", 0);
        const until = num("until", Infinity);
        const rows = batches
          .filter((b) => b.t_max >= since && b.t_min <= until)
          .map(({ r2_key, t_min, t_max, received_at }) => ({ r2_key, t_min, t_max, received_at }));
        return send(200, { batches: rows });
      }
      case "/v1/admin/batch": {
        const batch = batches.find((b) => b.r2_key === q.get("key"));
        if (!batch) return send(404, { error: "not_found" });
        res.writeHead(200, { "content-type": "application/gzip" });
        return res.end(batch.body);
      }
      case "/v1/admin/sessions":
        return send(200, { sessions });
      case "/v1/admin/find": {
        const id = q.get("id");
        const matches = sessions
          .filter((s) => s.session === id || (s.conv && s.conv === id))
          .map((s) => ({ ...s, install: INSTALL, support_id: SUPPORT_ID, name: "julia", app_version: "0.9.0" }));
        return send(200, { matches });
      }
    }
    return send(404, { error: "not_found" });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return { url: `http://127.0.0.1:${port}`, requests, close: () => new Promise((r) => server.close(r)), batches };
}
