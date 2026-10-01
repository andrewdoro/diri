---
title: Diri documentation
nav: Introduction
description: How to install diri, run coding agents side by side, keep their work apart, review it, and let agents start and coordinate other agents over MCP.
lead: Everything diri does, from the first session to agents that run other agents. New here? The [guides](/guides/) walk through the basics with screenshots.
---
diri runs Claude Code, Codex, Cursor, Gemini and 16 other terminal agents side by side in one native window. It tells you which agents are working, which ones need you, and which are done. Every agent can get its own git worktree, and every change lands in one place to review.

## Start here

| Page | What you get |
| --- | --- |
| [Install](/docs/install/) | macOS and Linux packages, updates, and where diri keeps its data. |
| [Quickstart](/docs/quickstart/) | Your first agent session, start to finish. |
| [Keyboard shortcuts](/docs/keyboard-shortcuts/) | Every binding, grouped by task. |

## How diri fits together

diri has three parts. You mostly see the first one.

| Part | What it does |
| --- | --- |
| The app | The window: sidebar, terminals, changes, notes, settings. Closing it never stops an agent. |
| The Engine | A background process that owns every session, its status, worktrees and history. The app and the CLI both talk to it. |
| Holders | One small process per session that owns the terminal. Agents keep running when the app or the Engine restarts. |

Agents you start inside diri can reach the Engine too, through the built-in [MCP server](/docs/mcp/). That is how one agent starts helpers, hands them tasks, waits for results and merges their branches.

## Work with many agents

- [Sessions](/docs/sessions/): status, notifications, the sidebar, history, and what survives a restart.
- [Worktrees and review](/docs/worktrees/): give each task its own checkout, then review and bring the changes back.
- [Notes](/docs/notes/): plans and to-do lists you can hand to agents.
- [Recipes](/docs/scheduled-tasks/): save a task and rerun it in one click.
- [Accounts and usage](/docs/accounts/): switch accounts and see what you spend.

## Automate

- [MCP server](/docs/mcp/): what agents can do with diri, and how to connect any agent.
- [MCP tool reference](/docs/mcp-tools/): every tool and argument, generated from the server itself.
- [dirijor CLI](/docs/cli/): script sessions, worktrees and notes from a terminal.
- [Supported agents](/docs/agents/): what each agent supports, and how to add your own.

## Use these docs from an agent

Every page has a Markdown version: add `.md` to its path, for example [/docs/mcp.md](/docs/mcp.md). [llms.txt](/llms.txt) lists them all and [llms-full.txt](/llms-full.txt) has the whole set in one file. To let an agent search the docs itself, add the [docs MCP server](/docs/mcp/#docs-mcp):

```sh
claude mcp add --transport http diri-docs https://diri.sh/mcp
```
