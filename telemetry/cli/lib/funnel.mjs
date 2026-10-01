// The activation funnel: `diri-debug funnel` over the Worker, and
// `diri-debug local activation` over this Mac's milestone markers and spool.
// Steps and their meaning: diri/TELEMETRY.md, Activation.

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { formatDuration, formatStamp } from "./time.mjs";

export const STEPS = ["first_launch", "agent_ready", "first_session", "second_session", "first_helper", "returned"];

const pct = (ratio) => (ratio === null || ratio === undefined ? "-" : `${(ratio * 100).toFixed(1)}%`);
const secs = (s) => (s === null || s === undefined ? "-" : formatDuration(s * 1000));

/**
 * The Worker's funnel (one cohort) as a table.
 * @param {{installs: number, steps: Array<{step: string, installs: number, of_cohort: number, of_previous: number | null, median_s: number | null, sources?: Record<string, number>}>}} funnel
 * @returns {string[]}
 */
export function renderFunnel(funnel) {
  const lines = [`${"step".padEnd(16)}${"installs".padStart(9)}${"of cohort".padStart(11)}${"of previous".padStart(13)}${"median".padStart(9)}`];
  for (const s of funnel.steps) {
    const sources = s.sources && Object.keys(s.sources).length
      ? `   ${Object.entries(s.sources)
          .sort((a, b) => b[1] - a[1])
          .map(([k, v]) => `${k} ${v}`)
          .join(", ")}`
      : "";
    lines.push(
      `${s.step.padEnd(16)}${String(s.installs).padStart(9)}${pct(s.of_cohort).padStart(11)}${pct(s.of_previous).padStart(13)}${secs(s.median_s).padStart(9)}${sources}`,
    );
  }
  return lines;
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return null;
  }
}

/**
 * This Mac's milestone state: the baseline and each marker under
 * `<state>/telemetry/activation/`, next to the spool.
 * @param {string} spoolDir
 */
export function readActivation(spoolDir) {
  const dir = join(dirname(spoolDir), "activation");
  const origin = readJson(join(dir, "origin.json"));
  const reached = {};
  if (existsSync(dir)) {
    for (const name of readdirSync(dir)) {
      const step = name.replace(/\.json$/, "");
      if (!STEPS.includes(step)) continue;
      const marker = readJson(join(dir, name));
      if (marker && typeof marker.t === "number") reached[step] = marker.t;
    }
  }
  return { dir, origin, reached };
}

/**
 * Joins the markers with the `activation.*` events still in the spool (the
 * spool is capped, so an old event may be gone while its marker stays).
 * @param {ReturnType<typeof readActivation>} state
 * @param {Array<{t: number, p: string, k: string, f: Record<string, unknown>}>} records
 */
export function activationSteps(state, records) {
  const events = new Map();
  for (const r of records) {
    if (!r.k.startsWith("activation.")) continue;
    const step = r.k.slice("activation.".length);
    if (!events.has(step)) events.set(step, []);
    events.get(step).push(r);
  }
  return STEPS.map((step) => {
    const list = events.get(step) ?? [];
    return { step, reached_t: state.reached[step] ?? null, event: list[0] ?? null, events: list.length };
  });
}

/**
 * `diri-debug local activation`: what this Mac has reached, for testing the
 * funnel end to end.
 */
export function renderActivation(state, steps, opts = {}) {
  const lines = [];
  if (!state.origin) {
    lines.push(`no activation baseline at ${state.dir}`);
    lines.push("(the app has not run with activation tracking yet, or DIRI_TELEMETRY=off)");
  } else {
    lines.push(
      `baseline ${formatStamp(state.origin.created_ms, opts)}  ${state.origin.preexisting ? "preexisting install (used Diri before activation tracking; left out of the new-user funnel)" : "new install"}`,
    );
  }
  lines.push(`markers: ${state.dir}`, "");
  const start = state.reached.first_launch ?? state.origin?.created_ms ?? null;
  for (const s of steps) {
    const when = s.reached_t === null ? "not yet" : formatStamp(s.reached_t, opts);
    const after = s.reached_t !== null && start !== null && s.step !== "first_launch" ? `+${formatDuration(s.reached_t - start)}` : "";
    let note = "";
    if (s.event) {
      const { preexisting, since_first_launch_s, ...rest } = s.event.f ?? {};
      note = `${s.event.p}  ${Object.entries(rest)
        .map(([k, v]) => `${k}=${v}`)
        .join(" ")}`.trim();
      if (s.events > 1) note += `  (${s.events} events: should be 1)`;
    } else if (s.reached_t !== null) {
      note = "(event no longer in the spool)";
    }
    lines.push(`${s.reached_t === null ? " " : "✓"} ${s.step.padEnd(15)} ${when.padEnd(17)} ${after.padEnd(8)} ${note}`.trimEnd());
  }
  return lines;
}
