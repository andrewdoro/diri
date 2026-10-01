import { createExecutionContext, createScheduledController, SELF, waitOnExecutionContext } from "cloudflare:test";
import { env } from "cloudflare:workers";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { sweep } from "../src/retention";
import { admin, adminJson, BASE, header, ingest, newInstall, rec, supportId } from "./helpers";

const T0 = 1790581979000;
const CONV = "0a40e747-fa0c-4e9a-b755-c195ab079cda";

describe("admin auth", () => {
  it("rejects a missing, wrong or non-bearer token", async () => {
    expect((await SELF.fetch(`${BASE}/v1/admin/installs`)).status).toBe(401);
    expect((await admin("/v1/admin/installs", "nope")).status).toBe(401);
    const basic = await SELF.fetch(`${BASE}/v1/admin/installs`, {
      headers: { authorization: "Basic dGVzdC1hZG1pbi10b2tlbi0wMTIzNDU2Nzg5" },
    });
    expect(basic.status).toBe(401);
    expect((await admin("/v1/admin/installs")).status).toBe(200);
  });

  it("fails closed when ADMIN_TOKEN is unset", async () => {
    const res = await worker.fetch(
      new Request(`${BASE}/v1/admin/installs`, { headers: { authorization: "Bearer " } }),
      { ...env, ADMIN_TOKEN: undefined },
    );
    expect(res.status).toBe(401);
  });
});

describe("admin queries", () => {
  it("finds installs by name, support id and uuid prefix", async () => {
    const install = newInstall();
    await ingest(install, [rec(T0, "a.b", "info")], { header: header(install, { name: "Zelda Q" }) });
    const byName = await adminJson("/v1/admin/installs?q=zelda");
    expect(byName.installs.map((i: any) => i.install)).toContain(install);
    const bySupport = await adminJson(`/v1/admin/installs?q=${supportId(install).toLowerCase()}`);
    expect(bySupport.installs.map((i: any) => i.install)).toContain(install);
    const byPrefix = await adminJson(`/v1/admin/installs?q=${install.slice(0, 8)}`);
    expect(byPrefix.installs[0].install).toBe(install);
    const detail = await adminJson(`/v1/admin/installs/${install}`);
    expect(detail.install.name).toBe("Zelda Q");
    expect(detail.batches.n).toBe(1);
    expect((await admin(`/v1/admin/installs/${newInstall()}`)).status).toBe(404);
  });

  it("lists and groups incidents across installs", async () => {
    const a = newInstall();
    const b = newInstall();
    const panic = (t: number) =>
      rec(t, "panic", "incident", { signature: "diri_term::grid::resize_uniq", frames: ["x"], message: "oob" }, "app");
    await ingest(a, [panic(T0), panic(T0 + 1), rec(T0 + 2, "rpc.failed", "error", { code: "timeout_uniq" })]);
    await ingest(b, [panic(T0 + 100)], { header: header(b, { app_version: "0.9.1" }) });

    const listed = await adminJson(`/v1/admin/incidents?install=${a}`);
    expect(listed.incidents).toHaveLength(3);
    expect(listed.incidents[0].t).toBe(T0 + 2);
    expect(listed.incidents[1].fields.message).toBe("oob");
    expect(listed.incidents[0]).toMatchObject({ name: "alex", support_id: supportId(a) });

    const onlyPanics = await adminJson(`/v1/admin/incidents?install=${a}&kind=pan*&sev=incident`);
    expect(onlyPanics.incidents).toHaveLength(2);

    const summary = await adminJson(`/v1/admin/incidents/summary?since=${T0}&until=${T0 + 1000}`);
    const group = summary.groups.find((g: any) => g.signature === "panic:diri_term::grid::resize_uniq");
    expect(group).toMatchObject({ count: 3, installs: 2, first_t: T0, last_t: T0 + 100, kind: "panic" });
    expect(group.versions.sort()).toEqual(["0.9.0", "0.9.1"]);

    const v091 = await adminJson(`/v1/admin/incidents/summary?version=0.9.1&kind=panic`);
    expect(v091.groups.find((g: any) => g.signature === group.signature).count).toBe(1);
  });

  it("finds the install that owns a conversation or a session", async () => {
    const install = newInstall();
    await ingest(install, [
      rec(T0, "session.spawn", "info", { session: "s_find_me", agent: "claude-code", conv: CONV, mode: "resume" }),
      rec(T0 + 50, "session.exit", "incident", { session: "s_find_me", code: "no_conversation" }),
    ]);
    const byConv = await adminJson(`/v1/admin/find?id=${CONV}`);
    const hit = byConv.matches.find((m: any) => m.install === install);
    expect(hit).toMatchObject({ session: "s_find_me", conv: CONV, agent: "claude-code", name: "alex" });
    const bySession = await adminJson("/v1/admin/find?id=s_find_me");
    expect(bySession.matches.filter((m: any) => m.install === install)).toHaveLength(2);
    const sessions = await adminJson(`/v1/admin/sessions?install=${install}`);
    expect(sessions.sessions).toHaveLength(2);
    expect((await admin("/v1/admin/find?id=has%20space")).status).toBe(400);
  });

  it("lists batches overlapping a window and streams a batch body", async () => {
    const install = newInstall();
    await ingest(install, [rec(T0, "a.one", "info"), rec(T0 + 60_000, "a.two", "info")]);
    await ingest(install, [rec(T0 + 3_600_000, "a.three", "info")]);
    const window = await adminJson(`/v1/admin/batches?install=${install}&since=${T0 + 30_000}&until=${T0 + 90_000}`);
    expect(window.batches).toHaveLength(1);
    const key = window.batches[0].r2_key;
    const res = await admin(`/v1/admin/batch?key=${encodeURIComponent(key)}`);
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("application/gzip");
    const text = await new Response(res.body!.pipeThrough(new DecompressionStream("gzip"))).text();
    expect(text).toContain('"k":"a.two"');
    expect((await admin("/v1/admin/batch?key=../etc/passwd")).status).toBe(400);
  });
});

describe("retention", () => {
  it("deletes D1 rows and R2 objects older than 30 days", async () => {
    const old = newInstall();
    const fresh = newInstall();
    await ingest(old, [rec(T0, "x.old", "error", { session: "s_old", code: "c" })]);
    await ingest(fresh, [rec(Date.now(), "x.new", "error", { session: "s_new", code: "c" })]);
    const { key } = (await env.DB.prepare("SELECT r2_key AS key FROM batches WHERE install = ?1")
      .bind(old)
      .first<{ key: string }>())!;
    // Age the old install's rows past the window.
    const ancient = Date.now() - 31 * 24 * 3600 * 1000;
    await env.DB.batch([
      env.DB.prepare("UPDATE batches SET received_at = ?2 WHERE install = ?1").bind(old, ancient),
      env.DB.prepare("UPDATE installs SET last_seen = ?2 WHERE install = ?1").bind(old, ancient),
      env.DB.prepare("UPDATE incidents SET received_at = ?2 WHERE install = ?1").bind(old, ancient),
      env.DB.prepare("UPDATE sessions SET received_at = ?2 WHERE install = ?1").bind(old, ancient),
    ]);

    const report = await sweep(env);
    expect(report.objects).toBeGreaterThanOrEqual(1);
    expect(await env.BATCHES.head(key)).toBeNull();
    const oldLeft = await env.DB.prepare(
      "SELECT (SELECT COUNT(*) FROM batches WHERE install = ?1) + (SELECT COUNT(*) FROM incidents WHERE install = ?1) + (SELECT COUNT(*) FROM sessions WHERE install = ?1) + (SELECT COUNT(*) FROM installs WHERE install = ?1) AS n",
    )
      .bind(old)
      .first<{ n: number }>();
    expect(oldLeft!.n).toBe(0);
    const freshLeft = await env.DB.prepare("SELECT COUNT(*) AS n FROM incidents WHERE install = ?1")
      .bind(fresh)
      .first<{ n: number }>();
    expect(freshLeft!.n).toBe(1);
  });

  it("runs from the cron trigger", async () => {
    const ctx = createExecutionContext();
    await worker.scheduled(createScheduledController({ scheduledTime: Date.now(), cron: "17 3 * * *" }), env, ctx);
    await waitOnExecutionContext(ctx);
  });
});
