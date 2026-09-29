import type { D1Migration } from "@cloudflare/vitest-pool-workers";

declare global {
  namespace Cloudflare {
    // Mirrors src/env.ts; the test pool injects TEST_MIGRATIONS.
    interface Env {
      DB: D1Database;
      BATCHES: R2Bucket;
      ADMIN_TOKEN?: string;
      RETENTION_DAYS?: string;
      RATE_LIMIT_PER_HOUR?: string;
      BUDGET_MONTHLY_PUTS?: string;
      BUDGET_MONTHLY_GETS?: string;
      BUDGET_STORED_BYTES?: string;
      TEST_MIGRATIONS: D1Migration[];
    }
  }
}

export {};
