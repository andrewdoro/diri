//! The website's MCP reference (diri.sh/docs/mcp-tools/) is generated from
//! `website/docs-src/mcp-tools.json`. This test keeps that file identical to
//! the live tool catalog, so the published docs cannot drift from the server.
//!
//! Regenerate after changing a tool: `DIRI_UPDATE_DOCS=1 cargo test -p dirijor-mcp --test docs_catalog`.

use std::path::Path;

use serde_json::{Value, json};

#[test]
fn website_mcp_reference_matches_the_tool_catalog() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifests = crate_dir.join("../diri-engine/manifests");
    let reference = crate_dir.join("../../../website/docs-src/mcp-tools.json");

    // The same kinds the Bridge advertises: every non-terminal agent's short
    // label, sorted, then `shell`.
    let mut kinds: Vec<String> = std::fs::read_dir(&manifests)
        .expect("manifests directory")
        .map(|entry| entry.expect("manifest entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let manifest: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read manifest"))
                    .expect("manifest json");
            let id = manifest["id"].as_str()?;
            (id != "shell" && id != "generic")
                .then(|| manifest["agent"]["shortLabel"].as_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds.push("shell".into());

    // `test_run` is only offered when DIRIJOR_TEST_RUN_AVAILABLE is set, so it
    // is left out here to keep the reference identical on every machine.
    let tools: Vec<Value> = dirijor_mcp::tools::tool_definitions_for(&kinds)
        .iter()
        .filter(|tool| tool.name != "test_run")
        .map(|tool| tool.wire_value())
        .collect();
    let catalog = json!({
        "server": "dirijor",
        "skills": diri_proto::skills::ALL.iter().map(|skill| json!({
            "name": skill.name,
            "description": skill.description,
            "markdown": skill.markdown,
        })).collect::<Vec<_>>(),
        "tools": tools,
    });
    // Object key order depends on whether serde_json's `preserve_order` is
    // unified into this build (a whole-workspace build turns it on, a
    // `-p dirijor-mcp` build does not). Compare contents, and always write
    // keys sorted, so the check passes however the crate was built.
    let expected = format!(
        "{}\n",
        serde_json::to_string_pretty(&sorted(&catalog)).expect("serialize")
    );

    if std::env::var_os("DIRI_UPDATE_DOCS").is_some() {
        std::fs::write(&reference, &expected).expect("write website reference");
        return;
    }
    let actual = std::fs::read_to_string(&reference).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&actual).unwrap_or(Value::Null);
    if sorted(&parsed) != sorted(&catalog) {
        let have = format!(
            "{}\n",
            serde_json::to_string_pretty(&sorted(&parsed)).expect("serialize")
        );
        if let Some((line, (want, have))) = expected
            .lines()
            .zip(have.lines())
            .enumerate()
            .find(|(_, (want, have))| want != have)
        {
            eprintln!(
                "first difference at line {}:\n  catalog: {want}\n  file:    {have}",
                line + 1
            );
        }
        panic!(
            "website/docs-src/mcp-tools.json is stale. Run: DIRI_UPDATE_DOCS=1 cargo test -p dirijor-mcp --test docs_catalog"
        );
    }
}

/// The same value with every object's keys in sorted order.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}
