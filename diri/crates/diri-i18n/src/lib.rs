//! The interface language: one message catalog per language and the one
//! currently shown.
//!
//! UI code names a message by id (`t("settings.tab.general")`) instead of
//! writing English inline, so a translation never touches UI logic. Catalogs
//! are flat JSON objects, one file per UI area at `locales/<tag>/<area>.json`,
//! compiled into the binary by `build.rs`: there is no resource bundle to go
//! missing at runtime. `en` is canonical; every other language has the same
//! area files holding exactly the same ids, which tests enforce.
//!
//! The shown language is process-wide so `&'static str` label functions can
//! translate without a GPUI context; after `set_language` the app refreshes
//! its windows and rebuilds its native menus.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// A language diri ships a catalog for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    English,
    SimplifiedChinese,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::SimplifiedChinese];

    /// The BCP 47 tag, as persisted in preferences.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-Hans",
        }
    }

    /// The language's own name for itself; a picker shows these untranslated
    /// so a reader can find their language from any other.
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.tag() == tag)
    }

    /// The catalog language for a system locale identifier such as
    /// `zh-Hans-CN`, `zh_CN.UTF-8` or `en-GB`. Traditional Chinese has no
    /// catalog, so `zh-Hant`/`zh-TW`/`zh-HK`/`zh-MO` match nothing rather than
    /// showing Simplified characters.
    pub fn for_locale(identifier: &str) -> Option<Self> {
        let identifier = identifier
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .replace('_', "-")
            .to_ascii_lowercase();
        let mut parts = identifier.split('-');
        match parts.next()? {
            "en" => Some(Self::English),
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                let traditional = rest
                    .iter()
                    .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo"));
                let simplified = rest.contains(&"hans");
                (simplified || !traditional).then_some(Self::SimplifiedChinese)
            }
            _ => None,
        }
    }

    /// The first of the reader's preferred locales diri has a catalog for,
    /// English when none matches.
    pub fn resolve<'a>(preferred: impl IntoIterator<Item = &'a str>) -> Self {
        preferred
            .into_iter()
            .find_map(Self::for_locale)
            .unwrap_or_default()
    }

    fn index(self) -> u8 {
        match self {
            Self::English => 0,
            Self::SimplifiedChinese => 1,
        }
    }

    fn from_index(index: u8) -> Self {
        match index {
            1 => Self::SimplifiedChinese,
            _ => Self::English,
        }
    }
}

mod sources {
    include!(concat!(env!("OUT_DIR"), "/sources.rs"));
}

type Catalog = HashMap<&'static str, &'static str>;

/// The area files of `language`, as `(area, json)`.
fn areas(language: Language) -> impl Iterator<Item = (&'static str, &'static str)> {
    sources::SOURCES
        .iter()
        .filter(move |(tag, _, _)| *tag == language.tag())
        .map(|(_, area, json)| (*area, *json))
}

/// Parses a language's area files once. Messages live for the whole process,
/// so leaking the owned strings serde produces keeps `t` returning
/// `&'static str`.
fn parse(language: Language) -> Catalog {
    let mut catalog = Catalog::new();
    for (area, json) in areas(language) {
        for (key, message) in parse_area(json)
            .unwrap_or_else(|error| panic!("{}/{area}.json: {error}", language.tag()))
        {
            catalog.insert(
                Box::leak(key.into_boxed_str()),
                Box::leak(message.into_boxed_str()),
            );
        }
    }
    catalog
}

fn parse_area(json: &str) -> Result<Vec<(String, String)>, String> {
    let messages: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).map_err(|error| error.to_string())?;
    messages
        .into_iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(message) => Ok((key, message)),
            _ => Err(format!("{key:?} is not a string")),
        })
        .collect()
}

static CATALOGS: LazyLock<[Catalog; 2]> = LazyLock::new(|| Language::ALL.map(parse));

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The language every message is shown in.
pub fn language() -> Language {
    Language::from_index(CURRENT.load(Ordering::Relaxed))
}

/// Shows `language` from the next render on. Callers refresh their windows.
pub fn set_language(language: Language) {
    CURRENT.store(language.index(), Ordering::Relaxed);
}

fn catalog(language: Language) -> &'static Catalog {
    &CATALOGS[usize::from(language.index())]
}

/// The shown language's message for `key`. A key the catalog lacks reads the
/// English message, and an unknown key reads as itself; tests keep both from
/// shipping.
pub fn t(key: &'static str) -> &'static str {
    lookup(language(), key)
}

/// The English message for `key`, whatever language is shown.
pub fn english(key: &'static str) -> &'static str {
    lookup(Language::English, key)
}

fn lookup(language: Language, key: &'static str) -> &'static str {
    catalog(language)
        .get(key)
        .or_else(|| catalog(Language::English).get(key))
        .copied()
        .unwrap_or(key)
}

/// `t(key)` with each `{name}` replaced by its argument. A placeholder without
/// an argument stays as written, so a mistake shows instead of panicking.
pub fn tf(key: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(key), args)
}

fn fill(message: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(message.len() + 16);
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match args.iter().find(|(arg, _)| *arg == name) {
            Some((_, value)) => out.push_str(&value.to_string()),
            None => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Every id in the canonical English catalog, for source-scanning tests.
pub fn keys() -> impl Iterator<Item = &'static str> {
    catalog(Language::English).keys().copied()
}

/// The `{name}` placeholders a message uses, sorted.
#[cfg(test)]
fn placeholders(message: &str) -> Vec<&str> {
    let mut names: Vec<&str> = message
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_holds_exactly_the_english_ids() {
        let english = catalog(Language::English);
        for language in Language::ALL {
            let other = catalog(language);
            let mut missing: Vec<_> = english
                .keys()
                .filter(|key| !other.contains_key(*key))
                .collect();
            let mut extra: Vec<_> = other
                .keys()
                .filter(|key| !english.contains_key(*key))
                .collect();
            missing.sort();
            extra.sort();
            assert!(missing.is_empty(), "{} misses {missing:?}", language.tag());
            assert!(extra.is_empty(), "{} has stray {extra:?}", language.tag());
        }
    }

    #[test]
    fn translations_keep_their_placeholders() {
        for language in Language::ALL {
            for (key, message) in catalog(language) {
                let english = catalog(Language::English)[key];
                assert_eq!(
                    placeholders(message),
                    placeholders(english),
                    "{} {key:?} changes its placeholders",
                    language.tag()
                );
                assert!(
                    !message.trim().is_empty(),
                    "{} {key:?} is empty",
                    language.tag()
                );
            }
        }
    }

    /// Ids as written in an area file, in file order. serde_json would
    /// silently keep the last of two equal ids, so tests read the raw lines.
    fn raw_ids(json: &str) -> Vec<&str> {
        json.lines()
            .filter_map(|line| line.trim().strip_prefix('"'))
            .filter_map(|line| line.split_once("\":").map(|(id, _)| id))
            .collect()
    }

    #[test]
    fn area_files_are_sorted_unique_and_one_id_per_line() {
        let mut seen = std::collections::HashMap::new();
        for (tag, area, json) in sources::SOURCES {
            let ids = raw_ids(json);
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                ids, sorted,
                "{tag}/{area}.json ids must be sorted and unique"
            );
            let parsed =
                parse_area(json).unwrap_or_else(|error| panic!("{tag}/{area}.json: {error}"));
            assert_eq!(
                ids.len(),
                parsed.len(),
                "{tag}/{area}.json must hold one id per line"
            );
            for id in ids {
                if let Some(other) = seen.insert((*tag, id), *area) {
                    panic!("{tag}: {id:?} is in both {other}.json and {area}.json");
                }
            }
        }
    }

    #[test]
    fn every_language_has_the_english_area_files() {
        let english: Vec<_> = areas(Language::English).map(|(area, _)| area).collect();
        assert!(!english.is_empty());
        for language in Language::ALL {
            for (area, json) in areas(language) {
                assert!(
                    english.contains(&area),
                    "{}/{area}.json has no English file",
                    language.tag()
                );
                let english_json = areas(Language::English)
                    .find(|(other, _)| *other == area)
                    .unwrap()
                    .1;
                assert_eq!(
                    raw_ids(json),
                    raw_ids(english_json),
                    "{}/{area}.json ids differ from en",
                    language.tag()
                );
            }
            assert_eq!(
                areas(language).count(),
                english.len(),
                "{} misses area files",
                language.tag()
            );
        }
        let tags: std::collections::BTreeSet<_> =
            sources::SOURCES.iter().map(|(tag, _, _)| *tag).collect();
        let known: std::collections::BTreeSet<_> = Language::ALL
            .iter()
            .map(|language| language.tag())
            .collect();
        assert_eq!(tags, known, "every locales/ directory is a Language");
    }

    #[test]
    fn a_message_fills_its_arguments() {
        let count = 3;
        assert_eq!(
            fill("{n} of {total}", &[("n", &count), ("total", &"7")]),
            "3 of 7"
        );
        assert_eq!(fill("{a}{a}", &[("a", &"x")]), "xx");
        assert_eq!(fill("{missing} stays", &[]), "{missing} stays");
        assert_eq!(fill("open { brace", &[]), "open { brace");
    }

    #[test]
    fn system_locales_resolve_to_a_catalog() {
        let cases = [
            ("zh-Hans-CN", Some(Language::SimplifiedChinese)),
            ("zh-CN", Some(Language::SimplifiedChinese)),
            ("zh_CN.UTF-8", Some(Language::SimplifiedChinese)),
            ("zh-SG", Some(Language::SimplifiedChinese)),
            ("zh", Some(Language::SimplifiedChinese)),
            ("zh-Hant-TW", None),
            ("zh-TW", None),
            ("zh_HK.UTF-8", None),
            ("en-GB", Some(Language::English)),
            ("en_US.UTF-8", Some(Language::English)),
            ("de-DE", None),
            ("C", None),
            ("", None),
        ];
        for (identifier, expected) in cases {
            assert_eq!(Language::for_locale(identifier), expected, "{identifier:?}");
        }
        assert_eq!(
            Language::resolve(["de-DE", "zh-Hans-DE", "en"]),
            Language::SimplifiedChinese
        );
        assert_eq!(Language::resolve(["en-GB", "zh-Hans"]), Language::English);
        assert_eq!(Language::resolve(["fr-FR"]), Language::English);
    }

    #[test]
    fn tags_round_trip() {
        for language in Language::ALL {
            assert_eq!(Language::from_tag(language.tag()), Some(language));
        }
        assert_eq!(Language::from_tag("system"), None);
    }

    #[test]
    fn switching_language_changes_messages() {
        let key = "settings.tab.general";
        assert_eq!(lookup(Language::English, key), "General");
        assert_eq!(lookup(Language::SimplifiedChinese, key), "通用");
    }
}
