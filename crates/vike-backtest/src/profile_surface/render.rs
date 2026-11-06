//! The profile sources as one set, and the rendered `profile.json` document.

use std::collections::BTreeMap;

use super::parse::{
    parse_enums, parse_refusals, parse_sizer_kinds, parse_structs, production_half,
};
use super::walk::{unwrap_type, walk};
use super::{
    Enum, FOREIGN_STRUCTS, HARNESS_MODULES, HarnessModule, Key, PLANE, PROFILE_JSON,
    PROFILE_SRC_PATH, ROOT_STRUCT, Refusal, SCHEMA_VERSION, SERIES_SRC, SERIES_SRC_PATH, Struct,
    Table,
};

/// The `harness::profile` module's files: the root and every child under `harness/profile/`.
fn profile_modules() -> impl Iterator<Item = &'static HarnessModule> {
    let child_dir = PROFILE_SRC_PATH.trim_end_matches(".rs");
    HARNESS_MODULES.iter().filter(move |m| {
        m.path == PROFILE_SRC_PATH
            || m.path.strip_prefix(child_dir).is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The whole profile set as one text, for the searches that need no evidence path.
///
/// Joined at run time from each file's OWN production half rather than `concat!`ed at compile
/// time: a `#[cfg(test)]` marker in any one file would otherwise cut every file after it away.
pub(super) fn profile_src() -> String {
    profile_modules().map(|m| production_half(m.src)).collect()
}

fn series_src() -> String {
    production_half(SERIES_SRC)
}

/// Every struct the export knows, from both sources, keyed by name.
pub(super) fn all_structs() -> BTreeMap<String, Struct> {
    let mut out = BTreeMap::new();
    for m in profile_modules() {
        for s in parse_structs(&production_half(m.src), m.path) {
            out.insert(s.name.clone(), s);
        }
    }
    // Only the struct the profile's `[[data.series]]` array maps onto is published from the
    // replay source; the rest of that file is runtime config, not a TOML surface.
    for s in parse_structs(&series_src(), SERIES_SRC_PATH) {
        if s.name == "SeriesRef" {
            out.insert(s.name.clone(), s);
        }
    }
    // `[risk]`, read through the owning crate's own export of its source. Same parser, same rules —
    // and the same panic-rather-than-drop totality, so a refactor in `vike-exec` fails this gate
    // instead of silently shortening the published table. See `FOREIGN_STRUCTS` for why this is
    // here rather than declared as a hole.
    for s in parse_structs(
        &production_half(vike_model::risk::surface::RISK_PROFILE_SRC),
        vike_model::risk::surface::RISK_PROFILE_SRC_PATH,
    ) {
        if s.name == vike_model::risk::surface::RISK_STRUCT {
            out.insert(s.name.clone(), s);
        }
    }
    out
}

fn all_enums() -> Vec<Enum> {
    let mut out = Vec::new();
    for m in profile_modules() {
        out.extend(parse_enums(&production_half(m.src), m.path));
    }
    for e in parse_enums(&series_src(), SERIES_SRC_PATH) {
        if e.name == "SeriesKind" {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// The published assets, by name.
pub fn rendered_files() -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    out.insert(PROFILE_JSON, render_profile_json());
    out
}

/// Render `profile.json`. Ends with a newline, like every other text asset this repo publishes.
pub(super) fn render_profile_json() -> String {
    let structs = all_structs();
    let w = walk(&structs);
    let enums = all_enums();

    // Which enum backs which key, so a renderer can bind a roster to a field without guessing.
    let mut roster_of: BTreeMap<String, String> = BTreeMap::new();
    for k in &w.keys {
        let (bare, _) = unwrap_type(&k.ty);
        if enums.iter().any(|e| e.name == bare) {
            roster_of.insert(k.path.clone(), bare);
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
                "evidence": { "file": e.source, "symbol": e.name },
            })
        })
        .collect();

    let sizer = parse_sizer_kinds(&profile_src());
    let sizer_file = structs.get("SizerCfg").map(|s| s.source).unwrap_or_else(|| {
        panic!("profile_surface: `SizerCfg` is gone from the profile set — the sizer roster moved")
    });
    rosters.push(serde_json::json!({
        "id": "SizerKindName",
        "members": sizer.iter().map(|s| s.kind.clone()).collect::<Vec<_>>(),
        "default": serde_json::Value::Null,
        "derived_from": "the arms of `build`'s `match kind.as_str()`",
        "keys": ["engine.sizer.kind"],
        "evidence": { "file": sizer_file, "symbol": "SizerCfg" },
    }));

    let doc = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "plane": PLANE,
        "root_struct": ROOT_STRUCT,
        "sources": [
            { "path": PROFILE_SRC_PATH, "owns": "the profile schema" },
            { "path": SERIES_SRC_PATH, "owns": "[[data.series]]" },
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
                "foreign": k.nested_is_foreign,
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
        "foreign_structs": FOREIGN_STRUCTS.iter().map(|f| serde_json::json!({
            "struct": f.strukt,
            "owner": f.owner,
            "why": f.why,
        })).collect::<Vec<_>>(),
        "rosters": rosters,
        "sizer_kinds": sizer.iter().map(|s| serde_json::json!({
            "kind": s.kind,
            "requires": s.requires,
            "wraps_base": s.wraps_base,
        })).collect::<Vec<_>>(),
        "refusals": refusals_json(&w.keys, &w.tables),
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("the schema document is plain data");
    s.push('\n');
    s
}

/// Every refusal every parsed harness module raises, with the keys its message names.
pub(super) fn all_refusals() -> (Vec<Refusal>, Vec<&'static str>) {
    let mut all = Vec::new();
    let mut indirect = Vec::new();
    for m in HARNESS_MODULES {
        let (rows, ind) = parse_refusals(&production_half(m.src), m.path);
        all.extend(rows);
        indirect.extend(ind);
    }
    all.sort_by(|a, b| (a.message.clone(), a.source).cmp(&(b.message.clone(), b.source)));
    all.dedup_by(|a, b| a.message == b.message && a.source == b.source);
    (all, indirect)
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
    let (all, _) = all_refusals();
    all.iter()
        .map(|r| {
            serde_json::json!({
                "module": r.source,
                "on_type": r.on_type,
                "on_fn": r.on_fn,
                "message": r.message,
                "names": named_in(&r.message),
                "evidence": { "file": r.source, "symbol": r.on_fn },
            })
        })
        .collect()
}
