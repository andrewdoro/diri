# Telemetry

Diri records what its processes do so a bug report ("julia's tab dropped to
zsh last night") can be investigated from a timeline instead of a
reproduction. This file is the contract between the recorder
(`crates/diri-telemetry`), the instrumentation in each process, the ingest
Worker (`telemetry/worker`) and the investigation CLI (`telemetry/cli`).

## Principles

- **Local first.** Every process records to a spool on disk. Uploading is a
  separate step the Engine performs, and the user can turn it off in
  Settings > Privacy. Recording itself costs a channel send per event.
- **No content, by type.** Fields are numbers, booleans, `&'static str`
  literals, `Id`s (`[A-Za-z0-9_.:-]{1,96}`) or scrubbed `Text`. There is no
  way to record terminal output, prompts, clipboard or pasted contents, file
  contents, environment variables, command lines, or URLs. Paths are recorded
  only as `path_hash`. Conversation UUIDs, session ids, agent ids, error codes
  and RPC method names are allowed: they identify, they don't reveal.
- **Never on the hot path.** No event per PTY read, per byte, or per
  terminal cell. Hot paths accumulate locally and report totals through
  `count`/`observe_ms`, which are drained into one `metrics` event per
  minute. Per frame, per RPC, per keystroke and per paste are fine.
- **Holders stay quiet when idle.** A Holder records facts as they happen
  (spawn, exit, attach, overflow) and never runs the health sampler.
- **Tests and dev builds never upload.** Debug builds upload only when
  `DIRI_TELEMETRY_ENDPOINT` is set at run time. `DIRI_TELEMETRY=off`
  disables recording entirely.

## Files

Under the platform state dir (`~/Library/Application Support/Dirijor`):

| Path | Owner | Content |
|---|---|---|
| `telemetry/install.json` | recorder | random install UUID, created time |
| `telemetry/config.json` | app Settings | `{ "upload": bool, "name": string\|null }` |
| `telemetry/spool/<proc>-<pid>-<start_ms>-<n>.open` | each process | JSONL being written |
| `telemetry/spool/*.jsonl` | each process | rolled at 2 MiB |
| `telemetry/spool/offsets.json` | uploader | bytes acknowledged per file |
| `telemetry/spool/urgent` | recorder | touched on an incident |

The spool is capped at 64 MiB; oldest sealed files go first.

The **Support ID** (`D-XXXXXXXX`, Crockford base32 of the first 40 bits of
the install UUID) is shown in Settings > Privacy and About. The **name**
defaults to the macOS login name; the user can edit or clear it.

## Record format

One JSON object per line:

```json
{"t":1790581979447,"seq":42,"p":"engine","pid":812,"k":"session.spawn","s":"info","f":{"session":"s_26bf32debd4c","agent":"claude-code","mode":"resume","conv":"0a40e747-fa0c-4e9a-b755-c195ab079cda"}}
```

- `t` wall-clock ms, `seq` per-process sequence, `p` `app|engine|holder`.
- `k` the event kind, dotted, lower snake case: `<area>.<what>`.
- `s` severity: `debug|info|warn|error|incident`. `incident` means
  user-visible breakage; it triggers an upload within a minute and is
  indexed server-side.
- `f` fields. Well-known keys other tools rely on:
  `session` (session id), `conv` (agent conversation UUID), `agent` (agent
  id), `host` (remote host id), `window` (window id), `code` (error code),
  `ms` (duration), `method` (RPC method).

## Events emitted by the recorder itself

| kind | sev | fields |
|---|---|---|
| `process.start` | info | `version, os, arch, debug_build` |
| `health` | info | `uptime_s, rss_mb, footprint_mb, cpu_pct, cpu_ms, threads, fds, fd_limit` + registered gauges |
| `metrics` | info | `window_s, counters{name:n}, timings{name:{n,avg,p50,p90,p99,max}}` |
| `panic` | incident | `message, location, thread, signature, frames[]` |
| `telemetry.dropped` | warn | `count` (channel was full) |
| `telemetry.upload_rejected` | warn | `status, lines` |

Each process adds its own catalog section below.

## Engine and Holder events

The Engine (`dirijord-rs`, `p: "engine"`) starts recording after it has
normalized its environment, runs the health sampler every 60 s, the uploader
(when `upload::endpoint()` names one), and the crash-report watcher. It
passes `--telemetry-state-dir <state dir>` to the Holder managers it
launches; a manager given that flag records as `p: "holder"` into the same
spool, and one launched without it (tests, manual `--spec` recovery) records
nothing. Holders record facts as they happen and run no sampler or timer.

`health` from the Engine carries these gauges: `sessions` (`records, live,
held, remote, hibernated, working, needs_input`), `clients` (open control
and data connections) and `attached` (terminal attachments, previews
excluded). `metrics` carries counters `rpc.calls, rpc.errors,
engine.connections, engine.accept_errors, attach.reseeds, remote.delta_gaps,
ssh.commands, ssh.channels` and timings `rpc, attach.seed, ssh.command`.

`modes` fields are `{mouse: "off"|"1000"|"1002"|"1003"|"unknown", sgr,
alt_screen, bracketed_paste, app_cursor}` from the Engine's own emulator. The
local Holder is a byte pipe with no parser, so terminal-mode facts are
recorded by the Engine, not the Holder.

### Engine process

| kind | sev | fields | catches |
|---|---|---|---|
| `engine.start` | info | `build, fd_soft, exit_when_orphaned` | which build ran; launchd fd limit not raised |
| `engine.login_path` | info | `ok, ms, shell, entries` | agents "not found" because the login PATH capture failed or timed out |
| `engine.duplicate_exit` | debug | | a relaunch racing the singleton lock |
| `engine.catalog` | info | `manifests, failed` | a short or unparsable Agent catalog |
| `engine.no_manifests` | incident | `failed` | the Engine refusing to start with no catalog |
| `engine.state_loaded` | info | `records, ms` | slow or empty state loads |
| `engine.state_quarantined` | incident | `error` | a corrupt state file (records moved aside) |
| `engine.state_unreadable` | incident | `io` | the Engine refusing to start over unreadable state |
| `engine.restore` | info | `adopted, records, live, ms` | sessions not coming back after an Engine restart |
| `engine.holders_lost` | warn | `count` | holders that died with the Engine or the machine |
| `engine.remote_restore` | info | `adopted, ms` | remote sessions not re-adopted after restart |
| `engine.bind_failed` | incident | `io` | the control socket could not be bound |
| `engine.accept_failed` | incident (EMFILE/ENFILE), error | `io` | descriptor exhaustion: blank terminals until resize (at most one a minute) |
| `engine.exit` | info | `reason: shutdown\|idle` | deliberate Engine exits (vs. crashes: no `engine.exit` before the next `process.start`) |
| `crash.report` | incident | `process, app_version, incident_id, crashed_at, timestamp, exception, signal, subtype, termination, namespace, code, thread, thread_name, signature, frames[]` | native crashes (SIGSEGV/abort in GPUI/objc) of `diri`, `dirijord-rs`, `diri-holder`, `dirijor`, `dirijor-mcp`, `diri-ssh-askpass` from `~/Library/Logs/DiagnosticReports/*.ips`, scanned at start and every 10 min past a watermark in `telemetry/crash_watermark.json` (first scan looks back 7 days). Frames are `image!symbol+offset`; no paths, registers or application-specific messages |
| `holder.manager_died` | incident | `pid, code, signal` | the Holder manager exiting abnormally (every local session with it) |

### Control RPC and clients

| kind | sev | fields | catches |
|---|---|---|---|
| `rpc.slow` | warn | `method, ms, ok, session` | any request over 250 ms (except `events.wait`, `task.get`) |
| `rpc.error` | error | `method, code, ms, session, message` | every error reply, with its structured code (`remote_transport_unavailable`, `initial_prompt_delivery_failed`, ...) |
| `rpc.op` | info | `method, session, ms` | successful lifecycle requests: spawn, kill, remove, archive, resume, fork, migrate, reconnect, hibernate/wake, account switch/login, worktree create/remove, shutdown, send_text |
| `client.hello` | info | `proto, build, ok` | stale or mismatched clients (`build` is the client's identity string) |
| `hook.report` | debug | `session, kind, event, parsed` | agent hooks arriving (or not) |

### Session lifecycle

| kind | sev | fields | catches |
|---|---|---|---|
| `session.spawn` | info | `session, agent, mode: fresh\|history, conv, host, project, worktree, parent, account, prompt, ms` | what was launched, where (`project` is a `path_hash`), with which conversation |
| `session.resume` | info | `session, agent, decision, conv, recorded_conv, transcript, host, status, exit_reason, archived` | resume decisions: `resume_verified` (transcript found), `resume` (id not verified), `fresh_unwritten` (Claude never wrote it; started fresh), `no_conversation`, `remote`, `already_live` |
| `session.launch` | info | `session, agent, transport: held_deferred\|held\|direct\|remote, ms` | every Session start, including resume, fork and account relaunches |
| `session.launch_failed` | incident | `session, agent, transport, stage: spawn\|holder_launch\|holder_wait, io, error, ms` | spawns that never produced a child (holder missing, manager down, exec errno) |
| `session.exec` | debug | `session, defer_ms, ms, cols, rows` | a deferred launch waiting on the first client size |
| `session.status` | debug | `session, from, to` | status transitions (`starting, idle, working, needs_input, exited, unknown`) |
| `session.exit` | info | `session, agent, code, signal, requested, runtime_s, adopted, modes` | how every PTY child ended; `requested` distinguishes kills from crashes |
| `session.early_exit` | incident | `session, agent, kind: exit\|returned_to_shell, code, signal, ms, modes` | an unrequested nonzero exit within 10 s of launch; or, for `returnToLoginShell` agents, the agent already gone and its login shell in the foreground 10 s after launch (a resume of a missing conversation: "No conversation found" → zsh) |
| `session.agent_exited` | info | `session, agent, source: session_end_hook, runtime_s, modes` | a wrapped agent that ended later and left its login shell (checked 2 s after Claude's `SessionEnd`) |
| `session.modes_left_on_exit` | warn | `session, agent, kind: pty_exit\|returned_to_shell, modes` | mouse tracking or bracketed paste still on after the program that enabled it exited: `^[[<35;14;25M` typed into the shell |
| `session.conversation` | info | `session, agent, conv, previous, source: hook\|cursor_store\|codex_repair` | conversation ids assigned, discovered or changed |
| `session.transcript` | debug | `session, path (hash), moved` | the transcript moving (worktree entry) |
| `session.lost` | info | `session, agent, conv, status` | each session whose holder was gone at Engine start |
| `session.adopted` | debug | `session, agent, hibernated, from_capsule` | each holder re-adopted at start |
| `holder.adopt_failed` | warn (stat), error (adopt) | `session, stage, error\|io` | a live holder the Engine could not re-adopt |
| `session.wake` | info | `session, reason, frozen_s` | a hibernated session thawed |
| `session.migrate` | info | `session, agent, from_host, to_host, transcript_migrated, warnings` | local↔remote handoffs |
| `governor.freeze` | info | `session, reason: idle\|memory_pressure, idle_s, footprint_mb, processes, idle_threshold_s` | sessions frozen and the evidence used (idle status, unattended, quiet CPU/output, no ports) |
| `governor.freeze_vetoed` | debug | `session, reason: listening_port, ports` | a freeze skipped for a serving tree |
| `governor.freeze_failed` | warn | `session, io` | SIGSTOP failing |

### Input and delivery

| kind | sev | fields | catches |
|---|---|---|---|
| `prompt.delivered` | info | `session, delivery: echo_verified\|blind_enter, chars, ms` | initial prompts and how they were confirmed |
| `prompt.delivery_failed` | error | `session, delivery, reason: session_ended\|submission_unconfirmed\|input_failed, chars, ms` | lost or unconfirmed initial prompts |
| `prompt.workspace_trust_accepted` | info | `session` | Claude's trust picker answered on the user's behalf |
| `message.deliver` | info | `session, delivery: sent\|unknown, duplicate, submit, chars` | agent-to-agent messages (MCP `send_prompt`) |

### Attach

| kind | sev | fields | catches |
|---|---|---|---|
| `attach.open` | debug | `session, preview, seed_bytes, ms` | slow seeds (blank pane on tab switch) |
| `attach.close` | debug | `session, preview, attached_s` | attachments ending |
| `attach.sink_dropped` | warn | `session, reason: backlog\|stalled, preview, attached_s` | a client that fell behind; it reattaches and is reseeded with a full grid |

### Remote

| kind | sev | fields | catches |
|---|---|---|---|
| `remote.connection` | info (incident when `to: failed`) | `session, from, to` | connecting/connected/reconnecting/failed/exited transitions |
| `remote.control_revoked` | warn | `session` | another controller took the session's lease |
| `remote.helper_error` | error | `session, code, fatal` | structured Helper errors (stale epoch, wrong incarnation, ...) |
| `remote.connection_fatal` | error | `session, reconnects` | protocol violations that fail the transport closed |
| `remote.uncertain_input` | error | `session` | input whose delivery could not be proven; the session fails closed |
| `remote.helper_ready` | info | `host, path: cached\|bootstrap\|reinstall, target, protocol, ms` | bootstrap and probe latency, artifact selection |
| `remote.helper_failed` | incident | `host, forced, io, error, ms` | bootstrap failures (the error keeps phase and status, never remote output) |
| `remote.helper_upload` | info | `host, target, bytes, ok, ms` | Helper uploads |
| `remote.persistence` | info | `host, capability: native-detach\|user-supervisor\|non-persistent` | persistence probe outcome |
| `remote.restore_skipped` | warn | `session, host, reason: helper_unavailable\|inspect_failed, io` | remote sessions left behind at Engine start |
| `ssh.command_failed` | warn | `phase, exit, signal, ssh_failure` | SSH exit codes per bootstrap/RPC phase (255 = OpenSSH itself: connect, auth, host key) |
| `ssh.command_timeout` | warn | `timeout` | SSH commands killed at their deadline |

### Accounts

| kind | sev | fields | catches |
|---|---|---|---|
| `account.switch` | info | `agent, switched, unchanged, deferred, failures, default_changed` | account switch outcomes |
| `account.switch_failed` | warn | `agent, session` | a tab that failed to follow the switch |

### Holder process (`p: "holder"`)

| kind | sev | fields | catches |
|---|---|---|---|
| `holder.manager_start` | info | `guard` | manager (re)starts; `guard: false` means crash cleanup is unavailable |
| `holder.manager_exit` | info | `ok` | idle retirement vs. accept failure |
| `holder.manager_failed` | incident | `error` | the manager exiting with an error |
| `holder.session_failed` | error | `session, error` | a session Holder that failed to run |
| `holder.spawn` | info | `session, cols, rows, ms` | the PTY child the Holder started |
| `holder.spawn_failed` | incident | `session, io` | PTY spawn/exec failures with errno |
| `holder.exit` | info | `session, code, signal, runtime_s` | the child's exit as the Holder reaped it |
| `holder.subscriber_dropped` | warn | `session, offset` | an Engine output subscriber too slow to keep up (it falls back to the log) |

<!-- App catalog: added by the app instrumentation. -->

## Upload

The Engine's uploader wakes once a minute. If `spool/urgent` exists, or ten
minutes have passed, it uploads. It sends every spool file's new complete
lines (across app, Engine and Holders) as gzip NDJSON, at most 4 MiB raw per
request:

```
POST {endpoint}/v1/ingest
Content-Type: application/x-ndjson
Content-Encoding: gzip
X-Diri-Install: <install uuid>
```

The first line is the batch header:

```json
{"v":1,"type":"batch","install":"<uuid>","support_id":"D-7K3MQ9XA","name":"julia","app_version":"0.9.0","build":"<sha>","channel":"stable","os":"macos","os_version":"27.0","arch":"aarch64","sent_at":1790581979447,"lines":1234}
```

The rest are records as above. Responses: `2xx` accepted; `400/413/422`
rejected permanently (the client skips the batch); `429/5xx` retried next
cycle.

## Worker (`telemetry/worker`)

Cloudflare Worker + R2 + D1.

- **Ingest** validates the header, caps sizes (5 MiB compressed, 64 MiB
  raw, 50k lines), rate-limits per install, stores the original gzip body in
  R2 at `v1/<install>/<yyyy-mm-dd>/<sent_at>-<rand>.ndjson.gz`, and indexes it
  in D1: the install (upsert), the batch (time range, processes, R2 key),
  incidents and errors (`s` of `error` or `incident`, with a grouping
  signature), and sessions seen (`session`, `agent`, `conv`, first/last seen).
- **Admin** endpoints under `/v1/admin/*` require
  `Authorization: Bearer <ADMIN_TOKEN>` (a Worker secret): find installs by
  name, support id or UUID prefix; list incidents (filter by install, kind,
  version, time) and group them; list batches in a time range; fetch a batch
  body; list sessions of an install or find the install that owns a session
  id or conversation UUID.
- **Retention**: R2 objects and D1 rows older than 30 days are deleted by a
  daily cron.

## CLI (`telemetry/cli`)

`diri-debug` answers "what happened to this person" from the admin API, and
reads a local spool with `--local` for the developer's own machine:

```
diri-debug who julia                        # installs matching a name / support id
diri-debug incidents [julia] --since 7d     # recent incidents
diri-debug top --since 7d --version 0.9.0   # grouped incidents across installs
diri-debug timeline julia --around "2026-09-27 23:40" --window 20m [--session s_…] [--kind 'session.*']
diri-debug health julia --since 24h         # memory / fds / cpu / frame-time trends per process
diri-debug sessions julia                   # sessions, agents, conversations
diri-debug find <session id | conversation uuid>
diri-debug local [--since 1h] [...]         # same views over ~/Library/Application Support/Dirijor/telemetry/spool
```

Configuration: `DIRI_TELEMETRY_URL` and `DIRI_TELEMETRY_ADMIN_TOKEN`, or
`~/.config/diri-debug/config.json`.
