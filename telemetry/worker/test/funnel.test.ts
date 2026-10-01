import { env } from "cloudflare:workers";
import { describe, expect, it } from "vitest";
import { aggregateFunnel, type CohortRow, milestoneOf } from "../src/funnel";
import { sweep } from "../src/retention";
import { adminJson, header, ingest, newInstall, rec } from "./helpers";

const T0 = 1795000000000;

function activation(t: number, step: string, f: Record<string, unknown> = {}, p = "app"): string {
  return rec(t, `activation.${step}`, "info", { preexisting: false, since_first_launch_s: 0, ...f }, p);
}

describe("funnel aggregation", () => {
  it("counts each install once per step, with conversion and medians", () => {
    const row = (install: string, step: string, since_s: number | null = null, source: string | null = null): CohortRow => ({
      install,
      step,
      preexisting: 0,
      since_s,
      source,
    });
    const funnel = aggregateFunnel([
      row("a", "first_launch", 0),
      row("b", "first_launch", 0),
      row("c", "first_launch", 0),
      row("d", "first_launch", 0),
      row("a", "agent_ready", 30, "onboarding_install"),
      row("b", "agent_ready", 10, "preexisting"),
      row("c", "agent_ready", 50, "onboarding_install"),
      row("a", "agent_ready", 99, "manual"), // a duplicate never counts twice
      row("a", "first_session", 60),
      row("b", "first_session", 20),
      row("a", "second_session", 600),
      row("a", "first_helper", 900),
      row("b", "returned", 90000),
      row("x", "not_a_step", 1),
    ]);
    expect(funnel.installs).toBe(4);
    const step = (name: string) => funnel.steps.find((s) => s.step === name)!;
    expect(funnel.steps.map((s) => s.step)).toEqual([
      "first_launch",
      "agent_ready",
      "first_session",
      "second_session",
      "first_helper",
      "returned",
    ]);
    expect(step("first_launch")).toMatchObject({ installs: 4, of_cohort: 1, of_previous: null, median_s: null });
    expect(step("agent_ready")).toMatchObject({
      installs: 3,
      of_cohort: 0.75,
      of_previous: 0.75,
      median_s: 30,
      sources: { onboarding_install: 2, preexisting: 1 },
    });
    expect(step("first_session")).toMatchObject({ installs: 2, of_cohort: 0.5, of_previous: 0.667, median_s: 40 });
    expect(step("second_session")).toMatchObject({ installs: 1, of_previous: 0.5 });
    expect(step("first_helper")).toMatchObject({ installs: 1, of_previous: 1, median_s: 900 });
    expect(step("returned")).toMatchObject({ installs: 1, of_cohort: 0.25, of_previous: null });
    expect(aggregateFunnel([]).steps[1]).toMatchObject({ installs: 0, of_cohort: 0, of_previous: 0 });
  });

  it("indexes only known steps and machine ids", () => {
    expect(milestoneOf("activation.bogus", T0, {})).toBeNull();
    expect(milestoneOf("session.spawn", T0, {})).toBeNull();
    expect(milestoneOf("activation.agent_ready", T0, { source: "a b", agent: "claude-code", since_first_launch_s: -1 })).toEqual({
      step: "agent_ready",
      t: T0,
      preexisting: false,
      since_s: null,
      source: null,
      agent: "claude-code",
    });
  });
});

describe("funnel endpoint", () => {
  it("indexes activation records at ingest and answers a cohort over a window", async () => {
    const version = `0.0.0-funnel-${crypto.randomUUID().slice(0, 8)}`;
    const send = (install: string, lines: string[]) =>
      ingest(install, lines, { header: header(install, { app_version: version, lines: lines.length }) });
    const fresh = newInstall();
    const stalled = newInstall();
    const upgraded = newInstall();
    const res = await send(fresh, [
      activation(T0, "first_launch"),
      activation(T0 + 40_000, "agent_ready", { agent: "claude-code", source: "onboarding_install", since_first_launch_s: 40 }),
      rec(T0 + 50_000, "session.spawn", "info", { session: "s_funnel", agent: "claude-code" }),
    ]);
    expect(res.status).toBe(202);
    expect(await res.json()).toMatchObject({ milestones: 2 });
    // Later batches add steps; a re-sent first_launch never moves the anchor.
    await send(fresh, [
      activation(T0 + 60_000, "first_session", { agent: "claude-code", mode: "fresh", since_first_launch_s: 60 }, "engine"),
      activation(T0 + 9_000_000, "first_launch"),
    ]);
    await send(stalled, [activation(T0 + 1_000, "first_launch")]);
    await send(upgraded, [
      activation(T0 + 2_000, "first_launch", { preexisting: true }),
      activation(T0 + 3_000, "first_session", { preexisting: true, since_first_launch_s: 1 }, "engine"),
    ]);

    const body = await adminJson(`/v1/admin/funnel?since=${T0 - 1}&until=${T0 + 10_000}&version=${version}`);
    expect(body.truncated).toBe(false);
    expect(body.new.installs).toBe(2);
    const step = (name: string) => body.new.steps.find((s: any) => s.step === name);
    expect(step("agent_ready")).toMatchObject({ installs: 1, of_cohort: 0.5, sources: { onboarding_install: 1 } });
    expect(step("first_session")).toMatchObject({ installs: 1, median_s: 60 });
    expect(step("second_session").installs).toBe(0);
    expect(body.preexisting.installs).toBe(1);
    expect(body.preexisting.steps.find((s: any) => s.step === "first_session").installs).toBe(1);

    const anchored = await env.DB.prepare("SELECT t FROM milestones WHERE install = ?1 AND step = 'first_launch'")
      .bind(fresh)
      .first<{ t: number }>();
    expect(anchored!.t).toBe(T0);

    const outside = await adminJson(`/v1/admin/funnel?since=${T0 + 20_000}&until=${T0 + 30_000}&version=${version}`);
    expect(outside.new.installs).toBe(0);
  });

  it("expires milestones with the rest of the index", async () => {
    const install = newInstall();
    await ingest(install, [activation(T0, "first_launch")]);
    await env.DB.prepare("UPDATE milestones SET received_at = 1 WHERE install = ?1").bind(install).run();
    const report = await sweep(env);
    expect(report.milestones).toBeGreaterThanOrEqual(1);
    const left = await env.DB.prepare("SELECT COUNT(*) AS n FROM milestones WHERE install = ?1")
      .bind(install)
      .first<{ n: number }>();
    expect(left!.n).toBe(0);
  });
});
