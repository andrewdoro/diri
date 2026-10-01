# Troubleshooting

> Diagnose diri problems with dirijor doctor, find logs and state on macOS and Linux, fix common agent and remote issues, and file a useful bug report.

Start with `dirijor doctor`, then check the problem list below. If you still need help, file a bug with the diagnostics report from Settings.

## Run dirijor doctor
`dirijor doctor` checks the parts of diri that run without the window:

```sh
/Applications/diri.app/Contents/Resources/bin/dirijor doctor
```

On Linux, run `dirijor doctor`. The output looks like this:

```text
✓ Rust Engine reachable (build ..., pid 4242, proto 1)
✓ Claude Code (claude) found at /Users/you/.local/bin/claude
✗ Aider (aider) not found on PATH
✓ state file present at /Users/you/Library/Application Support/Dirijor/state.json
```

| Line | Meaning |
| --- | --- |
| Engine reachable | The background Engine answers on its socket. If it is unreachable, doctor exits with code `4`. |
| Agent found or not found | Whether each known agent's command is on the `PATH` of the shell you ran doctor from. |
| State file | Whether the Engine's saved session state exists. |

Agent checks use your terminal's `PATH`. **Settings → Agents** shows what diri itself detected, which is the result that counts. See the [CLI reference](/docs/cli/) for every command.

## Logs and state

| Platform | What | Path |
| --- | --- | --- |
| macOS | Engine state, sockets, host config, manifest overrides | `~/Library/Application Support/Dirijor` |
| macOS | Engine log | `~/Library/Application Support/Dirijor/logs/dirijord.log` |
| macOS | Diagnostics log | `~/Library/Application Support/Dirijor/telemetry/spool` |
| macOS | App preferences and caches | `~/Library/Application Support/diri` |
| macOS | Downloaded updates | `~/Library/Caches/diri/updates` |
| Linux | Session state and logs | `~/.local/state/diri` |
| Linux | Engine log | `~/.local/state/diri/logs/dirijord.log` |
| Linux | Host config, preferences, manifest overrides | `~/.config/diri` |
| Linux | Data | `~/.local/share/diri` |
| Linux | Cache | `~/.cache/diri` |

On Linux the `XDG_*` variables override these roots. On a remote host, each session's files are under `~/.local/state/diri/sessions/` and helpers under `~/.cache/diri/bin/`.

> [!WARNING]
> Logs and terminal replay files can contain prompts, command output, personal paths and credentials. Share only the lines that matter and redact the rest.

## Common problems

### An agent is not detected
1. Open **Settings → Agents** and click **Refresh**.
2. Check that the agent runs in a new terminal window. diri reads the `PATH` from your login shell, then looks in common user install folders such as pnpm, Bun, Cargo, mise and Volta.
3. If it is installed somewhere unusual, click **Add…** next to the agent and choose its executable.

For a remote host, pick the host under **Execution target** first. The agent has to be installed on the server and on the `PATH` of your login shell there.

### A tab shows a shell instead of the agent
Most agents return to your login shell when they exit, so the tab stays useful. Scroll up to see why the agent stopped. To start it again, type its command or use the exit card's **Resume Conversation** where offered.

If a custom agent opens a bare shell from the start, its manifest may have failed to load. A malformed manifest file is skipped without an error dialog. Check the file name, `id` and JSON against [Add your own agent](/docs/agents/).

### A session shows as exited
The agent's process ended. The pane shows why, with **Resume Conversation** for agents that can resume, or **Restart Terminal** for shells. Agents that resume only the "latest in folder" conversation can reopen a different one if you used them elsewhere in the same folder. See the resume column in [Supported agents](/docs/agents/).

### Status looks wrong
Open **Session Inspector → Info → Why Diri thinks this** and click **Copy status debug info**. It shows which detection rule matched and its timing, without a screenshot or your prompt. Paste it into a bug report.

### Agent output has no colour
diri sets `TERM=xterm-256color` and `COLORTERM=truecolor` for every agent it starts and removes inherited `NO_COLOR`, `FORCE_COLOR`, `CLICOLOR` and `CLICOLOR_FORCE`. If an agent is still monochrome, check whether your own shell startup files or the agent's settings turn colour off.

### Remote errors

| Error | What to do |
| --- | --- |
| `remote_transport_unavailable` | This build cannot run remote sessions. Install an official release. |
| `unsupported remote platform` | Use a Linux x86_64, Linux aarch64 or Apple silicon macOS server. |
| **No detach** on a remote session | The host may stop sessions when SSH disconnects. Stay connected or use another host. |
| **Connection lost · Last received screen** | diri is reconnecting. Click **Reconnect** to retry now. |
| Prompt for password never appears (Linux) | Install `zenity` or `kdialog`, or use key-based login. |

More in [Remote hosts](/docs/remote-hosts/).

## Report a bug
1. Check for an existing issue and try the latest release.
2. Open **Settings → General → Support** and click **Copy diagnostics**. Review the preview, then paste it into the issue. It includes app, platform and Engine details, agent availability, remote host reachability and storage reachability. It does not include raw logs.
3. Or choose **Help → Report a Problem…**. It marks the moment in the diagnostics log, sends it, copies your Support ID and opens a GitHub issue with the ID filled in.

File it with the [bug report form](https://github.com/cristicretu/diri/issues/new?template=bug_report.yml). Include:

- Steps to reproduce, what you expected and what happened.
- diri version, OS version and how you installed diri.
- The agent CLI's version, and whether the session was local or on an SSH host.
- On macOS, your chip. For Linux display problems, the display server, desktop environment, GPU and driver.

Questions about setup and workflows go to [Discussions](https://github.com/cristicretu/diri/discussions). Report security problems privately, as described in the [security model](/docs/security/).
