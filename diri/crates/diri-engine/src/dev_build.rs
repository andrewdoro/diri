//! Whether this process is a development build: a binary cargo left in its
//! target directory, not one a package or the app bundle installed.
//!
//! Agents developing diri start Engines and Holder managers from `target/`
//! for tests, demos and screenshots, and nothing ever stops them: on
//! 2026-10-04 five such Engines and nine managers (still hosting Claude
//! sessions) had piled up over three weeks, about 1.2 GB. A development
//! build therefore retires once nothing needs it; an installed one keeps
//! today's policy (its Holders must outlive Engine crashes and upgrades).

use std::path::Path;
use std::sync::OnceLock;

/// True when the running executable sits in a cargo profile directory
/// (`target/<profile>/` or its `deps/`), which cargo marks with
/// `.fingerprint`. Decided once per process.
pub fn is_development_build() -> bool {
    static DEVELOPMENT: OnceLock<bool> = OnceLock::new();
    *DEVELOPMENT.get_or_init(|| {
        std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .is_ok_and(|exe| in_cargo_profile(&exe))
    })
}

fn in_cargo_profile(executable: &Path) -> bool {
    executable
        .ancestors()
        .skip(1)
        .take(2)
        .any(|directory| directory.join(".fingerprint").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_profiles_are_development_and_bundles_are_not() {
        // This test binary runs from target/<profile>/deps.
        assert!(is_development_build());
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("target/debug");
        std::fs::create_dir_all(profile.join(".fingerprint")).unwrap();
        std::fs::create_dir_all(profile.join("deps")).unwrap();
        assert!(in_cargo_profile(&profile.join("dirijord-rs")));
        assert!(in_cargo_profile(&profile.join("deps/holder-1234")));
        let bundle = root.path().join("diri.app/Contents/Resources/bin");
        std::fs::create_dir_all(&bundle).unwrap();
        assert!(!in_cargo_profile(&bundle.join("dirijord-rs")));
    }
}
