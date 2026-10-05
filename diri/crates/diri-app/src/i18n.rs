//! Applies the interface-language preference. Messages and catalogs live in
//! `diri-i18n`; this module resolves `System` against the platform's preferred
//! languages and re-renders everything after a change.

use gpui::App;

use crate::store::{Prefs, UiLanguage};

pub(crate) use diri_i18n::{Language, english, t, tf};

/// The language a preference shows.
pub(crate) fn resolve(preference: UiLanguage) -> Language {
    match preference {
        UiLanguage::Fixed(language) => language,
        UiLanguage::System => {
            let preferred = preferred_languages();
            Language::resolve(preferred.iter().map(String::as_str))
        }
    }
}

/// Sets the shown language from preferences at startup, before any window.
pub(crate) fn apply_prefs(prefs: &Prefs) {
    diri_i18n::set_language(resolve(prefs.ui_language));
}

/// Shows `preference` everywhere now: every window renders again (cached
/// views included) and the native menus are rebuilt with their new titles.
pub(crate) fn apply_live(preference: UiLanguage, cx: &mut App) {
    let language = resolve(preference);
    if language == diri_i18n::language() {
        return;
    }
    diri_i18n::set_language(language);
    crate::refresh_app_menus(cx);
    cx.refresh_windows();
}

/// The reader's preferred languages, most preferred first.
#[cfg(target_os = "macos")]
fn preferred_languages() -> Vec<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .iter()
        .map(|language| language.to_string())
        .collect()
}

/// POSIX lookup order: `LANGUAGE` is a colon list, then the first set of
/// `LC_ALL`, `LC_MESSAGES` and `LANG` decides.
#[cfg(not(target_os = "macos"))]
fn preferred_languages() -> Vec<String> {
    let mut preferred: Vec<String> = std::env::var("LANGUAGE")
        .unwrap_or_default()
        .split(':')
        .filter(|language| !language.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(locale) = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
    {
        preferred.push(locale);
    }
    preferred
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }

    /// Every string literal in `source`, escapes left as written.
    fn literals(source: &str) -> impl Iterator<Item = &str> {
        let mut rest = source;
        std::iter::from_fn(move || {
            loop {
                let open = rest.find('"')?;
                // Skip char literals such as '"'.
                if rest[..open].ends_with('\'') {
                    rest = &rest[open + 1..];
                    continue;
                }
                let body = &rest[open + 1..];
                let mut end = 0;
                let bytes = body.as_bytes();
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                let literal = &body[..end.min(body.len())];
                rest = &body[(end + 1).min(body.len())..];
                return Some(literal);
            }
        })
    }

    /// The literals `source` hands to the catalog: the first argument of a
    /// `t(`, `tf(` or `english(` call, and every literal in a `t(match ...)`
    /// arm list.
    fn fed_ids(source: &str) -> Vec<&str> {
        let mut ids = Vec::new();
        for call in ["t(", "tf(", "english("] {
            for (at, _) in source.match_indices(call) {
                let before = source[..at].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let args = source[at + call.len()..].trim_start();
                if let Some(arm_list) = args.strip_prefix("match ") {
                    let end = arm_list.find("})").unwrap_or(arm_list.len());
                    ids.extend(literals(&arm_list[..end]));
                } else if args.starts_with('"') {
                    ids.extend(literals(args).next());
                }
            }
        }
        ids
    }

    /// Every id fed to the catalog exists, and every catalog id is still
    /// written somewhere in the app.
    #[test]
    fn source_ids_and_catalog_ids_agree() {
        let keys: BTreeSet<&str> = diri_i18n::keys().collect();
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = BTreeSet::new();
        let mut unknown = Vec::new();
        // This module's own tests name the calls they look for.
        files.retain(|(path, _)| !path.ends_with("src/i18n.rs"));
        for (path, source) in &files {
            used.extend(literals(source).filter(|literal| keys.contains(literal)));
            for id in fed_ids(source) {
                if !keys.contains(id) {
                    unknown.push(format!("{path}: {id:?}"));
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "ids missing from the catalog:\n{}",
            unknown.join("\n")
        );
        let unused: Vec<_> = keys.iter().filter(|key| !used.contains(*key)).collect();
        assert!(unused.is_empty(), "catalog ids no source uses: {unused:?}");
    }

    #[test]
    fn preference_resolves_fixed_languages_verbatim() {
        use crate::store::UiLanguage;
        for language in super::Language::ALL {
            assert_eq!(super::resolve(UiLanguage::Fixed(language)), language);
        }
    }
}
