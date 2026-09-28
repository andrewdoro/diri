// `health`: per-process trends of the recorder's `health` and `metrics`
// samples, as sparklines, plus leak hints for series that only go up.

import { formatDuration, formatStamp } from "./time.mjs";

const BARS = "▁▂▃▄▅▆▇█";
export const HEALTH_SERIES = ["rss_mb", "footprint_mb", "fds", "threads", "cpu_pct"];
/** Growth that is worth a hint, per series, in its own unit. */
const LEAK_FLOOR = { rss_mb: 50, footprint_mb: 50, fds: 20, threads: 10 };

/**
 * Downsamples to `width` buckets (bucket max, so spikes survive).
 * @param {number[]} values
 * @param {number} width
 */
export function sparkline(values, width = 40) {
  const clean = values.filter((v) => Number.isFinite(v));
  if (clean.length === 0) return "";
  const buckets = bucketMax(clean, Math.min(width, clean.length));
  const lo = Math.min(...buckets);
  const hi = Math.max(...buckets);
  return buckets.map((v) => (hi === lo ? BARS[0] : BARS[Math.round(((v - lo) / (hi - lo)) * (BARS.length - 1))])).join("");
}

function bucketMax(values, n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    const start = Math.floor((i * values.length) / n);
    const end = Math.max(start + 1, Math.floor(((i + 1) * values.length) / n));
    out.push(Math.max(...values.slice(start, end)));
  }
  return out;
}

function bucketMin(values, n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    const start = Math.floor((i * values.length) / n);
    const end = Math.max(start + 1, Math.floor(((i + 1) * values.length) / n));
    out.push(Math.min(...values.slice(start, end)));
  }
  return out;
}

/**
 * A leak looks like a floor that keeps rising: the minimum of each tenth of
 * the run is at or above the previous one, and the growth is large both in
 * absolute terms and relative to the start.
 * @param {string} name
 * @param {{t:number, v:number}[]} points
 */
export function leakHint(name, points) {
  const floor = LEAK_FLOOR[name];
  if (floor === undefined || points.length < 6) return null;
  const span = points[points.length - 1].t - points[0].t;
  if (span < 30 * 60_000) return null;
  const values = points.map((p) => p.v);
  const lows = bucketMin(values, Math.min(10, values.length));
  let rising = 0;
  for (let i = 1; i < lows.length; i++) if (lows[i] >= lows[i - 1]) rising += 1;
  const growth = lows[lows.length - 1] - lows[0];
  if (growth < floor || growth < lows[0] * 0.2 || rising < (lows.length - 1) * 0.8) return null;
  const perHour = growth / (span / 3_600_000);
  return `${name} floor +${round(growth)} over ${formatDuration(span)} (+${round(perHour)}/h), rising in ${rising}/${lows.length - 1} intervals`;
}

const round = (v) => (Math.abs(v) >= 100 ? Math.round(v) : Math.round(v * 10) / 10);

/**
 * Timings from `metrics` whose name marks them as frame or RPC latency.
 * @param {string} name
 */
function timingSeries(name) {
  if (/frame|render|paint/.test(name)) return "frame";
  if (/rpc|request|method/.test(name)) return "rpc";
  return null;
}

/**
 * Groups health and metrics records by process instance (`p:pid`; a restart
 * is a new instance) and builds each series.
 * @param {import("./records.mjs").Rec[]} records
 */
export function analyze(records) {
  /** @type {Map<string, any>} */
  const procs = new Map();
  const proc = (r) => {
    const key = `${r.p}:${r.pid}`;
    let entry = procs.get(key);
    if (!entry) {
      entry = { proc: r.p, pid: r.pid, first_t: r.t, last_t: r.t, samples: 0, series: {}, timings: {}, fd_limit: null, hints: [] };
      procs.set(key, entry);
    }
    entry.first_t = Math.min(entry.first_t, r.t);
    entry.last_t = Math.max(entry.last_t, r.t);
    return entry;
  };
  const push = (entry, name, t, v) => {
    if (typeof v !== "number" || !Number.isFinite(v)) return;
    (entry.series[name] ??= []).push({ t, v });
  };
  for (const r of [...records].sort((a, b) => a.t - b.t)) {
    if (r.k === "health") {
      const entry = proc(r);
      entry.samples += 1;
      for (const name of HEALTH_SERIES) push(entry, name, r.t, r.f[name]);
      if (typeof r.f.fd_limit === "number") entry.fd_limit = r.f.fd_limit;
    } else if (r.k === "metrics" && r.f.timings && typeof r.f.timings === "object") {
      const entry = proc(r);
      for (const [name, stats] of Object.entries(r.f.timings)) {
        const kind = timingSeries(name);
        if (!kind || !stats || typeof stats.p99 !== "number") continue;
        (entry.timings[name] ??= []).push({ t: r.t, v: stats.p99, n: stats.n ?? 0 });
      }
    }
  }
  for (const entry of procs.values()) {
    for (const [name, points] of Object.entries(entry.series)) {
      const hint = leakHint(name, points);
      if (hint) entry.hints.push(hint);
    }
    const fds = entry.series.fds;
    if (entry.fd_limit && fds?.length) {
      const last = fds[fds.length - 1].v;
      if (last >= entry.fd_limit * 0.8) entry.hints.push(`fds at ${Math.round((last / entry.fd_limit) * 100)}% of the ${entry.fd_limit} limit`);
    }
  }
  return [...procs.values()].sort((a, b) => a.proc.localeCompare(b.proc) || a.first_t - b.first_t);
}

function describe(points) {
  const values = points.map((p) => p.v);
  const first = values[0];
  const last = values[values.length - 1];
  return { first, last, min: Math.min(...values), max: Math.max(...values) };
}

/**
 * @param {ReturnType<typeof analyze>} procs
 * @param {{utc?: boolean, width?: number}} opts
 */
export function renderHealth(procs, opts = {}) {
  const width = opts.width ?? 40;
  const lines = [];
  if (procs.length === 0) return "no health or metrics samples in this window";
  for (const entry of procs) {
    lines.push(
      `${entry.proc} ${entry.pid}  ${formatStamp(entry.first_t, opts)} → ${formatStamp(entry.last_t, opts)}  (${formatDuration(entry.last_t - entry.first_t)}, ${entry.samples} samples)`,
    );
    const rows = [
      ...Object.entries(entry.series).map(([name, points]) => [name, points]),
      ...Object.entries(entry.timings).map(([name, points]) => [`${name} p99`, points]),
    ];
    for (const [name, points] of rows) {
      const d = describe(points);
      lines.push(
        `  ${name.padEnd(22)} ${sparkline(points.map((p) => p.v), width).padEnd(width)}  ${round(d.first)}→${round(d.last)}  min ${round(d.min)} max ${round(d.max)}`,
      );
    }
    for (const hint of entry.hints) lines.push(`  ⚠ leak? ${hint}`);
    lines.push("");
  }
  return lines.join("\n").trimEnd();
}

/**
 * One line per process for the timeline's folded periodic samples.
 * @param {ReturnType<typeof analyze>} procs
 */
export function summarizeHealth(procs) {
  return procs.map((entry) => {
    const parts = [];
    for (const name of ["rss_mb", "footprint_mb", "fds", "threads", "cpu_pct"]) {
      const points = entry.series[name];
      if (!points?.length) continue;
      const d = describe(points);
      parts.push(name === "cpu_pct" ? `cpu max ${round(d.max)}%` : `${name.replace("_mb", "")} ${round(d.first)}→${round(d.last)}`);
    }
    for (const [name, points] of Object.entries(entry.timings)) {
      parts.push(`${name} p99 max ${round(describe(points).max)}ms`);
    }
    const hint = entry.hints.length ? `  ⚠ ${entry.hints.join("; ")}` : "";
    return `${entry.proc} ${entry.pid}: ${entry.samples} samples  ${parts.join("  ")}${hint}`;
  });
}
