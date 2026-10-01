# Accounts and usage

> Save Claude Code and Codex logins as profiles, switch every open tab to another account in one click, and see estimated cost, tokens and plan limits.

diri can remember more than one Claude Code or Codex login, such as work and personal, and switch every open tab of that agent between them. It also estimates what you spend from the agents' own transcripts and shows how much of your plan's limits you have used.

## Account profiles
A profile is a named login for one agent on one machine. Profile names are your own labels. diri does not check them against an email address.

Open **Settings → Accounts**, or choose **Manage accounts…** in the account menu at the bottom left of the sidebar.

1. Click **Add profile** and choose the agent.
2. Give it a name, such as `Work`.
3. Save it with one of these:

| Action | What it does |
| --- | --- |
| **Save current login** | Stores the login already active on this Mac in the profile. It replaces anything the profile held before. It does not switch accounts. |
| **Sign in** | Opens a login-only tab. Finish the browser login, then close the tab. The new login goes into the profile's private store. It does not change the active login. |

Each profile can also choose where it runs with **Run on** (this Mac or one of your [remote hosts](/docs/remote-hosts/)) and can be marked **Use by default for this Agent on this host**.

## Switch accounts
Choose an account in the account menu at the bottom left. diri then:

1. Stops that agent's conversations open in tabs and split panes.
2. Installs the selected login.
3. Resumes the same conversations on the new login. Sleeping tabs restart and go back to sleep. Stopped tabs stay stopped.
4. Makes that account the default for new tabs.

Nothing is copied or migrated. Conversations, MCP setup and settings stay in the agent's usual home folder (`~/.claude` or `~/.codex`).

| Agent | How the switch works |
| --- | --- |
| Codex | Swaps only `auth.json` in `~/.codex`. Open tabs relaunch with `codex resume <thread>`. |
| Claude Code | Each profile owns a private credential store, so no tokens are copied. Open tabs relaunch with `claude --resume <conversation>`. A tab that has not saved a transcript yet starts fresh with the same conversation id. |

> [!WARNING]
> Running tools are interrupted, not replayed. Let long tool calls finish before you switch.

### Things to know
- A switch is never refused because of one tab. A Codex tab diri cannot identify keeps the previous login and is reported. It switches the next time it restarts.
- Closed and archived sessions are not restarted. They use whatever login is current when you resume them.
- Agent processes started outside diri are not restarted and may keep their cached login.
- Hosted connectors are tied to the provider account, not to files on your Mac. After a switch, a connector such as Slack can show as not installed until you connect it on the selected account.
- Codex switching works with Codex's file credential store. If Codex is set to use the Keychain or another backend, the switch stops with an error before changing anything.
- Editing or removing a profile affects future launches. Running sessions keep their account. Removing a profile leaves the provider's files and credentials in place.

## Where logins are stored
Profiles are kept in `accounts.json` next to the Engine's socket, in `~/Library/Application Support/Dirijor` on macOS. Saved Codex logins live under `codex-logins/` and Claude stores under `claude-logins/` in the same folder. Everything is owner-only.

These files are credentials. Protect the folder like the agent's own login file. diri never returns them in responses, logs them or passes them as command-line arguments. Before replacing a Codex login it keeps the previous one in `codex-logins/previous-auth.json`.

## Usage and cost
Open **Settings → Usage** to see tokens and estimated cost over a date range.

| Source | How it is counted |
| --- | --- |
| Claude Code | Read from local and remote transcripts, priced at diri's bundled model rates. |
| Codex | Read from local and remote transcripts, priced at diri's bundled model rates. |
| Cursor | Billed usage from Cursor's dashboard, when you are signed in on this Mac. |

The page shows processed tokens, cached and uncached input, output, and cache read savings, compared with the previous period of the same length. Switch between **This Mac** and **All machines** to include remote hosts.

- Transcripts are included even for sessions you ran outside diri.
- Claude and Codex costs are estimates at API rates, not your bill. Usage on an unknown model is left out of cost.
- Cache read savings compare cached reads with uncached input rates. Cache write premiums are excluded.
- Remote hosts refresh every 5 minutes over SSH. Only totals cross SSH. An unreachable host keeps its last saved totals.

Click **Share** to make an image of your usage. You can pick cost or tokens, the graph, a per-agent breakdown and a theme, then copy, save or post it.

## Plan limits
The account menu at the bottom left shows one meter per plan window that Claude or Codex reports, such as the 5-hour and weekly limits, with time until reset. Limits refresh when you open the menu. A meter marked **stale** has passed its reset time or could not be refreshed.

These numbers come from the provider for your signed-in account. They are separate from the cost estimates above. If you see **Sign in to Claude to see limits** or **Sign in to Codex to see limits**, sign in to that agent first.

## Learn more
- [Account profiles design](https://github.com/cristicretu/diri/blob/main/diri/ACCOUNTS.md)
- [Supported agents](/docs/agents/)
- [Remote hosts](/docs/remote-hosts/)
