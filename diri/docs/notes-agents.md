# Diri Notes: Engine and agent integration

Status: design, 2026-09-30. Builds on PR #597 (`diri-notes`, the Notes
window, `dirijor note`) and on the lead's note-session work on `feat/notes`.

## Decisions already made (by the lead with the user)

1. A note is a sidebar item exactly like a session. It is an Engine session
   of `AgentKind` id `note` with no PTY, Holder, or process, and its
   `SessionRecord` gets an additive `note_id: Option<String>` naming the
   notes file id. The lead builds the proto kind and field, Engine
   spawn/lifecycle for note sessions, and the app sidebar/main area. This
   work stays out of those code paths.
2. Note files stay out of project repos, in `<app support>/notes/<id>.md`
   (unchanged). Agents therefore *discover* notes through the `dirijor` MCP
   server and CLI. Discovery is first-class.
3. Notes can @-mention sessions and notes. Mentions are plain Markdown links
   so the existing codec, the rich-nodes renderer, and `@` autocomplete all
   share one representation.
4. Starting work from a note or to-do is an ordinary spawn whose `parent` is
   the note session. The sidebar indents the child under the note the way it
   indents every child today. No parallel lineage mechanism.

## Ownership split

| Concern | Owner | Where |
|---|---|---|
| `AgentKind::note`, `SessionRecord.note_id`, create/archive/remove note sessions | lead | diri-proto, diri-engine |
| Sidebar rows, main area, tab strip for notes | lead | diri-app |
| Mention model, link parsing/writing, to-do links, handoff prompt | this work | `diri-notes` (GUI-free) |
| Discovery + write tools for agents | this work | `dirijor-mcp` tools + `dirijor note` CLI |
| Safe concurrent writes (app, CLI, agents) | this work | `diri-notes::store` |
| `@` autocomplete, mention rendering | rich-nodes agent | diri-app notes editor |

The Engine keeps no dependency on `diri-notes` and never reads note
content. It knows only `note_id`. Everything that reads or writes Markdown
runs in the client processes (app, CLI, MCP) against the files.

## Mentions

Wire form, stored verbatim in the note body:

```
[@Codex: fix resize](diri://session/s_4e97a43bd495)
[@Resize PRD](diri://note/20260930-1412-ab12)
```

- `diri://session/<SessionId>` names a session (a note session is still a
  session).
- `diri://note/<note id>` names a note file. File ids are stable, and they
  also work for notes that have no session yet (CLI captures made while Diri
  was not running).
- The label is display text only. Tools always resolve by id, so a renamed
  session never breaks a mention.
- Ids are validated on parse (`store::validate_id` rules for notes; the
  `s_` session-id charset for sessions). A malformed link stays an ordinary
  link and is not treated as a mention.

The codec already round-trips links (`Style::Link(url)`), so this needs no
Markdown change. The model is `diri_notes::mention` from the rich-nodes work
(PR #600): `MentionTarget`, `in_block`, `Document::mentions()`,
`session_label`. This work adds no second copy. `NoteMeta.mentions` holds
`Document::mentions()`, which `list()` computes during its existing read,
so "notes that mention me" costs no extra I/O.

### What an agent does with mentions

`read_note` returns the mentions resolved against `session.list`: id, kind,
title, status, and whether the session is live. The agent then uses the tools
it already has on those ids (`read_output`, `wait_for_agent`, `get_diff`,
`list_children`). No new inspection tools are needed.

## To-do ↔ session links

A to-do that has been handed off carries its session as a trailing mention in
its own text:

```
- [ ] Fix resize flicker [@Codex](diri://session/s_91c0…)
```

- The link is the only stored fact. Status (working, idle, exited, and so on)
  is always derived live from `session.list` and is never written into the
  file.
- A to-do can have several session links (retries, parallel attempts).
- Checking a to-do off stays a human action by default. When an agent
  reports a task `completed`, the app may *suggest* checking the to-do off; it
  never checks it silently.

## Discovery

All discovery is client-side: `session.list` plus the note files. No new
Engine RPC.

| Question | How |
|---|---|
| Notes for my project | Note sessions whose `project_id` equals the caller's `project_id`, joined by `note_id` to files. Also include files whose front-matter `project` is the caller's project root, so notes without a session still show up. |
| The note I was spawned from | Walk the caller's `parent` chain to the first session with kind `note`, then load `note_id`. Nearest wins. |
| Notes that mention me | Files whose `mentions` contain `diri://session/<caller>`, plus notes that mention any of the caller's ancestors (an agent spawned by a mentioned agent is also relevant; these are marked `via: ancestor`). |
| Search | Existing `haystack` substring match. |

### MCP tools (new, in `dirijor-mcp/src/tools.rs`)

Read tools (allowed for every hosted caller, like `list_agents`):

- `list_notes {project?: "mine"|path|"all", mentions?: "me"|session_id,
  query?, include_archived?}` returns compact metas: id, title, snippet,
  project, pinned, open to-do count, note session id, mentions.
- `read_note {note: id|title|"origin"}` returns the full Markdown, front
  matter, open to-dos with their linked sessions and live status, and the
  resolved mentions. `"origin"` is the note I was spawned from.
- `whoami` gains `origin_note: {note_id, session_id, title}` when the caller
  descends from a note.

Write tools:

- `write_note {note, append?: markdown, check_todo?: {index|text, checked},
  link_todo?: {index|text, session_id}}` makes small, additive edits only. It
  never deletes or rewrites user text.
- `start_from_note {note, todo?, kind?, worktree?, prompt?, task?}` is the
  handoff (below). It has the same result shape as `spawn_agent`.

CLI parity (`dirijor note`): `list --mentions me|ID`, `list --project .`,
`show origin`, `link TODO SESSION`, `check TODO`, `start NOTE [--todo T]
[--kind K] [--worktree]`. The CLI and MCP share one implementation in
`dirijor-mcp` (the CLI already reuses `Bridge`).

## Handoff: starting work from a note or to-do

1. Resolve the note and its note session (by `note_id`). If the note has no
   session yet, create one through the lead's note-session create path (see
   seam S1).
2. Build `SessionSpawnParams` with `parent = note session id`,
   `cwd = note's project root` (so `project_id` matches the note session and
   the sidebar can indent the child; `resolve_parents` only links within one
   project group), and optionally `new_worktree`.
3. The initial prompt comes from `diri_notes::handoff::prompt(note, todo)`
   and contains:
   - the to-do text (if a to-do was chosen) as the task;
   - the note's title, id, and Markdown, capped at 16 KiB with a pointer to
     `read_note` for the rest;
   - the resolved mentions ("related sessions: … use read_output /
     wait_for_agent");
   - one line saying the agent can report progress with `report_to_parent`,
     which lands in the note (see below).
4. Spawn through the existing Engine spawn path (seam S2 covers which one).
5. On success, append the child's mention to the to-do (`link_todo`).
   Guard the write with the concurrency rule below.

The app's "Start" action on a to-do calls the same `diri_notes::handoff`
functions and `session.spawn` directly, as the app already does for ⌘T.

### Reports from children of a note

`report_to_parent` today delivers to the parent's terminal or records the
report on an open task. A note has no terminal. In `dirijor-mcp`, when the
parent is a note session, `report_to_parent` appends a dated, attributed line
under a `## Updates` heading in the note (creating the heading if missing)
and links the reporter. This is a client-side change in the bridge and needs
nothing from the Engine.

### Policy (`bridge/policy.rs`, this work)

Today "root" means `parent.is_none()`, and delegated agents may write only to
their parent or children. A child of a note was started by the user through
their note, so:

- A session whose parent is a note session is treated as a **root** for
  write policy. It can `send_prompt` to the sessions its note mentions.
- Spawn depth (`DIRIJOR_MAX_SPAWN_DEPTH`) is counted from the first non-note
  ancestor.
- Note write tools: root callers may write to any note. Delegated callers may
  write only to their origin note and to notes that mention them.
  `write_note` stays additive either way.

## Concurrent writes

The app, the CLI, and agents all write the same files. Atomic rename
prevents torn files but not lost updates. The app today skips reloads while
it has unsaved typing and then saves its whole buffer, so an agent append
that lands during typing is overwritten.

- `NoteStore::update(id, |note| …)` does read-modify-write under an advisory
  `flock` on `<notes>/.lock`. CLI and MCP writes use it.
- `NoteStore::save_if_unchanged(id, note, expected_source)` lets the app
  save only when the file still matches what it loaded. On mismatch it
  returns `Conflict(current)`, and the app merges: agent edits are
  append-only or single-block, so it re-applies the user's editor state onto
  the new file, or keeps the user's blocks and appends the outside blocks.
  The app merge itself is the editor owner's call. The store gives the
  primitive and a test that proves the race.

## Seams the lead needs to decide (not built here)

- **S1: creating a note session from outside the app. Resolved** by
  `session.spawn` kind `note` (feat/notes 49f5fc47). `dirijor note add/todo`
  now create through it; see "CLI creation" below.
- **S2: agent-initiated handoff with the note as parent.**
  `session.spawn_tracked` requires `spawn.parent == senderID`
  (`control/operations.rs:129`). When an agent calls `start_from_note`, the
  parent is the note, not the caller. Options:
  - (a) Allow `spawn.parent` to be a session of kind `note` that the sender
    may act for. This keeps idempotency. The authorization stays in
    `McpPolicy`; the Engine checks only that the parent is a note session.
    **Recommended.**
  - (b) Use untracked `session.spawn` for handoffs. This loses at-most-once
    dedup on MCP retries.
  - The app path is unaffected either way (`session.spawn` already accepts
    any parent).
- **S3: terminal-only RPCs on note sessions.** Guarded client-side: MCP
  `send_prompt`, `submit_task`, manage, and release refuse a note target
  ("is a note, not an agent"), and `report_to_parent` to a note writes into
  it. The Engine still accepts `session.deliver_message` / `task.submit` for
  a note from other clients; a structured `session_has_no_terminal` there
  would close the gap for good.
- **S4: project of a note session.** The note session's `cwd`/`project_id`
  must be derived from the front-matter `project` root exactly as for agents
  (`session_project_id(root, None)`), or children will not indent under it.
  Inbox notes (no project): children land in whatever project their `cwd`
  is, so they show at the root of that project. Decide whether that is
  acceptable or whether Inbox notes need a pseudo-project.
- **S5: one source of truth for pinned/archived/title.** Front matter holds
  `pinned`/`archived` and the document holds the title, while the sidebar
  sorts by `SessionRecord`. Proposal: the session record is authoritative for
  sidebar state, and the app mirrors changes into front matter so the CLI and
  agents see them when Diri is not running. Title flows file → session, via
  the app on save.
- **S6: removing a note session.** Decide whether removing the session
  trashes the file (`NoteStore::trash`) or only unlinks it. Proposal:
  archive ↔ `archived: true`, and remove → trash. Both are done by the app,
  not the Engine.

- **S7: adopting orphan note files.** Notes written before note sessions
  (the PR #597 Notes window, or `dirijor note` while the Engine was down)
  have files but no Session, and with the Notes window gone the app no longer
  shows them. Adopting by copying content into a new note session would change
  the note id, which breaks `diri://note/` mentions and loses `created`.
  Proposal: additive `SessionSpawnParams.note_id`. For kind `note` with
  `note_id` set, `session_spawn_note` skips `store.create`, checks that the
  file exists (`store.meta`), and returns the existing live note Session if
  one already carries that `note_id`. Otherwise it inserts the record as it
  does today, with title from the file and `cwd` from its `project`, falling
  back to the request's `cwd`. The app, or the Engine at start, can then adopt
  unarchived orphans once, and the CLI can adopt the note it touches. Until
  then, orphans are still found by `list_notes` and `dirijor note list`.

## CLI creation

`dirijor note add` and `dirijor note todo` (when it has to create the
project's "To-dos" note) go through `Bridge::spawn_note` → `session.spawn`
kind `note`:

- The project is `--project`, else the calling agent's project root (so a
  worktree agent's note lands in the repo's project), else the current
  directory. `--inbox` writes a project-less file with no Session.
- The parent is the calling session (`DIRIJOR_SESSION_ID`), so a note an
  agent writes is indented under it. The shared "To-dos" note has no parent.
- An unreachable Engine, or one that predates note sessions (`no manifest
  for agent "note"`), writes the file directly and says so on stderr. Any
  other Engine error fails the command and never falls back silently.
- Other edits (`append`, `todo --to`, `check`, `link`) stay direct file
  writes under the store lock. The Engine never needs to know about them.

## Status

Built on `notes/engine-agents` (base `feat/notes` 49f5fc47, plus
`diri_notes::mention` taken unchanged from PR #600):

- Mentions in `NoteMeta`; `NoteStore::update` / `save_if_unchanged` under a
  store lock; `handoff` helpers (to-dos, links, Updates, prompt).
- MCP `list_notes` (joins note sessions: `session_id`, project via the
  Session's `project_id`), `read_note` (id, title, note Session id, or
  `origin`), `write_note`, `whoami.origin_note`, `report_to_parent` → note.
- Policy: children of a note are roots, spawn depth skips notes, notes are
  never agent targets, delegated `write_note` reaches the origin note.
- CLI creation through note sessions with a file fallback; `check`, `link`,
  `list --mentions`.
- Verified: unit tests against a fake Engine; `tests/notes_cli.rs` drives the
  real `dirijor` binary against an in-process `ControlServer`; a manual run
  against the live Engine found a note mentioning the calling session.
- Not built: `start_from_note` (waits on S2), orphan adoption (S7), app use
  of `save_if_unchanged` (editor owner).

## Build order (this work)

1. `NoteMeta.mentions` on the shared `diri_notes::mention` model + tests.
2. `NoteStore::update` with lock + `save_if_unchanged` + race test.
3. `diri_notes::handoff` (prompt builder, to-do link insertion) + tests.
4. MCP `list_notes` / `read_note` / `write_note` + CLI parity. Discovery by
   file project and mentions works before note sessions exist.
5. Done after note sessions landed: note-session joins, `origin`,
   `whoami.origin_note`, `report_to_parent` → note, policy changes, CLI
   creation. Remaining: `start_from_note` once S2 is decided.
