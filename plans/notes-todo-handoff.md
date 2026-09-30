# Notes: to-dos as tracked agent work

Status: design, 2026-09-30. Branch `notes/todo-handoff` on `feat/notes`
(52811da7). Builds on `diri-notes::handoff`, `diri-notes::mention` and the
MCP note tools (PR #601). Renders beside the rich-nodes work (PR #600).

The loop, in the user's words: "I have a to-do, maybe with a toggle list
that gets all the context, and from there a button to spawn a diri agent,
and we track it live in the note."

```
- [ ] Draft 3 LinkedIn posts for the launch   [@Claude: Draft 3 LinkedIn…]   Working on it
  ▾ context (hidden once work starts)
  - Tone: plain, confident, no emojis
  - [Launch brief](https://example.com/brief)
  - [@Launch plan](diri://note/20260930-1412-ab12)
  - [@Claude: Draft 3 LinkedIn…](diri://session/s_91c0) 14:02 Drafted post 1 of 3
```

## 1. What is stored and what is live

The file stores only facts a person could write by hand, in plain Markdown:

| Fact | Where in the file |
|---|---|
| The work item | a to-do block |
| Its context | the to-do's indented children (list blocks deeper than the to-do, up to the next block at the to-do's depth or shallower) |
| The agent working on it | a trailing session mention in the to-do's text, `[@Claude: …](diri://session/<id>)`, one per attempt, newest last |
| Progress | child bullets that begin with the chip of one of the to-do's own sessions (written by `report_to_parent`) |
| Done | the checkbox, ticked by a person |

Everything else is derived live from `session.list` and never written:
state, question, PR, and whether the context is folded.

Why children and not a separate section: moving, copying, or deleting the
to-do in any Markdown editor carries its context and history with it.

### Context vs. updates

A child block whose text starts with a mention of one of the to-do's linked
sessions is an **update** (authored by that agent). Every other child is
**context**. That rule needs no markup, survives hand edits, and lets a
retry see the earlier attempt's updates, labelled as such.

## 2. States

Derived by `diri_notes::work::state(todo, facts)` from the newest linked
session. `facts` is a proto-free struct the app fills from `SessionRecord`,
so the reducer is unit-tested in `diri-notes` without the app.

| State | When | Shown as |
|---|---|---|
| `Ready` | unchecked, no linked session | nothing until hover/caret, then `Start ⌥⌘↩` |
| `Starting` | spawn in flight (view-only), or session `Starting`, or idle before its first completed turn | "Starting…" |
| `Working` | `Working` | "Working on it" |
| `NeedsYou` | `NeedsInput` | "Needs you" + the question (`needs_input.summary`, `prompt_excerpt`) + `Open` |
| `Review` | idle or exited cleanly after a completed turn | "Ready to review" (+ "PR #123 open" when `pull_requests` has an open PR) + `Open` |
| `Stopped` | exited without a completed turn, non-zero exit, or signalled | "Stopped" + `Open` / `Start again` |
| `Gone` | session archived or not in the list | "Session archived" / "Session not found" + `Start again` |
| `Done` | checkbox ticked | no status line; the chip stays for history |
| `Failed` | spawn request failed (view-only) | "Couldn't start: <reason>" + `Try again` |

"Idle ⇒ Review" needs `last_turn_completed_at` so a freshly spawned agent
sitting at its prompt is not mistaken for finished. The to-do is never
ticked automatically (design doc §To-do links).

Plain wording everywhere; "PR" appears only when the session actually has
one.

## 3. Context-assembly contract (`diri_notes::work::brief`)

Input: the note, the to-do's document index, the resolved mentions the app
could look up, and the note id. Output: `Brief { prompt, parts, bytes,
truncated }`. `prompt` is exactly what the agent receives; the Start panel
shows it verbatim.

Order and budgets (total cap 24 KiB, UTF-8 safe cuts, every cut says so and
points at `read_note`):

1. The task: the to-do's text without its session chips. Never cut.
2. Context from the to-do's children, as Markdown list lines (links and
   mentions kept as links). Cap 12 KiB.
3. Resolved mentions found in the to-do and its children:
   - notes: title + id + the first 2 KiB of the body (at most 4 notes);
   - sessions: id, kind, status, title, and the tools to inspect them.
4. Where it sits: the note title and the nearest heading above the to-do
   with that section's other blocks, cap 4 KiB. Not the whole note: the
   agent gets `read_note "origin"` for that.
5. Earlier attempts: updates already under the to-do, cap 2 KiB.
6. How to report: `report_to_parent` lands under this to-do; do not tick
   the to-do, the person reviews it; for non-code work, put the result in
   the report (or a linked file) so it can be reviewed from the note.

Links to web pages are passed as links; Diri does not fetch them.

## 4. Starting

- **Affordance:** a quiet `Start` text button with the plain shortcut at the
  right end of an unlinked, unchecked to-do row. Visible on row hover or
  when the caret is in the row, like Things/Linear. Linked rows show the
  status line instead.
- **Shortcut:** ⌃⌘↩ with the caret in a to-do. ⌘⇧↩ is Toggle pane zoom
  (`commands.rs`), ⌘↩ ticks a to-do, and ⌥⌘↩ folds (PR #602).
- **Start panel:** hosted like the `/` and `@` menus (`host_menu`: a glass
  panel window in the app), under the row, with the shared menu row:
  - agent rows (installed agents, the default first with a checkmark,
    20px logos, one row shape), ↑/↓ to choose;
  - a hairline, then "What it gets" with the exact prompt in a scrolling
    mono block and its size ("2.1 KB, 3 context lines, 1 note");
  - Return starts the highlighted agent; Escape cancels.
  The panel always shows the preview; it's quiet enough to leave on and it
  keeps the "exactly what is sent" promise without a first-use flag.
- **Spawn:** `SessionSpawnParams { kind, cwd: note session's cwd, parent:
  note session, title: to-do text (one line, 60 chars), initial_prompt:
  brief.prompt }` through a new `StoreEffect::StartWorkItem`, which unlike
  `Spawn` does not select the new session: you stay in the note and watch.
- **Link:** on success the executor writes the chip into the file under the
  store lock (`NoteStore::update`, to-do found by exact text), and the pane
  applies the same chip to its open buffer by block id. Both are idempotent
  (`handoff::link_session` dedups), so a pane with unsaved typing that saves
  over the file still carries the link.
- **Fold:** folding is the editor's (PR #602: gutter chevron, ⌥⌘↩, view
  state only). Work items use it: a to-do's children fold when Start is
  pressed, and a note opens with started, unticked to-dos folded.

## 5. Live tracking

The pane rebuilds a `WorkDirectory` (session id → `SessionFacts`) from the
store on every render and hands it to the editor, like `set_colors`. The
editor draws one status line under each linked to-do (state text, question,
`Open`/`Start again`). The chip itself is (b)'s: it renders the link, dot,
and click-to-open.

Reports: `report_to_parent` from a session linked to a to-do inserts a
child bullet under that to-do (after its last child):
`- [@Claude: …](diri://session/<id>) 14:02 <text>`. A session linked to no
to-do keeps the note-level `## Updates` section. `handoff::append_update`
chooses; MCP and CLI call it unchanged.

"Needs you" answers: the app deliberately stopped sending blind approve
keystrokes from notifications ("captured terminal keystrokes cannot safely
approve a prompt after it changes", `notifications.rs`). The note follows
the same rule: it shows the question and an `Open` button that selects the
session; no approve from the note.

## 6. Finishing

- Review is a live state, not a tick.
- Ticking a to-do whose newest session is `Starting`/`Working`/`NeedsYou`
  opens a small panel: "The agent is still working." rows `Tick and stop
  the agent` (archives the session), `Tick, keep it running` (the Return
  default, since stopping should be picked on purpose), `Cancel`.
- Ticking in `Review`/`Stopped`/`Gone` just ticks.

## 7. Navigating

- To the session: the status line's `Open` selects the session in the
  sidebar (same path as a sidebar click). Chip clicks are (b)'s
  `EditorEvent::OpenMention`, which the pane handles the same way.
- Back to the to-do: the session header of a session whose parent is a note
  shows the note's title as a link; clicking it selects the note and scrolls
  its editor to the to-do that links the session. The request travels as
  `SessionStore::reveal_in_note(note_session, child)`, consumed by
  `NotePane::show`.

## 8. Failure cases

| Case | Behaviour |
|---|---|
| No agents installed / catalog not scanned | panel says "No agents installed" with `Open Settings`; nothing spawns |
| The default agent is not installed | the panel preselects the first installed agent |
| Spawn fails | row shows "Couldn't start: <reason>" + `Try again`; no link written; the prompt is unchanged |
| Session archived / removed | `Gone` with `Start again`, which adds a second chip; the first stays as history |
| To-do text edited while running | fine: the link is by id; reports find the to-do by session id |
| Chip deleted by hand | the to-do is unlinked; later reports fall back to `## Updates` |
| To-do deleted while the spawn is in flight | the session still sits under the note in the sidebar; no link is written |
| Note edited while the agent writes | agent writes go through `NoteStore::update`; the pane reloads when clean; the lost-update window while typing is closed by (a)'s `save_if_unchanged` in NotePane |
| Remote note project | spawns use the note session's `cwd`/`host` like any spawn |

## 9. Where the code goes

- `diri-notes/src/work.rs` (new, GUI-free, unit-tested): `children`,
  `context`/`updates` split, `SessionFacts` + `state`, `brief`,
  `append_todo_update`, `todo_for_session`.
- `diri-notes/src/handoff.rs`: `append_update` routes to the to-do when the
  reporter is linked; prompt wording points at the to-do.
- `diri-app/src/notes/work_item.rs` (new): `WorkDirectory` from the store,
  status-line and Start-affordance elements, the Start panel and the
  tick-while-running panel, fold state.
- `diri-app/src/notes/editor_view.rs`: surgical hooks only (below).
- `diri-app/src/notes/mod.rs`: build the directory, start requests, apply
  outcomes, handle `Open`.
- `diri-app/src/store/mod.rs`: `StoreEffect::StartWorkItem`, outcomes, reveal.
- `dirijor-mcp/src/bridge/notes.rs`: nothing beyond the new `handoff`
  behaviour and its test.

### Hooks in `editor_view.rs` (as built)

1. `NoteEditorView.work: work_item::WorkView` (live facts, starts in
   flight, open panels); `reload` remaps it by to-do text.
2. In the block loop: `work_accessory(..)` on the row, and
   `work_status_line(..)` after a visible to-do row.
3. `check()` and ⌘↩ go through `guard_tick(..)` so a running to-do asks
   first.
4. Action `StartWork` on `ctrl-cmd-enter`; Return/arrows/Escape reach an
   open panel first (`work_panel_key`).
5. `EditorEvent::Work(WorkRequest)` for Prepare/Start/Open/Stop, handled by
   the pane (`impl NotePane` in `work_item.rs`).
6. `anchor_work_menu()` at the top of render, and `work_menu(..)` beside
   the `/` and `@` menus.

## 10. Cuts and open questions for the lead

- No answering or approving from the note (see §5). Revisit if the Engine
  grows a structured, prompt-bound answer RPC.
- `start_from_note` for agents is still blocked on S2; the app path works.
- Fold state is the editor's view state (not persisted), per #602.
- Hosting inside a workspace pane uses the same NotePane, so it works
  there too; not separately screenshotted.
