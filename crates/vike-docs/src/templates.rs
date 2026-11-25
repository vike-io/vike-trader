//! `templates.json` — the shipped strategy files' rosters, parameter surfaces and live-mount
//! verdicts, read from each file's HEADER (`vike_strategy::file::StrategyFileHeader`) in the scan
//! of the shipped `strategies/` folder ([`shipped_strategies`](crate::shipped_strategies)).
//!
//! - `portable` — every file a paper or live mount may run (`strategy.runs_on` = `paper` | `live`),
//!   each with its `[params]` (name, type, default) and its live verdict: `true` exactly when
//!   `runs_on = "live"`.
//! - `simulator_only` — every file whose header says `runs_on = "backtest"`: the backtest may run
//!   it, a paper or live mount refuses it.
//! - `script_only` — always empty: it listed the by-name inline-script arm, which no file is.
//!
//! A record's `reason` is the header's `strategy.why`; a header that states none is rendered as
//! the `runs_on` it declares, never as an invented argument.

use serde_json::{Value, json};
use vike_strategy::file::{ParamType, StrategyFileHeader, StrategyFileRunsOn};

/// Kebab-case name of a [`ParamType`] — the accessor class a file's reader reads a key through.
/// The words the registry-era asset published, plus `array`.
const fn param_type_name(kind: &ParamType) -> &'static str {
    match kind {
        ParamType::Number => "number",
        ParamType::Integer => "integer",
        ParamType::Text => "string",
        ParamType::Bool => "bool",
        ParamType::Table => "table",
        ParamType::TextOrInteger => "string-or-integer",
        ParamType::Array => "array",
        ParamType::Model => "model",
    }
}

/// The header word of a `runs_on`.
const fn runs_on_word(runs_on: StrategyFileRunsOn) -> &'static str {
    match runs_on {
        StrategyFileRunsOn::Backtest => "backtest",
        StrategyFileRunsOn::Paper => "paper",
        StrategyFileRunsOn::Live => "live",
    }
}

/// Whether a file sits on `simulator_only` rather than `portable`: its header says the simulator
/// only. `stats.json` counts the two rosters through this same predicate.
pub(crate) fn is_simulator_only(header: &StrategyFileHeader) -> bool {
    header.strategy.runs_on == StrategyFileRunsOn::Backtest
}

/// Why a file does not run live: its header's `why`, else the `runs_on` it declares.
fn not_live_reason(header: &StrategyFileHeader) -> String {
    let runs_on = runs_on_word(header.strategy.runs_on);
    let stated = header.strategy.why.clone();
    stated.unwrap_or_else(|| format!("runs_on = \"{runs_on}\": the header states no reason"))
}

/// A file's `[params]` as a tagged object, `declared` with the key list in the header's order. The
/// tag is kept from the registry-era asset, whose second variant (`not-enumerated`) no header can
/// produce: a file declares every key it reads.
fn params_value(header: &StrategyFileHeader) -> Value {
    json!({
        "kind": "declared",
        "keys": header
            .params
            .iter()
            .map(|(name, param)| json!({
                "name": name,
                "kind": param_type_name(&param.r#type),
                "default": param.default,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A live verdict: `verdict` is `true` exactly when the header says `runs_on = "live"`, and
/// `reason` is why not otherwise — `null` on a `true` verdict, never an empty string, so a consumer
/// cannot mistake one for the other.
fn live_capable_value(header: &StrategyFileHeader) -> Value {
    match header.strategy.runs_on {
        StrategyFileRunsOn::Live => json!({ "verdict": true, "reason": Value::Null }),
        _ => json!({ "verdict": false, "reason": not_live_reason(header) }),
    }
}

/// One portable file's complete `templates.json` record; `id` is its path relative to the
/// strategies folder (a file's identity, what a profile's `[strategy] file` names).
#[must_use]
pub fn template_record(id: &str, header: &StrategyFileHeader) -> Value {
    json!({
        "id": id,
        "params": params_value(header),
        "live_capable": live_capable_value(header),
    })
}

/// The whole `templates.json` document over `strategies` (path, header), in the scan's path order:
/// `portable` (one [`template_record`] per file a paper or live mount may run), `simulator_only`
/// (one `{ id, reason }` per backtest-only file) and `script_only` (empty; see the module doc).
#[must_use]
pub fn templates_value(strategies: &[(String, StrategyFileHeader)]) -> Value {
    let (simulator_only, portable): (Vec<_>, Vec<_>) =
        strategies.iter().partition(|(_, header)| is_simulator_only(header));
    json!({
        "portable": portable
            .iter()
            .map(|(id, header)| template_record(id, header))
            .collect::<Vec<_>>(),
        "simulator_only": simulator_only
            .iter()
            .map(|(id, header)| json!({ "id": id, "reason": not_live_reason(header) }))
            .collect::<Vec<_>>(),
        "script_only": Vec::<Value>::new(),
    })
}
