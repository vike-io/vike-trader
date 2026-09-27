//! **The published SHAPE of the document `vike-cli report` emits over a STORED run** — a version
//! on the document itself, a JSON Schema the CLI can print, a fixed top-level key set, and one
//! written-down rule for what a degenerate metric looks like.
//!
//! # Why a version at all, when a sibling argues against one
//!
//! `crates/vike-backtest/src/binutil.rs`'s `stats_provenance` argues AGAINST schema versions, and
//! it is right about the documents it is about — ones whose only readers are a human and an ad-hoc
//! `jq` filter. This document fails that test in the one way that matters: it is the machine half
//! of a read-only verb, so its whole purpose is to be parsed by something that did not write it.
//! `vike_model::runs::SERIES_SCHEMA` already made exactly this call for the run directory's own
//! documents and carries the argument in full; this module INHERITS it rather than re-deriving it.
//!
//! ⚠ **The field is `schema`, not `schema_version`, and that is deliberate.**
//! `vike_model::runs::RunSeries`'s and `vike_model::runs::RunTrades`'s own version field is spelled
//! `schema`, and the documents this verb re-renders ARE those documents. `crate::cmd::runs::gate`'s
//! `render_json` already uses the same spelling for the verdict document one plane over. A second
//! spelling for the same idea inside one family means a consumer needs two readers to answer one
//! question — which is the whole defect a version is supposed to remove. So the precedent wins over
//! the prettier name.
//!
//! ⚠ What this module adds over that precedent is that the number is a CONST rather than a literal
//! in a `json!` block: `gate`'s `"schema": 1` is typed into its document and nothing else in the
//! tree can read it, so there is no way to publish it. [`REPORT_SCHEMA`] is read by the emitter AND
//! by [`json_schema`], so `report --schema` and the document it describes cannot come to disagree.
//!
//! # ⚠ What is versioned, and the ONE document that is NOT
//!
//! This covers the STORED-run document [`crate::cmd::report_stored`] builds. It does **not** cover
//! the LIVE tearsheet a `vike-tradehub` node answers with: that body is `vike_report::LiveTearsheet`
//! and [`crate::cmd::report`]'s `--json` passes it through VERBATIM, because the producer owns its
//! shape and stamping a version onto bytes this side did not write would be this crate claiming a
//! contract it cannot keep. A consumer tells the two apart by the [`DOCUMENT_ID`] key: our document
//! carries `document` and `schema`, the node's carries neither. That is a declared hole rather than
//! an oversight — closing it means versioning the document in `vike-report`, beside its producer.
//!
//! # Why no `$id`
//!
//! A JSON Schema conventionally carries an `$id` URL. Nothing serves one for this document, and a
//! published URL that 404s is the same defect `vike_config::CONSUMPTION` exists to catch one layer
//! up: positive confirmation of something false. So the schema identifies itself with
//! [`DOCUMENT_ID`] and the version above, and `$id` is the field to add on the day
//! `crate::surface`'s release-asset path publishes this beside `cli.json` — not before.

use serde_json::{Value, json};

/// The version the emitted document declares in its `schema` key, bumped on any change a consumer
/// could not read. `1` is the first shape.
///
/// A REMOVED or RETYPED key is a bump; an ADDED key is not, because the top-level key set is
/// declared `required` and `additionalProperties: false`, so a consumer validating against version
/// `1` would refuse a version-`1` document that grew one. Growing the set is therefore itself a
/// bump — which is the property that makes this number worth reading.
pub(crate) const REPORT_SCHEMA: u32 = 1;

/// What the document calls itself. A consumer holding one JSON blob and no filename needs to know
/// which shape it is before it reads a field, and the node's passthrough document (see this
/// module's doc) is the other thing it could be holding.
pub(crate) const DOCUMENT_ID: &str = "vike-cli-report";

/// **The document's top-level key set, and it is FIXED.** Every key is always present; an absent
/// section is `null`, never a missing key.
///
/// ⚠ That is the opposite of `crate::cmd::runs::show`'s `show_json`, whose `trades` key is present
/// only when `--trades` was given, and the difference is argued rather than accidental: there, the
/// flag is the question and an absent key answers "you did not ask". Here there is no per-section
/// flag — the document carries everything the artifact holds — so a missing key could only ever
/// mean "this build did not write one", which is exactly what a consumer cannot distinguish from a
/// section that was empty. `vike_model::runs::RunManifest::git_sha` makes the same call for the
/// same reason: always serialized, `null` included.
///
/// The one key that is `null` for a REASON OTHER than absence is `breakdown`: `null` means nobody
/// asked for one. Asking for a breakdown that cannot be computed is a REFUSAL with a non-zero rung
/// and no document at all (`crate::cmd::report_stored`'s `breakdown_of` carries why), so `null`
/// here can never be read as "asked and got nothing".
pub(crate) const TOP_LEVEL_KEYS: [&str; 8] =
    ["document", "schema", "run", "metrics", "trades", "curve", "derived", "breakdown"];

/// The period buckets `--breakdown` accepts.
///
/// ⚠ **`week` is deliberately absent.** `vike_model::time` — this workspace's one home for calendar
/// math — has `civil_from_days` and `utc_weekday` and no ISO-week labeller, and inventing one here
/// would be a second calendar home in the crate that is meant to have none. A day and a month are
/// both a direct read of `civil_from_days`, so they cost no new arithmetic at all.
pub(crate) const PERIODS: [&str; 2] = ["day", "month"];

/// The NULL CONVENTION, written into the published schema rather than left as a comment.
///
/// It is the rule `vike_analytics::report::BacktestReport`'s `ser_f64_null_when_nonfinite` already
/// applies to `profit_factor`, restated as a property of this whole document: a statistic that is
/// UNDEFINED for a run is `null`. Never `0`, which is a real and different answer — a flat run and
/// a run with no curve at all would otherwise be indistinguishable — and never omitted, because a
/// missing key cannot be told apart from a build that never wrote one.
pub(crate) const NULL_CONVENTION: &str = "A statistic that is UNDEFINED for this run is null — \
     never 0 (a flat run is a real answer and must stay distinguishable) and never omitted (a \
     missing key cannot be told apart from a build that never wrote one). A statistic that cannot \
     be computed EXACTLY from what the run record kept is null too, and `derived.unavailable` says \
     why in one sentence.";

/// `{"type": ["number", "null"]}` plus its own sentence — the shape [`NULL_CONVENTION`] describes,
/// spelled once so eight properties cannot disagree about it.
fn nullable_number(description: &str) -> Value {
    json!({ "type": ["number", "null"], "description": description })
}

/// The JSON Schema for the stored-run document, as a value.
///
/// Draft 2020-12, and the top level is CLOSED (`additionalProperties: false` over exactly
/// [`TOP_LEVEL_KEYS`]) while every SECTION is open. That split is the contract
/// `crate::surface`'s module doc states for this crate's published assets — key SETS are the
/// contract, key ORDER is not — with one addition: a section's slot carries three different shapes
/// (the document, `null`, or an `{error, detail}` object naming a file that is on disk and
/// unusable), so a closed sub-object would refuse a fault report. The fault shapes are
/// `crate::cmd::runs::show`'s `ReportState` and `trades_json`, which own that convention and are
/// called directly rather than re-implemented.
pub(crate) fn json_schema() -> Value {
    let required = Value::from(TOP_LEVEL_KEYS.to_vec());
    let periods = Value::from(PERIODS.to_vec());
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "vike-cli report — one stored run, re-rendered",
        "description": NULL_CONVENTION,
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": {
            "document": {
                "const": DOCUMENT_ID,
                "description": "What this document is. The LIVE tearsheet `report --node` returns \
                                is the node's own `vike_report::LiveTearsheet` and carries neither \
                                this key nor `schema`, which is how a consumer tells them apart.",
            },
            "schema": {
                "type": "integer",
                "const": REPORT_SCHEMA,
                "description": "The document's shape version. Spelled `schema` to match \
                                `vike_model::runs::RunSeries`, whose documents this one re-renders.",
            },
            "run": {
                "type": "object",
                "description": "The run's identity, straight off the COMMON manifest \
                                `vike_model::runs::RunManifest` — never recomputed here.",
                "properties": {
                    "run_id": { "type": "string" },
                    "dir": { "type": "string" },
                    "kind": { "type": "string" },
                    "produced_by": { "type": "string" },
                    "started_at": { "type": "string" },
                    "finished_at": { "type": "string" },
                    "git_sha": { "type": ["string", "null"] },
                },
            },
            "metrics": {
                "type": ["object", "null"],
                "description": "`report.json` VERBATIM — the producer's own scalars, carried \
                                through unnormalised so a null that means 'undefined' cannot \
                                become a 0 that means 'flat'. `null` when the run wrote none; an \
                                `{error, detail}` object when the file is on disk and unusable.",
            },
            "trades": {
                "type": ["object", "null"],
                "description": "The stored closed-trade ledger: `closed` is what the run actually \
                                closed and `kept` is how much of it survived \
                                `vike_model::runs::MAX_TRADES`, so a reader can tell a PREFIX from \
                                a whole ledger. `null` when the producer wrote none.",
            },
            "curve": {
                "type": ["object", "null"],
                "description": "The equity curve's PROVENANCE, never the samples: `stride` above 1 \
                                means `vike_model::runs::decimate` dropped samples, and `whole` is \
                                that question answered. `null` when the producer kept no curve.",
                "properties": {
                    "samples": { "type": "integer" },
                    "source_len": { "type": "integer" },
                    "stride": { "type": "integer" },
                    "whole": { "type": "boolean" },
                    "cap": { "type": "integer" },
                },
            },
            "derived": {
                "type": ["object", "null"],
                "description": "Statistics this verb RECOMPUTES from the stored curve. `exact` is \
                                false on a thinned curve, and every statistic a thinned curve \
                                cannot answer is then null with `unavailable` saying so. `null` \
                                when there is no curve to derive anything from.",
                "properties": {
                    "exact": { "type": "boolean" },
                    "final_equity": nullable_number(
                        "The last sample. Exact even on a THINNED curve, because \
                         `vike_model::runs::decimate` keeps the final sample unconditionally.",
                    ),
                    "peak_equity": nullable_number(
                        "null on a thinned curve: the peak may be one of the dropped samples.",
                    ),
                    "max_drawdown": nullable_number(
                        "The same fold as `vike_analytics::metrics::max_drawdown`, so on a WHOLE \
                         curve it agrees with `metrics.max_drawdown` exactly. null on a thinned \
                         curve, where it could only ever be shallower.",
                    ),
                    "unavailable": { "type": ["string", "null"] },
                },
            },
            "breakdown": {
                "type": ["object", "null"],
                "description": "Per-period returns off the stored curve. `null` means nobody asked \
                                — a breakdown that was asked for and cannot be computed is a \
                                REFUSAL with a non-zero exit and no document, never a null here.",
                "properties": {
                    "period": { "type": "string", "enum": periods },
                    "rows": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "period": { "type": "string" },
                                "start_equity": nullable_number("The base the return is over."),
                                "end_equity": nullable_number("The period's last kept sample."),
                                "return": nullable_number(
                                    "end/start - 1, or null when the base is zero or non-finite — \
                                     the null convention, applied per row.",
                                ),
                            },
                        },
                    },
                },
            },
        },
    })
}

/// The schema as the text `vike-cli report --schema` prints. Pretty-printed always: the whole point
/// of the flag is that a human or a code generator reads it once, and there is no second form of a
/// schema worth having a flag for.
pub(crate) fn schema_text() -> String {
    serde_json::to_string_pretty(&json_schema())
        .unwrap_or_else(|e| format!("{{\"error\":\"cannot render the schema: {e}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema's own consistency: everything it declares REQUIRED is a property it declares, and
    /// every property it declares is required. Both directions, because either half alone lets the
    /// two lists drift — a required key with no property definition validates nothing, and a
    /// property nobody requires is a key a producer may silently stop writing.
    ///
    /// ⚠ This says nothing about whether the RENDERER emits those keys. That is
    /// `crate::cmd::report_stored`'s `the_document_carries_exactly_the_keys_the_schema_publishes`,
    /// which is the half that makes the published schema true rather than merely well-formed.
    #[test]
    fn the_schema_requires_exactly_the_properties_it_declares() {
        let s = json_schema();
        let props = s["properties"].as_object().expect("an object of properties");
        let required: Vec<&str> =
            s["required"].as_array().expect("an array").iter().filter_map(|v| v.as_str()).collect();

        assert_eq!(required, TOP_LEVEL_KEYS.to_vec(), "the roster IS the required list, in order");
        for key in TOP_LEVEL_KEYS {
            assert!(props.contains_key(key), "`{key}` is required and has no property definition");
        }
        for key in props.keys() {
            assert!(
                TOP_LEVEL_KEYS.contains(&key.as_str()),
                "`{key}` is a declared property nothing requires — a producer could stop writing it"
            );
        }
        assert_eq!(
            s["additionalProperties"],
            json!(false),
            "the top level is CLOSED, which is what makes growing the key set a version bump"
        );
    }

    /// The two facts a consumer branches on before it reads anything else, pinned as literals: a
    /// document that renamed itself or silently kept version `1` through a shape change is a
    /// document nothing can safely parse.
    #[test]
    fn the_document_names_itself_and_its_version_in_the_schema() {
        let s = json_schema();
        assert_eq!(s["properties"]["document"]["const"], json!(DOCUMENT_ID));
        assert_eq!(s["properties"]["schema"]["const"], json!(REPORT_SCHEMA));
        assert_eq!(DOCUMENT_ID, "vike-cli-report");
        assert_eq!(REPORT_SCHEMA, 1);
    }

    /// ⚠ The null convention is PUBLISHED, not merely honoured. A consumer deciding what to do with
    /// a `null` needs the rule in the artifact it validates against, not in a Rust doc comment it
    /// will never read — and the reason `null` is not `0` is the part that has to travel.
    #[test]
    fn the_null_convention_travels_with_the_schema() {
        let s = json_schema();
        let described = s["description"].as_str().expect("a description");
        assert_eq!(described, NULL_CONVENTION);
        assert!(described.contains("never 0"), "the rule must say what null is NOT: {described}");
        for key in ["final_equity", "peak_equity", "max_drawdown"] {
            let ty = &s["properties"]["derived"]["properties"][key]["type"];
            assert_eq!(ty, &json!(["number", "null"]), "{key} must admit null: {ty}");
        }
    }

    /// `--breakdown`'s value roster is the one in the schema, so the parser's refusal and the
    /// published `enum` cannot name different sets. `week` is the absence that matters — this
    /// module's doc argues it — so it is asserted absent rather than left to be noticed.
    #[test]
    fn the_period_roster_is_the_schema_enum_and_omits_week() {
        let s = json_schema();
        let published = &s["properties"]["breakdown"]["properties"]["period"]["enum"];
        assert_eq!(published, &json!(PERIODS.to_vec()));
        assert!(!PERIODS.contains(&"week"), "there is no ISO-week labeller to build one on");
    }

    /// The printed form parses back. It is the only surface `--schema` has, so a pretty-printer
    /// that emitted something a validator cannot read would make the flag worse than nothing.
    #[test]
    fn the_printed_schema_is_parseable_json() {
        let text = schema_text();
        let back: Value = serde_json::from_str(&text).expect("the printed schema re-parses");
        assert_eq!(back, json_schema());
        assert!(text.contains('\n'), "pretty-printed, for the reader it exists for");
    }
}
