//! A JSON document as a flat list of DOTTED LEAF PATHS — the machinery `diff` compares two
//! documents with and `gate` looks one number up in.
//!
//! # Why a generic flattener rather than a typed comparison
//!
//! §6.3 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` argues the input half
//! of a comparison is "a real diff rather than a guess" BECAUSE the resolved config, the data
//! fingerprint and the build stamp are in the run record. A verb built on NAMED FIELDS would have to
//! be edited every time the record grows one — and `vike_analytics::report::BacktestReport`, whose
//! ten scalars are the output half, derives `Serialize` ONLY and lives in a crate this one does not
//! link at all (`crates/vike-cli/Cargo.toml` argues why it may not).
//!
//! So the comparison is over the DOCUMENT. That works today over exactly what is recorded, it works
//! on a research run's report and on a kind invented next year, and every field the run record gains
//! joins it with no edit here. What it costs is that no field gets a bespoke renderer: a moved data
//! window renders as two changed leaf rows rather than one sentence.
//!
//! # ⚠ Arrays are INDEX-keyed, never compared as sets
//!
//! `per_symbol_pnl[0][1]`, not "the BTCUSDT row". A reordered array therefore renders as N changed
//! rows rather than as "reordered", and that is deliberate: `vike_model::py_sum` bit-parity depends
//! on summation ORDER, so a set comparison would hide a real change in the one place this workspace
//! cares most about ordering.
//!
//! # ⚠ A JSON `null` is a LEAF, not an absence
//!
//! `vike_model::runs::RunManifest::git_sha`'s own doc requires it: "this producer could not name a
//! build" is an answer worth showing, and a MISSING key is a different fact from a null one. So
//! [`leaves`] emits a row for a null and [`number_at`] still answers `None` for it — the two
//! questions are asked by different callers for different reasons.

use serde_json::Value;

/// Every scalar and null in `doc`, as `(dotted path, value)`, SORTED by path.
///
/// Sorted so two documents' leaf lists can be walked in lockstep with no lookup per key — which is
/// what makes a diff over two large documents linear rather than quadratic, and what makes its
/// output stable between runs.
///
/// An empty object or empty array contributes NO row. That is a real residual rather than an
/// oversight: `{"a": {}}` and `{}` flatten identically, so a diff cannot see an emptied subtree. It
/// is accepted because every document this is pointed at is a flat-ish record of scalars, and the
/// alternative — synthesising a `a = {}` row — would put a row in the diff that names no value.
pub(crate) fn leaves(doc: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    walk(doc, &mut String::new(), &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(node: &Value, prefix: &mut String, out: &mut Vec<(String, Value)>) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                let mark = prefix.len();
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(k);
                walk(v, prefix, out);
                prefix.truncate(mark);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                let mark = prefix.len();
                prefix.push_str(&format!("[{i}]"));
                walk(v, prefix, out);
                prefix.truncate(mark);
            }
        }
        // A scalar or a null: the leaf itself. The ROOT being a scalar gives one row with an empty
        // key, which is a document nothing in this tree writes and is still better than dropping it.
        scalar => out.push((prefix.clone(), scalar.clone())),
    }
}

/// The FINITE number at a leaf path, or `None`.
///
/// ⚠ **Resolved THROUGH [`leaves`] rather than by walking the document**, and that is the load-
/// bearing half: it makes the keys [`number_keys`] OFFERS exactly the keys this ACCEPTS. A
/// hand-rolled `split('.')` walk answers `None` for every array element, so `gate` would refuse a
/// criterion naming `per_symbol_pnl[0][1]` while the refusal's own "keys this document carries"
/// clause listed it — a message that contradicts itself in two adjacent lines. The documents this
/// is pointed at are a few dozen leaves, so the linear scan costs nothing worth measuring.
///
/// ⚠ `None` covers four different documents on purpose — a missing key, a `null`, a non-number, and
/// a non-FINITE number — because the caller does the same thing with all four: it cannot judge them.
///
/// ⚠ The fourth is a BELT and is measured to be unreachable, which is worth saying rather than
/// leaving a reader to assume it does work: `serde_json::Number` cannot hold a non-finite value, and
/// the root manifest pins the `float_roundtrip` feature, whose parser REFUSES an out-of-range
/// literal instead of saturating it. `a_non_finite_number_cannot_reach_this_at_all` pins both.
/// `vike_analytics::report::BacktestReport`'s `ser_f64_null_when_nonfinite` writes `null` for a
/// non-finite `profit_factor` and that is the shape this actually meets.
pub(crate) fn number_at(doc: &Value, key: &str) -> Option<f64> {
    leaves(doc)
        .into_iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_f64())
        .filter(|n| n.is_finite())
}

/// Every leaf path in `doc` whose value is a finite number, sorted.
///
/// What a refusal NAMES: a criterion that matched nothing must be able to say what the document DOES
/// carry, or the operator is left guessing at a spelling.
pub(crate) fn number_keys(doc: &Value) -> Vec<String> {
    leaves(doc)
        .into_iter()
        .filter(|(_, v)| v.as_f64().is_some_and(f64::is_finite))
        .map(|(k, _)| k)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Value {
        serde_json::json!({
            "sharpe": 1.82,
            "profit_factor": null,
            "config": { "path": "p.toml", "name": null },
            "detail": { "data": { "series": ["binance:BTCUSDT", "binance:ETHUSDT"] } },
            "per_symbol_pnl": [["BTCUSDT", 500.0]]
        })
    }

    /// Leaves are DOTTED paths, sorted, with arrays INDEXED. Sorted so two documents' leaf lists can
    /// be walked in lockstep; indexed because an array whose ORDER changed is a real change.
    #[test]
    fn a_document_flattens_to_sorted_dotted_leaves() {
        let ls = leaves(&doc());
        let keys: Vec<&str> = ls.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys[0], "config.name", "sorted, so `config.*` leads");
        assert!(keys.contains(&"detail.data.series[1]"), "{keys:?}");
        assert!(keys.contains(&"per_symbol_pnl[0][1]"), "nested arrays index twice: {keys:?}");
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "sorted: {keys:?}");
    }

    /// A JSON `null` is a LEAF, not an absence: "this producer could not name a build" is an answer
    /// worth diffing, and a missing key is a different fact from a null one.
    #[test]
    fn a_null_is_a_leaf_and_number_at_still_says_no() {
        assert!(leaves(&doc()).iter().any(|(k, v)| k == "profit_factor" && v.is_null()));
        assert_eq!(number_at(&doc(), "profit_factor"), None);
        assert_eq!(number_at(&doc(), "sharpe"), Some(1.82));
        assert_eq!(number_at(&doc(), "nope"), None);
        assert_eq!(number_at(&doc(), "config.path"), None, "a string is not a number");
        assert_eq!(number_at(&doc(), "detail.data"), None, "…and neither is an object");
    }

    /// `number_keys` is what a refusal names — a criterion that matched nothing must be able to say
    /// what the document DOES carry.
    #[test]
    fn number_keys_lists_what_a_criterion_could_have_named() {
        let ks = number_keys(&doc());
        assert!(ks.contains(&"sharpe".to_string()));
        assert!(!ks.contains(&"profit_factor".to_string()), "a null is not a number");
        assert!(!ks.contains(&"config.path".to_string()), "a string is not a number");
        assert!(ks.contains(&"per_symbol_pnl[0][1]".to_string()), "{ks:?}");
    }

    /// ⚠ **Every key offered is a key that resolves.** `number_keys` is what a refusal recites, so a
    /// key it lists and `number_at` cannot read would make that message contradict itself in two
    /// adjacent lines — which is exactly what a hand-rolled `split('.')` walk does to every array
    /// element.
    #[test]
    fn every_key_number_keys_offers_is_one_number_at_resolves() {
        let d = doc();
        for key in number_keys(&d) {
            assert!(number_at(&d, &key).is_some(), "`{key}` is offered and does not resolve");
        }
    }

    /// ⚠ **A non-finite number cannot reach this code at all, and the `is_finite` filter is a belt
    /// rather than the thing that handles it.** Worth pinning because the obvious test — parse
    /// `1e400` and check it comes back `None` — does NOT test what it looks like it tests, and
    /// failed on the lane the first time it ran.
    ///
    /// Two mechanisms, both measured:
    ///
    /// * the root manifest pins `serde_json`'s `float_roundtrip` feature, and that parser REFUSES an
    ///   out-of-range literal outright (`Error("number out of range")`) instead of saturating it to
    ///   an infinity — so such a document never becomes a `Value` in the first place;
    /// * `serde_json::Number` cannot HOLD a non-finite value, so the constructor turns one into a
    ///   `null`, which [`number_at`] already answers `None` for through the null path.
    #[test]
    fn a_non_finite_number_cannot_reach_this_at_all() {
        let refused = serde_json::from_str::<Value>(r#"{"pf": 1e400}"#);
        assert!(refused.is_err(), "float_roundtrip refuses it at PARSE time: {refused:?}");

        let v = serde_json::json!({ "pf": f64::INFINITY });
        // ⚠ `get` and not `v["pf"]`. Indexing a `Value` with a MISSING key yields `Null`, so
        // `v["pf"].is_null()` passes identically whether the key is present-and-null or absent
        // entirely — and "absent" is the one outcome that would make this test prove nothing. Assert
        // the key IS THERE first, then that its value is null.
        let pf = v.get("pf").expect("the key is present — `json!` inserts it either way");
        assert!(pf.is_null(), "serde_json cannot carry an infinity: {v}");
        assert_eq!(number_at(&v, "pf"), None);
        assert!(number_keys(&v).is_empty(), "…and it is not offered as a key either");
    }

    /// A key with no leaves under it contributes nothing — the declared residual, pinned so nobody
    /// discovers it as a surprise inside a diff.
    #[test]
    fn an_empty_subtree_contributes_no_row() {
        let v = serde_json::json!({ "a": {}, "b": [], "c": 1 });
        let keys: Vec<String> = leaves(&v).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["c".to_string()]);
    }
}
