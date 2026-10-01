import { env } from "cloudflare:workers";
import { describe, expect, it } from "vitest";
import { capFields, MAX_LINES, scanGzip, signatureOf } from "../src/ingest";
import { gzip, header, ingest, newInstall, rec } from "./helpers";

const T0 = 1790581979000;

describe("ingest", () => {
  it("stores the original gzip in R2 and indexes install, batch, incidents and sessions", async () => {
    const install = newInstall();
    const lines = [
      rec(T0, "process.start", "info", { version: "0.9.0" }, "app"),
      rec(T0 + 10, "session.spawn", "info", {
        session: "s_26bf32debd4c",
        agent: "claude-code",
        mode: "resume",
        conv: "0a40e747-fa0c-4e9a-b755-c195ab079cda",
      }),
      rec(T0 + 20, "session.exit", "incident", { session: "s_26bf32debd4c", code: "no_conversation", exit: 1 }),
      rec(T0 + 30, "health", "info", { rss_mb: 120 }, "holder"),
    ];
    const res = await ingest(install, lines);
    expect(res.status).toBe(202);
    const body = (await res.json()) as { key: string; accepted: number; incidents: number; sessions: number };
    expect(body).toMatchObject({ accepted: 4, incidents: 1, sessions: 2 });
    expect(body.key).toMatch(new RegExp(`^v1/${install}/2026-09-28/1790581979447-[0-9a-f]{8}\\.ndjson\\.gz$`));

    const object = await env.BATCHES.get(body.key);
    expect(object).not.toBeNull();
    expect(object!.customMetadata).toMatchObject({ install, lines: "4", app_version: "0.9.0" });
    const stored = await new Response(object!.body.pipeThrough(new DecompressionStream("gzip"))).text();
    expect(stored.split("\n")[2]).toBe(lines[1]);

    const installRow = await env.DB.prepare("SELECT * FROM installs WHERE install = ?1").bind(install).first();
    expect(installRow).toMatchObject({ name: "alex", app_version: "0.9.0", os: "macos", arch: "aarch64" });

    const batch = await env.DB.prepare("SELECT * FROM batches WHERE install = ?1").bind(install).first();
    expect(batch).toMatchObject({ t_min: T0, t_max: T0 + 30, procs: "app,engine,holder", lines: 4, bad_lines: 0 });

    const incident = await env.DB.prepare("SELECT * FROM incidents WHERE install = ?1").bind(install).first();
    expect(incident).toMatchObject({
      kind: "session.exit",
      sev: "incident",
      session: "s_26bf32debd4c",
      code: "no_conversation",
      signature: "session.exit:no_conversation",
      proc: "engine",
      app_version: "0.9.0",
    });

    const { results: sessions } = await env.DB.prepare(
      "SELECT session, conv, agent, first_t, last_t FROM sessions WHERE install = ?1 ORDER BY conv",
    )
      .bind(install)
      .all();
    expect(sessions).toEqual([
      { session: "s_26bf32debd4c", conv: "", agent: null, first_t: T0 + 20, last_t: T0 + 20 },
      {
        session: "s_26bf32debd4c",
        conv: "0a40e747-fa0c-4e9a-b755-c195ab079cda",
        agent: "claude-code",
        first_t: T0 + 10,
        last_t: T0 + 10,
      },
    ]);
  });

  it("upserts install and widens session spans across batches", async () => {
    const install = newInstall();
    expect((await ingest(install, [rec(T0, "session.attach", "info", { session: "s_a" })])).status).toBe(202);
    const second = await ingest(install, [rec(T0 + 5000, "session.detach", "info", { session: "s_a" })], {
      header: header(install, { app_version: "0.9.1", name: "alex k" }),
    });
    expect(second.status).toBe(202);
    const row = await env.DB.prepare("SELECT app_version, name FROM installs WHERE install = ?1").bind(install).first();
    expect(row).toEqual({ app_version: "0.9.1", name: "alex k" });
    const span = await env.DB.prepare("SELECT first_t, last_t FROM sessions WHERE install = ?1").bind(install).first();
    expect(span).toEqual({ first_t: T0, last_t: T0 + 5000 });
  });

  it("counts malformed records without rejecting the batch", async () => {
    const install = newInstall();
    const res = await ingest(install, [
      "not json",
      '{"broken":',
      rec(T0, "ok", "info"),
      JSON.stringify({ k: "reordered", s: "error", t: T0 + 1, p: "app", f: { code: "x" } }),
      "",
    ]);
    expect(res.status).toBe(202);
    expect(await res.json()).toMatchObject({ accepted: 4, bad_lines: 2, incidents: 1 });
  });

  it("accepts a body a proxy already decompressed", async () => {
    const install = newInstall();
    const text = `${JSON.stringify(header(install))}\n${rec(T0, "a.b", "info")}\n`;
    const res = await ingest(install, [], { body: new TextEncoder().encode(text) });
    expect(res.status).toBe(202);
    const { key } = (await res.json()) as { key: string };
    const object = await env.BATCHES.get(key);
    const bytes = new Uint8Array(await object!.arrayBuffer());
    expect([bytes[0], bytes[1]]).toEqual([0x1f, 0x8b]);
  });

  describe("header rejection", () => {
    const cases: [string, (install: string) => Record<string, unknown> | string, number][] = [
      ["not json", () => "{nope", 400],
      ["wrong version", (i) => header(i, { v: 2 }), 422],
      ["wrong type", (i) => header(i, { type: "record" }), 422],
      ["install not a uuid", (i) => header(i, { install: "alex" }), 422],
      ["bad support id", (i) => header(i, { support_id: "X-1" }), 422],
      ["sent_at missing", (i) => header(i, { sent_at: undefined }), 422],
    ];
    for (const [name, make, status] of cases) {
      it(name, async () => {
        const install = newInstall();
        const res = await ingest(install, [rec(T0, "a.b", "info")], { header: make(install) });
        expect(res.status).toBe(status);
      });
    }

    it("truncates over-long descriptive fields instead of dropping the batch", async () => {
      // 0.8.10's Engine sends a 90-character build id; rejecting it lost
      // every batch from every install.
      const install = newInstall();
      const build = "diri-engine-0.1.0+catalog.eba923e3392a2559bbe740ee39afd62ed1c98fdecc8b7dd178b1dd58fbfcb22d";
      const res = await ingest(install, [rec(T0, "a.b", "info")], {
        header: header(install, { build, name: "j".repeat(80), app_version: "0.9\u0000" }),
      });
      expect(res.status).toBe(202);
      const row = await env.DB.prepare("SELECT build, name, app_version FROM installs WHERE install = ?1")
        .bind(install)
        .first<{ build: string; name: string; app_version: string }>();
      expect(row).toEqual({ build: build.slice(0, 64), name: "j".repeat(64), app_version: "0.9" });
    });

    it("install header must match the batch header", async () => {
      const install = newInstall();
      const res = await ingest(install, [], { installHeader: newInstall() });
      expect(res.status).toBe(422);
    });

    it("missing X-Diri-Install", async () => {
      const install = newInstall();
      const res = await ingest(install, [], { installHeader: "" });
      expect(res.status).toBe(400);
    });

    it("body that is not gzip", async () => {
      const install = newInstall();
      const res = await ingest(install, [], { body: new Uint8Array([0x1f, 0x8b, 8, 0, 1, 2, 3, 4]) });
      expect(res.status).toBe(400);
    });

    it("rejected batches leave nothing behind", async () => {
      const install = newInstall();
      await ingest(install, [], { header: header(install, { v: 2 }) });
      const listed = await env.BATCHES.list({ prefix: `v1/${install}/` });
      expect(listed.objects).toHaveLength(0);
      const row = await env.DB.prepare("SELECT 1 FROM installs WHERE install = ?1").bind(install).first();
      expect(row).toBeNull();
    });
  });

  describe("caps", () => {
    it("rejects more than 5 MiB compressed", async () => {
      const install = newInstall();
      const junk = new Uint8Array(5 * 1024 * 1024 + 1);
      crypto.getRandomValues(junk.subarray(0, 65536));
      const res = await ingest(install, [], { body: junk });
      expect(res.status).toBe(413);
    });

    it("stops decompressing at the raw cap", async () => {
      // A small gzip that inflates past the cap: the scanner must give up at
      // the cap instead of materialising the whole body.
      const install = newInstall();
      const filler = `${rec(T0, "x.y", "info", { pad: "a".repeat(900) })}\n`.repeat(2000);
      const gz = await gzip(`${JSON.stringify(header(install))}\n${filler}`);
      await expect(scanGzip(gz, 1024 * 1024)).rejects.toMatchObject({ status: 413 });
    });

    it("rejects more than 50k records", async () => {
      const install = newInstall();
      const line = rec(T0, "x", "debug");
      const res = await ingest(install, new Array(MAX_LINES + 1).fill(line));
      expect(res.status).toBe(413);
    });

    it("accepts exactly 50k records", async () => {
      const install = newInstall();
      const line = rec(T0, "x", "debug");
      const res = await ingest(install, new Array(MAX_LINES).fill(line));
      expect(res.status).toBe(202);
    });
  });

  it("rate limits per install", async () => {
    // RATE_LIMIT_PER_HOUR is 5 in the test config.
    const install = newInstall();
    for (let i = 0; i < 5; i++) {
      expect((await ingest(install, [rec(T0 + i, "a.b", "info")])).status).toBe(202);
    }
    const limited = await ingest(install, [rec(T0 + 9, "a.b", "info")]);
    expect(limited.status).toBe(429);
    const other = await ingest(newInstall(), [rec(T0, "a.b", "info")]);
    expect(other.status).toBe(202);
  });

  it("caps incident rows per batch and keeps the rest in R2", async () => {
    const install = newInstall();
    const lines = Array.from({ length: 250 }, (_, i) => rec(T0 + i, "rpc.failed", "error", { code: "timeout" }));
    const res = await ingest(install, lines);
    expect(await res.json()).toMatchObject({ accepted: 250, incidents: 200, incidents_unindexed: 50 });
  });
});

describe("signatures and fields", () => {
  it("groups panics by signature, crash reports by frame, the rest by kind and code", () => {
    expect(signatureOf("panic", { signature: "diri_term::grid::resize", message: "oob" }, null)).toBe(
      "panic:diri_term::grid::resize",
    );
    expect(signatureOf("crash.report", { crashed_frame: "objc_msgSend" }, "EXC_BAD_ACCESS")).toBe(
      "crash.report:objc_msgSend",
    );
    expect(signatureOf("crash.report", { frames: ["gpui::window::draw", "main"] }, null)).toBe(
      "crash.report:gpui::window::draw",
    );
    expect(signatureOf("session.spawn_failed", {}, "127")).toBe("session.spawn_failed:127");
    expect(signatureOf("render.blank", {}, null)).toBe("render.blank");
  });

  it("caps stored fields at 2 KiB", () => {
    const big = capFields({ message: "m".repeat(5000), frames: Array(100).fill("frame::name::here"), code: "x" });
    expect(big.length).toBeLessThanOrEqual(2048);
    expect(JSON.parse(big).code).toBe("x");
  });
});
