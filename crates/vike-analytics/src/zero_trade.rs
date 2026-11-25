//! `zero_trade` — the zero / near-zero-trade cause analyzer (a LEAN `FlatEquityCurveAnalysis`
//! analog). A PURE report-side composition over diagnostic counters the engine ALREADY
//! accumulates on [`TradingRecord`]; it adds NO counting to any fill lane.
//!
//! When a run closes zero trades AND its equity curve never moved off the starting cash, the bare
//! metrics table ("total_return 0%, sharpe NaN, max_drawdown 0%, …") says nothing about WHY. This
//! module turns the signals that lane already recorded — the gate-drop channel
//! (`vike_simulator::OrderBrokerSimulator::dropped`: `RiskGate` / cash-gate / `"volume_cap"` / `"latency_reject"`),
//! the stale-price wait deferrals ([`TradingRecordSimulator::stale_deferrals`]), the session-gate skips
//! ([`TradingRecordSimulator::session_deferrals`]), the strategy's warm-up requirement
//! ([`TradingRecordSimulator::warmup`]), and the raw step count ([`TradingRecord::equity_curve`] length)
//! — into a RANKED list of probable causes with actionable messages.
//!
//! OFF / inert by construction: [`ZeroTradeReport::analyze`] returns `None` for any run that
//! closed a trade OR moved equity, so a [`crate::results::TradingResults`] composed for a normal run carries
//! no diagnosis and is byte-identical to before this module existed. The analyzer only ever fires
//! on the all-zero / flat case — which is why an open-and-hold position (0 CLOSED trades but a real,
//! marked position) is deliberately NOT flagged: its equity curve moved.
//!
//! FEATURE-FREE like [`crate::results`] and `vike_backtest::search::objective`: the composition needs only
//! `serde` + `indexmap` + [`crate::record`], which is why it now lives in this vike-model-only
//! crate rather than inside vike-backtest's harness — the DataFusion-free leaf
//! crates that reuse `TradingResults` inherit the diagnosis for free.

use serde::{Deserialize, Serialize};

use indexmap::IndexMap;
use vike_model::OrderDrop;

use crate::record::{TradingRecord, TradingRecordSimulator};

/// One diagnosed probable cause of a zero-trade / flat-equity run. Its position inside
/// [`ZeroTradeReport::causes`] IS the ranking (most-probable first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroTradeCause {
    /// Stable machine tag (kebab-case): `"no-data"`, `"warmup-shortfall"`, `"orders-denied"`,
    /// `"stale-price"`, `"session-closed"`, `"no-orders"`.
    ///
    /// ⚠ **A `String` rather than a `&'static str`, and the reason is `Deserialize`.** serde can
    /// borrow a `&'a str` out of the document it is reading but never a `'static` one out of an
    /// owned document, so a static field makes this type underivable — and a stored `report.json`
    /// then has no typed reader for the one half a zero-trade run most needs. The alternative, a
    /// `deserialize_with` mapping the six tags back to statics, silently REWRITES an unrecognised
    /// tag into a sentinel; six small allocations on a path that already allocates two `String`s per
    /// cause, and only on a zero-trade run, is the cheaper honesty.
    pub code: String,
    /// One-line human headline.
    pub headline: String,
    /// Actionable detail — what to check or change.
    pub detail: String,
}

/// The ranked diagnosis of a zero-trade / flat-equity run. `causes` is NEVER empty (a catch-all
/// `"no-orders"` cause is emitted when nothing more specific applies) and is ordered
/// most-probable-first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroTradeReport {
    pub causes: Vec<ZeroTradeCause>,
}

/// The already-accumulated signals [`rank_causes`] ranks — extracted from a [`TradingRecord`] by
/// [`ZeroTradeReport::analyze`], or built directly in a test. Nothing here is NEWLY counted: every
/// field mirrors a counter the fill lane recorded during the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZeroTradeInputs {
    /// Equity-curve steps = bars/ticks processed. `0` = the data slice was empty.
    pub steps: usize,
    /// Market / market-close orders the stale-price wait gate deferred
    /// ([`TradingRecordSimulator::stale_deferrals`]).
    pub stale_deferrals: u64,
    /// Fills the session gate skipped because the venue was closed
    /// ([`TradingRecordSimulator::session_deferrals`]).
    pub session_deferrals: u64,
    /// Gate-dropped orders aggregated by reason, first-seen order preserved (from
    /// [`TradingRecordSimulator::dropped`] via [`aggregate_denials`]): `RiskGate` reason strings,
    /// order-kind cash-gate drops, `"volume_cap"`, `"latency_reject"`.
    pub denials: Vec<(String, u64)>,
    /// The warm-up requirement the run GATED on, in bars/ticks ([`TradingRecordSimulator::warmup`]);
    /// `0` = none. The EFFECTIVE number rather than `Strategy::warmup()` alone — an event engine
    /// gates on `max(configured floor, strategy.warmup())` — so the shortfall below is diagnosed
    /// against whichever side asked for the longer warm-up.
    pub warmup: usize,
}

/// Aggregate the raw gate-drop channel ([`TradingRecordSimulator::dropped`]) into `reason -> count`,
/// FIRST-SEEN order preserved (`IndexMap`, the repo's order-stable map — the same rule the engine's
/// own `dropped` push order follows), a reason counted by its [`denial_template`] (decision 0078's
/// amendment of 2026-11-19): "it has 0, this sells 0.0629" and "it has 0, this sells 0.0631" are one
/// reason. [`group_denials`] with no times; its details are dropped.
pub fn aggregate_denials(dropped: &[OrderDrop]) -> Vec<(String, u64)> {
    group_denials(dropped.iter().map(|drop| (drop, 1, None))).0
}

/// What a number becomes in a denial's reason template ([`denial_template`]); a `#` of the reason's own text is
/// written twice there.
pub const DENIAL_NUMBER: &str = "#";

/// How many denials of a group [`group_denials`] keeps as its [`DenialDetail::examples`].
pub const DENIAL_EXAMPLES: usize = 3;

/// **A denial's reason with its numbers lifted out** (the owner's yes of 2026-11-19, decision 0078's amendment):
/// `reason` with every number replaced by [`DENIAL_NUMBER`], and each number's own text in order, so the words stay
/// the same and the quantities each sentence embedded become fields. A number is a run of ASCII digits with an
/// optional fraction and exponent, signed when its `-` follows no word character, that STANDS APART from its
/// neighbours: a letter, a digit, `_`, `#` or a `.` joined to a digit on either side keeps it in the words, so the
/// `27` of `BTC-27MAR26`, a version's `1.2.3` and an order tag's `long#3` stay.
///
/// ⚠ **A `#` of the reason's own text is written `##`** (an order tag, `long#3 is busy: …`, reads `long##3 is busy:
/// …`). Since no number next to a `#` is lifted, a lone `#` is always a number and `##` always the reason's own `#`,
/// so the template reads back without doubt: [`denial_sentence`] with the numbers in order gives `reason` again.
pub fn denial_template(reason: &str) -> (String, Vec<String>) {
    let b = reason.as_bytes();
    let word = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'#';
    // The reason's own text, its `#` doubled.
    let words = |template: &mut String, text: &str| {
        template.push_str(&text.replace(DENIAL_NUMBER, "##"));
    };
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    // Joined to what is before `i`, or to what is from `i` on.
    let joined_before =
        |i: usize| i > 0 && (word(i - 1) || (b[i - 1] == b'.' && i > 1 && digit(i - 2)));
    let joined_after = |i: usize| i < b.len() && (word(i) || (b[i] == b'.' && digit(i + 1)));
    let digits_from = |mut i: usize| {
        while digit(i) {
            i += 1;
        }
        i
    };
    let (mut template, mut numbers, mut kept, mut i) = (String::new(), Vec::new(), 0, 0);
    while i < b.len() {
        let body = if b[i] == b'-' { i + 1 } else { i };
        if digit(body) && !joined_before(i) {
            let mut end = digits_from(body);
            if b.get(end) == Some(&b'.') && digit(end + 1) {
                end = digits_from(end + 1);
            }
            if matches!(b.get(end), Some(b'e' | b'E')) {
                let sign = usize::from(matches!(b.get(end + 1), Some(b'+' | b'-')));
                if digit(end + 1 + sign) {
                    end = digits_from(end + 1 + sign);
                }
            }
            if !joined_after(end) {
                words(&mut template, &reason[kept..i]);
                template.push_str(DENIAL_NUMBER);
                numbers.push(reason[i..end].to_string());
                (kept, i) = (end, end);
                continue;
            }
        }
        i += 1;
    }
    words(&mut template, &reason[kept..]);
    (template, numbers)
}

/// **The sentence a reason template and its numbers came from** ([`denial_template`]'s inverse): each lone `#` the
/// next of `numbers`, each `##` the reason's own `#`. A template with more `#`s than numbers keeps the rest as `#`.
pub fn denial_sentence(template: &str, numbers: &[String]) -> String {
    let mut out = String::with_capacity(template.len());
    let (mut numbers, mut rest) = (numbers.iter(), template);
    while let Some(at) = rest.find(DENIAL_NUMBER) {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        match rest.strip_prefix(DENIAL_NUMBER) {
            Some(after) => {
                out.push_str(DENIAL_NUMBER);
                rest = after;
            }
            None => out.push_str(numbers.next().map_or(DENIAL_NUMBER, String::as_str)),
        }
    }
    out.push_str(rest);
    out
}

/// The times and examples of one group of a run's denials ([`group_denials`]): beside its `(reason, count)` row,
/// which keeps the words and the count where every reader of a denial list already finds them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DenialDetail {
    /// When the group's first denial was refused: the step's time, epoch ms. `None` when the record holds no times
    /// (a decoded wire record, a vector kernel's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ms: Option<i64>,
    /// When its last one was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<i64>,
    /// Its first [`DENIAL_EXAMPLES`] denials whose numbers or symbol differ, in the order they were refused.
    pub examples: Vec<DenialExample>,
}

/// One denial of a group, its numbers lifted out of the reason ([`denial_template`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DenialExample {
    /// The order's symbol.
    pub symbol: String,
    /// The size the gate refused; `None` when it was not finite, which JSON writes as `null` and cannot read
    /// back into an `f64`, so one such order would make the whole summary or report undecodable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// The numbers its reason embedded, in order: the group's reason with each lone [`DENIAL_NUMBER`] replaced by
    /// these and each `##` by `#` ([`denial_sentence`]) is the sentence the gate wrote.
    pub numbers: Vec<String>,
    /// When it was refused, epoch ms, when the record holds times.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
}

/// **A run's denials grouped by their reason without its numbers** (decision 0078's amendment of 2026-11-19):
/// `drops` as `(drop, how many times, when)`, one `(template, count)` row a group in first-seen order and each group's
/// [`DenialDetail`] beside it, in the same order. The counts sum to the drops' (saturating), and every drop is in
/// exactly one group.
pub fn group_denials<'a>(
    drops: impl IntoIterator<Item = (&'a OrderDrop, u64, Option<i64>)>,
) -> (Vec<(String, u64)>, Vec<DenialDetail>) {
    let mut groups: IndexMap<String, (u64, DenialDetail)> = IndexMap::new();
    for (drop, times, at_ms) in drops {
        let (template, numbers) = denial_template(&drop.reason);
        let (count, detail) = groups.entry(template).or_default();
        *count = count.saturating_add(times);
        if let Some(at) = at_ms {
            detail.first_ms = Some(detail.first_ms.map_or(at, |first| first.min(at)));
            detail.last_ms = Some(detail.last_ms.map_or(at, |last| last.max(at)));
        }
        let seen = |e: &DenialExample| e.numbers == numbers && e.symbol == drop.symbol;
        if detail.examples.len() < DENIAL_EXAMPLES && !detail.examples.iter().any(seen) {
            detail.examples.push(DenialExample {
                symbol: drop.symbol.clone(),
                size: drop.size.is_finite().then_some(drop.size),
                numbers,
                at_ms,
            });
        }
    }
    groups.into_iter().map(|(reason, (count, detail))| ((reason, count), detail)).unzip()
}

/// Render the denial reasons as `"reason (count), …"` ordered by count desc (ties keep first-seen
/// order — a STABLE sort over the aggregation's insertion order).
fn format_reasons(denials: &[(String, u64)]) -> String {
    let mut sorted: Vec<&(String, u64)> = denials.iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    sorted
        .into_iter()
        .map(|(reason, count)| format!("{reason} ({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rank the probable causes of a zero-trade run, most-probable first. PURE: deterministic, no I/O,
/// TOTAL (always ≥ 1 cause). Ranking is by evidence weight — a near-certain structural explanation
/// (empty data, an unsatisfiable warm-up) outranks a "the strategy tried but was blocked" signal,
/// which outranks the catch-all. Ties keep the fixed push order below (denials, then stale, then
/// session), because `slice::sort_by` is stable.
pub fn rank_causes(inputs: &ZeroTradeInputs) -> Vec<ZeroTradeCause> {
    // (weight, cause) — a STABLE sort by weight desc then keeps the push order for ties.
    let mut scored: Vec<(u64, ZeroTradeCause)> = Vec::new();

    // --- structural certainties (sentinel weights so they always lead when present) ---
    if inputs.steps == 0 {
        scored.push((
            u64::MAX,
            ZeroTradeCause {
                code: "no-data".to_string(),
                headline: "No market data was loaded for this run.".to_string(),
                detail: "0 bars/ticks were processed - the strategy never saw a single event. \
                         Check the profile's symbol, venue, and date range against what the hist \
                         store actually holds."
                    .to_string(),
            },
        ));
    } else if inputs.warmup >= inputs.steps {
        // `steps > 0` here, so this needs a real warm-up: `warmup >= steps` means the engine's
        // `i >= strategy.warmup()` dispatch gate never opened, so on_bar/on_tick ran ZERO times.
        // A strategy with the default `warmup() == 0` never trips this (`0 >= steps>0` is false).
        scored.push((
            u64::MAX - 1,
            ZeroTradeCause {
                code: "warmup-shortfall".to_string(),
                headline: format!(
                    "Strategy warm-up ({} bars) exceeds the {} bar(s) loaded.",
                    inputs.warmup, inputs.steps
                ),
                detail: format!(
                    "The strategy stayed in warm-up for the whole run: it needs {} warm-up \
                     bars/ticks but only {} were available, so its on_bar/on_tick logic never ran. \
                     Widen the date range or reduce the indicator lookback.",
                    inputs.warmup, inputs.steps
                ),
            },
        ));
    }

    // --- "the strategy tried but was blocked" — ranked by how many orders/fills each blocked ---
    let denied: u64 = inputs.denials.iter().map(|(_, count)| *count).sum();
    if denied > 0 {
        scored.push((
            denied,
            ZeroTradeCause {
                code: "orders-denied".to_string(),
                headline: format!("All {denied} submitted order(s) were rejected before filling."),
                detail: format!(
                    "Rejections by reason: {}. These are RiskGate / cash-gate / volume-cap / \
                     latency drops - the strategy submitted orders but none was admitted. Review \
                     the gate limits (margin, min-qty/notional, price collar, buying power) or the \
                     order sizes.",
                    format_reasons(&inputs.denials)
                ),
            },
        ));
    }
    if inputs.stale_deferrals > 0 {
        scored.push((
            inputs.stale_deferrals,
            ZeroTradeCause {
                code: "stale-price".to_string(),
                headline: format!(
                    "{} market order(s) were deferred waiting for a fresh price print.",
                    inputs.stale_deferrals
                ),
                detail: "The stale-price wait gate (max_price_staleness_ms) held every market \
                         order because the tape never printed fresh data within the bound - the \
                         data may be forward-filled / zero-volume across this window."
                    .to_string(),
            },
        ));
    }
    if inputs.session_deferrals > 0 {
        scored.push((
            inputs.session_deferrals,
            ZeroTradeCause {
                code: "session-closed".to_string(),
                headline: format!(
                    "{} fill(s) were skipped because the market was closed.",
                    inputs.session_deferrals
                ),
                detail: "The session gate refused fills for the whole window - the backtest range \
                         may fall entirely outside the venue's trading hours (weekend / holiday / \
                         after-hours)."
                    .to_string(),
            },
        ));
    }

    // Stable, highest weight first. Ties keep push order (denials, stale, session).
    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut causes: Vec<ZeroTradeCause> = scored.into_iter().map(|(_, cause)| cause).collect();

    // Catch-all: nothing more specific fired, so the strategy simply never submitted. This is also
    // where the bar-mode `SYMBOL.VENUE` symbol-routing footgun surfaces (a strategy routing on the
    // bare symbol while `snap_to_properties` re-tagged the bars submits nothing that routes).
    if causes.is_empty() {
        causes.push(ZeroTradeCause {
            code: "no-orders".to_string(),
            headline: "No order-blocking cause was detected.".to_string(),
            // Worded to stay accurate for the one case the result cannot disambiguate: an
            // open-and-hold position in a market that never moved is flat + zero-CLOSED-trades too,
            // and the result carries no fill counter to tell it apart from "never submitted".
            detail: format!(
                "Over {} bar(s)/tick(s) nothing was denied, deferred, or gated and no trade \
                 closed, so no gate explains the flat result. The strategy most likely never \
                 submitted an order - check its entry condition and that it reads the SAME symbol \
                 the data is tagged with (bar-mode `snap_to_properties` re-tags bars as \
                 `SYMBOL.VENUE`, which a strategy routing on the bare symbol will miss).",
                inputs.steps
            ),
        });
    }

    causes
}

impl ZeroTradeReport {
    /// Diagnose a run that closed NO trades and whose equity never moved off the start. Returns
    /// `None` for any run that closed a trade (`n_trades > 0`) OR moved equity — so a
    /// [`crate::results::TradingResults`] composed for a normal run carries no diagnosis and is
    /// byte-identical to before this analyzer existed.
    ///
    /// The trigger is deliberately `n_trades == 0 AND flat equity`: an open-and-hold position moves
    /// the marked equity curve, so a buy-and-hold run (0 CLOSED trades but a real position) is NOT
    /// flagged — the analyzer fires only when nothing at all happened.
    pub fn analyze(result: &TradingRecord) -> Option<ZeroTradeReport> {
        // A curve kept by time holds closes: the path's own count and flatness are the sampler's.
        let (steps, flat) = match result.sampling {
            Some(sampling) => (sampling.events, sampling.flat),
            None => (result.equity_curve.len(), equity_is_flat(&result.equity_curve)),
        };
        if result.n_trades != 0 || !flat {
            return None;
        }
        // A record no simulator produced has none of the simulator's counters: every one reads zero.
        let none = TradingRecordSimulator::default();
        let simulator = result.simulator().unwrap_or(&none);
        let inputs = ZeroTradeInputs {
            steps,
            stale_deferrals: simulator.stale_deferrals,
            session_deferrals: simulator.session_deferrals,
            denials: aggregate_denials(&simulator.dropped),
            warmup: simulator.warmup,
        };
        Some(ZeroTradeReport { causes: rank_causes(&inputs) })
    }
}

/// A run's equity is "flat" when it never moved off its first value — the signature of a run in
/// which no fill ever changed the account. Empty (no data) counts as flat. Exact equality is
/// correct here: with no open position `equity_now() = cash` bit-for-bit every step, so a genuinely
/// idle run has a perfectly constant curve (no float drift to tolerance-away).
fn equity_is_flat(curve: &[f64]) -> bool {
    match curve.first() {
        None => true,
        Some(&first) => curve.iter().all(|&e| e == first),
    }
}

impl std::fmt::Display for ZeroTradeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "no trades executed - probable cause(s), most likely first:")?;
        for (i, cause) in self.causes.iter().enumerate() {
            writeln!(f, "  [{}] {}", i + 1, cause.headline)?;
            writeln!(f, "      {}", cause.detail)?;
        }
        Ok(())
    }
}

#[path = "zero_trade_tests.rs"]
#[cfg(test)]
mod zero_trade_tests;
