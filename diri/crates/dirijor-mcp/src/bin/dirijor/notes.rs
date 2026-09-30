//! `dirijor note` — capture into and read Diri Notes from any shell or agent.
//!
//! Notes are plain files, so this works whether or not Diri is running; the
//! Notes window's file watcher shows every change the moment it lands.

use std::io::{IsTerminal, Read};
use std::path::Path;

use diri_notes::doc::{Block, Document};
use diri_notes::markdown;
use diri_notes::store::{NoteMeta, NoteStore};

use super::CliError;

pub(crate) const HELP: &str = "dirijor note TEXT                     capture a note into the Inbox (first line is the title)\n  \
dirijor note add [TEXT] [--title T] [--project PATH] [--pin]   create a note (reads stdin when TEXT is omitted)\n  \
dirijor note append NOTE TEXT          append Markdown to a note (NOTE is an id or part of its title)\n  \
dirijor note todo TEXT [--to NOTE] [--project PATH]   add a to-do (default: the Inbox note \"To-dos\")\n  \
dirijor note list [--project PATH] [--all] [--json]\n  \
dirijor note show NOTE                 print a note's Markdown\n  \
dirijor note path                      print the notes directory";

pub(crate) fn run(arguments: &[String]) -> Result<(), CliError> {
    let store = open_store()?;
    let Some(first) = arguments.first().map(String::as_str) else {
        println!("{HELP}");
        return Ok(());
    };
    let rest = &arguments[1..];
    match first {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        "add" | "new" => add(&store, rest),
        "append" => append(&store, rest),
        "todo" => todo(&store, rest),
        "list" | "ls" => list(&store, rest),
        "show" | "cat" => show(&store, rest),
        "path" => {
            println!("{}", store.dir().display());
            Ok(())
        }
        _ => add(&store, arguments),
    }
}

fn open_store() -> Result<NoteStore, CliError> {
    let dir = NoteStore::resolve_dir()
        .ok_or_else(|| CliError::failure("cannot find a home directory for notes"))?;
    NoteStore::open(dir).map_err(|e| CliError::failure(format!("cannot open notes: {e}")))
}

struct Flags {
    positional: Vec<String>,
    title: Option<String>,
    project: Option<String>,
    to: Option<String>,
    pin: bool,
    all: bool,
    json: bool,
}

fn flags(arguments: &[String]) -> Result<Flags, CliError> {
    let mut out = Flags {
        positional: Vec::new(),
        title: None,
        project: None,
        to: None,
        pin: false,
        all: false,
        json: false,
    };
    let mut iter = arguments.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| CliError::failure(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--title" | "-t" => out.title = Some(value("--title")?),
            "--project" | "-p" => out.project = Some(project_root(&value("--project")?)?),
            "--to" => out.to = Some(value("--to")?),
            "--pin" => out.pin = true,
            "--all" => out.all = true,
            "--json" => out.json = true,
            _ => out.positional.push(arg.clone()),
        }
    }
    Ok(out)
}

/// A project is named by its root path, as Diri projects are.
fn project_root(path: &str) -> Result<String, CliError> {
    let path = Path::new(path);
    let absolute = std::fs::canonicalize(path)
        .map_err(|e| CliError::failure(format!("project {}: {e}", path.display())))?;
    Ok(absolute.to_string_lossy().into_owned())
}

fn text_or_stdin(positional: &[String]) -> Result<String, CliError> {
    if !positional.is_empty() {
        return Ok(positional.join(" "));
    }
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::failure(
            "nothing to write: pass TEXT or pipe it on stdin",
        ));
    }
    let mut text = String::new();
    stdin
        .read_to_string(&mut text)
        .map_err(|e| CliError::failure(format!("reading stdin: {e}")))?;
    Ok(text)
}

fn add(store: &NoteStore, arguments: &[String]) -> Result<(), CliError> {
    let flags = flags(arguments)?;
    let text = text_or_stdin(&flags.positional)?;
    let (title, body) = match flags.title {
        Some(title) => (title, text),
        None => {
            let text = text.trim_start();
            let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
            (
                first.trim().trim_start_matches("# ").to_owned(),
                rest.to_owned(),
            )
        }
    };
    let (_, parsed) = markdown::parse(&format!("\n{body}"));
    let doc = Document::new(title, parsed.blocks);
    let (id, mut note) = store
        .create(doc, flags.project.as_deref())
        .map_err(|e| CliError::failure(format!("cannot create note: {e}")))?;
    if flags.pin {
        note.front.set_flag(diri_notes::store::KEY_PINNED, true);
        store
            .save(&id, &note)
            .map_err(|e| CliError::failure(format!("cannot save note: {e}")))?;
    }
    println!("{id}");
    Ok(())
}

fn append(store: &NoteStore, arguments: &[String]) -> Result<(), CliError> {
    let flags = flags(arguments)?;
    let Some((target, text)) = flags.positional.split_first() else {
        return Err(CliError::failure("usage: dirijor note append NOTE TEXT"));
    };
    let meta = resolve(store, target)?;
    let text = text_or_stdin(text)?;
    store
        .append(&meta.id, &text)
        .map_err(|e| CliError::failure(format!("cannot append: {e}")))?;
    println!("{}", meta.id);
    Ok(())
}

fn todo(store: &NoteStore, arguments: &[String]) -> Result<(), CliError> {
    let flags = flags(arguments)?;
    let text = text_or_stdin(&flags.positional)?;
    let items: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("- [ ] {}", line.trim_start_matches("- [ ] ")))
        .collect();
    if items.is_empty() {
        return Err(CliError::failure("nothing to add"));
    }
    let meta = match &flags.to {
        Some(target) => resolve(store, target)?,
        None => inbox_todos(store, flags.project.as_deref())?,
    };
    store
        .append(&meta.id, &items.join("\n"))
        .map_err(|e| CliError::failure(format!("cannot append: {e}")))?;
    println!("{}", meta.id);
    Ok(())
}

/// The note loose to-dos collect in: "To-dos" in the Inbox or the project.
fn inbox_todos(store: &NoteStore, project: Option<&str>) -> Result<NoteMeta, CliError> {
    let notes = store
        .list()
        .map_err(|e| CliError::failure(format!("cannot list notes: {e}")))?;
    if let Some(found) = notes
        .into_iter()
        .find(|n| !n.archived && n.title == "To-dos" && n.project.as_deref() == project)
    {
        return Ok(found);
    }
    let doc = Document::new("To-dos", Vec::<Block>::new());
    let (id, _) = store
        .create(doc, project)
        .map_err(|e| CliError::failure(format!("cannot create note: {e}")))?;
    store
        .meta(&id)
        .map_err(|e| CliError::failure(format!("cannot read note: {e}")))
}

/// An exact id, else the single note whose title contains `query`.
fn resolve(store: &NoteStore, query: &str) -> Result<NoteMeta, CliError> {
    let notes = store
        .list()
        .map_err(|e| CliError::failure(format!("cannot list notes: {e}")))?;
    if let Some(exact) = notes.iter().find(|n| n.id == query) {
        return Ok(exact.clone());
    }
    let needle = query.to_lowercase();
    let exact_title: Vec<&NoteMeta> = notes
        .iter()
        .filter(|n| n.title.to_lowercase() == needle)
        .collect();
    if exact_title.len() == 1 {
        return Ok(exact_title[0].clone());
    }
    let matches: Vec<&NoteMeta> = notes
        .iter()
        .filter(|n| !n.archived && n.title.to_lowercase().contains(&needle))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CliError::not_found(format!("no note matches \"{query}\""))),
        many => Err(CliError::failure(format!(
            "\"{query}\" matches {} notes: {}",
            many.len(),
            many.iter()
                .map(|n| format!("{} ({})", n.display_title(), n.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn list(store: &NoteStore, arguments: &[String]) -> Result<(), CliError> {
    let flags = flags(arguments)?;
    let notes = store
        .list()
        .map_err(|e| CliError::failure(format!("cannot list notes: {e}")))?;
    let notes: Vec<&NoteMeta> = notes
        .iter()
        .filter(|n| flags.all || !n.archived)
        .filter(|n| flags.project.is_none() || n.project == flags.project)
        .collect();
    if flags.json {
        let rows: Vec<serde_json::Value> = notes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "title": n.title,
                    "path": n.path,
                    "project": n.project,
                    "pinned": n.pinned,
                    "archived": n.archived,
                    "todos": { "done": n.todos_done, "total": n.todos_total },
                    "open_todos": n.open_todos.iter().map(|(_, t)| t).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return Ok(());
    }
    for n in notes {
        let todos = if n.todos_total > 0 {
            format!("  [{}/{}]", n.todos_done, n.todos_total)
        } else {
            String::new()
        };
        let pin = if n.pinned { "📌 " } else { "" };
        println!("{}  {pin}{}{todos}", n.id, n.display_title());
    }
    Ok(())
}

fn show(store: &NoteStore, arguments: &[String]) -> Result<(), CliError> {
    let flags = flags(arguments)?;
    let Some(target) = flags.positional.first() else {
        return Err(CliError::failure("usage: dirijor note show NOTE"));
    };
    let meta = resolve(store, target)?;
    let note = store
        .load(&meta.id)
        .map_err(|e| CliError::failure(format!("cannot read note: {e}")))?;
    // The body without front matter reads best in a terminal or a prompt.
    let body = markdown::write(&Default::default(), &note.doc);
    print!("{body}");
    Ok(())
}
