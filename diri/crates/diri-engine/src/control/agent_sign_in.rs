//! Whether an installed Agent already has a login on this Mac.
//!
//! A newcomer's first Claude Code or Codex launch opens on the CLI's own
//! sign-in screens. Knowing that ahead of time lets the welcome say "Sign in
//! to Claude Code" instead of promising a session that starts with a login
//! menu. The answer comes from the same stores the CLIs read, without reading
//! a secret: file existence, a Keychain item's attributes, and non-secret
//! JSON fields.
//!
//! It is deliberately one-sided. Any sign of another way to authenticate
//! (an API key in the environment, a key helper, a cloud provider, a Diri
//! account profile that picks its own store) makes the answer `None`, so a
//! signed-in user is never told to sign in.
use super::*;
use std::fs;

/// Environment that authenticates Claude Code without its OAuth store.
const CLAUDE_ALTERNATE_AUTH: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

/// Environment that authenticates Codex without `auth.json`, or moves it.
const CODEX_ALTERNATE_AUTH: &[&str] = &["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_HOME"];

/// Files the probe reads are small JSON; anything larger is not ours to parse.
const LIMIT: u64 = 1024 * 1024;

impl ControlServer {
    /// The sign-in fact for one local Agent, or `None` when Diri cannot tell.
    pub(super) fn agent_signed_in(&self, id: &str) -> Option<bool> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|home| !home.is_empty())?);
        let env = |key: &str| std::env::var_os(key).is_some_and(|value| !value.is_empty());
        match id {
            diri_proto::AgentKind::CLAUDE_CODE_ID => {
                if self.has_default_profile(id) {
                    return None;
                }
                claude_signed_in(&home, &env, claude_accounts::default_login_present)
            }
            diri_proto::AgentKind::CODEX_ID => {
                if self.has_default_profile(id) {
                    return None;
                }
                codex_signed_in(&home, &env)
            }
            _ => None,
        }
    }

    /// A default local account profile launches with its own store, which
    /// this probe does not model.
    fn has_default_profile(&self, agent: &str) -> bool {
        let Ok(accounts) = self.accounts.lock() else {
            return true;
        };
        accounts.catalog().map_or(true, |catalog| {
            catalog.profiles.iter().any(|profile| {
                profile.is_default && profile.agent == agent && profile.host.is_none()
            })
        })
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn claude_signed_in(
    home: &Path,
    env: &dyn Fn(&str) -> bool,
    keychain_login: impl FnOnce(&Path) -> Option<bool>,
) -> Option<bool> {
    if CLAUDE_ALTERNATE_AUTH.iter().any(|key| env(key)) {
        return None;
    }
    let config_home = home.join(".claude");
    if let Some(global) = read_json(&home.join(".claude.json")) {
        // `oauthAccount` is the identity `/status` shows once signed in, and
        // `primaryApiKey` is a Console key Claude stored itself.
        if global
            .get("oauthAccount")
            .is_some_and(|value| !value.is_null())
            || global
                .get("primaryApiKey")
                .is_some_and(|value| !value.is_null())
        {
            return Some(true);
        }
    }
    if let Some(settings) = read_json(&config_home.join("settings.json")) {
        let routed = settings.get("apiKeyHelper").is_some()
            || settings
                .get("env")
                .and_then(|env| env.as_object())
                .is_some_and(|env| {
                    CLAUDE_ALTERNATE_AUTH
                        .iter()
                        .any(|key| env.contains_key(*key))
                });
        if routed {
            return None;
        }
    }
    keychain_login(&config_home)
}

fn codex_signed_in(home: &Path, env: &dyn Fn(&str) -> bool) -> Option<bool> {
    if CODEX_ALTERNATE_AUTH.iter().any(|key| env(key)) {
        return None;
    }
    let codex_home = home.join(".codex");
    // A keyring credential store keeps the login out of auth.json.
    if fs::read_to_string(codex_home.join("config.toml"))
        .is_ok_and(|config| config.contains("cli_auth_credentials_store"))
    {
        return None;
    }
    if let Some(auth) = read_json(&codex_home.join("auth.json")) {
        let present = |value: &serde_json::Value| value.as_str().is_some_and(|s| !s.is_empty());
        return Some(present(&auth["OPENAI_API_KEY"]) || present(&auth["tokens"]["refresh_token"]));
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn a_fresh_mac_reads_as_signed_out_for_both_agents() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            claude_signed_in(home.path(), &none, |_| Some(false)),
            Some(false)
        );
        assert_eq!(codex_signed_in(home.path(), &none), Some(false));
    }

    #[test]
    fn claude_logins_are_found_in_the_keychain_or_its_global_config() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            claude_signed_in(home.path(), &none, |_| Some(true)),
            Some(true)
        );
        fs::write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"a@example.com"}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_signed_in(home.path(), &none, |_| Some(false)),
            Some(true)
        );
    }

    #[test]
    fn any_other_way_to_authenticate_claude_is_never_called_signed_out() {
        let home = tempfile::tempdir().unwrap();
        let api_key = |key: &str| key == "ANTHROPIC_API_KEY";
        assert_eq!(
            claude_signed_in(home.path(), &api_key, |_| Some(false)),
            None
        );
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude/settings.json"),
            r#"{"env":{"CLAUDE_CODE_USE_BEDROCK":"1"}}"#,
        )
        .unwrap();
        assert_eq!(claude_signed_in(home.path(), &none, |_| Some(false)), None);
        assert_eq!(
            claude_signed_in(home.path(), &none, |_| None),
            None,
            "a Keychain that would not answer is unknown, not signed out"
        );
    }

    #[test]
    fn codex_reads_auth_json_and_stays_unknown_for_a_keyring_store() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            codex.join("auth.json"),
            r#"{"tokens":{"access_token":"a","refresh_token":"r"}}"#,
        )
        .unwrap();
        assert_eq!(codex_signed_in(home.path(), &none), Some(true));
        fs::write(codex.join("auth.json"), r#"{"OPENAI_API_KEY":null}"#).unwrap();
        assert_eq!(codex_signed_in(home.path(), &none), Some(false));
        fs::write(
            codex.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        assert_eq!(codex_signed_in(home.path(), &none), None);
        let key = |key: &str| key == "OPENAI_API_KEY";
        fs::remove_file(codex.join("config.toml")).unwrap();
        assert_eq!(codex_signed_in(home.path(), &key), None);
    }
}
