// Keeps R2 inside its free tier. Workers and D1 on the Workers Free plan fail
// closed over their limits, but R2 bills anything past the free tier once it
// is enabled, and Cloudflare offers no spending cap for it. So every R2 write
// and read goes through here: the counters live in D1 (`budget`), the caps sit
// at 90% of the free tier, and configuration can only lower them. Over a cap,
// ingest answers 429, which the app treats as "keep it locally, retry later".

import { DAY_MS, type Env, retentionDays } from "./env";

/** R2 free tier: 1M Class A, 10M Class B per month, 10 GB-month stored. */
export const FREE_TIER = { puts: 1_000_000, gets: 10_000_000, bytes: 10_000_000_000 } as const;
export const DEFAULT_CAPS = { puts: 900_000, gets: 9_000_000, bytes: 9_000_000_000 } as const;

export interface Caps {
  puts: number;
  gets: number;
  bytes: number;
}

export interface Usage {
  month: string;
  puts: number;
  gets: number;
  /** Bytes written within the retention window: what R2 holds now. */
  bytes: number;
  caps: Caps;
}

/** A configured value may lower a cap, never raise it past the default. */
function cap(configured: string | undefined, fallback: number): number {
  const value = Number(configured);
  return Number.isFinite(value) && value >= 0 ? Math.min(value, fallback) : fallback;
}

export function caps(env: Env): Caps {
  return {
    puts: cap(env.BUDGET_MONTHLY_PUTS, DEFAULT_CAPS.puts),
    gets: cap(env.BUDGET_MONTHLY_GETS, DEFAULT_CAPS.gets),
    bytes: cap(env.BUDGET_STORED_BYTES, DEFAULT_CAPS.bytes),
  };
}

export function monthOf(now: number): string {
  return new Date(now).toISOString().slice(0, 7);
}

export function dayOf(now: number): string {
  return new Date(now).toISOString().slice(0, 10);
}

/** Current counters: at most two month rows plus one row per retained day. */
export async function usage(env: Env, now = Date.now()): Promise<Usage> {
  const month = monthOf(now);
  const since = dayOf(now - retentionDays(env) * DAY_MS);
  const { results } = await env.DB.prepare(
    `SELECT period, metric, value FROM budget
     WHERE (period = ?1 AND metric IN ('puts', 'gets')) OR (metric = 'bytes' AND period > ?2)`,
  )
    .bind(month, since)
    .all<{ period: string; metric: string; value: number }>();
  const out: Usage = { month, puts: 0, gets: 0, bytes: 0, caps: caps(env) };
  for (const row of results) {
    if (row.metric === "bytes") out.bytes += row.value;
    else if (row.metric === "puts") out.puts = row.value;
    else if (row.metric === "gets") out.gets = row.value;
  }
  return out;
}

/** Why a write of `bytes` must be refused, or null when it fits. */
export function refuseWrite(current: Usage, bytes: number): string | null {
  if (current.puts + 1 > current.caps.puts) return `monthly R2 write budget (${current.caps.puts}) used up`;
  if (current.bytes + bytes > current.caps.bytes) return `R2 storage budget (${current.caps.bytes} bytes) used up`;
  return null;
}

export function refuseRead(current: Usage): string | null {
  return current.gets + 1 > current.caps.gets ? `monthly R2 read budget (${current.caps.gets}) used up` : null;
}

/** Statements recording one R2 write of `bytes`, for the ingest D1 batch. */
export function recordWrite(env: Env, now: number, bytes: number): D1PreparedStatement[] {
  const add = `INSERT INTO budget (period, metric, value) VALUES (?1, ?2, ?3)
               ON CONFLICT (period, metric) DO UPDATE SET value = value + excluded.value`;
  return [
    env.DB.prepare(add).bind(monthOf(now), "puts", 1),
    env.DB.prepare(add).bind(dayOf(now), "bytes", bytes),
  ];
}

export async function recordRead(env: Env, now = Date.now()): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO budget (period, metric, value) VALUES (?1, 'gets', 1)
     ON CONFLICT (period, metric) DO UPDATE SET value = value + 1`,
  )
    .bind(monthOf(now))
    .run();
}

/** Drops counters older than any window that reads them. */
export async function pruneBudget(env: Env, now = Date.now()): Promise<void> {
  const oldestDay = dayOf(now - (retentionDays(env) + 2) * DAY_MS);
  const lastMonth = monthOf(now - 32 * DAY_MS);
  await env.DB.prepare(
    `DELETE FROM budget WHERE (metric = 'bytes' AND period < ?1) OR (metric IN ('puts', 'gets') AND period < ?2)`,
  )
    .bind(oldestDay, lastMonth)
    .run();
}
