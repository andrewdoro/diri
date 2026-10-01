# Supported agents

> Every coding agent diri ships a manifest for, what each supports for status, resume, fork and MCP, and how to add your own agent with a custom manifest.

diri knows how to launch and read 20 terminal coding agents, plus a plain shell. Each one is described by a JSON manifest that says how to start it, how to resume it, and how to tell from its screen whether it is working, idle or waiting for you.

## Agent catalog

Each agent also has its own page with setup steps and what diri adds: see [all agents](/agents/), for example [Claude Code](/agents/claude-code/) or [Codex](/agents/codex/).

| Agent | Command | Status from | Resume | Fork | Quick approve | diri MCP | Hooks |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Claude Code | `claude` | Hooks | Exact conversation | Yes | Yes | Auto | Yes |
| Codex | `codex` | Screen | Exact conversation | Yes | Yes | Auto | Turn-complete notify |
| Cursor | `cursor-agent` | Screen | Exact conversation | No | Yes | Auto | Yes |
| Gemini | `gemini` | Screen | Exact conversation | No | Yes | No | No |
| Aider | `aider` | Screen | Latest in folder | No | No | No | No |
| Amp | `amp` | Screen | No | No | No | No | No |
| Antigravity | `agy` | Screen | Latest in folder | No | No | No | No |
| Cline CLI | `cline` | Screen | No | No | Yes | No | No |
| Copilot CLI | `copilot` | Screen | Latest in folder | No | Yes | No | No |
| Devin | `devin` | Screen | Latest in folder | No | No | No | No |
| Droid | `droid` | Screen | Latest in folder | No | No | No | No |
| Grok | `grok` | Screen | Latest in folder | No | Yes | No | No |
| Hermes | `hermes` | Screen | Latest in folder | No | No | No | No |
| Kilo Code | `kilo` | Screen | Latest in folder | No | No | No | No |
| Kimi | `kimi` | Screen | Latest in folder | No | Yes | No | No |
| Kiro | `kiro-cli` | Screen | Latest in folder | No | No | No | No |
| Maki | `maki` | Screen | Latest in folder | No | Yes | No | No |
| OpenCode | `opencode` | Screen | Latest in folder | No | Yes | No | No |
| Pi | `pi` | Screen | Per session | No | No | No | No |
| Qoder CLI | `qodercli` | Screen | Latest in folder | No | No | No | No |
| Shell | your login shell | Process only | No | No | No | No | No |

What the columns mean:

| Column | Meaning |
| --- | --- |
| Status from | **Hooks**: the agent reports its own lifecycle events. **Screen**: diri reads the terminal with manifest rules. **Process only**: diri knows only whether the process is alive. |
| Resume | **Exact conversation**: diri knows the conversation id and reopens that one. **Per session**: diri gives the agent its own storage folder per session, so "continue" picks the right one. **Latest in folder**: the agent's own "continue the most recent session" flag, which can pick a different conversation if you ran the agent elsewhere in the same folder. |
| Fork | Start a new conversation that branches from this one. |
| Quick approve | The manifest defines a safe one-key answer for permission prompts. |
| diri MCP | diri's [MCP server](/docs/mcp/) is added to the agent at launch, so it can start and coordinate other agents. Any other agent can be connected by hand. |
| Hooks | diri installs the agent's hook or notify callback for more accurate status. |

> [!NOTE]
> The table reflects the manifests in [diri/crates/diri-engine/manifests](https://github.com/cristicretu/diri/tree/main/diri/crates/diri-engine/manifests). Agents change their screens often. If status looks wrong, see [Troubleshooting](/docs/troubleshooting/).

## Install and detect agents
diri does not bundle any agent. It finds the CLIs already on your machine.

Open **Settings → Agents**:

| Control | What it does |
| --- | --- |
| **Execution target** | Pick your Mac or a [remote host](/docs/remote-hosts/). Each has its own agent list. |
| **Refresh** | Rescan for installed agents. |
| **Add…** | Point an agent that was not found at its executable. Shown as **Change** once an agent is found. |
| **Install** | For agents with a published one-line installer, shows the full command in a confirmation sheet. Only after you confirm does diri type it into a Terminal session in your home folder. Offered for this Mac only. |
| **Quick** | Include the agent in quick-create menus. |

On your Mac, diri looks for agents on the `PATH` from your login shell, then in common user install folders such as pnpm, Bun, Cargo, mise and Volta. A path you choose with **Add…** wins over `PATH`. Missing agents stay listed in Settings but are hidden from quick-create menus.

## Add your own agent
You can add an agent diri does not ship, or replace a built-in manifest, without building diri.

1. Write a JSON manifest. The filename, top-level `id` and `agent.id` must match, for example `my-agent.json` with `"id": "my-agent"`.
2. Put it in the overrides folder:

| Platform | Overrides folder |
| --- | --- |
| macOS | `~/Library/Application Support/Dirijor/manifests/overrides/` |
| Linux | `~/.config/diri/manifests/overrides/` |

3. Restart the Engine. The catalog is read once when the Engine starts. Quitting diri with no running sessions stops the Engine, and opening diri starts it again.

A file with the same `id` as a built-in manifest replaces it. A new `id` adds an agent. A malformed file is skipped and the rest of the catalog still loads.

### A minimal manifest
This screen-driven manifest is enough for most terminal agents:

```json
{
  "schemaVersion": 2,
  "id": "my-agent",
  "version": "2026.10.01.1",
  "statusModel": "full",
  "agent": {
    "id": "my-agent",
    "displayName": "My Agent",
    "shortLabel": "my-agent",
    "aliases": ["myagent"],
    "firstClass": true,
    "statusAuthority": "screen",
    "binary": "my-agent",
    "returnToLoginShell": true,
    "approve": { "text": "y", "submit": true }
  },
  "rules": [
    {
      "id": "permission",
      "state": "blockedPermission",
      "priority": 1000,
      "region": "bottom_non_empty_lines",
      "regionLines": 8,
      "when": { "contains": "allow this command?" }
    },
    {
      "id": "working",
      "state": "working",
      "priority": 900,
      "region": "bottom_non_empty_lines",
      "regionLines": 3,
      "when": { "contains": "esc to cancel" }
    },
    {
      "id": "idle",
      "state": "idle",
      "priority": 500,
      "region": "bottom_non_empty_lines",
      "regionLines": 1,
      "when": { "lineRegex": "^>\\s*$" }
    }
  ]
}
```

### Essentials

| Field | What it does |
| --- | --- |
| `binary` | The command to run. diri runs it directly, never through a shell string. |
| `statusModel` | `full` for rule-driven status, `processOnly` for liveness only. |
| `returnToLoginShell` | When the agent exits, leave a login shell in the tab instead of closing it. |
| `approve`, `deny` | Text typed for quick approve or deny. Omit `approve` when no answer is always safe. |
| `conversation` | Argument lists for fresh, resumed and forked conversations, using `{id}`, `{newId}` and `{sessionDir}`. |
| `rules` | Checked from highest to lowest `priority`. The first match sets the status. |

Rule states are `working`, `idle`, `blockedPermission`, `blockedQuestion` and `skip`. Predicates are `contains`, `regex`, `lineRegex`, `progress`, and `any`, `all` and `not` to combine them. Regexes use Rust's `regex` syntax, which has no lookaround or backreferences.

Put blockers around priority 1000, working around 900 and idle around 500, so a permission form beats a spinner still visible behind it. Match several literal strings the agent really draws rather than one broad regex.

> [!WARNING]
> The `injection` switches (MCP and hooks) select code that diri already implements for specific agents. Setting them for another CLI does not make it work.

### Validate it
- Start the agent from diri and walk it through idle, working and a permission prompt.
- If a state is wrong, open **Session Inspector → Info → Why Diri thinks this** and use **Copy status debug info** to see which rule matched.
- When contributing a manifest to diri, run the engine tests, which decode every bundled manifest and reject unsupported regexes:

```sh
cd diri
cargo test -p diri-engine
```

The full schema, regions, capture settings and safe fixture capture are in [docs/AGENT-MANIFESTS.md](https://github.com/cristicretu/diri/blob/main/docs/AGENT-MANIFESTS.md).
