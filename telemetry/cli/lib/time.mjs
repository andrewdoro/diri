// Time parsing and formatting. Record times are epoch milliseconds; humans
// see local time unless --utc.

const UNITS = { ms: 1, s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000, w: 604_800_000 };

/**
 * "20m", "1h30m", "7d" → milliseconds, or null when it is not a duration.
 * @param {string} text
 * @returns {number | null}
 */
export function parseDuration(text) {
  const s = String(text).trim();
  if (!/^(\d+(\.\d+)?(ms|s|m|h|d|w))+$/.test(s)) return null;
  let total = 0;
  for (const [, n, , unit] of s.matchAll(/(\d+(\.\d+)?)(ms|s|m|h|d|w)/g)) total += Number(n) * UNITS[unit];
  return total;
}

/**
 * A point in time: "now", a duration meaning that long ago ("2h"), epoch ms,
 * "HH:MM[:SS]" (its latest occurrence, local), "YYYY-MM-DD[ HH:MM[:SS]]" (local), or ISO 8601
 * with a zone.
 * @param {string} text
 * @param {number} now
 * @param {{utc?: boolean}} [opts]
 * @returns {number}
 */
export function parseTime(text, now, opts = {}) {
  const s = String(text).trim();
  if (s === "now") return now;
  const ago = parseDuration(s);
  if (ago !== null) return now - ago;
  if (/^\d{12,}$/.test(s)) return Number(s);
  const clock = /^(\d{1,2}):(\d{2})(?::(\d{2}))?$/.exec(s);
  if (clock) {
    const d = new Date(now);
    if (opts.utc) d.setUTCHours(Number(clock[1]), Number(clock[2]), Number(clock[3] ?? 0), 0);
    else d.setHours(Number(clock[1]), Number(clock[2]), Number(clock[3] ?? 0), 0);
    // The most recent such time: "23:40" asked just after midnight means last night.
    return d.getTime() > now ? d.getTime() - UNITS.d : d.getTime();
  }
  const local = /^(\d{4})-(\d{2})-(\d{2})(?:[ T](\d{1,2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?)?$/.exec(s);
  if (local) {
    const [, y, mo, da, h = "0", mi = "0", se = "0", ms = "0"] = local;
    const parts = [Number(y), Number(mo) - 1, Number(da), Number(h), Number(mi), Number(se), Number(ms.padEnd(3, "0"))];
    return opts.utc ? Date.UTC(...parts) : new Date(...parts).getTime();
  }
  const parsed = Date.parse(s);
  if (Number.isFinite(parsed)) return parsed;
  throw new Error(`cannot read time "${s}" (try "2026-09-27 23:40", "23:40", "2h" or epoch ms)`);
}

const pad = (n, w = 2) => String(n).padStart(w, "0");

/**
 * "09-27 23:40:12.345" in local time (or UTC).
 * @param {number} t
 * @param {{utc?: boolean, date?: boolean}} [opts]
 */
export function formatTime(t, opts = {}) {
  const d = new Date(t);
  const u = opts.utc;
  const date = `${pad((u ? d.getUTCMonth() : d.getMonth()) + 1)}-${pad(u ? d.getUTCDate() : d.getDate())}`;
  const clock = `${pad(u ? d.getUTCHours() : d.getHours())}:${pad(u ? d.getUTCMinutes() : d.getMinutes())}:${pad(u ? d.getUTCSeconds() : d.getSeconds())}.${pad(u ? d.getUTCMilliseconds() : d.getMilliseconds(), 3)}`;
  return opts.date === false ? clock : `${date} ${clock}`;
}

/** "2026-09-27 23:40" local, for headers and lists. */
export function formatStamp(t, opts = {}) {
  if (t === null || t === undefined) return "-";
  const d = new Date(t);
  const u = opts.utc;
  const y = u ? d.getUTCFullYear() : d.getFullYear();
  // formatTime starts "MM-DD HH:MM".
  return `${y}-${formatTime(t, opts).slice(0, 11)}${u ? "Z" : ""}`;
}

/** 3725000 → "1h2m". */
export function formatDuration(ms) {
  const abs = Math.abs(ms);
  if (abs < 1000) return `${Math.round(ms)}ms`;
  if (abs < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.round(abs / 60_000);
  const sign = ms < 0 ? "-" : "";
  if (m < 60) return `${sign}${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${sign}${h}h${m % 60 ? `${m % 60}m` : ""}`;
  return `${sign}${Math.floor(h / 24)}d${h % 24 ? `${h % 24}h` : ""}`;
}

/** "3h ago" relative to now. */
export function ago(t, now) {
  if (t === null || t === undefined) return "-";
  return `${formatDuration(now - t)} ago`;
}

/**
 * Resolves --around/--window and --since/--until into [since, until].
 * @param {Record<string, any>} flags
 * @param {number} now
 * @param {string} defaultSince duration used when nothing is given
 */
export function resolveWindow(flags, now, defaultSince) {
  const opts = { utc: Boolean(flags.utc) };
  if (flags.around) {
    const center = parseTime(flags.around, now, opts);
    const width = parseDuration(flags.window ?? "20m");
    if (width === null) throw new Error(`--window "${flags.window}" is not a duration (e.g. 20m)`);
    return { since: center - width / 2, until: center + width / 2 };
  }
  const since = parseTime(flags.since ?? defaultSince, now, opts);
  const until = flags.until ? parseTime(flags.until, now, opts) : now;
  if (since > until) throw new Error("--since is after --until");
  return { since, until };
}
