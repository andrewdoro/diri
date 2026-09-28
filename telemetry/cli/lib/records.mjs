// Records as the recorder writes them (diri/TELEMETRY.md, "Record format"):
// {"t","seq","p","pid","k","s","f"}. Parsing, filtering, ordering and the
// one-line human rendering shared by every view.

import { formatTime } from "./time.mjs";

/** @typedef {{t:number, seq:number, p:string, pid:number, k:string, s:string, f:Record<string, any>}} Rec */

export const SEVERITIES = ["debug", "info", "warn", "error", "incident"];
const RANK = Object.fromEntries(SEVERITIES.map((s, i) => [s, i]));

/**
 * NDJSON text → records. Skips a batch header line and anything malformed.
 * @param {string} text
 * @returns {{records: Rec[], header: any, bad: number}}
 */
export function parseNdjson(text) {
  const records = [];
  let header = null;
  let bad = 0;
  for (const line of text.split("\n")) {
    if (!line) continue;
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      bad += 1;
      continue;
    }
    if (value && value.type === "batch" && value.v !== undefined) {
      header = value;
      continue;
    }
    if (!value || typeof value.t !== "number" || typeof value.k !== "string") {
      bad += 1;
      continue;
    }
    if (!value.f || typeof value.f !== "object") value.f = {};
    records.push(value);
  }
  return { records, header, bad };
}

/** `session.*`, `*.failed`, comma-separated alternatives. */
export function globMatcher(pattern) {
  if (!pattern) return () => true;
  const regexes = String(pattern)
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => new RegExp(`^${p.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`));
  return (kind) => regexes.some((r) => r.test(kind));
}

/** "error" (exact), "warn,error" (list) or "warn+" (at least warn). */
export function severityMatcher(spec) {
  if (!spec) return () => true;
  const s = String(spec);
  if (s.endsWith("+")) {
    const min = RANK[s.slice(0, -1)];
    if (min === undefined) throw new Error(`unknown severity "${s}"`);
    return (sev) => (RANK[sev] ?? 0) >= min;
  }
  const set = new Set(s.split(","));
  for (const sev of set) if (RANK[sev] === undefined) throw new Error(`unknown severity "${sev}"`);
  return (sev) => set.has(sev);
}

/**
 * @param {Rec[]} records
 * @param {{since?:number, until?:number, session?:string, conv?:string, kind?:string, sev?:string, proc?:string}} opts
 */
export function filterRecords(records, opts) {
  const kind = globMatcher(opts.kind);
  const sev = severityMatcher(opts.sev);
  const procs = opts.proc ? new Set(String(opts.proc).split(",")) : null;
  return records.filter(
    (r) =>
      (opts.since === undefined || r.t >= opts.since) &&
      (opts.until === undefined || r.t <= opts.until) &&
      (!opts.session || r.f.session === opts.session) &&
      (!opts.conv || r.f.conv === opts.conv) &&
      (!procs || procs.has(r.p) || procs.has(`${r.p}:${r.pid}`)) &&
      kind(r.k) &&
      sev(r.s),
  );
}

/**
 * Merges records from many batches and processes into one order: time, then
 * each process's own sequence. Exact duplicates (a batch re-sent after a
 * lost response) are dropped.
 * @param {Rec[]} records
 */
export function mergeSort(records) {
  const seen = new Set();
  const out = [];
  for (const r of records) {
    const key = `${r.p}:${r.pid}:${r.seq}:${r.t}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(r);
  }
  return out.sort((a, b) => a.t - b.t || a.p.localeCompare(b.p) || a.pid - b.pid || a.seq - b.seq);
}

export const isIncident = (r) => r.s === "error" || r.s === "incident";
/** Periodic samples that the timeline folds into a summary. */
export const isPeriodic = (r) => r.k === "health" || r.k === "metrics";

const MARK = { debug: "·", info: " ", warn: "W", error: "E", incident: "!" };

/** Compact `k=v` rendering; strings with spaces are quoted, long values cut. */
export function formatFields(f, opts = {}) {
  const max = opts.full ? Infinity : 120;
  const skip = new Set(opts.skip ?? []);
  const parts = [];
  for (const [key, value] of Object.entries(f ?? {})) {
    if (skip.has(key) || value === null || value === undefined) continue;
    let v;
    if (typeof value === "string") v = /^[^\s"=]+$/.test(value) ? value : JSON.stringify(value);
    else if (typeof value === "number") v = Number.isInteger(value) ? String(value) : String(Math.round(value * 100) / 100);
    else v = JSON.stringify(value);
    if (v.length > max) v = `${v.slice(0, max)}…`;
    parts.push(`${key}=${v}`);
  }
  return parts.join(" ");
}

const COLORS = { reset: "\x1b[0m", red: "\x1b[31m", boldRed: "\x1b[1;31m", yellow: "\x1b[33m", dim: "\x1b[2m" };

/**
 * One scannable line: `09-27 23:40:12.345 engine  812 ! session.exit  session=… code=…`
 * @param {Rec} r
 * @param {{utc?: boolean, color?: boolean, full?: boolean, date?: boolean}} opts
 */
export function formatRecord(r, opts = {}) {
  const proc = `${String(r.p).padEnd(6)} ${String(r.pid ?? "").padStart(6)}`;
  const line = `${formatTime(r.t, opts)} ${proc} ${MARK[r.s] ?? "?"} ${r.k.padEnd(22)} ${formatFields(r.f, opts)}`.trimEnd();
  if (!opts.color) return line;
  if (r.s === "incident") return `${COLORS.boldRed}${line}${COLORS.reset}`;
  if (r.s === "error") return `${COLORS.red}${line}${COLORS.reset}`;
  if (r.s === "warn") return `${COLORS.yellow}${line}${COLORS.reset}`;
  if (r.s === "debug") return `${COLORS.dim}${line}${COLORS.reset}`;
  return line;
}

export const LEGEND = "severity: · debug, blank info, W warn, E error, ! incident";
