//! The `params` readers: every knob the binary had, read strictly from the study's `params`.

use vike_user_research::StudyError;

use super::matrix::{Lane, Market, Strat, parse_lanes, parse_strats};

// The binary's arg readers, re-expressed over `params`: an ABSENT key takes its default (the
// strategy tier's lenient-reader convention), a PRESENT key of the wrong type is refused — the
// silent-default hazard the binary's own `--latency-ms` reader was written to avoid.

/// The refusal a wrong-typed knob produces: names the key, what was wanted, and what was found.
fn wrong_type(key: &str, want: &str, got: &toml::Value) -> StudyError {
    StudyError::Study(format!("`{key}` must be {want}, found a {}", got.type_str()))
}

fn opt_bool(params: &toml::Value, key: &str) -> Result<Option<bool>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| wrong_type(key, "a boolean", v)),
    }
}

/// Accepts a TOML integer as well as a float, because `floor = 0` is an Integer to the parser and
/// an operator writing a whole number should not have to know that.
fn opt_f64(params: &toml::Value, key: &str) -> Result<Option<f64>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_float()
            .or_else(|| v.as_integer().map(|n| n as f64))
            .map(Some)
            .ok_or_else(|| wrong_type(key, "a number", v)),
    }
}

fn opt_i64(params: &toml::Value, key: &str) -> Result<Option<i64>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_integer().map(Some).ok_or_else(|| wrong_type(key, "an integer", v)),
    }
}

fn opt_str<'a>(params: &'a toml::Value, key: &str) -> Result<Option<&'a str>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| wrong_type(key, "a string", v)),
    }
}

/// The universe, read as DATA rather than from a path — see the module doc for why a path is not
/// available to a study at all. Every malformed row is a refusal naming its index; the binary
/// silently dropped the market, which shrinks the denominator of every aggregate below it with no
/// trace in the output.
pub(super) fn read_universe(params: &toml::Value) -> Result<Vec<Market>, StudyError> {
    let Some(raw) = params.get("universe") else {
        return Err(StudyError::Study(
            "`universe` is required: an array of tables, one per market, each with `family` \
             (the slug whose `-15m` suffix picks the tenor), `token` (the Polymarket \
             outcome-token id, which is the store's `symbol`) and `end_date_ms` (the market's \
             close, epoch ms) — the three columns of the batch binary's TSV"
                .to_string(),
        ));
    };
    let Some(rows) = raw.as_array() else {
        return Err(wrong_type("universe", "an array of tables", raw));
    };
    if rows.is_empty() {
        return Err(StudyError::Study(
            "`universe` is empty — there is nothing to sweep, and an empty sweep would report \
             zeroes that look like a result"
                .to_string(),
        ));
    }
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if !row.is_table() {
            return Err(wrong_type(&format!("universe[{i}]"), "a table", row));
        }
        let family = opt_str(row, "family")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `family`")))?;
        let token = opt_str(row, "token")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `token`")))?;
        let end_date_ms = opt_i64(row, "end_date_ms")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `end_date_ms`")))?;
        out.push(Market {
            family: family.trim().to_string(),
            token: token.trim().to_string(),
            end_date_ms,
        });
    }
    Ok(out)
}

/// Everything the binary's flags resolved to, resolved once before any work starts.
///
/// `Debug` is not decoration: the knob tests use `Result::expect_err`, which requires the OK type
/// to be `Debug`.
#[derive(Debug)]
pub(super) struct Knobs {
    pub(super) fee: bool,
    pub(super) floor: f64,
    pub(super) latency_ms: i64,
    pub(super) lanes: Vec<Lane>,
    pub(super) strats: Vec<Strat>,
}

pub(super) fn read_knobs(params: &toml::Value) -> Result<Knobs, StudyError> {
    let fee = opt_bool(params, "fee")?.unwrap_or(false);
    // Default 0 (NO floor) so the spread-source formulas are not collapsed onto the same tick —
    // the `=1` floor this default replaced made LS-LMSR and GM produce identical quotes.
    let floor = opt_f64(params, "floor")?.unwrap_or(0.0);
    if !floor.is_finite() || floor < 0.0 {
        return Err(StudyError::Study(format!(
            "`floor` must be a finite, non-negative number of ticks, found {floor}"
        )));
    }
    let latency_ms = opt_i64(params, "latency_ms")?.unwrap_or(0);
    if latency_ms < 0 {
        return Err(StudyError::Study(format!(
            "`latency_ms` must be a non-negative integer of milliseconds, found {latency_ms}"
        )));
    }
    let lane = opt_str(params, "lane")?;
    let lanes = parse_lanes(lane).ok_or_else(|| {
        StudyError::Study(format!("unknown `lane` {:?} (l1 | l2 | both)", lane.unwrap_or_default()))
    })?;
    let strategy = opt_str(params, "strategy")?;
    let strats = parse_strats(strategy).ok_or_else(|| {
        StudyError::Study(format!(
            "unknown `strategy` {:?} (spread | trailing | both)",
            strategy.unwrap_or_default()
        ))
    })?;
    Ok(Knobs { fee, floor, latency_ms, lanes, strats })
}
