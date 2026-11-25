//! The rendered `profile.json` document: the walked schema, its rosters and its refusals.

use std::collections::BTreeMap;

use vike_fills::refusals::CostRefusalId;

use super::refusal_sites::{Raised, Site, cost_raised_at, profile_raised_at};
use super::walk::{unwrap_type, walk};
use super::{
    Key, PLANE, PROFILE_JSON, PROFILE_SOURCE_PATH, ROOT_STRUCT, SCHEMA_VERSION, SERIES_SOURCE_PATH,
    Table, profile_rosters, profile_tables,
};
use crate::run::profile::SizerKind;
use crate::run::refusals::ProfileRefusalId;

/// The published assets, by name.
pub fn rendered_files() -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    out.insert(PROFILE_JSON, render_profile_json());
    out
}

/// The profile's JSON schema, as the derive on the structs states it.
pub(super) fn profile_schema() -> serde_json::Value {
    schemars::schema_for!(crate::run::profile::ProfileBacktest).to_value()
}

/// Render `profile.json`. Ends with a newline, like every other text asset this repo publishes.
pub(super) fn render_profile_json() -> String {
    let structs = profile_tables();
    let schema = profile_schema();
    let w = walk(&schema, &structs);
    let enums = profile_rosters();

    // Which enum backs which key, so a renderer can bind a roster to a field without guessing.
    let mut roster_of: BTreeMap<String, String> = BTreeMap::new();
    for k in &w.keys {
        let (bare, _) = unwrap_type(&k.ty);
        if enums.iter().any(|e| e.name == bare) {
            roster_of.insert(k.path.clone(), bare);
        } else {
            let def = &schema["$defs"][&bare];
            assert!(
                def.get("enum").is_none() && def.get("oneOf").is_none(),
                "profile_surface: `{}` takes the enum `{bare}`, which no `profile_rosters` row names",
                k.path
            );
        }
    }

    let mut rosters: Vec<serde_json::Value> = enums
        .iter()
        .map(|e| {
            let keys: Vec<&String> =
                roster_of.iter().filter(|(_, v)| **v == e.name).map(|(k, _)| k).collect();
            serde_json::json!({
                "id": e.name,
                "members": e.members,
                "default": e.default,
                "derived_from": format!("the `{}` enum's variants", e.name),
                "keys": keys,
                "evidence": { "file": e.file, "symbol": e.name },
            })
        })
        .collect();

    let sizer_file = structs.get("ProfileEngineSizer").map(|s| s.file).unwrap_or_else(|| {
        panic!("profile_surface: `ProfileEngineSizer` has no `profile_tables` row — the sizer roster moved")
    });
    rosters.push(serde_json::json!({
        "id": "SizerKindName",
        "members": SizerKind::WORDS,
        "default": serde_json::Value::Null,
        "derived_from": "the `SizerKind` enum's words",
        "keys": ["engine.sizer.kind"],
        "evidence": { "file": sizer_file, "symbol": "SizerKind" },
    }));

    let doc = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "plane": PLANE,
        "root_struct": ROOT_STRUCT,
        "sources": [
            { "path": PROFILE_SOURCE_PATH, "owns": "the profile schema" },
            { "path": SERIES_SOURCE_PATH, "owns": "[[data.series]]" },
        ],
        "tables": w.tables.iter().map(|t| serde_json::json!({
            "table": t.table,
            "struct": t.strukt,
            "closed": t.closed,
            "array_of_tables": t.array_of_tables,
            "required": t.required,
            "evidence": { "file": t.source, "symbol": t.strukt },
        })).collect::<Vec<_>>(),
        "keys": w.keys.iter().map(|k| serde_json::json!({
            "table": k.table,
            "key": k.key,
            "path": k.path,
            "type": k.ty,
            "required": k.required,
            "nullable": k.nullable,
            "shape": k.shape,
            "default": {
                "kind": k.default_kind,
                "fn": k.default_fn,
                "value": k.default_value,
            },
            "aliases": k.aliases,
            "nested": {
                "struct": k.nested_struct,
                "table": k.nested_table,
                // A released field, `false` since every table's struct is derived (`[risk]` was the
                // last one read from its source).
                "foreign": false,
                "recursive": k.recursive,
            },
            "roster_id": roster_of.get(&k.path),
            "short": if k.doc.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(k.doc.clone()) },
            "evidence": { "file": k.source, "symbol": k.strukt },
        })).collect::<Vec<_>>(),
        "skipped_fields": w.skipped.iter().map(|s| serde_json::json!({
            "struct": s.strukt,
            "field": s.field,
            "why": s.doc,
        })).collect::<Vec<_>>(),
        // A released field, empty for the reason `nested.foreign` is `false`.
        "foreign_structs": Vec::<serde_json::Value>::new(),
        "rosters": rosters,
        "sizer_kinds": SizerKind::ALL.iter().map(|kind| serde_json::json!({
            "kind": kind.word(),
            "requires": kind.requires(),
            "wraps_base": kind.wraps_base(),
        })).collect::<Vec<_>>(),
        "refusals": refusals_json(&w.keys, &w.tables),
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("the schema document is plain data");
    s.push('\n');
    s
}

/// Every published refusal row, `(message, id, site)`: each id's template at each site it is raised
/// (`super::refusal_sites`), carriers left out, sorted by message then file.
pub(super) fn all_refusals() -> Vec<(&'static str, &'static str, Site)> {
    let mut rows = Vec::new();
    for id in ProfileRefusalId::ALL {
        if let Raised::At(sites) = profile_raised_at(id) {
            rows.extend(sites.iter().map(|site| (id.template(), id.word(), *site)));
        }
    }
    for id in CostRefusalId::ALL {
        rows.extend(cost_raised_at(id).iter().map(|site| (id.template(), id.word(), *site)));
    }
    rows.sort_by(|a, b| (a.0, a.2.file).cmp(&(b.0, b.2.file)));
    rows
}

fn refusals_json(keys: &[Key], tables: &[Table]) -> Vec<serde_json::Value> {
    // Which profile keys/tables a message NAMES. It is what lets a renderer put a refusal beside
    // the key it governs instead of in one undifferentiated list — and it is derived from the
    // message text against the parsed schema, never tagged by hand.
    let named_in = |message: &str| -> Vec<String> {
        let mut out: Vec<String> =
            keys.iter().filter(|k| message.contains(&k.path)).map(|k| k.path.clone()).collect();
        for t in tables {
            if !t.table.is_empty() && message.contains(&format!("[{}]", t.table)) {
                out.push(format!("[{}]", t.table));
            }
        }
        out.sort();
        out.dedup();
        out
    };
    all_refusals()
        .into_iter()
        .map(|(message, id, site)| {
            serde_json::json!({
                "id": id,
                "module": site.file,
                "on_type": site.on_type,
                "on_fn": site.on_fn,
                "message": message,
                "names": named_in(message),
                "evidence": { "file": site.file, "symbol": site.on_fn },
            })
        })
        .collect()
}
