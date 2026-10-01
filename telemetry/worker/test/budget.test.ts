import { env } from "cloudflare:test";
import { afterEach, describe, expect, it } from "vitest";
import { DEFAULT_CAPS, FREE_TIER, caps, dayOf, monthOf, usage } from "../src/budget";
import { admin, adminJson, ingest, newInstall, rec } from "./helpers";

const T0 = 1790581979447;

async function setCounter(period: string, metric: string, value: number): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO budget (period, metric, value) VALUES (?1, ?2, ?3)
     ON CONFLICT (period, metric) DO UPDATE SET value = excluded.value`,
  )
    .bind(period, metric, value)
    .run();
}

describe("spend guard", () => {
  afterEach(async () => {
    await env.DB.prepare("DELETE FROM budget").run();
  });

  it("caps sit under the R2 free tier and config can only lower them", () => {
    for (const k of ["puts", "gets", "bytes"] as const) expect(DEFAULT_CAPS[k]).toBeLessThan(FREE_TIER[k]);
    expect(caps({ ...env, BUDGET_MONTHLY_PUTS: "5000000" }).puts).toBe(DEFAULT_CAPS.puts);
    expect(caps({ ...env, BUDGET_MONTHLY_PUTS: "10" }).puts).toBe(10);
    expect(caps({ ...env, BUDGET_STORED_BYTES: "nonsense" }).bytes).toBe(DEFAULT_CAPS.bytes);
  });

  it("counts every accepted batch's write and bytes", async () => {
    const res = await ingest(newInstall(), [rec(T0, "a.b", "info")]);
    expect(res.status).toBe(202);
    const now = await usage(env);
    expect(now.puts).toBe(1);
    expect(now.bytes).toBeGreaterThan(0);
    expect((await adminJson("/v1/admin/budget")).puts).toBe(1);
  });

  it("refuses ingest with 429 once the monthly write budget is used, storing nothing", async () => {
    await setCounter(monthOf(Date.now()), "puts", DEFAULT_CAPS.puts);
    const install = newInstall();
    const res = await ingest(install, [rec(T0, "a.b", "info")]);
    expect(res.status).toBe(429);
    expect(((await res.json()) as { error: string }).error).toBe("budget_exhausted");
    expect((await env.BATCHES.list({ prefix: `v1/${install}/` })).objects).toHaveLength(0);
    expect((await usage(env)).puts).toBe(DEFAULT_CAPS.puts);
  });

  it("refuses ingest when stored bytes would pass the storage budget", async () => {
    await setCounter(dayOf(Date.now()), "bytes", DEFAULT_CAPS.bytes - 10);
    const res = await ingest(newInstall(), [rec(T0, "a.b", "info")]);
    expect(res.status).toBe(429);
  });

  it("only counts stored bytes inside the retention window", async () => {
    await setCounter(dayOf(Date.now() - 40 * 86_400_000), "bytes", DEFAULT_CAPS.bytes);
    expect((await ingest(newInstall(), [rec(T0, "a.b", "info")])).status).toBe(202);
  });

  it("stops serving batch bodies once the monthly read budget is used", async () => {
    const res = await ingest(newInstall(), [rec(T0, "a.b", "info")]);
    const { key } = (await res.json()) as { key: string };
    expect((await admin(`/v1/admin/batch?key=${encodeURIComponent(key)}`)).status).toBe(200);
    expect((await usage(env)).gets).toBe(1);
    await setCounter(monthOf(Date.now()), "gets", DEFAULT_CAPS.gets);
    expect((await admin(`/v1/admin/batch?key=${encodeURIComponent(key)}`)).status).toBe(429);
  });
});
