//! Where a local Agent reports working, folded from its hooks.
//!
//! `SessionRecord.cwd` is the launch directory and owns the Session's
//! project. An Agent can still move into another checkout of the same
//! repository (`git worktree add ../fix && cd ../fix`, Claude's worktree
//! isolation) or edit files there by absolute path. The record keeps the
//! directory the Agent last moved to and a short list of files it edited, so
//! a client can follow the work without scraping the terminal. Mapping those
//! paths into worktrees is the client's job: it needs Git, and the Engine's
//! hook path must never wait on a subprocess.

use diri_proto::{AgentPlace, AgentWorkspace, DateMillis};

/// Folds one hook's `cwd` and edited file into `workspace`. Returns whether
/// anything a client would see changed.
///
/// The cwd keeps the time it was *first* reported: every hook repeats it, and
/// refreshing the time on each report would make an edit in another checkout
/// look older than a cwd that never moved. Re-editing the file already at the
/// front changes nothing, so a run of edits to one file publishes once.
pub(crate) fn fold(
    workspace: &mut Option<AgentWorkspace>,
    cwd: Option<&str>,
    edited: Option<&str>,
    now: DateMillis,
) -> bool {
    if cwd.is_none() && edited.is_none() {
        return false;
    }
    let state = workspace.get_or_insert_with(AgentWorkspace::default);
    let mut changed = false;
    if let Some(cwd) = cwd
        && state.cwd.as_ref().is_none_or(|place| place.path != cwd)
    {
        state.cwd = Some(AgentPlace {
            path: cwd.to_owned(),
            at: now,
        });
        changed = true;
    }
    if let Some(path) = edited
        && state.edits.first().is_none_or(|place| place.path != path)
    {
        state.edits.retain(|place| place.path != path);
        state.edits.insert(
            0,
            AgentPlace {
                path: path.to_owned(),
                at: now,
            },
        );
        state.edits.truncate(AgentWorkspace::MAX_EDITS);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: f64) -> DateMillis {
        DateMillis(ms)
    }

    #[test]
    fn a_repeated_cwd_keeps_the_time_it_moved_there() {
        let mut workspace = None;
        assert!(fold(&mut workspace, Some("/repo"), None, at(1.0)));
        assert!(!fold(&mut workspace, Some("/repo"), None, at(2.0)));
        let cwd = workspace.as_ref().unwrap().cwd.clone().unwrap();
        assert_eq!((cwd.path.as_str(), cwd.at), ("/repo", at(1.0)));
        assert!(fold(&mut workspace, Some("/repo-fix"), None, at(3.0)));
        assert_eq!(workspace.unwrap().cwd.unwrap().at, at(3.0));
    }

    #[test]
    fn edits_are_distinct_most_recent_first_and_bounded() {
        let mut workspace = None;
        assert!(!fold(&mut workspace, None, None, at(0.0)));
        assert!(workspace.is_none());
        assert!(fold(&mut workspace, None, Some("/repo/a"), at(1.0)));
        assert!(!fold(&mut workspace, None, Some("/repo/a"), at(2.0)));
        assert!(fold(&mut workspace, None, Some("/wt/b"), at(3.0)));
        assert!(fold(&mut workspace, None, Some("/repo/a"), at(4.0)));
        let edits = &workspace.as_ref().unwrap().edits;
        assert_eq!(
            edits
                .iter()
                .map(|place| (place.path.as_str(), place.at))
                .collect::<Vec<_>>(),
            [("/repo/a", at(4.0)), ("/wt/b", at(3.0))]
        );
        for index in 0..40 {
            fold(
                &mut workspace,
                None,
                Some(&format!("/repo/{index}")),
                at(10.0 + f64::from(index)),
            );
        }
        let edits = &workspace.unwrap().edits;
        assert_eq!(edits.len(), AgentWorkspace::MAX_EDITS);
        assert_eq!(edits[0].path, "/repo/39");
    }
}
