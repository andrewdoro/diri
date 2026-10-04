use std::fs;
use std::path::PathBuf;

use sha2::{Digest as _, Sha256};

fn main() {
    emit_app_version();

    let manifest_dir = PathBuf::from("manifests");
    println!("cargo:rerun-if-changed={}", manifest_dir.display());

    // A packaging that ships the crate without its catalog (vendoring, a sparse
    // checkout) should still build; it just cannot claim a catalog identity.
    let Ok(entries) = fs::read_dir(&manifest_dir) else {
        println!(
            "cargo:warning=no Agent catalog at {}; the Engine build identity will not track manifests",
            manifest_dir.display()
        );
        println!("cargo:rustc-env=DIRI_AGENT_CATALOG_BUILD_ID=unknown");
        return;
    };

    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut digest = Sha256::new();
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_name()
            .expect("Agent manifest file name")
            .to_string_lossy();
        let bytes = fs::read(&path).expect("read Agent manifest");
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    let catalog_id = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("cargo:rustc-env=DIRI_AGENT_CATALOG_BUILD_ID={catalog_id}");
}

/// The product version is `diri-app`'s; this crate stays at 0.1.0. Outside a
/// macOS bundle (Linux packages, source builds) it is the only way the Engine
/// can name the release it shipped in.
fn emit_app_version() {
    let manifest = PathBuf::from("../diri-app/Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let version = fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines()
                .skip_while(|line| line.trim() != "[package]")
                .skip(1)
                .take_while(|line| !line.trim_start().starts_with('['))
                .find_map(|line| {
                    let value = line.trim().strip_prefix("version")?.trim_start();
                    let value = value.strip_prefix('=')?.trim();
                    Some(value.trim_matches('"').to_owned())
                })
        })
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=DIRI_APP_VERSION={version}");
}
