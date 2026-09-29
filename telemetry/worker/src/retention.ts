// Daily retention sweep. The R2 lifecycle rule (README) is the primary
// mechanism for objects and also catches any object whose D1 row was never
// written; this sweep deletes the D1 index rows and the objects they point
// at, so the index never names a batch that is gone. R2 deletes are free
// operations; listing the bucket is not, so we walk D1 instead.

import { pruneBudget } from "./budget";
import { DAY_MS, type Env, retentionDays } from "./env";

/** R2 accepts up to 1000 keys per delete call. */
const CHUNK = 1000;
/** Bounds one run; anything left is picked up tomorrow. */
const MAX_CHUNKS = 50;

export interface SweepReport {
  objects: number;
  batches: number;
  incidents: number;
  sessions: number;
}

export async function sweep(env: Env, now = Date.now()): Promise<SweepReport> {
  const cutoff = now - retentionDays(env) * DAY_MS;
  const report: SweepReport = { objects: 0, batches: 0, incidents: 0, sessions: 0 };
  for (let i = 0; i < MAX_CHUNKS; i++) {
    const { results } = await env.DB.prepare(
      "SELECT id, r2_key FROM batches WHERE received_at < ?1 ORDER BY id LIMIT ?2",
    )
      .bind(cutoff, CHUNK)
      .all<{ id: number; r2_key: string }>();
    if (results.length === 0) break;
    await env.BATCHES.delete(results.map((r) => r.r2_key));
    const maxId = results[results.length - 1].id;
    const deleted = await env.DB.prepare("DELETE FROM batches WHERE received_at < ?1 AND id <= ?2")
      .bind(cutoff, maxId)
      .run();
    report.objects += results.length;
    report.batches += deleted.meta.changes;
    if (results.length < CHUNK) break;
  }
  const [incidents, sessions] = await env.DB.batch([
    env.DB.prepare("DELETE FROM incidents WHERE t < ?1").bind(cutoff),
    env.DB.prepare("DELETE FROM sessions WHERE last_t < ?1").bind(cutoff),
  ]);
  report.incidents = incidents.meta.changes;
  report.sessions = sessions.meta.changes;
  // Installs stay while they have any data; an install silent for the whole
  // retention window has nothing left to investigate.
  await env.DB.prepare("DELETE FROM installs WHERE last_seen < ?1").bind(cutoff).run();
  await pruneBudget(env, now);
  return report;
}
