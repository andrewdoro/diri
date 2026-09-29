export interface Env {
  DB: D1Database;
  BATCHES: R2Bucket;
  INGEST_RATE_LIMITER: RateLimit;
  GLOBAL_RATE_LIMIT_PER_HOUR?: string;
  /** Worker secret. Unset means the admin API refuses everything. */
  ADMIN_TOKEN?: string;
  RETENTION_DAYS?: string;
  RATE_LIMIT_PER_HOUR?: string;
  /** Optional lower spend caps; see src/budget.ts (they can't be raised). */
  BUDGET_MONTHLY_PUTS?: string;
  BUDGET_MONTHLY_GETS?: string;
  BUDGET_STORED_BYTES?: string;
}

export const DAY_MS = 24 * 60 * 60 * 1000;

export function retentionDays(env: Env): number {
  const days = Number(env.RETENTION_DAYS ?? 30);
  return Number.isFinite(days) && days >= 1 ? days : 30;
}

export function rateLimitPerHour(env: Env): number {
  const limit = Number(env.RATE_LIMIT_PER_HOUR ?? 120);
  return Number.isFinite(limit) && limit >= 1 ? limit : 120;
}

export function json(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

export function error(status: number, code: string, detail?: string): Response {
  return json(detail ? { error: code, detail } : { error: code }, status);
}
