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
