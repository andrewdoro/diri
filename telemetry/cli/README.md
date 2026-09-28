# diri-debug

`diri-debug` answers "what happened to this person" from diri's telemetry. It reads the ingest Worker's admin API (`../worker`), or with `local`, a spool directory on this Mac. It is plain Node 24 ESM with no dependencies and no build step.

```sh
node telemetry/cli/bin/diri-debug.mjs --help
# or put it on PATH:
ln -s "$PWD/telemetry/cli/bin/diri-debug.mjs" ~/.local/bin/diri-debug
```

## Configure

Set the Worker's URL and the `ADMIN_TOKEN` secret, either in the environment:

```sh
export DIRI_TELEMETRY_URL=https://telemetry.diri.sh
export DIRI_TELEMETRY_ADMIN_TOKEN=…
```

or in `~/.config/diri-debug/config.json` (keep it `chmod 600`):

```json
{ "url": "https://telemetry.diri.sh", "token": "…" }
```

`local` needs neither. It reads `~/Library/Application Support/Dirijor/telemetry/spool`, or the directory given with `--dir`.

## Commands

| Command | What it shows |
|---|---|
| `who <name \| support id \| uuid prefix>` | matching installs: Support ID, name, version, OS, last and first seen |
| `incidents [who] [--since 7d]` | errors and incidents, newest first; without `who`, across every install |
| `top [--since 7d] [--version v]` | incidents grouped by signature: count, installs affected, first and last seen, versions |
| `timeline <who> --around T [--window 20m]` | every record of every process in the window, merged in time order; health and metrics samples are folded into a per-process summary |
| `health <who> [--since 24h]` | per process (`p:pid`): sparklines of rss, footprint, fds, threads, cpu and frame/RPC p99, plus leak hints |
| `sessions <who>` | sessions with their agent and every conversation they ran |
| `find <session id \| conversation uuid>` | which install owns it, and when it was seen |
| `raw <batch key>` | one uploaded batch, decompressed, header first |
| `local [timeline\|incidents\|top\|health\|sessions\|find]` | the same views over a local spool (`timeline` is the default) |

`<who>` can be a name (`julia`), a Support ID (`D-7K3MQ9XA`) or an install UUID or prefix. When a name matches several installs, the command lists them and asks for a Support ID.

Flags shared by the commands:

| Flag | Meaning |
|---|---|
| `--json` | machine-readable output (every command) |
| `--since`, `--until` | `2h` (ago), `2026-09-27 23:40`, `23:40` (its latest occurrence), ISO 8601 or epoch ms |
| `--around T --window 20m` | a window of that total width, centred on `T` |
| `--session`, `--conv` | only records naming that session or conversation |
| `--kind 'session.*,rpc.*'` | glob over event kinds |
| `--sev incident \| warn,error \| warn+` | exact severities, or at least one |
| `--proc engine` | only one process kind (`engine`, `app`, `holder`) or instance (`holder:4242`) |
| `--all` | `timeline`: show `health`/`metrics` samples inline |
| `--full` | do not shorten long field values |
| `--utc` | read and print UTC instead of local time |

Timeline lines read as `local time, process, pid, severity mark, kind, fields`. The marks are `·` debug, blank info, `W` warn, `E` error, `!` incident. On a terminal, incidents are bold red.

```
09-27 23:38:00.000 engine    812   session.spawn          session=s_26bf32debd4c agent=claude-code mode=resume conv=0a40e747-…
09-27 23:38:02.500 holder   4242 ! session.exit           session=s_26bf32debd4c code=exit_1 exit=1 ms=2100
09-27 23:38:02.600 engine    812 ! session.resume_failed  session=s_26bf32debd4c agent=claude-code conv=0a40e747-… code=no_conversation
09-27 23:39:10.000 app       900 W terminal.mouse_mode_stuck session=s_26bf32debd4c mode=1003 window=w1
```

A leak hint appears when a series' floor keeps rising. The minimum of each tenth of the run must be at or above the previous one in at least 80% of steps. The growth must also be at least 20% of the starting value and at least 50 MB (rss/footprint), 20 fds or 10 threads, over at least 30 minutes. The command also warns when fds reach 80% of the limit.

## Test

```sh
cd telemetry/cli && node --test "test/*.test.mjs"
```

The tests run against a fixture spool and a stub of the admin API. The Worker's own tests cover the real API.
