// Runs main() in-process against the fixture spool and a stub admin API.
process.env.TZ = "UTC";

import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { main } from "../lib/cli.mjs";
import { analyze, leakHint, sparkline } from "../lib/health.mjs";
import { globMatcher, mergeSort, severityMatcher } from "../lib/records.mjs";
import { supportIdOf } from "../lib/sources.mjs";
import { parseDuration, parseTime, resolveWindow } from "../lib/time.mjs";
import { buildRecords, CONV, INSTALL, NOW, SESSION, startStub, SUPPORT_ID, T0, TOKEN, writeSpool } from "./fixtures.mjs";

let stub;
let spool;
before(async () => {
  stub = await startStub();
  spool = writeSpool();
});
after(() => stub.close());

async function run(args, env = {}) {
  let stdout = "";
  let stderr = "";
  const code = await main(args, {
    stdout: (s) => (stdout += s),
    stderr: (s) => (stderr += s),
    env: { HOME: "/nonexistent", DIRI_TELEMETRY_URL: stub.url, DIRI_TELEMETRY_ADMIN_TOKEN: TOKEN, ...env },
    now: NOW,
    isTTY: false,
  });
  return { code, stdout, stderr };
}

describe("time", () => {
  it("parses durations, clock times, dates and epoch ms", () => {
    assert.equal(parseDuration("1h30m"), 5_400_000);
    assert.equal(parseDuration("alex"), null);
    assert.equal(parseTime("2h", NOW), NOW - 7_200_000);
    assert.equal(parseTime("2026-09-27 23:40", NOW), T0);
    assert.equal(parseTime("23:40", NOW), T0);
    assert.equal(parseTime(String(T0), NOW), T0);
    assert.equal(parseTime("2026-09-27T23:40:00Z", NOW), T0);
    assert.throws(() => parseTime("yesterday-ish", NOW));
  });

  it("centres --around on a --window", () => {
    assert.deepEqual(resolveWindow({ around: "23:40", window: "20m" }, NOW, "1h"), { since: T0 - 600_000, until: T0 + 600_000 });
    assert.deepEqual(resolveWindow({}, NOW, "1h"), { since: NOW - 3_600_000, until: NOW });
  });
});

describe("records", () => {
  it("matches kind globs and severity specs", () => {
    assert.ok(globMatcher("session.*")("session.exit"));
    assert.ok(!globMatcher("session.*")("holder.spawn"));
    assert.ok(globMatcher("rpc.*,panic")("panic"));
    assert.ok(severityMatcher("warn+")("incident"));
    assert.ok(!severityMatcher("warn+")("info"));
    assert.ok(severityMatcher("warn,error")("warn"));
    assert.throws(() => severityMatcher("loud"));
  });

  it("merges processes by time then sequence and drops duplicates", () => {
    const a = { t: 5, seq: 2, p: "engine", pid: 1, k: "a", s: "info", f: {} };
    const b = { t: 5, seq: 1, p: "app", pid: 2, k: "b", s: "info", f: {} };
    const c = { t: 4, seq: 9, p: "holder", pid: 3, k: "c", s: "info", f: {} };
    assert.deepEqual(mergeSort([a, b, c, { ...a }]).map((r) => r.k), ["c", "b", "a"]);
  });

  it("computes the Support ID like the recorder", () => {
    assert.equal(supportIdOf("00000000-0000-0000-0000-000000000000"), "D-00000000");
    assert.equal(supportIdOf("ffffffff-ff00-0000-0000-000000000000"), "D-ZZZZZZZZ");
    assert.equal(supportIdOf(INSTALL), SUPPORT_ID);
  });
});

describe("health analysis", () => {
  it("draws sparklines and flags a rising floor, not a flat noisy series", () => {
    assert.equal(sparkline([1, 2, 3, 4, 5, 6, 7, 8], 8), "▁▂▃▄▅▆▇█");
    const procs = analyze(buildRecords());
    const engine = procs.find((p) => p.proc === "engine");
    const app = procs.find((p) => p.proc === "app");
    assert.ok(engine.hints.some((h) => h.startsWith("rss_mb floor +")), engine.hints.join("; "));
    assert.deepEqual(app.hints, []);
    assert.ok(app.timings.frame_ms.length > 300);
    assert.equal(leakHint("rss_mb", [{ t: 0, v: 1 }]), null);
  });
});

describe("remote commands", () => {
  it("who finds an install by name and prints its Support ID", async () => {
    const { code, stdout } = await run(["who", "alex"]);
    assert.equal(code, 0);
    assert.match(stdout, new RegExp(`${SUPPORT_ID}\\s+alex\\s+0\\.9\\.0`));
    assert.ok(stub.requests.includes("/v1/admin/installs?q=alex"));
  });

  it("incidents resolves who, lists newest first and suggests the timeline", async () => {
    const { stdout } = await run(["incidents", "alex", "--since", "7d"]);
    const lines = stdout.split("\n");
    assert.match(lines[0], /alex \(D-190EEHZT\).*3 errors\/incidents/);
    assert.match(lines[1], /^09-27 23:45:00\.000 app\s+900 ! panic\s+message="index out of bounds"/);
    assert.match(stdout, /session\.resume_failed\s+session=s_26bf32debd4c agent=claude-code conv=0a40e747/);
    assert.match(stdout, /next: diri-debug timeline alex --around "2026-09-27 23:45" --window 20m/);
  });

  it("incidents across installs name each install; filters by kind glob", async () => {
    const { stdout } = await run(["incidents", "--kind", "session.*"]);
    assert.match(stdout, /^all installs: 2 errors/);
    assert.match(stdout, /D-190EEHZT alex\s+09-27/);
    assert.doesNotMatch(stdout, /panic/);
  });

  it("top groups by signature", async () => {
    const { stdout } = await run(["top", "--since", "7d"]);
    assert.match(stdout, /1\s+1\s+incident\s+.*0\.9\.0\s+panic:diri_term::grid::Grid::resize/);
    assert.match(stdout, /session\.resume_failed:no_conversation/);
  });

  it("timeline merges batches across processes, folds health and shows alex's failure in one screen", async () => {
    const { code, stdout } = await run(["timeline", "alex", "--around", "2026-09-27 23:38", "--window", "10m"]);
    assert.equal(code, 0);
    const body = stdout.split("\n").filter((l) => /^\d\d-\d\d /.test(l));
    const kinds = body.map((l) => l.split(/\s+/)[4] === "!" || l.split(/\s+/)[4] === "W" ? l.split(/\s+/)[5] : l.split(/\s+/)[4]);
    assert.deepEqual(kinds, ["rpc.slow", "session.spawn", "holder.spawn", "session.exit", "session.resume_failed", "terminal.mouse_mode_stuck"]);
    // Sorted by time across engine, holder and app.
    const times = body.map((l) => l.slice(0, 18));
    assert.deepEqual(times, [...times].sort());
    assert.match(stdout, /health\/metrics \(\d+ samples folded/);
    assert.match(stdout, /engine 812: \d+ samples\s+rss \d+/);
    // Only batches overlapping the window were fetched.
    const fetched = stub.requests.filter((r) => r.startsWith("/v1/admin/batch?")).length;
    assert.ok(fetched >= 1 && fetched < stub.batches.length, `fetched ${fetched} of ${stub.batches.length}`);
  });

  it("timeline filters by session and emits JSON", async () => {
    const { stdout } = await run(["timeline", SUPPORT_ID, "--since", "2026-09-27 23:30", "--until", "2026-09-27 23:50", "--session", SESSION, "--json"]);
    const doc = JSON.parse(stdout);
    assert.equal(doc.install.install, INSTALL);
    assert.deepEqual(doc.records.map((r) => r.k), ["session.spawn", "holder.spawn", "session.exit", "session.resume_failed", "terminal.mouse_mode_stuck"]);
  });

  it("timeline dedupes a batch delivered twice", async () => {
    const { stdout } = await run(["timeline", "alex", "--around", "23:45", "--window", "2m", "--kind", "panic", "--json"]);
    assert.equal(JSON.parse(stdout).records.length, 1);
  });

  it("health plots per-process trends with a leak hint", async () => {
    const { stdout } = await run(["health", "alex", "--since", "24h"]);
    assert.match(stdout, /^engine 812 /m);
    assert.match(stdout, /^ {2}rss_mb\s+[▁▂▃▄▅▆▇█]+\s+\d+/m);
    assert.match(stdout, /^ {2}frame_ms p99\s+[▁▂▃▄▅▆▇█]+/m);
    assert.match(stdout, /⚠ leak\? rss_mb floor \+\d+ over 6h/);
    assert.doesNotMatch(stdout.split("engine 812")[0], /leak/, "the flat, noisy app series is not a leak");
  });

  it("sessions groups conversations under their session", async () => {
    const { stdout } = await run(["sessions", "alex"]);
    assert.match(stdout, /s_26bf32debd4c\s+claude-code/);
    assert.match(stdout, new RegExp(`conv ${CONV}`));
  });

  it("find locates the install from a conversation uuid", async () => {
    const { stdout } = await run(["find", CONV]);
    assert.match(stdout, /alex \(D-190EEHZT\).*session s_26bf32debd4c conv 0a40e747/);
    assert.match(stdout, /next: diri-debug timeline D-190EEHZT --session s_26bf32debd4c/);
    const json = JSON.parse((await run(["find", CONV, "--json"])).stdout);
    assert.equal(json.matches[0].install, INSTALL);
  });

  it("budget shows R2 usage against the spend caps", async () => {
    const { code, stdout } = await run(["budget"]);
    assert.equal(code, 0);
    assert.match(stdout, /writes\s+450000 \/ 900000\s+50\.0%/);
    assert.match(stdout, /stored\s+0\.90 GB \/ 9\.00 GB\s+10\.0%/);
  });

  it("raw prints a batch decompressed", async () => {
    const { stdout } = await run(["raw", stub.batches[0].r2_key]);
    assert.match(stdout.split("\n")[0], /"type":"batch"/);
    assert.equal(stdout.trim().split("\n").length, stub.batches[0].records.length + 1);
  });

  it("every command supports --json", async () => {
    for (const args of [["who", "alex"], ["incidents", "alex"], ["top"], ["health", "alex"], ["sessions", "alex"]]) {
      const { code, stdout } = await run([...args, "--json"]);
      assert.equal(code, 0, args.join(" "));
      assert.doesNotThrow(() => JSON.parse(stdout), args.join(" "));
    }
  });

  it("fails clearly without configuration or with a bad token", async () => {
    const missing = await run(["who", "alex"], { DIRI_TELEMETRY_URL: "" });
    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /DIRI_TELEMETRY_URL/);
    const bad = await run(["who", "alex"], { DIRI_TELEMETRY_ADMIN_TOKEN: "wrong" });
    assert.match(bad.stderr, /HTTP 401/);
    const unknown = await run(["timeline", "nobody"]);
    assert.match(unknown.stderr, /no install matches "nobody"/);
  });
});

describe("local spool", () => {
  it("timeline reads open and sealed files from every process", async () => {
    const { code, stdout } = await run(["local", "timeline", "--dir", spool, "--around", "23:38", "--window", "10m"]);
    assert.equal(code, 0);
    assert.match(stdout.split("\n")[0], /^alex \(D-190EEHZT\).*6 records \(2 errors\/incidents\) from 6 spool files, 3 unreadable lines/);
    assert.match(stdout, /holder\s+4242 ! session\.exit\s+session=s_26bf32debd4c code=exit_1/);
  });

  it("incidents, top, health, sessions and find work over the spool", async () => {
    assert.match((await run(["local", "incidents", "--dir", spool, "--since", "7d"])).stdout, /this Mac: 3 errors\/incidents/);
    assert.match((await run(["local", "top", "--dir", spool, "--since", "7d"])).stdout, /panic:diri_term::grid::Grid::resize/);
    assert.match((await run(["local", "health", "--dir", spool, "--since", "24h"])).stdout, /leak\? rss_mb/);
    assert.match((await run(["local", "sessions", "--dir", spool])).stdout, /s_older\s+codex/);
    assert.match((await run(["local", "find", CONV, "--dir", spool])).stdout, /session s_26bf32debd4c/);
  });

  it("filters by --sev and --proc", async () => {
    const { stdout } = await run(["local", "timeline", "--dir", spool, "--since", "7h", "--sev", "warn+", "--proc", "engine", "--json"]);
    const kinds = JSON.parse(stdout).records.map((r) => r.k);
    assert.deepEqual(kinds, ["rpc.slow", "session.resume_failed"]);
  });

  it("reports a missing spool", async () => {
    const { code, stderr } = await run(["local", "--dir", "/nonexistent/spool"]);
    assert.equal(code, 1);
    assert.match(stderr, /no spool at/);
  });
});
