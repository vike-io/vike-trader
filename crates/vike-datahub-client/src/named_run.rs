//! **The NAMED RUN** — run a strategy the SERVER already holds, over a window the server bounds,
//! from a request that has nowhere to put a script.
//!
//! `docs/decisions/0064-a-named-run-carries-no-source.md` is the record. Two sentences of it are
//! the whole of this module's shape:
//!
//! > Read the predicate. **It is the SOURCE the verb can carry, not the act of running.**
//!
//! > **The classification is CONDITIONAL on bounds that do not exist yet.** […]
//! > [`crate::proto::required_scope`]'s Observe arm is not to be written until the single-point
//! > shape, the window ceiling, the interval set, the symbol validator and the run-slot count each
//! > exist and are declared where they are enforced.
//!
//! This file is four of those five (the fifth — the run-slot count — is the SERVER's alone and
//! lives in `crates/vike-backtest/src/compute_server.rs`, because a client that knew it could only
//! ever mis-predict it; the same split [`crate::seed`]'s module doc draws).
//!
//! # The structural half: [`NamedParam`] has no string variant, and that is the point
//!
//! Every other way to reach this server's engine carries TEXT that a compiler reads — a profile's
//! `[strategy.params].src`, `WireSpec::Rhai(src)`, and (a finding 0064's decision 5 records)
//! `WireSpec::Native`'s `params_toml`, whose TOML text parses into the very table the `"rhai"` arm
//! reads `src` out of. **A named run carries `i64`, `f64` and `bool` and nothing else.** A script
//! is not any of those, so the request type has nowhere to put one — which is a stronger statement
//! than "the server refuses a `src` key", because a refusal is a check a refactor can move and a
//! missing field is a fact the type system holds.
//!
//! ⚠ It is deliberately NOT the whole defence, and would be a weak one alone: a field can be widened
//! by one PR. The fence 0064's decision 2 actually rests on is a DEPENDENCY CLOSURE — a named run
//! resolves through `vike_user_strategies::named_run::resolve`, whose crate cannot name
//! `vike-script` — so even a string that did arrive would reach no reader. Two independent
//! defences, each in the layer that can hold it.
//!
//! [`RESERVED_SRC_KEY`](vike_model::RESERVED_SRC_KEY) is refused as a params KEY by
//! [`validate_named_run`] anyway. That is a BELT and is labelled as one: under the numeric carrier
//! a key called `src` is structurally a number, and under the closure above nothing would read it.
//! It exists so a caller who believes they are shipping a script is TOLD they are not, rather than
//! having their intention silently ignored.
//!
//! # The cost half: every dimension the request names is bounded HERE
//!
//! 0064's decision 3 is blunt about the state of the daemon this verb joins: *"today NOT ONE
//! dimension a request names is bounded"* — not the window (`Option<i64>`, `None` meaning the whole
//! store), not the grid (an unchecked `product()` allocated before a point runs), not the trial
//! count, not the run duration, not the connection count. Removing the compiler does not remove
//! that denial of service, so the bounds below are the CONDITION of the Observe classification
//! rather than a claim about it:
//!
//! 1. **No grid, no trials, no splits, no walk-forward.** [`NamedRunSpec`] has no field a search can
//!    occupy — the single largest bound in the design, and structural rather than numeric.
//! 2. **[`NAMED_RUN_MAX_BARS`]** on the window, REFUSED BY NAME rather than clamped.
//! 3. **[`NAMED_RUN_INTERVALS`]**, a server-owned set.
//! 4. **The symbol** through [`crate::seed::validate_seed_symbol`] — 0064's bound 4 names that
//!    function outright rather than minting a second spelling — and the venue through
//!    [`crate::catalog::validate_catalog_venue`], for the same reason.
//! 5. **[`NAMED_RUN_MAX_PARAMS`]** and a key charset, so the params list is not a free allocation.
//!
//! ⚠ **The residual 0064 declares and this module cannot close: a run is not CANCELLABLE.**
//! `vike_backtest::harness` has no cancellation point, so the duration bound is an ADMISSION bound
//! — bars × one pass over an operator-chosen strategy — and not a deadline. What makes it hold is
//! that the roster is SERVER-OWNED: the client names WHICH of the operator's own strategies and HOW
//! MANY BARS, and both are bounded. The second declared residual is a param's VALUE — a numeric knob
//! can still size an allocation inside a roster arm's `from_params`, which the slot count and the
//! unit's `MemoryMax` bound rather than this file.
//!
//! # Why the constants live HERE and not in `vike-backtest`
//!
//! The rule [`crate::seed`] states: a client cannot guard a rule the server does not know, nor the
//! reverse. The client's door is [`crate::DatahubClient::run_named`] and the server's is
//! `vike_backtest::compute_server`'s named-run arm; both call [`validate_named_run`], so a refusal
//! an operator reads locally is the refusal the server would have given. The duplication is for the
//! MESSAGE, never for the enforcement — the server re-checks before it touches the store, which is
//! where the property actually lives.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::wire_studio::WireRunResult;

/// The widest window one named run may cover, in BARS.
///
/// Two derivations meet here and the smaller one wins; both are arithmetic over constants in this
/// tree rather than a measurement, and this doc says so rather than implying a stopwatch was used.
///
/// **The ANSWER.** [`NamedRunOutcome::Ran`] carries one `f64` equity sample and one `i64` timestamp
/// per bar, JSON-encoded as decimal text. A 17-significant-digit `f64` plus a 13-digit epoch-ms
/// stamp plus separators is under [`NAMED_RUN_ANSWER_BYTES_PER_BAR`]; the compile-time assertion
/// below holds that product an order of magnitude under [`crate::proto::MAX_FRAME_LEN`], leaving the
/// trade list and the report the rest of the frame.
///
/// **The WORK.** `crates/vike-model/src/strategy/mod.rs`'s module doc records the engine's bar
/// dispatch at roughly 70 ns; the store read dominates it by orders of magnitude, so what this
/// number really bounds is a DataFusion scan of that many rows. 50,000 bars is about five years of
/// hourly data or five weeks of one-minute data — a real backtest rather than a chart — and it is
/// far above [`crate::seed::SEED_BARS`] (600, a chart's visible width) and above
/// `vike_app_core::store_bars::STORE_READ_BARS` (3,000) because those two answer a different
/// question.
///
/// ⚠ **A wider window is REFUSED BY NAME, never CLAMPED.** Clamping answers a different question
/// than the one asked and reports it as the answer, which is the lie
/// `docs/decisions/0062`'s decision 5 fences against — and here it would be worse than usual,
/// because a silently shortened backtest returns plausible numbers for a period nobody chose.
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
// tweak of either constant still compiles while a change that would let a legitimate answer exceed
// the frame ceiling does not. The factor of 8 is the headroom the trade list and the report JSON
// share; a run that closes a trade on every bar is not a thing a bounded strategy roster does, but
// the margin is there rather than argued away.
const _: () = assert!(
    (NAMED_RUN_MAX_BARS as u64) * NAMED_RUN_ANSWER_BYTES_PER_BAR * 8
        < crate::proto::MAX_FRAME_LEN as u64,
    "NAMED_RUN_MAX_BARS x NAMED_RUN_ANSWER_BYTES_PER_BAR must stay an order of magnitude under \
     MAX_FRAME_LEN, or a legitimate answer is refused by the frame codec instead of by a bound \
     somebody chose. Re-run the derivation on NAMED_RUN_MAX_BARS' doc."
);

/// The bar intervals a named run will read.
///
/// ⚠ **This is NOT [`crate::seed::SEED_INTERVALS`] and must not become it**, even though the two
/// overlap in eleven of twelve entries. That set is the intersection of what three venues' PUBLIC
/// REST APIs serve, because a seed CALLS A VENUE; this one reads the SERVER'S OWN STORE, so the
/// venue question does not arise and the derivation is different:
///
/// 1. `vike_model::time::interval_ms` must give it a POSITIVE bar width, because the window ceiling
///    is priced in bars and an interval with no width has no price;
/// 2. it must be a spelling the store's own partition vocabulary uses, so a named run asks for a
///    partition that can exist rather than for one that never will.
///
/// **`1s` IS here and is excluded from the seed set**, which is the single entry where the two
/// derivations visibly disagree: `crates/vike-datahub-client/src/seed.rs` excludes it because bybit
/// and okx refuse it from their own code tables, and none of that is true of a row already sitting
/// in the store. `1w`/`1M` are absent from BOTH, by rule 1 — `interval_ms` splits on a single
/// trailing character over `s`/`m`/`h`/`d`, so neither has a width.
pub const NAMED_RUN_INTERVALS: [&str; 12] =
    ["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d"];

/// The most numeric knobs one named run may carry.
///
/// The widest `[strategy.params]` surface on the roster is the Avellaneda–Stoikov maker's
/// (`vike_mm::SpreadMaker::from_params` — gamma, kappa, the intensity pair, the reservation and
/// horizon knobs, the quote-style knobs, `qty`, `tick_size`), which is a dozen or so. 32 is about
/// 2.5x that, the same slack [`crate::seed::SEED_MAX_SYMBOL_BYTES`] leaves over its own worst case.
///
/// It bounds an ALLOCATION a hostile frame can name, not a strategy: a list this long is already
/// wrong, and what the cap actually stops is a million-entry params vector decoded before anything
/// looks at it.
pub const NAMED_RUN_MAX_PARAMS: usize = 32;

/// The longest params KEY a named run will carry. The longest key any roster arm reads is
/// `base_intensity_a` at 16 bytes; 48 is three times it.
pub const NAMED_RUN_MAX_PARAM_KEY_BYTES: usize = 48;

/// The longest STRATEGY NAME a named run will carry.
///
/// `vike_user_strategies::codegen`'s `valid_name` is `[a-z][a-z0-9_]*` and the longest built-in is
/// `cheap_catch_updown_fair_value` at 29 bytes; 64 is a little over twice that. Like
/// [`crate::catalog::CATALOG_MAX_VENUE_BYTES`] this bounds the allocation rather than classifying
/// the name — the server then looks the name up in its OWN roster, which is the real bound.
pub const NAMED_RUN_MAX_STRATEGY_BYTES: usize = 64;

/// One knob of a named strategy — **and the type that makes this verb structurally incapable of
/// carrying source.**
///
/// There is deliberately NO `Text(String)` variant. Adding one would not merely widen the surface:
/// it would reintroduce, field for field, the door
/// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 5 records as undescribed
/// anywhere in this tree — `WireSpec::Native { name, params_toml }`, the arm LABELLED native, whose
/// client TOML text parses into the very table `vike_backtest::harness::registry`'s `"rhai"` arm
/// reads `src` out of. That arm is inert only because its verb is already `VerbScope::Write`. This
/// one is not, so the field simply does not exist.
///
/// ⚠ **`Int` is not a convenience over `Num`.** The roster's readers split on TOML type: `as_f64`
/// accepts a float or an integer, but `Grid::from_params`' `rungs` and
/// `TrailingScalper::from_params`' `exit_delay_ms` read `Value::as_integer`, which answers `None`
/// for a TOML float. A carrier with only `Num` would silently hand every integer knob its default —
/// the lenient-reader convention's failure mode, arriving over a wire where nobody can see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NamedParam {
    /// A TOML INTEGER knob (`rungs = 4`, `exit_delay_ms = 2000`).
    Int(i64),
    /// A TOML FLOAT knob (`size = 2.5`, `gamma = 0.2`).
    ///
    /// ⚠ Must be FINITE. serde_json encodes a non-finite `f64` as JSON `null`, which then fails to
    /// decode back into an `f64` — so a `NaN` would be a protocol desync rather than a bad
    /// parameter. [`validate_named_run`] refuses it by name at both doors.
    Num(f64),
    /// A TOML BOOLEAN knob.
    Flag(bool),
}

/// Everything a named run names. **Every field is a bounded dimension, and there is no sixth.**
///
/// Compare what it deliberately omits, each one a cost term
/// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 3 found unbounded on the verbs
/// that do carry it: a parameter GRID (`crates/vike-backtest/src/harness/sweep.rs`'s
/// `expand_paramscan_overrides` takes an unchecked `product()` over client-supplied arrays and
/// allocates with it before one point runs), a TRIAL count, a SPLIT count, an OPEN-ENDED range
/// (`WireSlice`'s `Option<i64>` pair, whose `None` means the whole store), a SYMBOL LIST, and the
/// engine's own cost knobs.
///
/// ⚠ **`start`/`end` are `i64`, not `Option<i64>`, and that is the window bound's structural half.**
/// An absent bound is the shape that means "the whole store" everywhere else on this wire; here
/// there is no way to spell it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedRunSpec {
    /// The strategy, by NAME, out of the server's own roster ([`Request::NamedStrategies`] is how a
    /// client learns what that roster holds — 0064's decision 7, "the roster the verb SERVES is the
    /// roster it ENUMERATES").
    ///
    /// ⚠ **`"rhai"` is not on it, and not because it is filtered out.** The named run resolves
    /// through `vike_strategy::strategy_by_name` and `vike_user_strategies::user_strategy_by_name`,
    /// neither of whose crates can name `vike-script`, so the script arm is not reachable from that
    /// closure at all. `crates/vike-strategy/src/registry.rs`'s `SCRIPT_ONLY` is the gated roster of
    /// the names that live elsewhere for exactly this reason.
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
/// [`crate::catalog::CatalogOutcome`] shape, for the reason that type's doc gives: a question a
/// client asks routinely must not answer with errors.
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
        /// ⚠ **Carried BESIDE the curve rather than derived from it, deliberately.** The rule is
        /// `crates/vike-backtest/src/compute_server.rs`'s `run_paramscan_profile`'s: *"RANKING
        /// HAPPENS HERE, over the real `BacktestReport`s — so no client re-implements
        /// sharpe/return/max_dd"*. A client that computed a Sharpe off `result.equity_curve` would
        /// be a second implementation of a statistic this server already owns, and the two would
        /// disagree the first time either moved. The report is kilobytes; the curve is the large
        /// field, and [`NAMED_RUN_MAX_BARS`] is what bounds it.
        report_json: String,
    },
    /// **The operator has not armed this lane**, so nothing ran — and this is a SUCCESS, not an
    /// error, exactly as [`crate::catalog::CatalogOutcome::NotArmed`] and
    /// [`crate::seed::SeedDone`]`{ armed: false }` are.
    ///
    /// ⚠ Unlike the venue-catalog lane, this outcome is REACHED IN NORMAL OPERATION rather than only
    /// by a client that ignored the advertisement, because [`crate::proto::FEATURE_NAMED_RUN`] is a
    /// BUILD fact and the arming is reported here. That is what makes an OLD server and an UNARMED
    /// one two different sentences for an operator instead of one silence — 0064's decision 8.
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
    /// correct itself without a second round trip — 0064's decision 7, one layer out: a client
    /// naming into the dark is the empty-answer-versus-refusal lie `docs/decisions/0062`'s decision
    /// 5 fences against.
    UnknownStrategy {
        /// The roster this server would have run, in its own order.
        known: Vec<String>,
    },
    /// Every run slot is busy. **Refused, never QUEUED** — 0064's decision 3, bound 5: a queue is a
    /// connection thread parked for an unbounded time, which is the denial of service this verb's
    /// whole classification turns on, wearing a different noun.
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
    /// ⚠ **`false` also means [`Self::strategies`] is EMPTY, and that is not a shortcut.** Read
    /// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8, whose third reason for
    /// arming the lane at all is that *"decision 7 publishes the operator's own strategy names,
    /// which is a disclosure nobody should make by shipping a version"*. A server that answered a
    /// TRUE roster while unarmed would make shipping the version publish those names, which is the
    /// exact thing that reason refuses — so the arming gates the ROSTER as well as the run.
    ///
    /// The client still renders [`Self::unarmed_note`] and never a bare empty list: `armed: false`
    /// with a note is a teaching refusal (`docs/decisions/0062`'s decision 4), where an empty list
    /// alone would be the empty-answer-versus-refusal lie that record fences against.
    pub armed: bool,
    /// Every name this server would resolve for a named run, in its own declared order: the portable
    /// registry's roster plus the operator's own compiled-in user strategies, with
    /// `vike_strategy::SCRIPT_ONLY` subtracted.
    ///
    /// EMPTY whenever [`Self::armed`] is `false` — see that field.
    pub strategies: Vec<String>,
}

impl NamedRoster {
    /// The one sentence a picker shows when [`Self::armed`] is `false` — the teaching refusal
    /// `docs/decisions/0062`'s decision 4 states, naming the variable and the box it goes on.
    pub fn unarmed_note() -> &'static str {
        "this compute daemon serves the named-run verb but its operator has not armed the lane, so \
         it will run nothing AND names no strategies: set VIKE_BACKTEST_NAMED_RUN=1 on the box \
         running `vike-backend backtest --addr` and restart it. It is off by default because a run \
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

/// The interval rule: membership in [`NAMED_RUN_INTERVALS`], exact and case-sensitive.
///
/// Case-SENSITIVE for the reason [`crate::seed::validate_seed_interval`] gives: `1M` and `1m` are a
/// calendar month and a minute, and coercing one into the other would price a window against the
/// wrong bar width. Neither is in the set, so the refusal is the correct answer.
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
/// charset `vike_user_strategies::codegen`'s `valid_name` already enforces on a user folder.
///
/// ⚠ **The message never ECHOES the name back**, the rule [`crate::seed::validate_seed_symbol`] and
/// [`crate::catalog::validate_catalog_venue`] both state: the field being refused is the untrusted
/// one, a refusal quoting it is that same untrusted string wearing a log line, and the file log
/// layer defaults to `trace`.
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
             `vike_user_strategies::codegen`'s `valid_name` enforces on a user strategy folder, so \
             anything else names no strategy this server could resolve."
        ));
    }
    Ok(())
}

/// The bytes a strategy name or a params key may be built from: ASCII lowercase letters, digits and
/// `_`.
///
/// An ALLOWLIST, for the reason [`crate::catalog::validate_catalog_venue`]'s is one: the set of
/// dangerous characters is open and the set of safe ones is one line. Unlike a seed symbol this
/// value never reaches a URL — it selects a row in the server's own roster — so the check is a cheap
/// early refusal rather than a security boundary.
fn is_named_ident_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}

/// **Every bound on a named run, in one place, each refusal naming the CONSTANT it hit.**
///
/// Called at BOTH doors — [`crate::DatahubClient::run_named`] before a frame is written, and
/// `vike_backtest::compute_server`'s named-run arm before the store is touched — so a refusal an
/// operator reads locally is the refusal the server would have given. The client copy is for the
/// MESSAGE; the server copy is the enforcement.
///
/// Cheapest first, and the ORDER is deliberate: a request that is wrong in several ways is told
/// about the cheapest fault, which is also the one whose message is most specific.
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
                 shipping a script learns they are not — ship it with `vike-cli backtest --script`, \
                 which is a Control-scope verb for exactly that reason \
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> NamedRunSpec {
        NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: vec![("size".to_string(), NamedParam::Num(1.0))],
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000 * 100,
        }
    }

    /// **THE STRUCTURAL PROPERTY, stated as a test because the whole classification rests on it.**
    ///
    /// A named run's params carrier has no variant a script could occupy, so a request cannot carry
    /// source even before anything refuses one. The proof is a serde round trip rather than an
    /// inspection: a `NamedParam` encoded from a JSON STRING simply does not decode, which is the
    /// same thing a hostile peer would discover.
    #[test]
    fn the_request_type_has_nowhere_to_put_a_script() {
        // Every legal shape decodes...
        for json in ["{\"Int\":4}", "{\"Num\":2.5}", "{\"Flag\":true}"] {
            serde_json::from_str::<NamedParam>(json)
                .unwrap_or_else(|e| panic!("{json} must decode: {e}"));
        }
        // ...and a script does not, under any tag this enum has.
        for json in [
            "{\"Text\":\"fn on_bar() { buy(1.0); }\"}",
            "{\"Str\":\"fn on_bar() {}\"}",
            "{\"Int\":\"fn on_bar() {}\"}",
            "{\"Num\":\"fn on_bar() {}\"}",
            "{\"Flag\":\"fn on_bar() {}\"}",
            "\"fn on_bar() {}\"",
        ] {
            assert!(
                serde_json::from_str::<NamedParam>(json).is_err(),
                "a NamedParam must have nowhere to put a script, yet {json} decoded"
            );
        }
        // ...and so does a whole spec whose params try to smuggle one.
        let hostile = "{\"strategy\":\"buy_hold\",\"params\":[[\"src\",\"fn on_bar() {}\"]],\
                       \"venue\":\"binance\",\"symbol\":\"BTCUSDT\",\"interval\":\"1h\",\
                       \"start\":0,\"end\":1}";
        assert!(
            serde_json::from_str::<NamedRunSpec>(hostile).is_err(),
            "a NamedRunSpec whose param value is a string must not decode at all"
        );
    }

    /// The BELT, and it is labelled one: a `src` key whose VALUE is a number is structurally inert
    /// (nothing in the resolution closure reads it, and it is not a string anyway), and it is still
    /// refused — so a caller who believes they are shipping a script is told they are not.
    #[test]
    fn the_reserved_source_key_is_refused_by_name_even_as_a_number() {
        let mut s = spec();
        s.params = vec![(vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Num(1.0))];
        let err = validate_named_run(&s).expect_err("the reserved key is refused");
        assert!(err.contains("carries no source"), "{err}");
        assert!(err.contains("--script"), "it names the verb that DOES ship source: {err}");
    }

    /// Every refusal names the CONSTANT it hit, which is what makes a bound actionable rather than
    /// merely present. One case per bound.
    #[test]
    fn every_bound_names_itself_in_its_refusal() {
        let cases: Vec<(NamedRunSpec, &str)> = vec![
            (NamedRunSpec { strategy: "x".repeat(65), ..spec() }, "NAMED_RUN_MAX_STRATEGY_BYTES"),
            (
                NamedRunSpec {
                    params: (0..33).map(|i| (format!("k{i}"), NamedParam::Int(1))).collect(),
                    ..spec()
                },
                "NAMED_RUN_MAX_PARAMS",
            ),
            (
                NamedRunSpec { params: vec![("k".repeat(49), NamedParam::Int(1))], ..spec() },
                "NAMED_RUN_MAX_PARAM_KEY_BYTES",
            ),
            (NamedRunSpec { interval: "1w".to_string(), ..spec() }, "NAMED_RUN_INTERVALS"),
            (NamedRunSpec { start: 0, end: 3_600_000 * 60_000, ..spec() }, "NAMED_RUN_MAX_BARS"),
            (NamedRunSpec { symbol: "B".repeat(33), ..spec() }, "SEED_MAX_SYMBOL_BYTES"),
            (NamedRunSpec { venue: "v".repeat(17), ..spec() }, "CATALOG_MAX_VENUE_BYTES"),
        ];
        for (s, needle) in cases {
            let err = validate_named_run(&s).expect_err("must be refused");
            assert!(err.contains(needle), "the refusal must name {needle}, got: {err}");
        }
    }

    /// The window ceiling REFUSES; it never clamps. Stated as its own test because "clamp it" is the
    /// obvious kindness and it is the one `docs/decisions/0062`'s decision 5 argues is a lie.
    #[test]
    fn an_over_wide_window_is_refused_rather_than_narrowed() {
        let s =
            NamedRunSpec { interval: "1m".to_string(), start: 0, end: 60_000 * 60_000, ..spec() };
        let err = validate_named_run(&s).expect_err("must be refused");
        assert!(err.contains("REFUSED rather than clamped"), "{err}");
        // …and the boundary itself is admitted, so the bound is a ceiling rather than an off-by-one.
        let exact = NamedRunSpec {
            interval: "1m".to_string(),
            start: 0,
            end: 60_000 * i64::from(NAMED_RUN_MAX_BARS - 1),
            ..spec()
        };
        validate_named_run(&exact).expect("exactly NAMED_RUN_MAX_BARS bars is admitted");
    }

    /// Rule 1 of [`NAMED_RUN_INTERVALS`]' derivation, asserted rather than trusted: an entry the
    /// store vocabulary cannot price would have no window, and [`named_run_bars`] would answer
    /// `None` for an interval [`validate_named_run_interval`] had just accepted.
    #[test]
    fn every_permitted_interval_has_a_bar_width_the_store_can_price() {
        for iv in NAMED_RUN_INTERVALS {
            let ms = vike_model::time::interval_ms(iv);
            assert!(matches!(ms, Some(n) if n > 0), "{iv} has no positive bar width: {ms:?}");
            assert!(named_run_bars(iv, 0, 1_000_000).is_some(), "{iv} has no window");
        }
    }

    /// The set is strictly increasing in bar width and free of duplicates — not cosmetic: it is
    /// rendered into a refusal an operator reads, and a duplicate would mean two rows of the
    /// derivation collapsed without anybody noticing.
    #[test]
    fn the_permitted_set_is_sorted_by_bar_width_and_free_of_duplicates() {
        let widths: Vec<i64> = NAMED_RUN_INTERVALS
            .iter()
            .map(|iv| vike_model::time::interval_ms(iv).unwrap())
            .collect();
        assert!(widths.windows(2).all(|w| w[0] < w[1]), "not strictly increasing: {widths:?}");
    }

    /// The one entry where this set and the SEED set visibly disagree, pinned so a future "align
    /// them" pass has to read both derivations first.
    #[test]
    fn one_second_is_readable_here_and_not_seedable() {
        assert!(NAMED_RUN_INTERVALS.contains(&"1s"), "the store can hold 1s bars");
        assert!(
            !crate::seed::SEED_INTERVALS.contains(&"1s"),
            "…and the seed lane still refuses it, because bybit and okx refuse it from their own \
             code tables. The two sets answer different questions; do not unify them."
        );
    }

    /// A non-finite numeric param is refused at the door rather than becoming a decode failure on
    /// the far side.
    #[test]
    fn a_non_finite_param_is_refused_before_it_becomes_a_desync() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let s =
                NamedRunSpec { params: vec![("g".to_string(), NamedParam::Num(bad))], ..spec() };
            let err = validate_named_run(&s).expect_err("must be refused");
            assert!(err.contains("non-finite"), "{err}");
        }
    }

    /// The unarmed sentence names the switch and the box — the teaching-refusal rule, held rather
    /// than trusted, because an empty roster and an unarmed lane look identical from a picker.
    #[test]
    fn the_unarmed_note_names_the_variable_and_the_box() {
        let note = NamedRoster::unarmed_note();
        assert!(note.contains("VIKE_BACKTEST_NAMED_RUN=1"), "{note}");
        assert!(note.contains("vike-backend backtest --addr"), "{note}");
        // ...and it says the roster is WITHHELD rather than absent, which is the half a picker
        // needs: an unarmed daemon answers an empty `strategies` list (0064's decision 8 leg 3),
        // and without this sentence that is indistinguishable from a daemon holding none.
        assert!(note.contains("names no strategies"), "{note}");
    }

    /// A well-formed request passes every bound — so the tests above cannot be passing because the
    /// validator refuses everything.
    #[test]
    fn an_ordinary_request_is_admitted() {
        validate_named_run(&spec()).expect("an ordinary named run is admitted");
    }
}
