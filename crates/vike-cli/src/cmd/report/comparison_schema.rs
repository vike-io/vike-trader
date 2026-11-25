//! **The published SHAPE of the document `vike-cli report --compare <A> <B> --json` emits**: a version on
//! the document, a JSON Schema `report --schema --compare` prints, and a FIXED top-level key set — the
//! stored document's discipline (`crate::cmd::report::schema`, whose module doc argues each choice), for the
//! second document this verb owns.
//!
//! The rows are `vike_analytics::results_comparison::TradingResultsComparison::rows`, one per catalog metric
//! either side's own table shows, in catalog order; a value that is not there is `null`, never `0`.

use serde_json::{Value, json};

/// The version the emitted document declares in its `schema` key; the stored document's rules for a bump
/// (`crate::cmd::report::schema`'s `REPORT_SCHEMA`) hold here too.
pub(crate) const COMPARISON_SCHEMA: u32 = 1;

/// What the document calls itself, so a consumer holding one JSON blob knows which of this verb's
/// documents it holds.
pub(crate) const DOCUMENT_ID: &str = "vike-cli-report-comparison";

/// **The document's top-level key set, and it is FIXED**: every key always present.
pub(crate) const TOP_LEVEL_KEYS: [&str; 5] = ["document", "schema", "a", "b", "rows"];

/// One side's keys: how it was named on the line, its label, its period, why it is invalid, and its notes.
pub(crate) const SIDE_KEYS: [&str; 5] = ["source", "label", "period", "invalid", "notes"];

/// One row's keys.
pub(crate) const ROW_KEYS: [&str; 4] = ["metric", "a", "b", "difference"];

/// `{"type": ["number", "null"]}` plus its sentence.
fn nullable_number(description: &str) -> Value {
    json!({ "type": ["number", "null"], "description": description })
}

/// One side's schema: closed, every key required.
fn side_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "additionalProperties": false,
        "required": SIDE_KEYS.to_vec(),
        "properties": {
            "source": {
                "type": "string",
                "description": "The source as the comparison read it: `run:<run id>` or \
                                `account:<venue>/<LABEL>`.",
            },
            "label": {
                "type": "string",
                "description": "The source and period in the one wording every view uses \
                                (`vike_analytics::results::TradingResults::source_label`).",
            },
            "period": {
                "type": ["object", "null"],
                "description": "The span the side covers, epoch-ms (UTC), or null when it states none.",
                "properties": {
                    "from_ms": { "type": "integer" },
                    "to_ms": { "type": "integer" },
                },
            },
            "invalid": {
                "type": ["string", "null"],
                "description": "Why the side's return metrics mean nothing (its equity reached \
                                zero), or null. An INVALID side keeps only its money and count values.",
            },
            "notes": {
                "type": "array",
                "items": { "type": "string" },
                "description": "What a live side counted, left out or could not convert, one \
                                sentence each (its foreign fills, its fills with no account, its fees \
                                in other assets); empty for a stored run.",
            },
        },
    })
}

/// The JSON Schema of the comparison document. Draft 2020-12, the top level, a side and a row CLOSED.
pub(crate) fn json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "vike-cli report --compare — two sources' results, side by side",
        "description": "A is the reference (the backtest, or the first source named), B is held \
                        against it. A value neither side's table shows is no row; a value one side \
                        does not have is null, never 0.",
        "type": "object",
        "additionalProperties": false,
        "required": TOP_LEVEL_KEYS.to_vec(),
        "properties": {
            "document": { "const": DOCUMENT_ID },
            "schema": { "type": "integer", "const": COMPARISON_SCHEMA },
            "a": side_schema("The reference side."),
            "b": side_schema("The side held against A."),
            "rows": {
                "type": "array",
                "description": "One row per catalog metric either side's own table shows, in \
                                catalog order.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ROW_KEYS.to_vec(),
                    "properties": {
                        "metric": {
                            "type": "string",
                            "description": "The catalog id (`vike_analytics::metric_catalog::METRICS`).",
                        },
                        "a": nullable_number("A's value."),
                        "b": nullable_number("B's value."),
                        "difference": nullable_number(
                            "b − a; null when a side has no value or the subtraction has no \
                             finite answer.",
                        ),
                    },
                },
            },
        },
    })
}

/// The schema as the text `vike-cli report --schema --compare` prints.
pub(crate) fn schema_text() -> String {
    serde_json::to_string_pretty(&json_schema())
        .unwrap_or_else(|e| format!("{{\"error\":\"cannot render the schema: {e}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema requires exactly what it declares, at the top, on a side and on a row, and names
    /// itself and its version.
    #[test]
    fn the_schema_requires_exactly_the_keys_it_declares() {
        let s = json_schema();
        let keys = |v: &Value| -> Vec<String> {
            let mut k: Vec<String> = v["properties"].as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let sorted = |k: &[&str]| -> Vec<String> {
            let mut k: Vec<String> = k.iter().map(|s| s.to_string()).collect();
            k.sort();
            k
        };
        assert_eq!(keys(&s), sorted(&TOP_LEVEL_KEYS));
        assert_eq!(s["required"], json!(TOP_LEVEL_KEYS.to_vec()));
        assert_eq!(keys(&s["properties"]["a"]), sorted(&SIDE_KEYS));
        assert_eq!(keys(&s["properties"]["rows"]["items"]), sorted(&ROW_KEYS));
        assert_eq!(s["properties"]["document"]["const"], json!("vike-cli-report-comparison"));
        assert_eq!(s["properties"]["schema"]["const"], json!(1));
        let back: Value = serde_json::from_str(&schema_text()).unwrap();
        assert_eq!(back, s, "the printed schema parses back");
    }
}
