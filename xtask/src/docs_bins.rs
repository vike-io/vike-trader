//! `docs-bins` — every `[[bin]]` target the workspace declares, rendered as the `bins.json`
//! docs-data release asset.
//!
//! This lives in xtask rather than beside the other renderers in `crates/vike-ops/src/docs_data.rs`
//! because its input is `cargo metadata`, the document [`crate::ci::graph::load_metadata`] already
//! reads for the crate roster — and a renderer over compile-time tables (which is what `docs_data`
//! is) cannot see a manifest. Same rule as the roster itself: DERIVED from the workspace, never
//! restated, so a binaries index built from this cannot be left incomplete by a `[[bin]]` somebody
//! forgot to mention.
//!
//! # Output contract
//!
//! `{ "bins": [ { "name", "crate", "required_features": [..] }, .. ] }`, one entry per target whose
//! `kind` is exactly `["bin"]` on a WORKSPACE MEMBER, sorted by `(crate, name)` — so a full
//! metadata document and a `--no-deps` one render identically, and the order does not depend on
//! how cargo listed packages. `required_features` is `[]` when the target declares none, never
//! absent. Workspace-EXCLUDED crates (the vendored `ibapi`, the ctrader `protogen` recipe) are not
//! members and therefore not listed: no lane builds them and they ship nothing.

use std::collections::BTreeSet;

use serde_json::{Value, json};

/// Render `bins.json` from a `cargo metadata --format-version 1` document.
///
/// # Errors
/// The document lacks a field this renderer needs (`workspace_members`, `packages`, a package's
/// `id`/`name`/`targets`, a target's `kind`/`name`) — a metadata format this code does not
/// understand is refused outright rather than rendered as an empty index.
pub fn bins_value(meta: &Value) -> Result<Value, String> {
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace_members array")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let packages = meta["packages"].as_array().ok_or("cargo metadata has no packages array")?;
    let mut bins: Vec<(String, String, Vec<String>)> = Vec::new();
    for pkg in packages {
        let id = pkg["id"].as_str().ok_or("a package has no id")?;
        if !members.contains(id) {
            continue;
        }
        let krate = pkg["name"].as_str().ok_or_else(|| format!("package {id} has no name"))?;
        let targets = pkg["targets"]
            .as_array()
            .ok_or_else(|| format!("package {krate} has no targets array"))?;
        for target in targets {
            let kinds = target["kind"]
                .as_array()
                .ok_or_else(|| format!("a target of {krate} has no kind array"))?;
            if !(kinds.len() == 1 && kinds[0] == "bin") {
                continue;
            }
            let name = target["name"]
                .as_str()
                .ok_or_else(|| format!("a bin target of {krate} has no name"))?;
            let required: Vec<String> = target["required-features"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            bins.push((krate.to_string(), name.to_string(), required));
        }
    }
    bins.sort();
    let entries: Vec<Value> = bins
        .into_iter()
        .map(|(krate, name, required_features)| {
            json!({ "name": name, "crate": krate, "required_features": required_features })
        })
        .collect();
    Ok(json!({ "bins": entries }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A planted document: two members (listed b-before-a, to prove the sort), one non-member
    /// with a bin of its own, a lib target, a build script, a bin with `required-features`, and
    /// one without.
    fn planted() -> Value {
        json!({
            "workspace_members": ["b 0.1.0 (path+file:///w/b)", "a 0.1.0 (path+file:///w/a)"],
            "packages": [
                {
                    "id": "b 0.1.0 (path+file:///w/b)", "name": "b",
                    "targets": [
                        { "kind": ["bin"], "name": "b" }
                    ]
                },
                {
                    "id": "dep 1.0.0 (registry+https://x)", "name": "dep",
                    "targets": [ { "kind": ["bin"], "name": "dep-cli" } ]
                },
                {
                    "id": "a 0.1.0 (path+file:///w/a)", "name": "a",
                    "targets": [
                        { "kind": ["lib"], "name": "a" },
                        { "kind": ["bin"], "name": "a-tool", "required-features": ["x", "y"] },
                        { "kind": ["custom-build"], "name": "build-script-build" }
                    ]
                }
            ]
        })
    }

    #[test]
    fn members_bins_only_sorted_by_crate_then_name_with_required_features_always_present() {
        let out = bins_value(&planted()).expect("a well-formed document renders");
        assert_eq!(
            out,
            json!({ "bins": [
                { "name": "a-tool", "crate": "a", "required_features": ["x", "y"] },
                { "name": "b", "crate": "b", "required_features": [] },
            ] })
        );
    }

    #[test]
    fn a_document_without_the_fields_this_needs_is_refused_not_rendered_empty() {
        assert!(bins_value(&json!({})).is_err(), "no workspace_members");
        assert!(bins_value(&json!({ "workspace_members": [] })).is_err(), "no packages");
        let bad_target = json!({
            "workspace_members": ["a 0.1.0 (path+file:///w/a)"],
            "packages": [{ "id": "a 0.1.0 (path+file:///w/a)", "name": "a",
                           "targets": [{ "kind": ["bin"] }] }]
        });
        assert!(bins_value(&bad_target).is_err(), "a bin target without a name");
    }

    #[test]
    fn a_member_with_no_bin_targets_contributes_nothing() {
        let libs_only = json!({
            "workspace_members": ["a 0.1.0 (path+file:///w/a)"],
            "packages": [{ "id": "a 0.1.0 (path+file:///w/a)", "name": "a",
                           "targets": [{ "kind": ["lib"], "name": "a" }] }]
        });
        assert_eq!(bins_value(&libs_only).expect("renders"), json!({ "bins": [] }));
    }
}
