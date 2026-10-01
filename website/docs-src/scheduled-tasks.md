---
title: Recipes and scheduled tasks
nav: Recipes
description: Save a prompt, agent, project and worktree choice as a recipe and rerun it in one click. Timed schedules are not available in diri yet.
---
A recipe is a saved task you can run again: the agent, the project or host, the prompt, an optional session title, and whether to use a fresh worktree. Each run starts a brand new session. diri does not run recipes on a timer yet; see [Scheduling](#scheduling) below.

## Save a recipe
1. Press <kbd>⌘</kbd><kbd>N</kbd> to open the launcher.
2. Choose the agent and project, and type the prompt.
3. Click **Save recipe** or press <kbd>⌘</kbd><kbd>S</kbd>.

diri confirms with "it is now a one-click recipe". Saving works even while agent detection is still running or a remote host is offline; diri checks the destination and agent when you run it.

If you started from a recipe and changed some fields, the button reads **Update recipe** instead. Changes you make without updating apply to that one run only, and the saved recipe stays as it was.

## Run a recipe
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

## What a run does
- **Starts a new session** with the saved prompt, agent, project or host, title and worktree choice. It never resumes or depends on the session from an earlier run.
- **Creates a fresh branch each time** when the recipe uses a new worktree. The branch prefix is a naming pattern; diri adds a unique suffix per run. Fresh worktrees are local only, so a recipe that targets a remote host cannot use one.
- **Follows moved projects.** A recipe points at the tracked project, so it uses the project's current folder.
- **Refuses to guess.** If the folder, host or agent is missing, the recipe asks you to repair it rather than launching somewhere else.

Prompts keep their exact whitespace. A prompt over 32,768 characters is rejected with an error, never cut short. You can keep up to 64 recipes.

> [!NOTE]
> Recipes store launch settings only. They do not store credentials, the agent's conversation, or a copy of the agent's own configuration, so your current agent settings apply to every run.

## Scheduling
diri cannot run a recipe at a set time yet. Nothing in the app installs cron jobs, LaunchAgents or any other background scheduler, and an agent does not need to stay open to act as one.

The design for Engine-owned schedules, including missed runs while the Mac sleeps and overlap rules, is in [SCHEDULED_TASKS.md](https://github.com/cristicretu/diri/blob/main/diri/SCHEDULED_TASKS.md). Until it ships, run a recipe by hand with <kbd>⌘</kbd><kbd>N</kbd> then <kbd>⌘</kbd><kbd>1</kbd>.

## Related
- [Sessions](/docs/sessions/) covers what happens to a session after it starts.
- [Worktrees and review](/docs/worktrees/) explains fresh worktrees and how to bring their work back.
- [Keyboard shortcuts](/docs/keyboard-shortcuts/) lists every launcher key.
