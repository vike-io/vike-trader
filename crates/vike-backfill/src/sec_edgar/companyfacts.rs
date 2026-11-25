//! **The SEC's `companyfacts` JSON** (`https://data.sec.gov/api/xbrl/companyfacts/CIK##########.json`):
//! every XBRL value a company's filings reported, each with the filing that reported it.
//!
//! ```json
//! {"cik": 320193, "entityName": "Apple Inc.", "facts": {"us-gaap": {"Revenues": {"label": "…",
//!   "units": {"USD": [{"start": "2022-09-25", "end": "2023-09-30", "val": 383285000000,
//!     "accn": "0000320193-23-000106", "fy": 2023, "fp": "FY", "form": "10-K",
//!     "filed": "2023-11-03", "frame": "CY2023"}]}}}}}
//! ```
//!
//! One row per value ([`vike_data::FinancialFactRow`]), knowable at the end of its `filed` date's EDGAR
//! day (`super::end_of_filing_day_ms`). A value missing `end`, `val`, `accn` or a real `filed` date
//! refuses the file; `start` (absent for an instant), `fy`, `fp`, `form` and `frame` may be absent.
//!
//! Why not the XBRL `frames` API: a frame holds ONE value per company for a period, the LAST one filed,
//! and states no filing date. A restatement would replace the value a backtest saw, at a time nobody
//! can tell, which is lookahead; `companyfacts` keeps every filing's value with its date.

use serde_json::Value;
use vike_data::FinancialFactRow;

/// Every value of the `companyfacts` document `json`, for the company `cik` (the dataset's, which the
/// document's own `cik` must equal).
pub fn financial_facts(json: &[u8], cik: &str) -> Result<Vec<FinancialFactRow>, String> {
    let document: Value =
        serde_json::from_slice(json).map_err(|e| format!("it is not JSON ({e})"))?;
    let stated = text(&document, "cik");
    if stated.is_empty() || format!("{stated:0>10}") != cik {
        return Err(format!("it is company {stated:?}'s, not {cik}'s"));
    }
    let entity = text(&document, "entityName");
    let taxonomies =
        document.get("facts").and_then(Value::as_object).ok_or("it holds no `facts`")?;
    let mut out = Vec::new();
    for (taxonomy, concepts) in taxonomies {
        for (concept, body) in concepts.as_object().into_iter().flatten() {
            for (unit, values) in body.get("units").and_then(Value::as_object).into_iter().flatten()
            {
                for value in values.as_array().into_iter().flatten() {
                    let at = || format!("{taxonomy}:{concept} [{unit}]");
                    let filed = super::parse_date(&text(value, "filed"))
                        .ok_or_else(|| format!("{}: a value with no real `filed` date", at()))?;
                    let end = text(value, "end");
                    let accession = text(value, "accn");
                    let number = value.get("val").and_then(Value::as_f64);
                    let (Some(number), false, true) =
                        (number, end.is_empty(), super::is_accession(&accession))
                    else {
                        return Err(format!(
                            "{}: a value lacks `val`, `end` or a real `accn`",
                            at()
                        ));
                    };
                    out.push(FinancialFactRow {
                        filed_ms: super::end_of_filing_day_ms(filed.0, filed.1, filed.2)
                            .ok_or("a date")?,
                        cik: cik.to_string(),
                        entity: entity.clone(),
                        taxonomy: taxonomy.clone(),
                        concept: concept.clone(),
                        unit: unit.clone(),
                        period_start: text(value, "start"),
                        period_end: end,
                        value: number,
                        accession,
                        fiscal_year: text(value, "fy"),
                        fiscal_period: text(value, "fp"),
                        form: text(value, "form"),
                        frame: text(value, "frame"),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// `field` of `value` as text: a string as it is, a number in its decimal form, anything else empty.
fn text(value: &Value, field: &str) -> String {
    match value.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "companyfacts_tests.rs"]
mod companyfacts_tests;
