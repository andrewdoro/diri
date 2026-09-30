//! Notes on disk: one Markdown file per note in a single flat directory.
//!
//! Organisation lives in front matter (`project`, `pinned`, `archived`), so
//! moving a note between Inbox, a project, and the archive rewrites one file
//! and never renames anything. Writes are atomic (temp file + rename) so a
//! crash, the CLI, and the app can never leave a half-written note.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::doc::{BlockKind, Document};
use crate::markdown::{self, FrontMatter};
use crate::mentions::{self, MentionTarget};

/// Notes larger than this are listed but not loaded into the editor.
pub const MAX_NOTE_BYTES: u64 = 4 * 1024 * 1024;
const SNIPPET_CHARS: usize = 140;
const TRASH_DIR: &str = ".trash";
const LOCK_FILE: &str = ".lock";

pub const KEY_ID: &str = "id";
pub const KEY_CREATED: &str = "created";
pub const KEY_PROJECT: &str = "project";
pub const KEY_PINNED: &str = "pinned";
pub const KEY_ARCHIVED: &str = "archived";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub front: FrontMatter,
    pub doc: Document,
}

impl Note {
    pub fn project(&self) -> Option<&str> {
        self.front.get(KEY_PROJECT).filter(|p| !p.is_empty())
    }

    pub fn to_markdown(&self) -> String {
        markdown::write(&self.front, &self.doc)
    }
}

/// A listing entry: everything the sidebar and note list show without
/// loading the whole note into the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteMeta {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub snippet: String,
    pub project: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    /// Unix seconds.
    pub created: u64,
    /// Unix milliseconds of the file's last write.
    pub modified_ms: u64,
    pub todos_done: usize,
    pub todos_total: usize,
    /// Open to-dos in document order: (block index, text).
    pub open_todos: Vec<(usize, String)>,
    /// Distinct `diri://` mention targets, in first-mention order.
    pub mentions: Vec<MentionTarget>,
    /// Lower-cased title + body, for search.
    pub haystack: String,
}

impl NoteMeta {
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            "Untitled"
        } else {
            &self.title
        }
    }

    pub fn from_note(id: &str, path: PathBuf, note: &Note, modified_ms: u64) -> Self {
        let (todos_done, todos_total) = note.doc.todo_progress();
        let open_todos = note
            .doc
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.kind == BlockKind::Todo { checked: false } && !b.text.is_empty())
            .map(|(i, b)| (i, b.text.clone()))
            .collect();
        let body = note.doc.plain_text();
        let snippet: String = body
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
            .chars()
            .take(SNIPPET_CHARS)
            .collect();
        let created = note
            .front
            .get(KEY_CREATED)
            .and_then(parse_timestamp)
            .unwrap_or(modified_ms / 1000);
        Self {
            id: id.to_owned(),
            path,
            title: note.doc.title.clone(),
            snippet,
            project: note.project().map(str::to_owned),
            pinned: note.front.flag(KEY_PINNED),
            archived: note.front.flag(KEY_ARCHIVED),
            created,
            modified_ms,
            todos_done,
            todos_total,
            open_todos,
            mentions: mentions::targets(&note.doc),
            haystack: format!("{}\n{}", note.doc.title, body).to_lowercase(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Written; `source` is what the file now holds.
    Saved { source: String },
    /// Someone else changed (or removed) the file since it was loaded.
    Conflict { current: Option<String> },
}

pub struct NoteStore {
    dir: PathBuf,
}

impl NoteStore {
    /// `<app support>/notes`, beside every other piece of Diri state.
    pub fn default_dir(home: impl AsRef<Path>) -> PathBuf {
        diri_proto::paths::DirijorPaths::app_support(home).join("notes")
    }

    /// Where this process should keep notes: `DIRI_NOTES_DIR` when set
    /// (tests, fixtures), else beside the rest of Diri's state, honouring the
    /// same `DIRIJOR_APP_SUPPORT` override every other component uses.
    pub fn resolve_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("DIRI_NOTES_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        if let Some(support) =
            std::env::var_os(diri_proto::paths::ENV_APP_SUPPORT).filter(|d| !d.is_empty())
        {
            return Some(PathBuf::from(support).join("notes"));
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(Self::default_dir(home))
    }

    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, id: &str) -> io::Result<PathBuf> {
        validate_id(id)?;
        Ok(self.dir.join(format!("{id}.md")))
    }

    /// Every note, newest modification first. Unreadable files are skipped.
    pub fn list(&self) -> io::Result<Vec<NoteMeta>> {
        let mut notes = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(id) = note_id(&path) else { continue };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_NOTE_BYTES {
                continue;
            }
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            let note = parse_note(&source);
            notes.push(NoteMeta::from_note(
                &id,
                path,
                &note,
                millis(metadata.modified().ok()),
            ));
        }
        notes.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then(b.id.cmp(&a.id)));
        Ok(notes)
    }

    pub fn load(&self, id: &str) -> io::Result<Note> {
        let path = self.path_for(id)?;
        let metadata = fs::metadata(&path)?;
        if metadata.len() > MAX_NOTE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "note is too large to open",
            ));
        }
        Ok(parse_note(&fs::read_to_string(path)?))
    }

    pub fn meta(&self, id: &str) -> io::Result<NoteMeta> {
        let path = self.path_for(id)?;
        let note = self.load(id)?;
        let modified = fs::metadata(&path)?.modified().ok();
        Ok(NoteMeta::from_note(id, path, &note, millis(modified)))
    }

    /// Atomically replaces the note's file.
    pub fn save(&self, id: &str, note: &Note) -> io::Result<()> {
        let _lock = self.lock()?;
        self.write(id, &note.to_markdown())
    }

    /// Saves only when the file still holds `expected` (the source the caller
    /// loaded), so an editor with unsaved typing never overwrites what the
    /// CLI or an agent wrote meanwhile. A missing file is a conflict too.
    pub fn save_if_unchanged(
        &self,
        id: &str,
        note: &Note,
        expected: &str,
    ) -> io::Result<SaveOutcome> {
        let _lock = self.lock()?;
        let current = match fs::read_to_string(self.path_for(id)?) {
            Ok(current) => Some(current),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if current.as_deref() != Some(expected) {
            return Ok(SaveOutcome::Conflict { current });
        }
        let source = note.to_markdown();
        self.write(id, &source)?;
        Ok(SaveOutcome::Saved { source })
    }

    /// Read-modify-write under the store lock: `edit` sees the note as it is
    /// on disk now, and the file is rewritten only if `edit` changed it.
    /// Every writer outside the editor (CLI, agents) goes through here.
    pub fn update<T>(
        &self,
        id: &str,
        edit: impl FnOnce(&mut Note) -> io::Result<T>,
    ) -> io::Result<(Note, T)> {
        let _lock = self.lock()?;
        let before = self.load(id)?;
        let mut note = before.clone();
        let out = edit(&mut note)?;
        if note != before {
            self.write(id, &note.to_markdown())?;
        }
        Ok((note, out))
    }

    /// An exclusive advisory lock over the whole store, held for one
    /// read-modify-write and released when the returned file drops. Not
    /// reentrant: never take it twice on one thread.
    fn lock(&self) -> io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK_FILE))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.lock()?;
        Ok(file)
    }

    fn write(&self, id: &str, contents: &str) -> io::Result<()> {
        let path = self.path_for(id)?;
        let tmp = self.dir.join(format!(".{id}.{}.tmp", nonce()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(contents.as_bytes())?;
            file.sync_data()?;
            fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Creates a note and returns its id. `project` is a project root path.
    pub fn create(&self, doc: Document, project: Option<&str>) -> io::Result<(String, Note)> {
        let now = SystemTime::now();
        let id = new_id(now);
        let mut front = FrontMatter::default();
        front.set(KEY_ID, Some(id.clone()));
        front.set(KEY_CREATED, Some(format_timestamp(secs(now))));
        front.set(KEY_PROJECT, project.map(str::to_owned));
        let note = Note { front, doc };
        self.save(&id, &note)?;
        Ok((id, note))
    }

    /// Appends Markdown to a note's body (quick capture from the CLI).
    pub fn append(&self, id: &str, markdown_body: &str) -> io::Result<Note> {
        self.update(id, |note| {
            append_markdown(note, markdown_body);
            Ok(())
        })
        .map(|(note, ())| note)
    }

    /// Moves the note into `.trash/`, from where it can be restored by hand.
    pub fn trash(&self, id: &str) -> io::Result<PathBuf> {
        let path = self.path_for(id)?;
        let trash = self.dir.join(TRASH_DIR);
        fs::create_dir_all(&trash)?;
        let target = trash.join(format!("{id}.md"));
        fs::rename(&path, &target)?;
        Ok(target)
    }

    /// Restores a note trashed by [`Self::trash`].
    pub fn restore(&self, id: &str) -> io::Result<()> {
        let path = self.path_for(id)?;
        fs::rename(self.dir.join(TRASH_DIR).join(format!("{id}.md")), path)
    }
}

/// Appends Markdown blocks to the end of a note, dropping blank paragraphs.
pub fn append_markdown(note: &mut Note, markdown_body: &str) {
    let (_, extra) = markdown::parse(&format!("\n{markdown_body}"));
    let mut blocks: Vec<_> = note
        .doc
        .blocks
        .iter()
        .filter(|b| !(b.kind == BlockKind::Paragraph && b.text.is_empty()))
        .cloned()
        .collect();
    blocks.extend(
        extra
            .blocks
            .into_iter()
            .filter(|b| !b.text.is_empty() || b.kind == BlockKind::Divider),
    );
    note.doc = Document::new(note.doc.title.clone(), blocks);
}

pub fn parse_note(source: &str) -> Note {
    let (front, doc) = markdown::parse(source);
    Note { front, doc }
}

/// The id of a note file, or `None` for anything else in the directory.
pub fn note_id(path: &Path) -> Option<String> {
    if path.extension()? != "md" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    validate_id(stem).ok()?;
    Some(stem.to_owned())
}

/// Note ids (and the session ids mentions carry) are single path-safe
/// components: ASCII alphanumerics, `-` and `_`, never hidden.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn validate_id(id: &str) -> io::Result<()> {
    if is_valid_id(id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid note id",
        ))
    }
}

/// Time-sortable, collision-resistant ids: `20260930-142501-3fa9`.
pub fn new_id(now: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = civil(secs(now));
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{}", &nonce()[..4])
}

fn nonce() -> String {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn millis(time: Option<SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// RFC 3339 in UTC, seconds precision.
pub fn format_timestamp(unix: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(unix);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

pub fn parse_timestamp(text: &str) -> Option<u64> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(|p| p.parse::<i64>());
    let (y, mo, d) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = time.split(':').map(|p| p.parse::<i64>());
    let (h, mi, s) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    let days = days_from_civil(y, mo, d);
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

/// Unix seconds → (year, month, day, hour, minute, second) in UTC.
pub fn civil(unix: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Block;

    #[test]
    fn timestamps_round_trip() {
        for unix in [0, 951_782_400, 1_790_000_000, 4_102_444_800] {
            assert_eq!(parse_timestamp(&format_timestamp(unix)), Some(unix));
        }
        assert_eq!(format_timestamp(1_790_769_600), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn create_list_append_trash() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path().join("notes")).unwrap();
        let doc = Document::new(
            "Groceries",
            vec![
                Block::new(0, BlockKind::Todo { checked: false }, "milk"),
                Block::new(0, BlockKind::Todo { checked: true }, "eggs"),
            ],
        );
        let (id, _) = store.create(doc, Some("/tmp/proj")).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        let meta = &listed[0];
        assert_eq!(meta.title, "Groceries");
        assert_eq!(meta.project.as_deref(), Some("/tmp/proj"));
        assert_eq!((meta.todos_done, meta.todos_total), (1, 2));
        assert_eq!(meta.open_todos, vec![(0, "milk".to_owned())]);

        let note = store.append(&id, "- [ ] bread").unwrap();
        assert_eq!(note.doc.todo_progress(), (1, 3));
        assert!(store.load(&id).unwrap().doc.same_content(&note.doc));

        store.trash(&id).unwrap();
        assert!(store.list().unwrap().is_empty());
        store.restore(&id).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_appends_are_never_lost() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path().join("notes")).unwrap();
        let (id, _) = store
            .create(Document::new("Log", Vec::new()), None)
            .unwrap();
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let (store, id) = (&store, &id);
                scope.spawn(move || {
                    // A store per writer, as separate processes would have.
                    let store = NoteStore::open(store.dir()).unwrap();
                    for n in 0..10 {
                        store.append(id, &format!("- w{writer} n{n}")).unwrap();
                    }
                });
            }
        });
        assert_eq!(store.load(&id).unwrap().doc.blocks.len(), 80);
    }

    #[test]
    fn save_if_unchanged_refuses_to_overwrite_outside_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path().join("notes")).unwrap();
        let (id, note) = store
            .create(Document::new("PRD", Vec::new()), None)
            .unwrap();
        let loaded = note.to_markdown();

        // An agent appends while the editor has unsaved typing.
        store.append(&id, "- [ ] from the agent").unwrap();
        let mut typed = note.clone();
        append_markdown(&mut typed, "typed in the editor");
        let SaveOutcome::Conflict {
            current: Some(current),
        } = store.save_if_unchanged(&id, &typed, &loaded).unwrap()
        else {
            panic!("expected a conflict");
        };
        assert!(current.contains("from the agent"));
        assert!(
            store
                .load(&id)
                .unwrap()
                .to_markdown()
                .contains("from the agent")
        );

        // Against the current source the save goes through.
        let SaveOutcome::Saved { source } = store.save_if_unchanged(&id, &typed, &current).unwrap()
        else {
            panic!("expected a save");
        };
        assert_eq!(
            fs::read_to_string(store.path_for(&id).unwrap()).unwrap(),
            source
        );

        store.trash(&id).unwrap();
        assert_eq!(
            store.save_if_unchanged(&id, &typed, &source).unwrap(),
            SaveOutcome::Conflict { current: None }
        );
    }

    #[test]
    fn update_skips_unchanged_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path().join("notes")).unwrap();
        let (id, _) = store
            .create(Document::new("Same", Vec::new()), None)
            .unwrap();
        let path = store.path_for(&id).unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (_, answer) = store.update(&id, |_| Ok(42)).unwrap();
        assert_eq!(answer, 42);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn meta_lists_mentions() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path().join("notes")).unwrap();
        let (id, _) = store.create(Document::new("M", Vec::new()), None).unwrap();
        store
            .append(
                &id,
                "Ask [@Codex](diri://session/s_1) about [@PRD](diri://note/n1)",
            )
            .unwrap();
        assert_eq!(
            store.meta(&id).unwrap().mentions,
            vec![
                MentionTarget::Session("s_1".into()),
                MentionTarget::Note("n1".into())
            ]
        );
    }

    #[test]
    fn rejects_path_like_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open(dir.path()).unwrap();
        for id in ["../x", "a/b", ".hidden", ""] {
            assert!(store.path_for(id).is_err(), "{id}");
        }
    }
}
