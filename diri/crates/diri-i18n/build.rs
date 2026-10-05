//! Compiles every `locales/<tag>/<area>.json` into the binary, so a new area
//! file needs no registration and no resource bundle ships beside the app.

use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out = String::from("pub(crate) const SOURCES: &[(&str, &str, &str)] = &[\n");
    let mut languages: Vec<_> = fs::read_dir(&root)
        .expect("locales directory")
        .map(|entry| entry.expect("locale entry").path())
        .filter(|path| path.is_dir())
        .collect();
    languages.sort();
    for language in languages {
        println!("cargo:rerun-if-changed={}", language.display());
        let tag = language.file_name().unwrap().to_str().unwrap().to_owned();
        let mut areas: Vec<_> = fs::read_dir(&language)
            .expect("locale directory")
            .map(|entry| entry.expect("area entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        areas.sort();
        for area in areas {
            println!("cargo:rerun-if-changed={}", area.display());
            let name = area.file_stem().unwrap().to_str().unwrap();
            writeln!(
                out,
                "    ({tag:?}, {name:?}, include_str!({:?})),",
                area.display().to_string()
            )
            .unwrap();
        }
    }
    out.push_str("];\n");
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("sources.rs");
    fs::write(dest, out).unwrap();
}
