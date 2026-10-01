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
parent is a note session, `report_to_parent` writes into the note through
`handoff::append_update`:

- a session linked to a to-do (a `diri://session/<id>` chip in the to-do's
  text) gets a dated, attributed child bullet under that to-do, after its
  other children, so progress folds with the work it belongs to
  (`diri_notes::work`, design in `plans/notes-todo-handoff.md`);
- any other child gets a line under a `## Updates` heading (created if
  missing).

The reply says which (`todo` is the block index, or null). Agents never tick
the to-do; the person reviews the work and ticks it.

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
prevents torn files; a lock and a merge prevent lost updates.

- `NoteStore::update(id, author, |note| …)` does read-modify-write under an
  advisory `flock` on `<notes>/.lock`. Every CLI, MCP and Engine write uses
  it. Concurrent writers are tested with 8 threads.
- The editor saves with `NoteStore::save_if_unchanged(id, note, expected)`,
  where `expected` is the text it last loaded or saved. When someone else
  wrote in between, the save returns `Conflict(current)` instead of
  overwriting.
- `diri_notes::merge::merge3(base, mine, theirs)` is a block-level three-way
  merge:
  - A block only one side changed takes that change.
  - Blocks either side inserted are all kept, the person's first.
  - When both sides edited the same block, the person's text is kept with
    the outside checkbox, or with a suffix the outside write appended (a
    session chip).
  - Any other outside edit of that same block yields to the person's text
    and stays in history.
  - Rewrites that share no text are different blocks, so both survive.
  - A 2,000-round randomized test checks these rules.
- `Editor::absorb` installs a merge as one undo step and keeps the caret in
  the text being typed.
- `NotePane` merges on the file watcher's reconcile while there is unsaved
  typing. It also merges on a save conflict, then saves over the result.
  App tests interleave typing with a `write_note`-style append, and with a
  tick plus chip on the very to-do being typed in.

## Version history

- **Storage:** every note keeps versions in `<notes>/.history/<id>/`, one
  owner-only (0600, dirs 0700) file per version, named
  `<unix ms>~<author>~<reason>.md`. There is no index to fall out of sync,
  and history survives trash and restore because it is keyed by note id.
- **When a version is taken:**
  - edits: at most one per 60 seconds;
  - always the note as it stood before any outside write (`update`), and
    the result of that write, attributed to the agent (`Session(id)`) or
    the command line;
  - always before and after a restore.
- **Comparison:** versions compare by body, so pins, archives and session
  stamps are not versions. A restore replaces only the text, and the note
  keeps its place, pin and Session.
- **Retention:** every version from the last hour, then the newest per 10
  minutes for a day, per day for 30 days, and per week after that. Caps are
  200 versions and 20 MiB per note, and the newest is always kept.
- **Summaries:** plain words, e.g. "1 new to-do, 1 to-do done", "added 3
  lines", "restored the version from …".
- **Where people and agents use it:**
  - MCP `note_history` lists or reads versions. It is read-only and there is
    no restore tool.
  - CLI: `dirijor note history NOTE [VERSION]` and `dirijor note restore
    NOTE VERSION`. `restore` refuses to run inside an agent's session.
  - App: **Version History…** in the palette whenever a note is selected. It
    opens a glass panel over the note with rows showing when and who (you,
    the command line, or the agent's sidebar name), what changed, and a
    readable preview. Restore asks through the system alert sheet.
    Screenshots: `docs/screenshots/notes-version-history-{light,dark}.png`.

## Agents keeping their note current

One contract, `dirijor_mcp::tools::NOTES_CONTRACT`, is part of the MCP
server's instructions, and every note tool's description points into it.
It is written for people who are not developers, because they read the
notes:

1. If `whoami` shows `origin_note`, you were started from a note. Read it
   first with `read_note {"note":"origin"}`: it is your brief.
2. As you find important things (a decision, a finding, a blocker, a result,
   a link), add one short entry with `write_note {"entry": …}`. It is filed
   under your own to-do (the one carrying your chip) or under the note's
   Updates. Entries are one or two plain sentences, with no progress chatter.
   Entries over 500 characters are refused, with a pointer to `create_note`.
3. Never rewrite or delete the person's text. Every agent write is additive.
4. Tick your own sub-tasks as you finish them (`write_note` todo + checked).
   Leave the to-do you were started from unticked: the person reviews the
   work and ticks it (the same rule as the app's Start brief).
5. Finish with a one-paragraph result via `report_to_parent` with status
   done. When the parent is a note, the result lands in the note.

The handoff brief that `start_from_note` gives an agent restates the same
steps. `tests` in `dirijor-mcp` check the instructions, the tool
descriptions, and that note tools avoid "repo/worktree/branch/commit".

## Editing a note in place

Agents edit a note the way they edit a Markdown file.

- **The text is canonical.** `read_note`'s `markdown` is the note's body as
  Diri's own writer prints it: title first as a `# ` line, front matter left
  out, list markers normalised, tables tidy, special characters escaped.
  `edit_note` matches against exactly that text (`diri_notes::text_edit::
  body`), and so does `dirijor note show`, so text an agent copies always
  matches.
- **`edit_note(note, old_string, new_string, replace_all?)`** follows the
  contract of a file Edit tool:
  - `old_string` must appear exactly once, unless `replace_all`.
  - An empty `new_string` deletes.
  - The title line is editable like any other line, but deleting it is
    refused.
  - A miss says whether spaces or capital letters were the difference and
    shows the closest line. A repeat lists where each match is.
  - It returns the changed lines with a line of context, plus the new
    version id.
- **`replace_section(note, heading, markdown)`** replaces everything under a
  heading, up to the next heading of the same or a higher level, and keeps
  the heading.
  - `## Status` picks a level when two headings share a name.
  - The title counts as the top heading.
  - It is an error when the heading is missing (the note's headings are
    listed) or ambiguous.
- **How changes are applied:** both tools go through `NoteStore::update`:
  the store lock, the agent as author, and a version before and after. An
  open editor merges the change live with the three-way merge, and the
  person's undo still steps over it as one change.
- **Policy:** the same as `write_note`. Root agents may edit any note.
  Delegated agents may edit only the note they were started from, or notes
  that mention them or an ancestor.
- **Contract:** prefer adding. Change or remove existing text when the
  person asks, or to keep your own entries current (tick a row, "Fix" →
  "Done"). Never silently delete the person's writing: every version is
  kept and the person can restore one.

### Editing the `.md` directly

`read_note` also returns `path`, and an agent may edit that file with its
own Read/Edit tools. Diri makes this safe:

- **The store remembers what it last wrote.** Every write records the exact
  text in `.history/<id>/last`, which is not a version.
- **The first to notice an outside change takes it in.** That is the open
  note's file watcher, or any store write: the app's save, `write_note`,
  `edit_note`, the CLI. It keeps the last-written text as a version, even
  when typing throttled it out of history, and records the new text by
  "a direct file edit" (`Author::File`).
- **Identity survives.** If the edit broke or dropped the front matter, the
  identity keys (`id`, `created`, `project`, `session`, pin and archive)
  are restored from the last known front matter and written back. The id
  always matches the file name.
- **Typing is never overwritten.** With unsaved typing in the open note,
  the direct edit is merged like any other outside write.
- **Not covered:** a note that is not open and that nothing writes to is
  noticed at its next store write, not at the moment of the edit.

CLI: `dirijor note edit NOTE --old TEXT --new TEXT [--all]` and
`dirijor note replace-section NOTE HEADING < new.md`.

## Agents creating and starting from notes

- **`create_note`** takes a title, rich Markdown, an optional project and
  `open`. It creates a note Session whose parent is the calling agent, in
  the agent's project, so the note sits under the agent in the sidebar.
  `open:true` sends `session.reveal`.
  - **New, additive: `session.reveal`.** It is a request (`{"sessionID"}`)
    and an event of the same name. The Engine checks that the session exists
    and publishes the event; the app selects the session without taking
    focus from another app. CLI: `dirijor note add|create --open`.
- **`start_from_note`** takes a note, an optional to-do, kind,
  `separate_copy`, prompt and task. It is a tracked spawn whose parent is
  the note Session, and its chip goes on the to-do. A note without a
  Session is adopted first.
  - With a to-do, the agent gets exactly the brief the app's Start button
    sends (`diri_notes::work::brief`).
  - Without one, it gets the whole note and the sessions it mentions.
  - `write_note` entries use the same `work` helpers as reports: under the
    named to-do, else under the caller's own to-do, else in Updates.
  - Policy: root agents may start work from any note. Delegated agents may
    start work only from the note they came from. Depth skips notes, and
    notes do not count as live children.

## Engine: note sessions without terminals (round 2, resolves S2, S3, S7)

- **S2, done.** `session.spawn_tracked` accepts a parent that is a note
  Session as well as the sender. Who may do so is decided in `McpPolicy`.
  The app path (`session.spawn`) always accepted any parent.
- **S3, done.** Terminal and process requests on a note fail at once with
  `session_has_no_terminal`, for every client:
  - `session.deliver_message`, `task.submit`, `send_text`, `send_key`,
    `resize`, `read_screen`, `terminal_title`, `reset_terminal`,
    `capture_find`, `read_scrollback(_cells)`, `read_transcript`, `resume`,
    `reconnect`, `hibernate`, `wake`, `fork`, `migrate` and
    `continue_with_account`.
  - Attach has no error frame, so it closes immediately, as it does for any
    id without a terminal, and records `attach.note_rejected`.
  - Adding an attach error frame would be a protocol change. It is not
    proposed.
- **S7, done.** Adoption keeps each note's id and created date.
  - **Adopting one file:** `SessionSpawnParams.note_id` (additive) adopts an
    existing notes file. It is idempotent per note id: the file's existing
    Session is returned, including under concurrent calls. The file keeps
    its id and `created`, and the record's `created_at` comes from it.
  - **The stamp:** every note that gets a Session is stamped
    `session: <id>` in its front matter, both new ones (`create_for_session`)
    and adopted ones. The stamp is written after the record exists, so a
    crash in between is repaired by the next scan, not duplicated.
  - **Automatic adoption: one scan at Engine startup.** It runs on a
    one-shot thread after `bind()`, so only the singleton Engine adopts. It
    handles unstamped, unarchived files with no Session, oldest first. It is
    cheap: one directory read per start. It is deterministic: running it
    twice adopts nothing new, which is tested.
    - This covers notes from before note Sessions and CLI notes written
      while the Engine was down. The app needs no work on connect.
    - A note whose Session was removed keeps its stamp, so it is never
      resurrected.
  - **Inbox notes** (no project) live in the home folder, the same place a
    new terminal without a project opens. The folder in the note's
    `project` wins while it still exists, then the requested `cwd`, then
    home.

## Seams still open for the lead

- **S4 (unchanged):** Inbox notes now have a home, the home folder. Children
  of a note still indent only within the note's project group.
- **S5:** pinned, archived and title in front matter versus the
  SessionRecord. Unchanged.
- **S6:** removing a note Session leaves its file, which stays stamped and so
  is not re-adopted. It still shows in `list_notes`, `dirijor note list` and
  `read_note`. Decide whether remove should also move the file to the trash
  (history survives either way).
- **Undo across a merge:** `absorb` is one undo step. Redoing past it is
  unaffected, but undoing past it also undoes the outside write in the
  editor. The next save writes that, and history keeps the agent's version.
  If this surprises people, the editor could skip absorbed steps when
  undoing.
- **Local times:** history rows in the app are relative ("2 hours ago"). The
  CLI and MCP give UTC times, labelled as such, because `diri-notes` has no
  time-zone data. A time-zone crate would only be worth it if people read
  those times directly.

## CLI

- **Creating:** `dirijor note add|create`, and `todo` when it creates the
  project's "To-dos" note, go through `session.spawn` kind `note`.
  - The project is `--project`, else the calling agent's project root, else
    the current folder.
  - `--inbox` writes a file with no project and no Session.
  - `--open` shows the note.
  - If the Engine is unreachable or too old, the file is written directly
    and stderr says Diri isn't running. Any other Engine error fails the
    command.
- **Editing:** `append`, `todo --to`, `check` and `link` are locked,
  attributed file writes.
- **History:** `history` and `restore` are described above.
