---
title: Remote hosts
description: Run agents on your own servers over SSH. diri reuses your OpenSSH setup and needs no tmux, sudo or remote service. Learn what survives a dropped connection.
---
diri can run an agent on any machine you reach with `ssh`. The agent runs on the server, its row in the sidebar works like a local one, and on most hosts it keeps running when your connection drops. For a walkthrough with screenshots, see [Run agents on your own server](/guides/remote-sessions/).

## What you need
- A server you can already reach with `ssh`, including aliases from `~/.ssh/config`.
- A project folder on that server.
- The agent CLI installed and signed in on the server. diri never copies your local agent logins to it.

Nothing else. The server does not need `tmux`, `screen`, Node.js, Python, `curl`, a preinstalled diri service, or administrator rights. diri never runs `sudo` and never changes host-wide configuration.

### Supported servers

| Platform | Supported |
| --- | --- |
| Linux x86_64 | Yes |
| Linux aarch64 (arm64) | Yes |
| macOS on Apple silicon | Yes |
| macOS on Intel | No. The host reports `unsupported-platform`. |

## Add a host
1. Open **Settings → Remote** and click **Add Host**.
2. Fill in the form:

| Field | What to enter |
| --- | --- |
| Name | A label for the host, such as `Forge`. |
| SSH destination | What you would type after `ssh`, for example `you@forge` or an alias from `~/.ssh/config`. |
| Default folder | Where the folder picker starts. Without one, it starts in the remote home folder. |

3. Click **Add Host**.

diri then connects, checks the platform, uploads and verifies its helper, loads the remote login environment, and tests whether sessions survive a disconnect. When it finishes, the card shows the folder, helper build, protocol version and persistence result, with **Use by default** to make it the default host.

Host settings are saved in `hosts.json`, in `~/Library/Application Support/Dirijor` on macOS and `~/.config/diri` on Linux.

To start a session on the host, open **New Agent**, click the folder row and pick the host under **Machine**. Then browse to a folder on it. The same path on two hosts counts as two different projects.

## How it works
SSH is only the authenticated, encrypted pipe. diri does not use the SSH terminal for the agent.

1. On first use, diri uploads a small helper program, `diri-remote`, built for that exact platform and shipped inside the app. It never downloads a binary from a URL the server chooses.
2. The upload lands in a temporary file first. diri checks its length, SHA-256, build ID and protocol before moving it into place.
3. Each session gets its own **Holder**: one helper process that owns the agent's terminal, the agent's processes and the current screen.
4. diri sends the agent launch as a structured argument list, folder and environment. It does not build shell command strings.

Helpers live in a private cache under your remote account:

| Path | Contents |
| --- | --- |
| `~/.cache/diri/bin/protocol-<major>/<build-id>/diri-remote` | Helper binaries, one per version |
| `~/.local/state/diri/sessions/<session-id>/` | Per-session state, socket and output log |

The state path follows `XDG_STATE_HOME` when it is set. Directories are `0700`, files and sockets are `0600`, and the helper binary is `0700`. Several helper versions can sit side by side. An update never replaces a helper that a running session still uses.

> [!NOTE]
> After you update diri, the first remote action installs the matching helper if needed. To force a fresh install, open the host in **Settings → Remote** and click **Reinstall Environment**. Running sessions are not interrupted.

### Environment on the server
diri reads your login shell from the server's user database and captures the login environment there, so tools installed with Homebrew, `nvm`, `mise` or in user folders are found. Your local environment is not copied over. Local `DIRI_` and `SSH_` variables, local sockets and credentials stay on your machine.

Each host has its own agent list. Open **Settings → Agents**, choose the host under **Execution target**, and click **Refresh** to rescan it. You can point an agent at a specific executable on that host with **Add…** or **Change**. The one-click **Install** button is offered only for your own machine; remote hosts keep the guide link.

## Persistence: what survives a disconnect
Some servers kill every process from a login session when you log out. diri tests this instead of assuming. It starts a temporary Holder, closes the SSH connection, reconnects on a new connection, and checks whether the same process is still there.

| Result | What it means for you |
| --- | --- |
| native detach | Sessions keep running on their own after you disconnect. |
| user supervisor | The server would stop detached processes, so diri runs sessions under a supervisor your account already has, without configuring anything. They keep running after you disconnect. |
| non-persistent | The server may stop the session when the connection closes. diri still lets you start sessions, but the row shows a **No detach** warning. Stay connected, or use another server. |

diri never installs a service or a persistent user unit, never enables lingering, and never falls back to `tmux` to get around a non-persistent result.

> [!WARNING]
> No result survives the server itself rebooting.

### After a dropped connection
On a host that keeps sessions alive, the agent keeps working while you are offline. The Holder keeps the agent's processes, the current screen and up to 4 MiB of scrollback.

- The terminal shows **Connection lost · Last received screen** until diri gets back in.
- diri reconnects to the same session and redraws the current screen. Click **Reconnect** in the terminal to retry by hand, or run `dirijor session reconnect <id>`.
- Input whose delivery is uncertain is not replayed. Check the screen before retyping.
- Do not start a duplicate agent because the connection blinked.

Only one diri window controls a session at a time. A new attach takes over and the old one is disconnected.

## Passwords and host keys
diri runs your normal OpenSSH client, so keys, agents, `ProxyJump` and the rest of `~/.ssh/config` work as usual. diri may keep a short-lived ControlMaster connection open to make repeat connections faster. Sessions never depend on it.

When OpenSSH needs to ask you something, diri shows a native prompt instead of reading the answer itself:

| Prompt | Dialog |
| --- | --- |
| New host key | **Verify SSH host**, with **Allow** and **Cancel** |
| Password or key passphrase | **SSH authentication**, with a secure field and **Connect** |

The prompt helper has no logging and no connection to the Engine, so what you type never reaches session state or diagnostics. On Linux it needs `zenity` or `kdialog`. Without either, use key-based authentication or load your key into `ssh-agent` first.

## Troubleshooting

| Problem | What to do |
| --- | --- |
| `remote_transport_unavailable` | This build has no valid remote helper catalog. Install an official release instead of a local development build. diri fails closed rather than falling back to another transport. |
| `unsupported remote platform` | The server is not Linux x86_64, Linux aarch64 or Apple silicon macOS. |
| Artifact length, SHA-256 or protocol mismatch | The bundled helper is damaged or does not match the app. Reinstall diri. |
| Agent missing on the host | Install it on the server so it is on your login shell's `PATH`, then **Refresh** under **Settings → Agents** with that host selected. |
| **No detach** warning | The host is non-persistent. Keep the connection open or use another server. |
| `remote_reconnect_failed` or `remote_owner_unavailable` | The host is unreachable or the session has no live binding. Try again when the host is back. |
| Remote usage shows "Usage unavailable" | diri retries every 5 minutes. Check that the host still connects in **Settings → Remote**. |

Run `ssh <destination>` in a terminal first. If that fails, diri cannot connect either. See [Troubleshooting](/docs/troubleshooting/) for logs and bug reports.

## Advanced: diri-node
`diri-node` is an optional per-user service for a VPS that you run on purpose. It adds per-machine account logins, fleet usage totals and moving Claude or Codex conversations between machines. It is not needed for ordinary remote sessions, and SSH stays configured as the install and recovery path.

- Run it as an ordinary user, never as root, under a systemd user service.
- Bind it only to loopback or a Tailscale address. Public TCP is unsupported.
- Enrollment gives you a token. Store it in an owner-only file on your Mac.

In **Settings → Remote**, edit the host and fill in the **First-party node (optional)** fields: **Node endpoint** (for example `tcp://100.64.0.2:7337`), **Local token file** and **Pinned node ID**. The token stays in that file. Setup steps are in [diri/NODE.md](https://github.com/cristicretu/diri/blob/main/diri/NODE.md).

## Learn more
- [Run agents on your own server](/guides/remote-sessions/)
- [Security model](/docs/security/)
- [Remote architecture](https://github.com/cristicretu/diri/blob/main/diri/REMOTE_PORT.md)
