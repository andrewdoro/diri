import { env } from "cloudflare:workers";
import { afterEach, expect, it } from "vitest";
import { handleIngest } from "../src/ingest";
import { sweep } from "../src/retention";
import { type Env } from "../src/env";
import { gzip, header, newInstall, rec } from "./helpers";

const now = Date.now();
async function request(time = now, ip = "192.0.2.40") {
  const install = newInstall();
  return new Request("https://telemetry.test/v1/ingest", {
    method: "POST",
    headers: { "x-diri-install": install, "cf-connecting-ip": ip, "content-encoding": "gzip" },
    body: await gzip(`${JSON.stringify(header(install))}\n${rec(time, "rpc.error", "error", { session: "s_future" })}\n`),
  });
}
function limitedEnv(sourceLimit: number, globalLimit = "10000"): Env {
  const counts = new Map<string, number>();
  return { ...env, GLOBAL_RATE_LIMIT_PER_HOUR: globalLimit, INGEST_RATE_LIMITER: {
    async limit({ key }: { key: string }) {
      const count = (counts.get(key) ?? 0) + 1;
      counts.set(key, count);
      return { success: count <= sourceLimit };
    },
  } } as Env;
}
afterEach(async () => {
  await env.DB.prepare("DELETE FROM budget").run();
});
it("rotating install ids cannot evade the source admission limit", async () => {
  const bindings = limitedEnv(2);
  expect((await handleIngest(await request(), bindings, now)).status).toBe(202);
  expect((await handleIngest(await request(), bindings, now)).status).toBe(202);
  expect((await handleIngest(await request(), bindings, now)).status).toBe(429);
});
it("global admission is atomic across different sources and install ids", async () => {
  const bindings = limitedEnv(100, "3");
  const requests = await Promise.all(Array.from({ length: 8 }, (_, i) => request(now, `192.0.2.${i}`)));
  const responses = await Promise.all(requests.map(r => handleIngest(r, bindings, now + 7 * 86_400_000)));
  expect(responses.filter(r => r.status === 202)).toHaveLength(3);
  expect(responses.filter(r => r.status === 429)).toHaveLength(5);
});
it("expires future-dated incidents and sessions by server receipt time", async () => {
  const response = await handleIngest(await request(now + 365 * 86_400_000), limitedEnv(10), now);
  expect(response.status).toBe(202);
  const { key } = await response.json() as { key: string };
  const install = key.split("/")[1];
  await sweep(env, now + 31 * 86_400_000);
  expect(await env.DB.prepare("SELECT * FROM incidents WHERE install = ?1").bind(install).first()).toBeNull();
  expect(await env.DB.prepare("SELECT * FROM sessions WHERE install = ?1").bind(install).first()).toBeNull();
  expect(await env.BATCHES.head(key)).toBeNull();
});
it("reserves the R2 budget atomically before concurrent uploads", async () => {
  const bindings = { ...limitedEnv(100), BUDGET_MONTHLY_PUTS: "1" };
  const requests = await Promise.all(Array.from({ length: 8 }, () => request()));
  const responses = await Promise.all(requests.map(r => handleIngest(r, bindings, now)));
  expect(responses.filter(r => r.status === 202)).toHaveLength(1);
  expect(responses.filter(r => r.status === 429)).toHaveLength(7);
});
it("refuses missing edge identity or admission binding before processing a body", async () => {
  const missingSource = await request();
  missingSource.headers.delete("cf-connecting-ip");
  expect((await handleIngest(missingSource, limitedEnv(10), now)).status).toBe(400);
  const bindings = { ...env, INGEST_RATE_LIMITER: undefined } as unknown as Env;
  expect((await handleIngest(await request(), bindings, now)).status).toBe(503);
});
it("reserves R2 read quota atomically", async () => {
  const { reserveRead } = await import("../src/budget");
  const bindings = { ...env, BUDGET_MONTHLY_GETS: "1" };
  const results = await Promise.all(Array.from({ length: 8 }, () => reserveRead(bindings, now)));
  expect(results.filter(Boolean)).toHaveLength(1);
});
