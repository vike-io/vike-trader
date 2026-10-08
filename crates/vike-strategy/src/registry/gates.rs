//! [`PARAM_GATES`] (test-only data: conditional reads) and [`unarmable_params`] (empty ladder).

use toml::Value;

use super::echo::resolved_params;
use crate::{DcaAccumulate, Grid};

#[cfg(doc)]
use super::keys::{PARAM_KEYS, mistyped_params, unknown_params};
#[cfg(doc)]
use super::routes::misrouted_params;
#[cfg(doc)]
use super::strategy_by_name;

/// The CONDITION under which a declared [`PARAM_KEYS`] key's value is actually CONSUMED — a
/// predicate over the SAME resolved rows [`resolved_params`] reports, so a gate and the echo can
/// never disagree about what the OTHER keys landed on.
///
/// ⚠ **This is a claim about ANOTHER key, never about this one's own value.** A key whose own value
/// selects a branch (`profit_target > 0`, `max_half_life <= 0.0`) is CONSUMED in both branches;
/// only a key the reader HOLDS and never looks at, because some OTHER key sent it down a different
/// path, gets a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Consumed only while `key` RESOLVED to one of these echo values (`anchor` ⇒ `"fixed"`).
    /// Compared against the RESOLUTION, not the input, so `anchor = "fixd"` — read as `first` —
    /// closes the gate as surely as an absent `anchor` does.
    Is(&'static str, &'static [&'static str]),
    /// Consumed only while `key`'s resolved value parses as a number strictly greater than zero —
    /// the literal `> 0` of `trailing_scalper.rs`'s `entries_allowed`.
    Positive(&'static str),
    /// Consumed only while EVERY listed sub-gate holds; reports ALL the failing conjuncts at once.
    ///
    /// ⚠ **No [`PARAM_GATES`] row constructs it today** — the six degenerate-ladder rows that did
    /// were deleted in round 7 (that table's doc). It stays as the multi-key combinator
    /// `unmet`/`keys` reach through, exercised by `the_gate_predicate_can_actually_fail`.
    All(&'static [Gate]),
}

impl Gate {
    /// The sub-gates that FAIL against `rows` (a [`resolved_params`] row list), each rendered as a
    /// `key=value`. EMPTY ⇒ the key really is consumed. A key the rows do not carry renders
    /// `key=(absent)` and counts as unmet: a loud wrong answer beats a silent "consumed".
    pub fn unmet(&self, rows: &[(&'static str, String)]) -> Vec<String> {
        fn value<'a>(rows: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
            rows.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
        }
        fn missing(key: &str) -> Vec<String> {
            vec![format!("{key}=(absent)")]
        }
        match self {
            Gate::Is(key, wanted) => match value(rows, key) {
                Some(v) if wanted.contains(&v) => Vec::new(),
                Some(v) => vec![format!("{key}={v}")],
                None => missing(key),
            },
            Gate::Positive(key) => match value(rows, key) {
                Some(v) if v.parse::<f64>().is_ok_and(|n| n > 0.0) => Vec::new(),
                Some(v) => vec![format!("{key}={v}")],
                None => missing(key),
            },
            Gate::All(gates) => gates.iter().flat_map(|g| g.unmet(rows)).collect(),
        }
    }

    /// Every key this gate READS — the reverse edge
    /// `every_gate_names_a_declared_key_of_the_same_strategy` walks.
    pub fn keys(&self) -> Vec<&'static str> {
        match self {
            Gate::Is(key, _) | Gate::Positive(key) => vec![key],
            Gate::All(gates) => gates.iter().flat_map(Gate::keys).collect(),
        }
    }
}

/// Which declared keys are read CONDITIONALLY, and on what — one row per `(strategy, key)`.
///
/// ⚠ **This is TEST-ONLY DATA, and its one consumer is the class-closer.**
/// `crates/vike-strategy/tests/param_gates.rs` demands that each declared key CHANGE the call trace
/// over a scripted market. A key legitimately read only under some OTHER key would fail that for a
/// reason that is not a defect, so a row here declares the condition and buys the key an exemption
/// the harness then proves both ways (call-for-call IDENTICAL while unmet, DIFFERENT once met).
/// Nothing operator-facing reads this table: [`resolved_params`] annotates nothing.
///
/// The class: a key spelled right ([`unknown_params`]), typed right ([`mistyped_params`]), routed
/// right ([`misrouted_params`]) and read by `from_params` — whose value the strategy never LOOKS AT
/// because another key sent it down a different branch. The rows: `grid`/`dca_accumulate`
/// `anchor_price` (inert unless `anchor` resolves to `fixed`), `grid` `tick` (inert unless
/// `bounded01`), and `trailing_scalper`'s two entry-timing pairs (`entries_allowed` needs BOTH
/// `> 0`).
///
/// ## ⚠ This table used to feed the mount echo. That is DELETED, not repaired again
///
/// Rounds 6 and 7 rendered a gated row on the daemon's mount line as
/// `anchor_price=60000 (inert: anchor=first)`. Round 6 found the marking PARTIAL at `rungs = 0`
/// (every key inert, two marked, so the unmarked read as in force); round 7 deleted the six
/// degenerate-ladder rows and found the survivors print bare in the same dead configuration. Each
/// fix MOVED the falsehood: **a marker is a positive claim about a key, and making positive claims
/// correctly across every combination of every other key is a static analysis, not a documentation
/// feature.** So [`resolved_params`] reports what each knob RESOLVED TO and claims nothing about
/// what is in force; the class stays closed by the test (`the_shapes_this_harness_cannot_see`
/// carries the cases no params predicate can express).
///
/// ## Why an inert key is not REFUSED either, when a misrouted one is refused
///
/// [`misrouted_params`] refuses because a wrong `symbol` is wrong under EVERY configuration of the
/// rest of the table, with an unrecoverable consequence. An inert key is the opposite on both
/// counts:
///
///   - **It is armed from the SAME table.** `anchor_price = 60000` is exactly right the moment
///     `anchor = "fixed"` appears one line above it; the profile is incomplete, not wrong.
///   - **Refusing would reject a shipped profile shape.** The batch tool described in the module
///     doc of `crates/vike-strategy/src/strategies/trailing_scalper.rs` supplies
///     `market_open_ms`/`market_close_ms` per run while the delay/cutoff knobs stay off — half of
///     an inert pair set, deliberately.
pub const PARAM_GATES: &[(&str, &str, Gate)] = &[
    ("grid", "anchor_price", Gate::Is("anchor", &["fixed"])),
    ("grid", "tick", Gate::Is("bounded01", &["true"])),
    ("dca_accumulate", "anchor_price", Gate::Is("anchor", &["fixed"])),
    ("trailing_scalper", "entry_open_delay_ms", Gate::Positive("market_open_ms")),
    ("trailing_scalper", "market_open_ms", Gate::Positive("entry_open_delay_ms")),
    ("trailing_scalper", "entry_cutoff_before_close_ms", Gate::Positive("market_close_ms")),
    ("trailing_scalper", "market_close_ms", Gate::Positive("entry_cutoff_before_close_ms")),
];

/// This `(name, key)`'s [`PARAM_GATES`] row, or `None` when the key is read unconditionally.
pub fn param_gate(name: &str, key: &str) -> Option<&'static Gate> {
    PARAM_GATES.iter().find(|(n, k, _)| *n == name && *k == key).map(|(_, _, g)| g)
}

/// The WHOLE-TABLE verdict the three key-level readers cannot reach: a params table under which
/// `name` rests NO order at all — on any market, at any price — with the reason, or `None` when the
/// mount can trade.
///
/// The fourth sibling of [`unknown_params`], [`mistyped_params`] and [`misrouted_params`], and the
/// case all three PASS: every key right, and the ladder they describe between them empty. Both
/// instances were MEASURED (`crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger): a
/// `dca_accumulate` whose `anchor = "fixed"` left `anchor_price` at its compiled `0`, and a `grid`
/// whose `bounded01` left `step` at its compiled `1.0`. Each loads, mounts, echoes a clean
/// configuration line and never submits.
///
/// ## Why this refuses where an INERT KEY is deliberately tolerated
///
/// Both halves of [`PARAM_GATES`]' argument invert: an empty ladder has no missing line (every key
/// is present and no value of any OTHER key rescues it — `arm` runs once, from the params), and it
/// rejects no shipped profile shape (`crates/vike-backtest/profiles/wf_grid.toml` and
/// `crates/vike-backtest/profiles/wf_dca_accumulate.toml` rest rungs).
///
/// ## What it deliberately does NOT ask
///
/// Not "would this mount place an order": a strategy that legitimately waits for a market condition
/// (`trailing_scalper` outside its entry window, `momentum` under its threshold) answers that
/// identically to a dead one, and no load-time check can separate them without simulating a market.
/// The decidable question is **does the order set this table describes contain anything at all**,
/// fixed at load for these two names because `arm` builds its whole ladder from the params plus one
/// anchor — answered by [`Grid::arms_no_rung`] / [`DcaAccumulate::arms_no_rung`], the ladder
/// builders themselves, never re-derived here. `None` for every other name: its order flow is a
/// function of the market it is fed.
///
/// ⚠ **The rule is EMPTINESS, never a suspicious value, and the boundary is deliberate in both
/// directions.** A `dca_accumulate` SHORT ladder at `anchor_price = 0` rests `step`, `2·step`, …
/// and loads (refusing the value `0` would be an over-refusal), and a `grid` at `anchor_price = 0`
/// with `bounded01` off rests rungs at NEGATIVE prices and also loads — a different defect this
/// predicate deliberately says nothing about.
///
/// ⚠ **It is a MOUNT rule, not a reader rule.** Nothing calls it from [`strategy_by_name`]: a
/// backtest sweep may legitimately visit a degenerate corner. The consumer is the daemon's profile
/// validation.
pub fn unarmable_params(name: &str, params: &Value) -> Option<String> {
    let empty = match name {
        "grid" => Grid::from_params(params).arms_no_rung(),
        "dca_accumulate" => DcaAccumulate::from_params(params).arms_no_rung(),
        _ => return None,
    };
    if !empty {
        return None;
    }
    // The reason is the RESOLUTION (`anchor_price=0`, `step=1 bounded01=true`) in the operator's
    // own vocabulary, without claiming to know which knob they meant to type.
    let echo = resolved_params(name, params)
        .map(|rows| rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    Some(format!(
        "`{name}` rests NO rung at any anchor these params permit, so this mount can never place an \
         order — on any market, at any price. It resolved to: {echo}"
    ))
}
