---
title: Recipes and schedules
nav: Schedules
description: Start an agent at a set time with a schedule that catches up after sleep and can wake the Mac, or save a launch as a recipe and rerun it in one click.
---
A schedule starts an agent at a set time and sends it a prompt. A recipe is a saved launch you run by hand. Both start a brand new session each time.

## Schedules
Open **Settings → Schedules**. Schedules belong to the Engine, not to a window or an agent, so they keep working after the session that created them closes.

### Create a schedule
1. Click **New schedule**.
2. Under **Prompt**, write what the agent should do.
3. Pick the **Agent** and click **Choose…** to pick the **Folder** it works in.
4. Under **When**, pick **Once**, **Every day**, **Weekdays** or **Every hour**, and enter a time such as `9:00` or `17:30`. For **Every hour** only the minute is used.
5. Set the options below, then click **Create schedule**.

| Option | What it does |
| --- | --- |
| **Catch up if missed** | On by default. If the Mac is asleep or diri isn't open at the due time, the run starts once when it is back, up to 12 hours late. |
| **Keep the Mac awake** | Prevents idle sleep from 10 minutes before each run until it finishes. Closing the lid still sleeps the Mac. |
| **Wake the Mac** | Wakes a sleeping Mac 2 minutes before the run. See [Wake the Mac](#wake-the-mac). |

**Once** runs at the next time that clock time comes around: today if it is still ahead, otherwise tomorrow. The schedule's name is the first 60 characters of the prompt.

### The schedule list
Each schedule shows when it runs, the agent, the folder, and whether it wakes the Mac or keeps it awake. Below that is **Next** with the next run, or **Paused**, and one line about the last run:

| Line | Meaning |
| --- | --- |
| Last ran … | The run started on time. |
| Ran late … because the Mac was asleep | A missed run caught up after the Mac woke. The reason can also be "because diri wasn't running". |
| Skipped the run due … : too late to catch up | The run was missed by more than the catch-up window, so nothing started. |
| Couldn't start the run due … | diri tried to start the session and failed. The error follows. |
| Last run started by hand … | You clicked **Run now**. |

Each row has **Open** to jump to the session the last run started, **Run now**, **Delete**, and a switch to pause or resume the schedule. A **Once** schedule whose time has passed cannot be resumed; create a new one.

### What a run does
At the due time diri opens a new tab with the chosen agent in the chosen folder and sends the prompt. The run does not see any earlier run or the conversation that created the schedule, so write a prompt that stands on its own.

### Missed runs
If the Mac was asleep or diri was not running at the due time, diri catches up when it is back:
- The newest missed run starts once, as long as it is less than the catch-up window late. The default window is 12 hours.
- Older missed runs do not start. The last-run line counts them, for example "covering 2 earlier missed runs".
- A run found after the window closed does not start either. The list shows it as skipped, so nothing is dropped silently.
- A run never starts twice for the same due time.

With **Catch up if missed** off, the window is zero: a missed run is recorded as skipped and does not start.

### Open at login
Schedules run only while diri is open. Under **When the Mac is asleep or restarts**, turn on **Open diri at login** so a run missed during a restart can catch up. If macOS asks, allow diri in **System Settings → General → Login Items**. This option needs diri installed in Applications.

### Wake the Mac
To run on a sleeping Mac, turn on **Allow diri to wake the Mac** in the same section, then turn on **Wake the Mac** for the schedule.

The switch installs `diri-wake-helper`, a small helper that can only set wakes for your runs and put the Mac back to sleep after them. macOS asks an administrator to approve it once in **System Settings → General → Login Items**. It needs diri installed in Applications.

For each run, diri:
1. Wakes the Mac 2 minutes before the due time.
2. Keeps it awake while the agent works.
3. Puts it back to sleep only if nobody used the keyboard, mouse or trackpad since the wake and no other session is still working or waiting for you. Otherwise the Mac sleeps on its own idle timer as usual.

> [!WARNING]
> The lid must be open. A laptop with its lid closed, or a Mac that is off, cannot be woken. Catch-up covers those cases once the Mac is back.

### Scheduled sessions in the sidebar
A session a schedule opened shows a clock after its title. The clock is grey for an ordinary scheduled run and indigo when diri woke the Mac for it. Hover it to see the schedule's name and when the run was due.

### Schedules from an agent
You can ask any agent in diri for a schedule in plain words, for example "every weekday at 9, triage new issues". Agents use diri's MCP tools for this instead of their own cron or loop tools, which stop when their session closes:
- `schedule_agent` creates a schedule. It takes a five-field cron expression in the Mac's local time, a one-off delay in minutes, or an exact time. It can also set the catch-up window (0 to 168 hours), a fresh worktree for each run, keep awake and wake the Mac.
- `list_schedules` shows each schedule's next run and recent runs.
- `delete_schedule` removes a schedule. Sessions its runs opened stay.

Schedules an agent creates appear in **Settings → Schedules** with the rest. A cron expression the form cannot show reads as `Cron` followed by the expression. Arguments are in the [MCP tool reference](/docs/mcp-tools/#schedules).

> [!NOTE]
> diri only starts the agent. If the task needs mail, a calendar or another service, the agent needs its own tools or connectors for that.

## Recipes
A recipe is a saved task you can run again: the agent, the project or host, the prompt, an optional session title, and whether to use a fresh worktree.

### Save a recipe
1. Press <kbd>⌘</kbd><kbd>N</kbd> to open the launcher.
2. Choose the agent and project, and type the prompt.
3. Click **Save recipe** or press <kbd>⌘</kbd><kbd>S</kbd>.

diri confirms with "it is now a one-click recipe". Saving works even while agent detection is still running or a remote host is offline; diri checks the destination and agent when you run it.

If you started from a recipe and changed some fields, the button reads **Update recipe** instead. Changes you make without updating apply to that one run only, and the saved recipe stays as it was.

### Run a recipe
With an empty prompt, the launcher shows your first three recipes under **Saved tasks · a fresh session each run**, each with a **Run** button.

| Action | How |
| --- | --- |
| Run one of the first three | <kbd>⌘</kbd><kbd>1</kbd>, <kbd>⌘</kbd><kbd>2</kbd> or <kbd>⌘</kbd><kbd>3</kbd> in an empty launcher |
| Open the full list | <kbd>⌘</kbd><kbd>R</kbd> in the launcher |
| Preview and change it for one run | <kbd>Space</kbd> on a highlighted recipe |
| Edit name, session title or branch prefix | <kbd>E</kbd> on a highlighted recipe |
| Duplicate | <kbd>⌘</kbd><kbd>D</kbd> |
| Reorder | <kbd>⌘</kbd><kbd>↑</kbd> / <kbd>⌘</kbd><kbd>↓</kbd> |
| Delete | <kbd>⌫</kbd>, then press it again to confirm |

The order of the list decides which recipes get ⌘1, ⌘2 and ⌘3.

### What a recipe run does
- **Starts a new session** with the saved prompt, agent, project or host, title and worktree choice. It never resumes or depends on the session from an earlier run.
- **Creates a fresh branch each time** when the recipe uses a new worktree. The branch prefix is a naming pattern; diri adds a unique suffix per run. Fresh worktrees are local only, so a recipe that targets a remote host cannot use one.
- **Follows moved projects.** A recipe points at the tracked project, so it uses the project's current folder.
- **Refuses to guess.** If the folder, host or agent is missing, the recipe asks you to repair it rather than launching somewhere else.

Prompts keep their exact whitespace. A prompt over 32,768 characters is rejected with an error, never cut short. You can keep up to 64 recipes.

> [!NOTE]
> Recipes store launch settings only. They do not store credentials, the agent's conversation, or a copy of the agent's own configuration, so your current agent settings apply to every run.

Recipes and schedules are separate. A schedule does not run a recipe; it stores its own prompt, agent and folder.

## Related
- [Sessions](/docs/sessions/) covers what happens to a session after it starts.
- [Worktrees and review](/docs/worktrees/) explains fresh worktrees and how to bring their work back.
- [MCP server](/docs/mcp/) explains how agents in diri use its tools.
- [Keyboard shortcuts](/docs/keyboard-shortcuts/) lists every launcher key.
