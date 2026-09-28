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

<!-- Engine and Holder catalog: added by the engine instrumentation. -->

## App (`crates/diri-app`, `diri-client`, `diri-term`)

Started in `main` via `telemetry::start` (never in tests, headless previews or
`DIRI_SETTINGS_PREVIEW`): `init_default(App)`, panic hook, 60 s health
sampler. Controls live in Settings › General › Privacy (upload toggle, name,
Support ID, *Show in Finder*) and Help › Report a Problem…. There is no About
surface, so the Support ID is shown only in Settings. The first run of a
recording build shows one 20 s toast (*diri shares diagnostics*) and writes
the default `telemetry/config.json`; the file's existence is the "seen" mark.

**Health gauges:** `windows_main`, `windows_floating` (menus, palette,
popovers: each is a window), `windows_opened` (lifetime), `terminal_panes`,
`attached_sessions` (mounted session transports), `app_active`.

**Metrics** (`timings` unless noted): `ui.frame` (root render → last paint
of a main window), `term.paint` (one terminal element's prepaint + paint),
`input.echo` (input → next screen change, first input of a burst, ≤ 2 s),
`pane.attach`, `pane.first_grid`, `pane.first_paint`, `client.connect`,
`rpc.<method>` per control method; counters `rpc.calls`, `rpc.errors`,
`rpc.disconnected`, `pane.reseed`, `pane.attach_retries`,
`pane.input_rejected`.

**Stall watchdog:** a background thread posts a ping to the main thread once
a second (every 5 s while diri is not frontmost); the answer's latency is the
stall. Idle cost is one wakeup per interval per side and no main-thread timer.
A ping unanswered for 5 s is recorded and flushed before the stall ends, so a
hang that ends in Force Quit still leaves a record. Durations are lower bounds
(± one interval).

| kind | sev | fields | catches |
|---|---|---|---|
| `app.launch` | info | `ms` (main → first painted frame), `version`, `windows` | slow launches; the app's version (`process.start.version` is the recorder crate's) |
| `app.activate` / `app.deactivate` | info | | context for stalls, OSC 52 refusals |
| `app.sleep` / `app.wake` | info | | gaps that are sleep, not hangs; reconnect storms after wake |
| `app.quit` | info | `uptime_s, windows_main, windows_opened` | clean exit vs crash (a timeline that just stops) |
| `window.open` / `window.close` | info (main), debug (floating) | `kind` (`main`\|`floating`), `window`, `lived_s`, `open` | window churn vs RSS growth (closed-window leaks) |
| `ui.frame` → `ui.slow_frame` | warn | `ms, window, surface` (`workbench`\|`settings`\|`palette`\|`launcher`), `workspace` | "diri is slow/janky"; frame ≥ 50 ms |
| `ui.stall` | warn (1–3 s), incident (≥ 3 s) | `ms, ongoing, active` | beachballs, hangs; `ongoing=true` is written at 5 s while still stuck |
| `ui.action` | debug | `action` (GPUI action name), `source` (`shortcut`\|`palette`) | what the user did just before a failure |
| `ui.toast` | info | `title` (static toast title) | errors the user was shown ("Terminal", "Target unavailable", …) |
| `privacy.notice_shown` / `privacy.upload_changed` | info | `upload` | consent history |
| `settings.privacy_save_failed` | error | `error` (io kind) | toggle that doesn't stick |
| `user.report` | incident | `version, support_id` | Help › Report a Problem…: the moment to look around |
| `client.connected` | info | `reconnect, attempts, down_ms, connect_ms, hello_ms, first_failure, engine_build, engine_pid, proto` | slow Engine start, how long an outage lasted |
| `client.disconnected` | warn | `kind, error, connected_s` | Engine crash/restart seen from the app |
| `client.connect_failing` | error | `attempts, down_ms, kind, error, handshake` | Engine never came up (≈ 45 s of retries) |
| `client.identity_rejected` | warn (`instance_changed`), error | `reason, engine_kind, engine_build, engine_pid, proto` | stale/foreign daemon on the socket |
| `rpc.error` | error | `method, kind, code, error, ms` | failing spawn/resume/kill… by method and Engine error code |
| `rpc.slow` | warn | `method, ms` (≥ 2 s; not `events.wait`/`test.run`) | slow Engine operations |
| `attach.closed` | warn, error (decode) | `session, reason` (`eof`\|`read_error`\|`write_error`\|`keepalive_timeout`\|`decode_error`\|`bad_grid`\|`bad_modes`\|`commands_closed`), `live_ms` | why a terminal connection dropped; protocol corruption |
| `pane.attached` | info | `session, reconnect, attempts, connect_ms, since_mount_ms` | attach latency, reattach loops |
| `pane.attach_failing` | error | `session, attempts, reason, error, since_mount_ms` | a session that cannot be attached (3 failures) |
| `pane.first_grid` | debug; warn if not a snapshot | `session, ms, snapshot` | first frame missing or a diff before a seed |
| `pane.first_paint` | debug | `session, ms, grid_ms, parked` | attach → first painted content |
| `pane.blank` | incident; warn if live with a (blank) grid | `session, agent, state, got_grid, frames, ms` | "session doesn't render": visible, running, nothing painted 10 s after mount |
| `pane.detached` | warn | `session, live_ms, grids, reseeds` | "Terminal connection interrupted" toast |
| `pane.drain_interrupted` | warn | `session` | input possibly lost on detach |
| `pane.input_rejected` | warn (≤ 1 per 5 s per session) | `session, input` (`input`\|`mouse`\|`mouse_motion`\|`scroll`), `reason` (`passive_view`\|`disconnected`\|`overloaded`) | typing that goes nowhere; lost lease |
| `pane.resize_storm` | warn (≤ 1/min) | `session, flips, cols, rows` | layouts fighting over the PTY size |
| `pane.modes` | debug | `session, mouse, mouse_bits, alt_screen, bracketed_paste` | mouse tracking left on after an agent exits (`^[[<35;…M` in zsh) |
| `pane.drop` | info | `session, files, outcome` (`paste`\|`upload`\|`refused`), `partial, remote` | Finder drops that did nothing |
| `pane.drop_upload_failed` | error | `session` | remote drop copy failed |
| `clipboard.copy` | info | `source` (`selection`\|`osc52`), `outcome` (`ok`\|`not_on_pasteboard`\|`empty_selection`\|`relayed`\|`stale`\|`app_inactive`\|`unknown_session`\|`no_listener`), `size`, `ms`/`age_ms`, `mouse_captured`, `session` | "copy doesn't work" (incl. agent-captured mouse) |
| `clipboard.write_failed` | error | `source, size` | an agent's OSC 52 copy that never reached the pasteboard |
| `clipboard.paste` | info | `outcome` (`sent`\|`review`\|`into_find`\|`image_staged`\|`image_stage_failed`\|`empty_clipboard`\|`no_session`\|`no_terminal`\|`no_text`\|`copy_mode`\|`ignored_in_find`), `kind, size, bracketed, ms` | "paste doesn't work" |
| `clipboard.image_upload_failed` | error | `session` | image paste into a remote session |
| `term.slow_paint` | warn | `ms, cols, rows, shape_misses` | one terminal paint ≥ 50 ms |
| `update.check` / `update.download` / `update.install` | info; error on failure | `outcome, from, to, user_initiated, ms, error_kind, error` | updates that fail or never arrive |

Sizes are buckets (`0`, `<64`, `<1k`, `<16k`, `<256k`, `<1m`, `>=1m`); no
clipboard, paste, keystroke or terminal content is ever recorded.


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
