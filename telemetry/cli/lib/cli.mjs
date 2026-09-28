// diri-debug: "what happened to this person", from the telemetry Worker's
// admin API or a local spool. See ../README.md and diri/TELEMETRY.md.

import { analyze, renderHealth, summarizeHealth } from "./health.mjs";
import { filterRecords, formatRecord, globMatcher, isIncident, isPeriodic, LEGEND, severityMatcher } from "./records.mjs";
import { defaultSpoolDir, loadConfig, LocalSource, RemoteSource } from "./sources.mjs";
import { ago, formatDuration, formatStamp, resolveWindow } from "./time.mjs";

export const USAGE = `diri-debug: investigate a diri user's bug from telemetry

usage:
  diri-debug who <name | support id | uuid prefix>
  diri-debug incidents [who] [--since 7d] [--kind glob] [--sev incident] [--version v] [--session id] [--conv uuid]
  diri-debug top [--since 7d] [--version v] [--kind glob]
  diri-debug timeline <who> --around "2026-09-27 23:40" [--window 20m] | [--since 2h --until 1h]
                      [--session id] [--conv uuid] [--kind 'session.*'] [--sev warn+] [--proc engine] [--all]
  diri-debug health <who> [--since 24h]
  diri-debug sessions <who>
  diri-debug find <session id | conversation uuid>
  diri-debug raw <batch key>
  diri-debug local [timeline|incidents|top|health|sessions|find] [--dir spool] [same flags]

flags:
  --json          machine-readable output (every command)
  --utc           read and print times in UTC instead of local time
  --full          do not shorten long field values
  --all           timeline: show health/metrics samples inline instead of folding them
  --limit N       cap rows (incidents, top)
  --sev S         "incident", "warn,error" or "warn+" (at least warn)
  --kind G        glob over event kinds, comma-separated: 'session.*,rpc.*'

times: "2026-09-27 23:40", "23:40" (the latest one), "2h" (ago), ISO 8601, epoch ms.
config: DIRI_TELEMETRY_URL + DIRI_TELEMETRY_ADMIN_TOKEN, or ~/.config/diri-debug/config.json {"url","token"}.
`;

const BOOLEAN_FLAGS = new Set(["json", "utc", "full", "all", "help", "no-color", "color"]);

/**
 * @param {string[]} argv
 * @returns {{positional: string[], flags: Record<string, any>}}
 */
export function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--") {
      positional.push(...argv.slice(i + 1));
      break;
    }
    if (arg === "-h") {
      flags.help = true;
    } else if (arg.startsWith("--")) {
      const eq = arg.indexOf("=");
      const name = arg.slice(2, eq === -1 ? undefined : eq);
      if (eq !== -1) flags[name] = arg.slice(eq + 1);
      else if (BOOLEAN_FLAGS.has(name)) flags[name] = true;
      else {
        const value = argv[i + 1];
        if (value === undefined) throw new Error(`--${name} needs a value`);
        flags[name] = value;
        i += 1;
      }
    } else {
      positional.push(arg);
    }
  }
  return { positional, flags };
}

/**
 * @typedef {{stdout: (s: string) => void, stderr: (s: string) => void, env: Record<string, string | undefined>,
 *   now?: number, fetch?: typeof fetch, isTTY?: boolean}} Io
 */

/**
 * @param {string[]} argv
 * @param {Io} io
 * @returns {Promise<number>} exit code
 */
export async function main(argv, io) {
  let parsed;
  try {
    parsed = parseArgs(argv);
  } catch (err) {
    io.stderr(`${err.message}\n`);
    return 2;
  }
  const { positional, flags } = parsed;
  const [command, ...rest] = positional;
  if (!command || flags.help || command === "help") {
    io.stdout(USAGE);
    return command || flags.help ? 0 : 2;
  }
  const ctx = {
    io,
    flags,
    now: io.now ?? Date.now(),
    color: flags.color || (!flags["no-color"] && !flags.json && io.isTTY && !io.env.NO_COLOR),
    out: (text) => io.stdout(text.endsWith("\n") ? text : `${text}\n`),
    json: (value) => io.stdout(`${JSON.stringify(value)}\n`),
  };
  try {
    if (command === "local") {
      const dir = flags.dir ?? defaultSpoolDir(io.env);
      const source = new LocalSource(dir);
      const [view = "timeline", ...args] = rest;
      const run = LOCAL_VIEWS[view];
      if (!run) throw new Error(`unknown local view "${view}" (timeline, incidents, top, health, sessions, find)`);
      await run(ctx, source, args, true);
      return 0;
    }
    const run = COMMANDS[command];
    if (!run) {
      io.stderr(`unknown command "${command}"\n\n${USAGE}`);
      return 2;
    }
    const source = new RemoteSource(loadConfig(io.env), io.fetch);
    await run(ctx, source, rest, false);
    return 0;
  } catch (err) {
    io.stderr(`diri-debug: ${err.message}\n`);
    return 1;
  }
}

const opts = (ctx) => ({ utc: Boolean(ctx.flags.utc), color: ctx.color, full: Boolean(ctx.flags.full) });

function requireWho(args, command) {
  if (!args[0]) throw new Error(`${command} needs <who>: a name, Support ID or install UUID`);
  return args[0];
}

function installLabel(i) {
  return `${i.name ?? "-"} (${i.support_id ?? "?"})`;
}

async function whoCmd(ctx, source, args) {
  const installs = await source.who(args[0] ?? "");
  if (ctx.flags.json) return ctx.json({ installs });
  if (installs.length === 0) return ctx.out(`no installs match "${args[0] ?? ""}"`);
  const lines = installs.map(
    (i) =>
      `${String(i.support_id).padEnd(10)}  ${String(i.name ?? "-").padEnd(16)}  ${String(i.app_version ?? "-").padEnd(8)}  ${[i.os, i.os_version, i.arch].filter(Boolean).join(" ").padEnd(18)}  last ${ago(i.last_seen, ctx.now).padEnd(10)}  first ${formatStamp(i.first_seen, opts(ctx))}  ${i.install}`,
  );
  ctx.out(lines.join("\n"));
}

function incidentFilters(ctx, install, window) {
  const { flags } = ctx;
  const sev = flags.sev && /^(error|incident)$/.test(flags.sev) ? flags.sev : undefined;
  return {
    install,
    kind: flags.kind && !flags.kind.includes(",") ? flags.kind : undefined,
    sev,
    version: flags.version,
    session: flags.session,
    conv: flags.conv,
    since: Math.floor(window.since),
    until: Math.ceil(window.until),
    limit: flags.limit ? Number(flags.limit) : undefined,
  };
}

function incidentAsRecord(row) {
  return { t: row.t, seq: row.seq, p: row.proc ?? "?", pid: row.pid ?? "", k: row.kind, s: row.sev, f: row.fields ?? {} };
}

async function incidentsCmd(ctx, source, args, local) {
  const window = resolveWindow(ctx.flags, ctx.now, "7d");
  const who = args[0];
  const install = who && !local ? await source.resolve(who) : null;
  let rows = await source.incidents(incidentFilters(ctx, install?.install, window));
  // Filters the API cannot express (severity ranges, kind lists) apply here.
  const sev = severityMatcher(ctx.flags.sev);
  const kind = globMatcher(ctx.flags.kind);
  rows = rows.filter((r) => sev(r.sev) && kind(r.kind));
  if (ctx.flags.json) return ctx.json({ install: install ?? null, since: window.since, until: window.until, incidents: rows });
  const header = install ? `${installLabel(install)} ${install.app_version ?? ""}`.trim() : local ? "this Mac" : "all installs";
  ctx.out(`${header}: ${rows.length} errors/incidents, ${formatStamp(window.since, opts(ctx))} → ${formatStamp(window.until, opts(ctx))}`);
  const cross = !install && !local;
  for (const row of rows) {
    const who = cross ? `${String(row.support_id ?? row.install.slice(0, 8)).padEnd(10)} ${String(row.name ?? "-").padEnd(12)} ` : "";
    const version = cross && row.app_version ? ` v=${row.app_version}` : "";
    ctx.out(`${who}${formatRecord(incidentAsRecord(row), opts(ctx))}${version}`);
  }
  if (rows.length && !ctx.flags.json) {
    const latest = rows[0];
    const target = install ? who : (latest.support_id ?? latest.install);
    ctx.out(`\nnext: diri-debug ${local ? "local timeline" : `timeline ${target}`} --around "${formatStamp(latest.t, opts(ctx))}" --window 20m`);
  }
}

async function topCmd(ctx, source, args, local) {
  const window = resolveWindow(ctx.flags, ctx.now, "7d");
  const filters = incidentFilters(ctx, undefined, window);
  if (args[0] && !local) filters.install = (await source.resolve(args[0])).install;
  const groups = await source.summary(filters);
  if (ctx.flags.json) return ctx.json({ since: window.since, until: window.until, groups });
  if (groups.length === 0) return ctx.out("no errors or incidents in this window");
  ctx.out(`count  installs  sev       last seen    first seen   versions         signature`);
  for (const g of groups) {
    ctx.out(
      `${String(g.count).padStart(5)}  ${String(g.installs).padStart(8)}  ${String(g.sev).padEnd(8)}  ${ago(g.last_t, ctx.now).padEnd(11)}  ${ago(g.first_t, ctx.now).padEnd(11)}  ${(g.versions ?? []).join(",").padEnd(15)}  ${g.signature}`,
    );
  }
}

async function timelineCmd(ctx, source, args, local) {
  const window = resolveWindow(ctx.flags, ctx.now, "1h");
  const install = local ? await source.resolve() : await source.resolve(requireWho(args, "timeline"));
  const { records, batches, bad } = await source.records(install.install, window.since, window.until);
  const filtered = filterRecords(records, {
    session: ctx.flags.session,
    conv: ctx.flags.conv,
    kind: ctx.flags.kind,
    sev: ctx.flags.sev,
    proc: ctx.flags.proc,
  });
  const periodic = ctx.flags.all || ctx.flags.kind ? [] : filtered.filter(isPeriodic);
  const shown = ctx.flags.all || ctx.flags.kind ? filtered : filtered.filter((r) => !isPeriodic(r));
  const health = analyze(periodic.length ? periodic : records.filter(isPeriodic));
  if (ctx.flags.json) {
    return ctx.json({ install, since: window.since, until: window.until, batches, bad_lines: bad, records: shown, health: summarizeHealth(health) });
  }
  const incidents = shown.filter(isIncident).length;
  ctx.out(
    `${installLabel(install)}  ${formatStamp(window.since, opts(ctx))} → ${formatStamp(window.until, opts(ctx))} ${ctx.flags.utc ? "UTC" : "local"}  ${shown.length} records (${incidents} errors/incidents) from ${batches} ${local ? "spool files" : "batches"}${bad ? `, ${bad} unreadable lines` : ""}`,
  );
  ctx.out(LEGEND);
  for (const r of shown) ctx.out(formatRecord(r, opts(ctx)));
  if (health.length) {
    ctx.out(`\nhealth/metrics (${periodic.length || "no"} samples folded; --all shows them, \`health\` plots them):`);
    for (const line of summarizeHealth(health)) ctx.out(`  ${line}`);
  }
}

async function healthCmd(ctx, source, args, local) {
  const window = resolveWindow(ctx.flags, ctx.now, "24h");
  const install = local ? await source.resolve() : await source.resolve(requireWho(args, "health"));
  const { records } = await source.records(install.install, window.since, window.until);
  const procs = analyze(filterRecords(records.filter(isPeriodic), { proc: ctx.flags.proc }));
  if (ctx.flags.json) return ctx.json({ install, since: window.since, until: window.until, processes: procs });
  ctx.out(`${installLabel(install)}  ${formatStamp(window.since, opts(ctx))} → ${formatStamp(window.until, opts(ctx))}  (each process instance is p:pid; a restart starts a new one)\n`);
  ctx.out(renderHealth(procs, opts(ctx)));
}

function groupSessions(rows) {
  const bySession = new Map();
  for (const row of rows) {
    const s = bySession.get(row.session) ?? { session: row.session, agent: row.agent, first_t: row.first_t, last_t: row.last_t, convs: [] };
    s.agent ??= row.agent;
    s.first_t = Math.min(s.first_t, row.first_t);
    s.last_t = Math.max(s.last_t, row.last_t);
    if (row.conv) s.convs.push({ conv: row.conv, first_t: row.first_t, last_t: row.last_t });
    bySession.set(row.session, s);
  }
  return [...bySession.values()].sort((a, b) => b.last_t - a.last_t);
}

async function sessionsCmd(ctx, source, args, local) {
  const install = local ? await source.resolve() : await source.resolve(requireWho(args, "sessions"));
  const sessions = groupSessions(await source.sessions(install.install));
  if (ctx.flags.json) return ctx.json({ install, sessions });
  ctx.out(`${installLabel(install)}: ${sessions.length} sessions`);
  for (const s of sessions) {
    ctx.out(`${s.session.padEnd(18)} ${String(s.agent ?? "-").padEnd(12)} ${formatStamp(s.first_t, opts(ctx))} → ${formatStamp(s.last_t, opts(ctx))} (${formatDuration(s.last_t - s.first_t)})`);
    for (const c of s.convs.sort((a, b) => a.first_t - b.first_t)) {
      ctx.out(`    conv ${c.conv}  ${formatStamp(c.first_t, opts(ctx))} → ${formatStamp(c.last_t, opts(ctx))}`);
    }
  }
}

async function findCmd(ctx, source, args, local) {
  const id = args[0];
  if (!id) throw new Error("find needs a session id or conversation UUID");
  const matches = await source.find(id);
  if (ctx.flags.json) return ctx.json({ id, matches });
  if (matches.length === 0) return ctx.out(`nothing seen with session or conversation "${id}" in the last 30 days`);
  for (const m of matches) {
    ctx.out(
      `${installLabel(m)} ${m.install}  session ${m.session}${m.conv ? ` conv ${m.conv}` : ""}  ${m.agent ?? ""}  ${formatStamp(m.first_t, opts(ctx))} → ${formatStamp(m.last_t, opts(ctx))}`,
    );
  }
  const m = matches[0];
  ctx.out(`\nnext: diri-debug ${local ? "local timeline" : `timeline ${m.support_id}`} --session ${m.session} --since "${formatStamp(m.first_t - 5 * 60_000, opts(ctx))}" --until "${formatStamp(m.last_t + 5 * 60_000, opts(ctx))}"`);
}

async function rawCmd(ctx, source, args) {
  if (!args[0]) throw new Error("raw needs a batch key (v1/<install>/<day>/<file>.ndjson.gz)");
  const text = await source.rawBatch(args[0]);
  ctx.io.stdout(text.endsWith("\n") ? text : `${text}\n`);
}

const COMMANDS = {
  who: whoCmd,
  incidents: incidentsCmd,
  top: topCmd,
  timeline: timelineCmd,
  health: healthCmd,
  sessions: sessionsCmd,
  find: findCmd,
  raw: rawCmd,
};

const LOCAL_VIEWS = {
  timeline: timelineCmd,
  incidents: incidentsCmd,
  top: topCmd,
  health: healthCmd,
  sessions: sessionsCmd,
  find: findCmd,
};

