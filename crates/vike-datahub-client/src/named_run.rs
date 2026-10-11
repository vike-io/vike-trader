//! **The NAMED RUN** — run a strategy the SERVER already holds, over a window the server bounds,
//! from a request that has nowhere to put a script.
//!
//! `docs/decisions/0064-a-named-run-carries-no-source.md` is the authority: the classification
//! turns on the SOURCE a verb can carry, not the act of running, and it is CONDITIONAL on bounds —
//! the single-point shape, the window ceiling, the interval set, the symbol validator and the
//! run-slot count — each declared where it is enforced. This file holds the first four; the run-slot
//! count is the SERVER's alone (`crates/vike-backtest/src/compute_server.rs`), because a client that
//! knew it could only mis-predict it. Declared ungated: a default build must DECODE the verb in order
//! to refuse it cleanly.
//!
//! # No source, structurally
//!
//! [`NamedParam`] has no string variant (`i64`, `f64`, `bool` only), so the request type has nowhere
//! to put a script. ⚠ Not the whole defence — a field can be widened by one PR: the fence 0064's
//! decision 2 rests on is a DEPENDENCY CLOSURE (`vike_user_strategies::named_run::resolve`'s crate
//! cannot name `vike-script`). [`RESERVED_SRC_KEY`](vike_model::RESERVED_SRC_KEY) is refused as a
//! params key anyway, a labelled BELT, so a caller shipping a script is TOLD it went nowhere.
//!
//! # Every dimension the request names is bounded HERE (0064's decision 3)
//!
//! 1. **No grid, no trials, no splits, no walk-forward:** [`NamedRunSpec`] has no field a search can
//!    occupy.
//! 2. **[`NAMED_RUN_MAX_BARS`]** on the window, REFUSED BY NAME rather than clamped.
//! 3. **[`NAMED_RUN_INTERVALS`]**, a server-owned set.
//! 4. **The symbol** through [`crate::seed::validate_seed_symbol`] (0064's bound 4 names it) and the
//!    venue through [`crate::catalog::validate_catalog_venue`].
//! 5. **[`NAMED_RUN_MAX_PARAMS`]** and a key charset, so the params list is not a free allocation.
//!
//! ⚠ **The declared residuals:** a run is not CANCELLABLE (`vike_backtest::harness` has no
//! cancellation point), so the duration bound is an ADMISSION bound, held by the SERVER-OWNED roster
//! plus bounded bars; and a numeric knob can still size an allocation inside a roster arm's
//! `from_params`, which the slot count and the unit's `MemoryMax` bound rather than this file.
//!
//! # Why the constants live HERE
//!
//! A client cannot guard a rule the server does not know, nor the reverse (the rule [`crate::seed`]
//! states). [`crate::DatahubClient::run_named`] and `vike_backtest::compute_server`'s named-run arm
//! both call [`validate_named_run`]: the client copy is for the MESSAGE, the server copy (before it
//! touches the store) is the enforcement.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::wire_studio::WireRunResult;

/// The widest window one named run may cover, in BARS — the smaller of two derivations, both
/// arithmetic over constants in this tree rather than a measurement.
///
/// **The ANSWER:** at most [`NAMED_RUN_ANSWER_BYTES_PER_BAR`] per bar, held by the assertion below an
/// order of magnitude under [`crate::proto::MAX_FRAME_LEN`]. **The WORK:** a store scan of that many
/// rows; 50,000 bars is about five years hourly or five weeks of one-minute data — a real backtest,
/// hence far above [`crate::seed::SEED_BARS`].
///
/// ⚠ **A wider window is REFUSED BY NAME, never CLAMPED:** a silently shortened backtest returns
/// plausible numbers for a period nobody chose (the lie `docs/decisions/0062`'s decision 5 fences).
pub const NAMED_RUN_MAX_BARS: u32 = 50_000;

/// The bytes one bar contributes to a named run's ANSWER, worst case — the input to
/// [`NAMED_RUN_MAX_BARS`]' first derivation.
///
/// `equity_curve` is a `Vec<f64>` and `equity_ts` a `Vec<i64>`. serde_json writes an `f64` in up to
/// 24 characters (a 17-significant-digit mantissa with sign, point and exponent) and an epoch-ms
/// `i64` in 13, each followed by a comma. 48 is comfortably above the 39 that arithmetic gives.
pub const NAMED_RUN_ANSWER_BYTES_PER_BAR: u64 = 48;

// The frame-budget half of `NAMED_RUN_MAX_BARS`' derivation, asserted rather than trusted — the
// `crates/vike-datahub-client/src/catalog.rs` idiom: an INEQUALITY with headroom, so a deliberate
// tweak of either constant still compiles while a legitimate answer outgrowing the frame does not.
// The factor of 8 is the headroom the trade list and the report JSON share.
const _: () = assert!(
    (NAMED_RUN_MAX_BARS as u64) * NAMED_RUN_ANSWER_BYTES_PER_BAR * 8
        < crate::proto::MAX_FRAME_LEN as u64,
    "NAMED_RUN_MAX_BARS x NAMED_RUN_ANSWER_BYTES_PER_BAR must stay an order of magnitude under \
     MAX_FRAME_LEN, or a legitimate answer is refused by the frame codec instead of by a bound \
     somebody chose. Re-run the derivation on NAMED_RUN_MAX_BARS' doc."
);

/// The bar intervals a named run will read.
///
/// ⚠ **NOT [`crate::seed::SEED_INTERVALS`], and must not become it.** A seed CALLS A VENUE, so its
/// set is what the venues' public REST APIs serve in common; a named run reads the SERVER'S OWN
/// STORE, so the rules here are: (1) `vike_model::time::interval_ms` gives it a POSITIVE bar width,
/// because the window ceiling is priced in bars; (2) it is a spelling the store's own partition
/// vocabulary uses. Hence `1s` IS here (the seed set excludes it only because bybit and okx refuse
/// it), and `1w`/`1M` are in neither, by rule 1.
pub const NAMED_RUN_INTERVALS: [&str; 12] =
    ["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d"];

/// The most numeric knobs one named run may carry. The widest `[strategy.params]` surface on the
/// roster (`vike_mm::SpreadMaker::from_params`) is a dozen or so; 32 is about 2.5x that. It bounds an
/// ALLOCATION a hostile frame can name — a million-entry params vector decoded before anything looks
/// at it — not a strategy.
pub const NAMED_RUN_MAX_PARAMS: usize = 32;

/// The longest params KEY a named run will carry. The longest key any roster arm reads is
/// `base_intensity_a` at 16 bytes; 48 is three times it.
pub const NAMED_RUN_MAX_PARAM_KEY_BYTES: usize = 48;

/// The longest STRATEGY NAME a named run will carry. `vike_model::host_build::scan`'s `valid_name`
/// is `[a-z][a-z0-9_]*` and the longest built-in, `cheap_catch_updown_fair_value`, is 29 bytes; 64
/// bounds the allocation, and the server's lookup in its OWN roster is the real bound.
pub const NAMED_RUN_MAX_STRATEGY_BYTES: usize = 64;

/// One knob of a named strategy — **and the type that makes this verb structurally incapable of
/// carrying source.**
///
/// There is deliberately NO `Text(String)` variant: it would reintroduce the door 0064's decision 5
/// records — `WireSpec::Native`'s `params_toml`, whose TOML text parses into the very table the
/// `"rhai"` arm reads `src` out of — on a verb that is not `VerbScope::Write`.
///
/// ⚠ **`Int` is not a convenience over `Num`.** Some roster readers (`Grid::from_params`' `rungs`,
/// `TrailingScalper::from_params`' `exit_delay_ms`) read `Value::as_integer`, which answers `None`
/// for a TOML float; with only `Num`, every integer knob would silently take its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NamedParam {
    /// A TOML INTEGER knob (`rungs = 4`, `exit_delay_ms = 2000`).
    Int(i64),
    /// A TOML FLOAT knob (`size = 2.5`, `gamma = 0.2`).
    ///
    /// ⚠ Must be FINITE: serde_json encodes a non-finite `f64` as `null`, which fails to decode back
    /// into an `f64` — a protocol desync rather than a bad parameter. [`validate_named_run`] refuses
    /// it by name at both doors.
    Num(f64),
    /// A TOML BOOLEAN knob.
    Flag(bool),
}

/// Everything a named run names. **Every field is a bounded dimension, and there is no sixth.**
///
/// It omits each cost term 0064's decision 3 found unbounded on the verbs that carry it: a parameter
/// GRID (`crates/vike-backtest/src/harness/sweep.rs`'s `expand_paramscan_overrides` allocates an
/// unchecked `product()` before one point runs), a TRIAL count, a SPLIT count, an OPEN-ENDED range, a
/// SYMBOL LIST, and the engine's own cost knobs.
///
/// ⚠ **`start`/`end` are `i64`, not `Option<i64>`:** the shape that means "the whole store"
/// everywhere else on this wire cannot be spelled here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedRunSpec {
    /// The strategy, by NAME, out of the server's own roster ([`Request::NamedStrategies`] lists it
    /// — 0064's decision 7).
    ///
    /// ⚠ **`"rhai"` is not on it, and not because it is filtered out:** the named run resolves
    /// through `vike_strategy::strategy_by_name` and `vike_user_strategies::user_strategy_by_name`,
    /// neither of whose crates can name `vike-script`. `crates/vike-strategy/src/registry.rs`'s
    /// `SCRIPT_ONLY` is the gated roster of the names that live elsewhere for exactly this reason.
    ///
    /// [`Request::NamedStrategies`]: crate::proto::Request::NamedStrategies
    pub strategy: String,
    /// The knobs, at most [`NAMED_RUN_MAX_PARAMS`] of them. A key the named strategy does not read
    /// is IGNORED — the lenient-reader convention every `from_params` arm in this tree follows —
    /// except [`RESERVED_SRC_KEY`](vike_model::RESERVED_SRC_KEY), which is refused by name.
    pub params: Vec<(String, NamedParam)>,
    /// The venue partition to read, in the canonical roster spelling. Bounded by
    /// [`crate::catalog::validate_catalog_venue`].
    pub venue: String,
    /// The symbol, in the venue's own spelling. Bounded by [`crate::seed::validate_seed_symbol`].
    pub symbol: String,
    /// The bar interval, checked against [`NAMED_RUN_INTERVALS`].
    pub interval: String,
    /// Inclusive window start, epoch milliseconds.
    pub start: i64,
    /// Inclusive window end, epoch milliseconds. `end - start` priced in bars must not exceed
    /// [`NAMED_RUN_MAX_BARS`].
    pub end: i64,
}

/// What a named run did. **Three outcomes, and only one of them is an error** — the
/// [`crate::catalog::CatalogOutcome`] shape: a question a client asks routinely must not answer with
/// errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NamedRunOutcome {
    /// The run happened.
    Ran {
        /// The rendered run — the equity curve, the closed trades, the per-symbol PnL. The SAME DTO
        /// [`crate::proto::Response::RunResult`] carries, built by the SAME
        /// `vike_backtest::wire_result::to_wire_result`, so a Studio renders a named run and a
        /// `RunSlice` through one path.
        result: Box<WireRunResult>,
        /// The metrics summary — a `vike_backtest::BacktestReport` as JSON text, the identical
        /// payload [`crate::proto::Response::Report`] carries.
        ///
        /// ⚠ **Carried BESIDE the curve, never derived from it by a client:** a Sharpe computed off
        /// `result.equity_curve` would be a second implementation of a statistic this server owns
        /// (`crates/vike-backtest/src/compute_server/verbs.rs`'s `run_paramscan_profile` rule: *"no
        /// client re-implements sharpe/return/max_dd"*). The report is kilobytes; the curve is the
        /// large field, and [`NAMED_RUN_MAX_BARS`] bounds it.
        report_json: String,
    },
    /// **The operator has not armed this lane**, so nothing ran — a SUCCESS, not an error, exactly
    /// as [`crate::catalog::CatalogOutcome::NotArmed`] and [`crate::proto::SeedDone`]`{ armed: false }`
    /// are.
    ///
    /// ⚠ Reached in NORMAL operation, because [`crate::proto::FEATURE_NAMED_RUN`] is a BUILD fact and
    /// the arming is reported here: an OLD server and an UNARMED one are two different sentences for
    /// an operator (0064's decision 8).
    NotArmed,
    /// The run was refused before the store was touched. See [`NamedRunRefusal`].
    Refused(NamedRunRefusal),
}

/// Why a named run was refused — the refusals that are facts about the SERVER rather than about the
/// request's shape (those are [`crate::proto::Response::Error`], raised by [`validate_named_run`]
/// before a frame is even written).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NamedRunRefusal {
    /// The name is on no roster this server serves. `known` is what it DOES serve, so a picker can
    /// correct itself without a second round trip (0064's decision 7).
    UnknownStrategy {
        /// The roster this server would have run, in its own order.
        known: Vec<String>,
    },
    /// Every run slot is busy. **Refused, never QUEUED** (0064's decision 3, bound 5): a queue is a
    /// connection thread parked for an unbounded time.
    NoSlot {
        /// The server's concurrent-run ceiling, so the caller can say what it collided with.
        limit: usize,
    },
}

/// The answer to [`crate::proto::Request::NamedStrategies`] — 0064's decision 7 in a type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedRoster {
    /// Whether the operator armed the lane.
    ///
    /// ⚠ **`false` also means [`Self::strategies`] is EMPTY:** arming publishes the operator's own
    /// strategy names (0064's decision 8), so an unarmed server withholds the ROSTER as well as the
    /// run. The client renders [`Self::unarmed_note`], never a bare empty list — a teaching refusal
    /// (`docs/decisions/0062`'s decision 4) rather than the empty-answer lie.
    pub armed: bool,
    /// Every name this server would resolve for a named run, in its own declared order: the portable
    /// registry's roster plus the operator's own compiled-in user strategies, with
    /// `vike_strategy::SCRIPT_ONLY` subtracted.
    ///
    /// EMPTY whenever [`Self::armed`] is `false` — see that field.
    pub strategies: Vec<String>,
}

impl NamedRoster {
    /// The one sentence a picker shows when [`Self::armed`] is `false` — the teaching refusal,
    /// naming the flag and the box it goes on.
    pub fn unarmed_note() -> &'static str {
        "this compute daemon serves the named-run verb but its operator has not armed the lane, so \
         it will run nothing AND names no strategies: restart it as \
         `vike-backend backtest --addr --named-run` on its box. It is off by default because a run \
         spends that box's CPU on behalf of a read-only client, and because arming it publishes \
         the operator's own compiled-in strategy names — which is why an unarmed server withholds \
         the roster rather than merely refusing the run"
    }
}

/// How many BARS a window covers at `interval`, or `None` when the interval is outside
/// [`NAMED_RUN_INTERVALS`] or the window runs backwards.
///
/// Saturating throughout: a hostile `start`/`end` pair must not panic a connection thread on an
/// overflow, and the answer only ever feeds a comparison against [`NAMED_RUN_MAX_BARS`].
pub fn named_run_bars(interval: &str, start: i64, end: i64) -> Option<u64> {
    if !NAMED_RUN_INTERVALS.contains(&interval) || end < start {
        return None;
    }
    let step = vike_model::time::interval_ms(interval).filter(|&ms| ms > 0)?;
    let span = (end as i128) - (start as i128);
    Some((span / step as i128).unsigned_abs() as u64 + 1)
}

/// The interval rule: membership in [`NAMED_RUN_INTERVALS`], exact and case-SENSITIVE — `1M` and
/// `1m` are a calendar month and a minute ([`crate::seed::validate_seed_interval`]'s reason), and
/// coercing one into the other would price a window against the wrong bar width.
pub fn validate_named_run_interval(interval: &str) -> Result<(), String> {
    if NAMED_RUN_INTERVALS.contains(&interval) {
        return Ok(());
    }
    let mut msg = format!(
        "interval {interval:?} is not one a NAMED RUN will read. It is refused by \
         NAMED_RUN_INTERVALS, before the store is touched. Permitted: ["
    );
    for (i, iv) in NAMED_RUN_INTERVALS.iter().enumerate() {
        if i > 0 {
            msg.push_str(", ");
        }
        let _ = write!(msg, "{iv}");
    }
    msg.push_str(
        "]. The set is what the store's own interval vocabulary can PRICE — the window ceiling is \
         counted in bars, so an interval with no bar width has no window to bound.",
    );
    Err(msg)
}

/// The strategy-NAME rules: non-empty, inside [`NAMED_RUN_MAX_STRATEGY_BYTES`], and built from the
/// charset `vike_model::host_build::scan`'s `valid_name` already enforces on a user folder.
///
/// ⚠ **The message never ECHOES the name** (the rule [`crate::seed::validate_seed_symbol`] and
/// [`crate::catalog::validate_catalog_venue`] state): a refusal quoting the untrusted string carries
/// it into a log whose file layer defaults to `trace`.
pub fn validate_named_run_strategy(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(
            "a named run's strategy is EMPTY, so it names nothing. Ask this server for its roster \
             (`Request::NamedStrategies`) and pass one of those names."
                .to_string(),
        );
    }
    if name.len() > NAMED_RUN_MAX_STRATEGY_BYTES {
        return Err(format!(
            "a strategy name of {} bytes exceeds NAMED_RUN_MAX_STRATEGY_BYTES = \
             {NAMED_RUN_MAX_STRATEGY_BYTES}. The longest name on any roster this server serves is \
             `cheap_catch_updown_fair_value` at 29 bytes.",
            name.len()
        ));
    }
    if let Some(at) = name.bytes().position(|b| !is_named_ident_byte(b)) {
        return Err(format!(
            "a strategy name carries a byte outside the permitted set at offset {at}. A name may \
             hold ASCII lowercase letters, digits and `_` only — that is exactly what \
             `vike_model::host_build::scan`'s `valid_name` enforces on a user strategy folder, so \
             anything else names no strategy this server could resolve."
        ));
    }
    Ok(())
}

/// The bytes a strategy name or a params key may be built from: ASCII lowercase letters, digits and
/// `_`. An ALLOWLIST (the safe set is one line); the value only selects a row in the server's own
/// roster and never reaches a URL, so this is a cheap early refusal rather than a security boundary.
fn is_named_ident_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}

/// **Every bound on a named run, in one place, each refusal naming the CONSTANT it hit.** Called at
/// BOTH doors (see the module doc). Cheapest first, deliberately: a request wrong in several ways is
/// told about the cheapest fault, which is also the one with the most specific message.
pub fn validate_named_run(spec: &NamedRunSpec) -> Result<(), String> {
    validate_named_run_strategy(&spec.strategy)?;
    crate::catalog::validate_catalog_venue(&spec.venue)?;
    crate::seed::validate_seed_symbol(&spec.symbol)?;
    validate_named_run_interval(&spec.interval)?;
    if spec.params.len() > NAMED_RUN_MAX_PARAMS {
        return Err(format!(
            "a named run carrying {} params exceeds NAMED_RUN_MAX_PARAMS = {NAMED_RUN_MAX_PARAMS}. \
             The widest `[strategy.params]` surface on this server's roster is about a dozen keys.",
            spec.params.len()
        ));
    }
    for (key, value) in &spec.params {
        if key == vike_model::RESERVED_SRC_KEY {
            return Err(format!(
                "a named run may not carry a param called `{}`. That key is a strategy's SOURCE \
                 rather than one of its knobs, and a NAMED run carries no source: the request type \
                 has no field a script could occupy, and the resolution closure holds no compiler \
                 to read one. This is refused rather than ignored so a caller who believes they are \
                 shipping a script learns they are not — ship it with \
                 `vike-cli backtest run --script`, which is a Control-scope verb for exactly that \
                 reason \
                 (docs/decisions/0064-a-named-run-carries-no-source.md).",
                vike_model::RESERVED_SRC_KEY
            ));
        }
        if key.is_empty() {
            return Err(
                "a named run carries a param with an EMPTY key, which names no knob.".to_string()
            );
        }
        if key.len() > NAMED_RUN_MAX_PARAM_KEY_BYTES {
            return Err(format!(
                "a params key of {} bytes exceeds NAMED_RUN_MAX_PARAM_KEY_BYTES = \
                 {NAMED_RUN_MAX_PARAM_KEY_BYTES}. The longest key any roster arm reads is \
                 `base_intensity_a` at 16 bytes.",
                key.len()
            ));
        }
        if let Some(at) = key.bytes().position(|b| !is_named_ident_byte(b)) {
            return Err(format!(
                "a params key carries a byte outside the permitted set at offset {at}. A key may \
                 hold ASCII lowercase letters, digits and `_` only."
            ));
        }
        if let NamedParam::Num(x) = value
            && !x.is_finite()
        {
            return Err(format!(
                "the params key {key:?} carries a non-finite number. serde_json encodes one as \
                 JSON `null`, which then fails to decode back into an `f64` — so it would be a \
                 protocol desync rather than a bad parameter."
            ));
        }
    }
    if spec.end < spec.start {
        return Err(format!(
            "a named run's window runs backwards: end {} is before start {}.",
            spec.end, spec.start
        ));
    }
    let bars = named_run_bars(&spec.interval, spec.start, spec.end)
        .ok_or_else(|| "a named run's window has no bar width to price it in.".to_string())?;
    if bars > u64::from(NAMED_RUN_MAX_BARS) {
        return Err(format!(
            "a named run's window covers {bars} {} bars, which exceeds NAMED_RUN_MAX_BARS = \
             {NAMED_RUN_MAX_BARS}. It is REFUSED rather than clamped: a silently shortened backtest \
             returns plausible numbers for a period nobody chose. Narrow the window, or ask for a \
             coarser interval.",
            spec.interval
        ));
    }
    Ok(())
}

#[path = "named_run_tests.rs"]
#[cfg(test)]
mod named_run_tests;
