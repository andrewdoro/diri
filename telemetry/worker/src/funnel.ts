// The activation funnel: which installs reached which milestone.
//
// Ingest indexes every `activation.*` record into `milestones` (one row per
// install and step). `GET /v1/admin/funnel` takes the installs whose
// `first_launch` falls in a window (the cohort), splits new installs from
// ones that used Diri before activation tracking (`preexisting`), and counts
// how many of each reached every later step.

/** Funnel steps, in order. The client's `activation.<step>` event kinds. */
export const STEPS = [
  "first_launch",
  "agent_ready",
  "first_session",
  "second_session",
  "first_helper",
  "returned",
] as const;
export type Step = (typeof STEPS)[number];

const STEP_SET = new Set<string>(STEPS);

export function isStep(step: string): step is Step {
  return STEP_SET.has(step);
}

/** A milestone as ingest indexes it. */
export interface MilestoneRow {
  step: Step;
  t: number;
  preexisting: boolean;
  since_s: number | null;
  source: string | null;
  agent: string | null;
}

/** Reads the indexed fields of one `activation.*` record's `f`. */
export function milestoneOf(kind: string, t: number, f: Record<string, unknown>): MilestoneRow | null {
  const step = kind.startsWith("activation.") ? kind.slice("activation.".length) : "";
  if (!isStep(step)) return null;
  const id = (value: unknown) => (typeof value === "string" && /^[A-Za-z0-9_.:-]{1,96}$/.test(value) ? value : null);
  const since = f.since_first_launch_s;
  return {
    step,
    t,
    preexisting: f.preexisting === true,
    since_s: typeof since === "number" && Number.isSafeInteger(since) && since >= 0 ? since : null,
    source: id(f.source),
    agent: id(f.agent),
  };
}

/** One cohort row: a milestone of an install whose first launch is in the window. */
export interface CohortRow {
  install: string;
  step: string;
  preexisting: number | boolean;
  since_s: number | null;
  source: string | null;
}

export interface StepSummary {
  step: Step;
  /** Installs of the cohort that reached the step. */
  installs: number;
  /** Share of the cohort (first_launch), 0..1. */
  of_cohort: number;
  /** Share of the previous step's installs, 0..1. Null for first_launch and
   *  for returned, which is retention rather than a next step. */
  of_previous: number | null;
  /** Median seconds from first launch to the step. */
  median_s: number | null;
  /** agent_ready only: installs by source. */
  sources?: Record<string, number>;
}

export interface Funnel {
  installs: number;
  steps: StepSummary[];
}

function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : Math.round((sorted[mid - 1] + sorted[mid]) / 2);
}

function ratio(n: number, d: number): number {
  return d === 0 ? 0 : Math.round((n / d) * 1000) / 1000;
}

/**
 * Counts each step over `rows`, which hold every milestone of every cohort
 * install (first_launch included). An install counts once per step.
 */
export function aggregateFunnel(rows: CohortRow[]): Funnel {
  const reached = new Map<string, Set<string>>();
  const seconds = new Map<string, number[]>();
  const sources: Record<string, number> = {};
  for (const row of rows) {
    if (!isStep(row.step)) continue;
    let installs = reached.get(row.step);
    if (!installs) reached.set(row.step, (installs = new Set()));
    if (installs.has(row.install)) continue;
    installs.add(row.install);
    if (row.since_s !== null && row.step !== "first_launch") {
      let list = seconds.get(row.step);
      if (!list) seconds.set(row.step, (list = []));
      list.push(row.since_s);
    }
    if (row.step === "agent_ready") {
      const source = row.source ?? "unknown";
      sources[source] = (sources[source] ?? 0) + 1;
    }
  }
  const cohort = reached.get("first_launch")?.size ?? 0;
  let previous = cohort;
  const steps = STEPS.map((step, index): StepSummary => {
    const installs = reached.get(step)?.size ?? 0;
    const summary: StepSummary = {
      step,
      installs,
      of_cohort: ratio(installs, cohort),
      of_previous: index === 0 || step === "returned" ? null : ratio(installs, previous),
      median_s: median(seconds.get(step) ?? []),
    };
    if (step === "agent_ready") summary.sources = sources;
    previous = installs;
    return summary;
  });
  return { installs: cohort, steps };
}

/** Splits cohort rows into new installs and preexisting ones, then aggregates each. */
export function splitFunnel(rows: CohortRow[]): { new: Funnel; preexisting: Funnel } {
  const isOld = (row: CohortRow) => row.preexisting === true || row.preexisting === 1;
  return {
    new: aggregateFunnel(rows.filter((row) => !isOld(row))),
    preexisting: aggregateFunnel(rows.filter(isOld)),
  };
}
