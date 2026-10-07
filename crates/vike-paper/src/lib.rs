//! Paper trading (R7): the R1/R2 backtest fill semantics running behind the live core's
//! `ExecutionClient` seam. The Python analog is `exec/sim_exchange.py` (the engine as fill
//! oracle, plus a lifecycle mirror onto the bus); the Rust twin inverts the packaging — the
//! PAPER CLIENT owns a resting book and fills it with the ENGINE'S OWN public primitives
//! ([`vike_fills::fill_model::BarFillModel::fill_price`],
//! [`vike_fills::fill_resolution::resolve_intrabar_fills`],
//! [`vike_fills::broker_sim::adverse_fill_price`], [`vike_fills::broker_sim::fee`]), so
//! backtest == paper is BY CONSTRUCTION, then gated bit-for-bit in `vike-backtest`'s
//! `tests/r7_gate.rs` — which stays there because it drives the ENGINE and the LIVE CORE, not
//! this client alone.
//!
//! ## Why this is a crate again
//!
//! It was one, briefly. `vike-paper` was folded INTO `vike-backtest` in crate-reorg Phase 0
//! (spec D8) for an explicit reason: it "dedups the fill model's home". That reason was real — the
//! equivalence law above is only checkable while exactly ONE definition of a fill exists, and at
//! the time the only way to guarantee that was to put the paper client in the same crate as the
//! engine.
//!
//! `vike-fills` removed the constraint. The shared fill core (`broker_sim` / `fill_model` /
//! `fill_resolution` / `staleness`) now has its own home, so the paper client and the engine share
//! a DEPENDENCY rather than a crate: the same single definition, no possibility of drift, and no
//! requirement that the two be co-located. Splitting again is therefore not a reversal of D8's
//! judgement — it is what D8's concern looks like once it has been answered structurally.
//!
//! The payoff is on the consumer side. `vike-mount` and `vike-run` (since merged into `vike-mount`,
//! docs/decisions/0098) referenced `vike-backtest` for
//! this client and NOTHING else (12 and 4 sites, every one of them `vike_backtest::paper::…`), so
//! both were compiling the entire simulator — engine, harness, analytics — to obtain an
//! `ExecutionClient`. They depend on this crate instead. `vike_backtest::paper` stayed as a
//! re-export so every historical path still resolved, until #2217 retired it on 2026-09-27 under
//! the one-name rule; `vike_paper::…` is the only path now. (This said the re-export "remains"
//! until 2026-09-28.)
//!
//! **Scope of that equivalence law:** it holds for `Gtc`/`Ioc`/`Fok` — i.e. for everything the
//! engine and the paper book both treat as "rests until filled or canceled". It does NOT hold for
//! the EXPIRING time-in-forces (`Gtd`/`Day`): this client enforces their resting deadline
//! (`expire_due`) while `StrategyEngine`/`SimBroker` ignore TIF entirely, so for such an order the
//! engine keeps resting and filling where paper expires. Nothing gates that divergence — r7_gate
//! only exercises `Gtc` — and nothing in-tree produces a non-`Gtc` TIF into either side today
//! (`BrokerContext::Submission` carries no TIF field, `strategy_drive` builds every request with
//! `..Default::default()` ⇒ `Gtc`, and `runtime/apply.rs` hardcodes `Gtc`), so the divergence is
//! currently unreachable from a strategy. Mirroring the sweep in `StrategyEngine::fill_pending` is
//! the follow-up that would restore the law for expiring TIFs.
//!
//! **Conditional verbs ARE gated** (trigger-law wave 2): `tests/r7_gate.rs` grew rows proving the
//! stop verb (engine `submit_stop` under `EngineParams::emulator_release_stops = true` vs live's
//! always-emulated `ConditionalBook` → market release through this book) and the bracket (engine
//! protective-stop + TP vs `submit_bracket`'s OTO/OCO triple resting here) fill bit-identically.
//! The engine's DEFAULT stop-verb semantics (same-event fill at the trigger) remain a documented
//! divergence from live, pinned in `tests/laws/trigger_law.rs`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use vike_exec::{ContingencyBook, ExecutionClient};
use vike_fills::broker_sim::{adverse_fill_price, fee};
use vike_fills::fill_model::{BarFillModel, FillModel};
use vike_fills::fill_resolution::resolve_intrabar_fills;
use vike_model::FeeSchedule;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderFilled, OrderModified,
    OrderPartiallyFilled, OrderSubmitted, TradeId,
};
use vike_model::{
    Bar, ComboLeg, OrderKind, OrderRequest, TimeInForce, WorkingOrder, combo_net_cross,
    order_request_to_working,
};

/// One leg's `(ratio, price)` pair as fed to [`vike_model::combo_net_cross`] — named so the
/// per-leg vectors below never spell a bare nested tuple type (clippy `type_complexity`).
type LegQuote = (i32, f64);

/// An optional UNDERLYING/index-price source for the paper book: `(venue, symbol, ts) -> Option<f64>`,
/// mirroring `vike_sim::EngineParams::properties`' `(venue, symbol, ts) -> Option<_>` shape
/// (PR-2a) exactly. It supplies the underlying (index) price a Deribit-options
/// [`FeeSchedule::PercentOfUnderlying`] fill needs to book the ACCURATE
/// `min(0.03% × underlying, 12.5% × premium)` commission via
/// [`FeeSchedule::commission_with_underlying`] instead of the premium-only approximation the paper
/// fill site otherwise degrades to — which understates the true fee ~20x for options priced well
/// below their underlying (see [`vike_model::FeeSchedule`]'s module note on the residual paper gap).
///
/// `None` (every constructor today) is the byte-identical legacy path: the commission is computed
/// exactly as before. A `Some` source only ever changes the number for a `PercentOfUnderlying`
/// schedule whose `(venue, symbol, ts)` the source actually prices; any other fee shape, or a
/// source that returns `None`, still books the previous number bit-for-bit. See
/// [`PaperExecutionClient::commission_for`].
type UnderlyingSource = Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>;

/// Time-in-force expiry state for one resting order (kept OUT of `WorkingOrder` so the fill
/// primitives stay TIF-blind, exactly like [`Contingency`]).
///
/// Recorded ONLY for orders whose TIF is expiring — `Gtc` (the default everywhere) inserts
/// nothing, so the resting book, the fill pass and the r7 gate are byte-identical when the
/// feature is unused.
///
/// **Session concept:** this crate has no exchange-session calendar (no venue trading hours are
/// modeled anywhere in vike-backtest), so `Day` is scoped to the **UTC calendar day** — and the
/// day it is scoped to is the day of the FIRST BAR the order sees, never `OrderRequest.ts`.
/// Nothing in the write path stamps `request.ts` (`ExecutionEngine::submit_order` takes `now` as a
/// separate argument and never writes it back, and `OrderRequest::ts` defaults to `0`), so
/// anchoring on it would make an ordinary `..Default::default()` request born expired — `utc_day(0)`
/// is 1970 while a live bar is ~20_650. Anchoring on the bar clock also removes any submitter-vs-bar
/// clock skew (the classic seconds-vs-ms mixup) from the deadline, and stays replay-deterministic
/// because the bar stream is.
///
/// **Not handled here — `Ioc`/`Fok` are NOT enforced and rest exactly like `Gtc`.** They are
/// immediate-execution semantics, not a resting deadline: enforcing them means changing the FILL
/// pass (fill-or-cancel on the arrival bar), which is a separate change to a parity-gated path.
/// This type enforces **GTD/DAY resting deadlines only** — do not read it as full server-side TIF
/// emulation.
#[derive(Debug, Clone, Copy)]
struct Expiry {
    tif: TimeInForce,
    /// good-till-date deadline (epoch ms), only meaningful for [`TimeInForce::Gtd`]
    gtd_expiry: Option<i64>,
    /// The `Day` session anchor: the ts of the first bar observed after submit, or `None` until
    /// that bar arrives. A `Day` order therefore never expires before it has seen a single bar.
    day_anchor: Option<i64>,
}

impl Expiry {
    /// Is this order expired as of `now_ms` (always a BAR ts — see [`Expiry`])?
    ///
    /// - `Gtd`: expired once `now_ms >= gtd_expiry`. A `Gtd` with no `gtd_expiry` has no deadline
    ///   and never expires.
    /// - `Day`: expired once `now_ms` lands on a later UTC day than the anchor bar. Un-anchored
    ///   (no bar seen yet) is never expired.
    /// - everything else (`Gtc`/`Ioc`/`Fok`): never expired by this check.
    fn is_expired(&self, now_ms: i64) -> bool {
        // The ONE expiry law (dedup A4): `vike_model::tif_expired`. `day_anchor` is always `Some`
        // for a `Day` order by the time `expire_due` calls this (it anchors on the first bar
        // first); an un-anchored `Day` falls back to `now_ms` here, which the law reads as
        // same-day ⇒ not expired — byte-identical to the old `is_some_and(..)` None-arm.
        vike_model::tif_expired(
            self.tif,
            self.gtd_expiry,
            self.day_anchor.unwrap_or(now_ms),
            now_ms,
        )
    }
}

/// One resting multi-leg combo order (combo PR-3). Kept in its OWN book, separate from the
/// single-symbol `pending` list, because a combo is not a `WorkingOrder`: it has no single symbol,
/// its `price` is a SIGNED NET per combo unit (negative for a credit structure), and it must fill
/// **all legs or none** — none of which the single-symbol fill primitives model.
///
/// One combo is ONE order downstream: one coid, one event stream, exactly one terminal event —
/// the invariant `vike_model`'s combo vocabulary is built around (see [`vike_model::ComboSpec`]).
///
/// **Downstream requirement:** the fills this combo eventually emits carry each LEG's own symbol,
/// so the consuming `ExecutionEngine` must accept every leg symbol (`extra_symbols`) or the bare
/// fills are dropped while the FSM wraps still land — order Filled, Account flat. See
/// `emit_combo_fills` and `tests/combo_engine_fold.rs`.
///
/// **Contingency links are NOT supported on a combo** — `submit` terminally rejects a combo
/// request carrying `parent_order_id` / `linked_order_ids` / `contingency_type`, because the
/// OTO-hold and expire-cascade machinery enforce against the single-symbol `pending` book only.
/// (The one enforced direction: a PLAIN order may OCO-link a resting combo — `apply_contingency`
/// sweeps the combo book for siblings — and a combo's FILL arms plain OTO children as any fill
/// does.)
#[derive(Debug, Clone)]
struct RestingCombo {
    /// +1 buy the combo / -1 sell it. Legs flip with it; NEVER 0 (`ComboSpec::validate`).
    side: i32,
    /// combo UNITS (a leg trades `|ratio| × qty` of its own instrument)
    qty: f64,
    /// SIGNED net limit per combo unit. `None` = combo MARKET (fills as soon as every leg carries
    /// a FRESH mark — see `combo_mark_staleness_ms`). **Never clamped, never absolute-valued** —
    /// a credit combo's limit is negative.
    net_limit: Option<f64>,
    /// 2..=N legs in venue-creation order — the fold order `combo_net` pins.
    legs: Vec<ComboLeg>,
}

/// One captured paper fill (the r7 gate compares these against the engine's fills).
#[derive(Debug, Clone, PartialEq)]
pub struct PaperFill {
    pub ts: i64,
    pub side: i32,
    pub qty: f64,
    pub px: f64,
    pub fee: f64,
    pub is_maker: bool,
}

/// The paper exchange behind the ExecutionClient seam. `submit` publishes
/// Submitted+Accepted and rests the order; `on_bar` (driven by the core on each closed
/// bar of the mounted series) fills the book with the engine's exact semantics —
/// next-open market fills, limit/stop triggers, adverse-first intrabar resolution,
/// slippage + maker/taker fees.
pub struct PaperExecutionClient {
    pub venue: String,
    pub symbol: String,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    pub multiplier: f64,
    /// Per-venue fee model (fee model 2/5). `None` (the `new` path) keeps the flat
    /// `maker_fee`/`taker_fee` × `broker_sim::fee` primitive byte-identical (the r7 gate + every
    /// existing caller). `Some` (the `with_fee_schedule` path) applies [`FeeSchedule::commission`]
    /// instead — the per-venue schedule the mount looks up.
    fee_schedule: Option<FeeSchedule>,
    /// Optional underlying/index-price source for the ACCURATE Deribit-options fee (fee model
    /// follow-up 2). `None` on every constructor today => byte-identical legacy commission. `Some`
    /// (via [`Self::with_underlying_source`]) routes a [`FeeSchedule::PercentOfUnderlying`] fill
    /// through [`FeeSchedule::commission_with_underlying`] when it prices `(venue, symbol, ts)`;
    /// see [`UnderlyingSource`] and [`Self::commission_for`].
    ///
    /// TODO(deribit-options-fee-wiring): no production caller supplies this yet — the seam is built
    /// and tested but `vike_mount::make_engine` does not thread an index-price feed into the paper
    /// Deribit book. Wiring the actual source (which index/mark feed, at what freshness) is the
    /// deferred product decision reported as a blocker; until then Deribit-options paper fills keep
    /// today's premium-only (understated) commission. See the module note in `vike_model::money::fees`.
    underlying_source: Option<UnderlyingSource>,
    pending: Vec<(String, WorkingOrder)>, // (coid, resting order)
    /// OTO/OCO bracket linkage, keyed by coid — the shared [`vike_exec::ContingencyBook`]
    /// resolver (trigger-law wave 2: ONE law for paper and the backtest SimBroker). Entries exist
    /// ONLY for contingent legs, so plain orders are untouched (byte-identical r7 path). A held
    /// OTO exit awaits its parent fill; on a leg's fill its children arm and its OCO siblings
    /// cancel. See `apply_contingency`.
    contingency: ContingencyBook,
    /// Time-in-force deadlines, keyed by coid — present ONLY for orders submitted with an
    /// EXPIRING TIF (`Gtd`/`Day`). A `Gtc` order (the default everywhere) inserts nothing, so this
    /// map stays empty and `expire_due` is a no-op ⇒ the r7 gate and every existing caller are
    /// byte-identical. See [`Expiry`].
    expiry: IndexMap<String, Expiry>,
    /// Resting multi-leg combos, keyed by coid (combo PR-3). EMPTY on every non-combo run — and
    /// the whole combo path is gated on it being non-empty, so the r7 gate, `leg_marks` and the
    /// single-symbol fill pass are byte-identical when combos are unused. See [`RestingCombo`].
    combos: Vec<(String, RestingCombo)>,
    /// Last observed bar per symbol — the per-leg mark source a combo prices its legs from.
    /// Recorded ONLY while a combo is resting (see `on_bar`), so a plain run allocates nothing —
    /// and DROPPED the moment the combo book empties (`drop_marks_if_idle`), so a combo submitted
    /// later can never price off a mark recorded during an earlier combo's life.
    ///
    /// A combo's legs are OTHER instruments than this book's own `symbol`, so the driver must feed
    /// this book symbol-tagged bars for every leg; `on_bar` never filters by symbol. A leg with no
    /// mark, or a mark older than [`Self::combo_mark_staleness_ms`] at trigger time, simply blocks
    /// the fill (all-or-none) — it never fills at a stale or guessed price.
    leg_marks: IndexMap<String, Bar>,
    /// Freshness bound (ms) every leg's mark must satisfy at combo-TRIGGER time, mirroring the
    /// engine's `EngineParams::max_price_staleness_ms` convention exactly (the shared
    /// [`vike_fills::staleness::is_stale`]: stale iff `pass_ts - mark.ts` STRICTLY exceeds the bound; a
    /// negative bound clamps to 0; a future-stamped mark is never stale).
    ///
    /// Default **0** — the strictest setting: every leg must have printed at the SAME ts as the
    /// triggering pass, so the net the trigger tests is one whose leg prices actually coexisted.
    /// Unlike the engine's knob this is NOT `Option` and has no off state, deliberately: "off"
    /// means a resting combo whose leg went dark keeps triggering off `A(t) + B(t−N)` — a net
    /// that never existed — and (with `leg_marks` retention) a later combo filling in full at
    /// arbitrarily old prices stamped with the current ts. A driver whose leg bar streams
    /// interleave across ts (e.g. per-leg feeds a few ms apart) widens this to its bar interval
    /// instead of turning the discipline off.
    pub combo_mark_staleness_ms: i64,
    /// The HALT sentinel this book consults at [`ExecutionClient::submit`], or `None` (the default
    /// on EVERY constructor) to consult none.
    ///
    /// ⚠ **Opt-in is the whole design, not caution.** This client wears two hats: it is the paper
    /// exchange a live DAEMON mounts (`vike_mount::make_engine`'s paper arms,
    /// `vike_mount::build_paper_maker_core_with`), and it is the SIMULATION primitive
    /// `vike-backtest`'s `tests/r7_gate.rs` drives to prove backtest == paper bit-for-bit. Reading a
    /// process-wide sentinel unconditionally would make a backtest's fills depend on a file lying
    /// around in `<project>/settings/state/`, so a stray `HALT` on a dev box or a CI runner would
    /// silently change results and redden the equivalence gate for a reason no one would find. A
    /// MOUNT arms it; a simulation never does, and `None` is byte-identical to the behaviour before
    /// this field existed.
    ///
    /// See [`Self::with_halt_path`] for why a mount passes a resolved path rather than this crate
    /// resolving one itself.
    halt_path: Option<PathBuf>,
    events: VecDeque<Event>,
    /// signed position tracked locally (resolve_intrabar_fills needs it for brackets)
    position: f64,
    fill_n: u64,
    /// every fill in emit order — the r7 gate's comparison stream (shared: the
    /// client moves into the core thread; the test keeps a clone)
    pub fills: Arc<Mutex<Vec<PaperFill>>>,
}

impl PaperExecutionClient {
    pub fn new(venue: &str, symbol: &str, slippage: f64, maker_fee: f64, taker_fee: f64) -> Self {
        PaperExecutionClient {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            slippage,
            maker_fee,
            taker_fee,
            multiplier: 1.0,
            fee_schedule: None,
            underlying_source: None,
            pending: Vec::new(),
            contingency: ContingencyBook::new(),
            expiry: IndexMap::new(),
            combos: Vec::new(),
            leg_marks: IndexMap::new(),
            combo_mark_staleness_ms: 0,
            halt_path: None,
            events: VecDeque::new(),
            position: 0.0,
            fill_n: 0,
            fills: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Construct a paper book that computes commission from a per-venue [`FeeSchedule`] instead of
    /// the flat `maker_fee`/`taker_fee` rates (fee model 2/5). `slippage` stays a SEPARATE cost
    /// (never folded into the fee model). Used by `vike_mount::make_engine` with
    /// `vike_model::fee_schedule_for(venue)`; the r7 gate and unit tests keep using [`Self::new`]
    /// (the byte-identical flat-rate path).
    pub fn with_fee_schedule(
        venue: &str,
        symbol: &str,
        slippage: f64,
        fee_schedule: FeeSchedule,
    ) -> Self {
        let mut c = Self::new(venue, symbol, slippage, 0.0, 0.0);
        c.fee_schedule = Some(fee_schedule);
        c
    }

    /// Thread an underlying/index-price source into this book so a Deribit-options
    /// [`FeeSchedule::PercentOfUnderlying`] fill books the ACCURATE
    /// `min(0.03% × underlying, 12.5% × premium)` commission (fee model follow-up 2). Mirrors
    /// `vike_sim::EngineParams::properties`' `(venue, symbol, ts) -> Option<_>` seam.
    ///
    /// **This is a deliberate BEHAVIOR CHANGE, gated entirely on the presence of the source**: a
    /// `PercentOfUnderlying` fill whose `(venue, symbol, ts)` the source prices is charged
    /// ~20x more than the premium-only approximation (0.03%-of-UNDERLYING vs 0.03%-of-premium). With
    /// NO source (every constructor above), or for any other fee shape, or when the source returns
    /// `None`, the commission is byte-identical to before. See [`Self::commission_for`].
    pub fn with_underlying_source(mut self, source: UnderlyingSource) -> Self {
        self.underlying_source = Some(source);
        self
    }

    /// Arm the operator HALT kill switch on this book: while `path` EXISTS, a submit that opens or
    /// adds risk is refused with a terminal `OrderRejected`
    /// ([`vike_exec::halt::HALT_REJECT_REASON`]), and a `reduce_only` submit still passes
    /// ([`vike_exec::halt::halt_admits_submit`] — the same predicate the venue adapters use, so paper
    /// and live cannot disagree about what a halt lets out).
    ///
    /// **Only a MOUNT calls this**, and that is the point. A paper daemon is where an operator
    /// REHEARSES the kill switch, and before this existed `touch <project>/settings/state/HALT` on a paper mount
    /// did nothing at all, silently — the switch appeared armed and was not. A switch that works on
    /// some mounts and not others is worse than one that works nowhere, because the operator cannot
    /// tell which they have. `vike-backtest` and the r7 gate never call this, so the equivalence law
    /// is untouched (see [`Self::halt_path`]'s field doc).
    ///
    /// ⚠ It takes a RESOLVED path instead of resolving one here, for two reasons. This
    /// workspace's standing rule is that libraries take configuration as parameters and only
    /// binaries read the environment (`crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN`
    /// ratchets that down and would refuse a new library read); and the resolution is not a plain
    /// variable lookup but a precedence (the declared project, else the exe directory) with an
    /// armability probe and a once-per-process report, which lives — deliberately, once — in `crates/vike-bridge-core/src/halt.rs`'s
    /// `halt_path_from_env`. A mount that can see that crate passes its answer in, so both mounts
    /// and every venue adapter consult ONE path.
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// The sentinel this book was armed with, or `None` when it is a simulation.
    ///
    /// Exists so a MOUNT SEAM can be gated on having armed one, on the CONSTRUCTED book: "every
    /// mount arms the kill switch" is otherwise a property held only by each call site remembering
    /// to — which is exactly how the paper mount came to observe no HALT file at all — and reading
    /// the source text cannot tell an armed construction from an unarmed one.
    ///
    /// ⚠ **A per-seam assertion is not a gate over the SET of seams, and this doc used to claim it
    /// was.** It said `vike_mount::paper_client` and `vike_run::paper_client_for` "each assert on
    /// this, so adding a third mount seam that forgets would fail" — while
    /// `crates/vike-mount/src/xemm.rs`'s `build_paper_xemm_core` was ALREADY that third seam, unarmed.
    /// An assertion inside seam A is evidence about A. Nothing was looking at the set, so nothing
    /// could fail. The two halves now in place:
    ///
    /// - **the roster gate** — `crates/vike-ops/tests/wiring/paper_mount_arming_gate.rs` walks every
    ///   crate's `src/` for calls to this client's constructors and fails on a file it does not
    ///   classify as MOUNT (must arm) or SIMULATION (must not). A FOURTH seam reddens there before
    ///   anyone has to notice it is a mount;
    /// - **the per-seam assertions**, which answer the question the roster gate cannot — is THIS
    ///   book armed: `vike_mount`'s `the_paper_fallback_every_venue_arm_uses_is_halt_armed`,
    ///   `vike_mount`'s `the_paper_maker_mount_is_halt_armed_on_both_fee_paths` and
    ///   `the_xemm_paper_mount_arms_both_books`.
    pub fn halt_path(&self) -> Option<&Path> {
        self.halt_path.as_deref()
    }

    /// True when this book has an armed sentinel and it currently EXISTS ⇒ new order placement must
    /// be refused. `None` (every non-mount caller) short-circuits without touching the filesystem,
    /// which is what keeps a backtest allocation- and syscall-free on this path.
    fn halt_engaged(&self) -> bool {
        self.halt_path.as_ref().is_some_and(|p| p.exists())
    }

    /// The commission for ONE leg fill of `qty` @ `px` on `symbol` at `ts`. This is the single
    /// commission decision both [`Self::emit_fill`] (single-symbol) and [`Self::emit_combo_fills`]
    /// (per combo leg) route through.
    ///
    /// - `fee_schedule = None` (the `new` path): the flat `maker_fee`/`taker_fee` × `broker_sim::fee`
    ///   primitive — byte-identical to before (the r7 gate path).
    /// - `fee_schedule = Some(PercentOfUnderlying)` AND an [`UnderlyingSource`] is wired AND it
    ///   prices `(venue, symbol, ts)`: the ACCURATE [`FeeSchedule::commission_with_underlying`]
    ///   (0.03%-of-underlying, premium-cap-bounded). `px` is the traded PREMIUM here (the fill
    ///   price), `underlying` comes from the source. THIS is the deliberate behavior change.
    /// - any other schedule, or no source, or a source that returns `None`: the previous
    ///   [`FeeSchedule::commission`] number, bit-for-bit.
    fn commission_for(&self, is_maker: bool, qty: f64, px: f64, symbol: &str, ts: i64) -> f64 {
        match &self.fee_schedule {
            Some(sched) => {
                // ONLY the Deribit-options `PercentOfUnderlying` shape has an underlying dimension;
                // for every other shape the underlying is irrelevant and `commission` is exact.
                if matches!(sched, FeeSchedule::PercentOfUnderlying { .. })
                    && let Some(src) = &self.underlying_source
                    && let Some(underlying) = src(&self.venue, symbol, ts)
                {
                    // `px` is the traded premium (the fill price); `qty` the contract count.
                    return sched.commission_with_underlying(qty, px, underlying);
                }
                sched.commission(is_maker, qty, px)
            }
            None => {
                let rate = if is_maker { self.maker_fee } else { self.taker_fee };
                fee(qty, px, rate, self.multiplier)
            }
        }
    }

    fn emit_fill(
        &mut self,
        coid: &str,
        requested_qty: f64,
        order: &WorkingOrder,
        raw_px: f64,
        ts: i64,
    ) {
        // Maker/taker by ORDER KIND, not by crossing aggressiveness: a MARKETABLE limit (priced
        // through the book, filled on the next bar's open) books as a maker fill, here and in the
        // engine's `dispatch_fill`. See the classification caveat in `vike_model::money::fees` — it flips
        // the fee SIGN under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        let is_maker = order.kind == OrderKind::Limit;
        let px = adverse_fill_price(raw_px, order.side, self.slippage);
        // `None` (the `new` path) = the byte-identical flat-rate primitive (r7 gate); `Some` = the
        // per-venue schedule the mount looked up. Slippage-adjusted `px` feeds both, as before. The
        // Deribit-options underlying accuracy (when an underlying source is wired) lives inside
        // `commission_for`; this single-symbol fill's instrument is this book's own `symbol`.
        let symbol = self.symbol.clone();
        let commission = self.commission_for(is_maker, order.size, px, &symbol, ts);
        self.position += order.side as f64 * order.size;
        // a filled order has left the resting book, so it can never expire afterwards
        // (no-op for the `Gtc` default, whose map entry was never inserted).
        self.expiry.shift_remove(coid);
        self.fill_n += 1;
        let fill = FillEvent {
            // Minted by the paper book, not read off a wire: `TradeId::prefixed` is infallible by
            // construction and renders the same `paper-<n>` bytes the `format!` did. The shape is a
            // journal key (the simulated-fill dedup set), so it must not change.
            trade_id: TradeId::prefixed("paper-", self.fill_n),
            client_order_id: coid.to_string(),
            venue: self.venue.clone().into(),
            symbol: self.symbol.clone().into(),
            side: order.side,
            last_qty: order.size,
            last_px: px,
            commission,
            commission_asset: String::new().into(),
            liquidity_side: if is_maker { "maker" } else { "taker" }.to_string().into(),
            ts,
            mark_price: None,
            position_side: "BOTH".to_string().into(),
        };
        self.fills.lock().unwrap().push(PaperFill {
            ts,
            side: order.side,
            qty: order.size,
            px,
            fee: commission,
            is_maker,
        });
        // bare fill FIRST (Account folds it), then the FSM wrap — the dual-publish rule
        self.events.push_back(Event::Fill(fill.clone()));
        // sim_exchange terminality: cumulative < requested - 1e-9 => partial
        let wrap = if order.size < requested_qty - 1e-9 {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: coid.to_string(),
                fill,
                ts,
            })
        } else {
            Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts })
        };
        self.events.push_back(wrap);
    }

    /// Time-in-force sweep, run as simulated time advances (once per `on_bar`, BEFORE the fill
    /// pass) — the paper twin of a venue's server-side **GTD/DAY** deadline enforcement, which a
    /// backtest otherwise lacks entirely (a `Gtd` order would rest forever). `Ioc`/`Fok` are NOT
    /// enforced here; see [`Expiry`].
    ///
    /// Every resting order whose [`Expiry`] says it is expired as of `now_ms` is removed from the
    /// book and terminalized with **`Event::OrderExpired`** — the model's purpose-built expiry
    /// vocabulary, the same variant binance/bybit/aster emit, so a paper GTD elapse drives
    /// `OrderEventKind::Expired` and the OMS `OrderStatus::Expired` exactly as the live venue does
    /// (`Accepted -> Expired` is a legal FSM edge; `submit` publishes `OrderAccepted`). It fires
    /// exactly once — the map entry is dropped with the order, and a filled/canceled order has
    /// already left both, so no order can expire twice or expire after it terminated.
    ///
    /// **Resolution caveat (real bars):** `now_ms` is `Bar.ts`, which is the bar's OPEN time, while
    /// a closed bar covers `[ts, ts+interval)`. The deadline is therefore evaluated at bar OPENS
    /// only: an order expires on **the first bar whose OPEN ts is at or past its deadline**, so a
    /// deadline lying strictly INSIDE a bar interval lets the order survive — and fill off that
    /// bar's full range — for up to one interval past the deadline. That coarseness is inherent to
    /// bar-resolution simulation; it is pinned by `a_deadline_inside_a_bar_overshoots_to_the_next`.
    ///
    /// Within a bar the ordering is deliberate: an order the sweep has already retired **cannot
    /// fill on that bar**. Expiry is a state change at the bar's timestamp and the bar's fills are
    /// attributed to that same timestamp, so filling an already-dead order would be a look-back.
    /// Cancels never touch the fill path or `position`.
    ///
    /// No-op (early return, no allocation) whenever no expiring-TIF order is resting — which is
    /// every `Gtc`-only run.
    fn expire_due(&mut self, now_ms: i64) {
        if self.expiry.is_empty() {
            return;
        }
        // A `Day` order's session is the UTC day of the FIRST bar it sees — anchor it here rather
        // than on the (frequently unstamped) request ts. See [`Expiry::day_anchor`].
        for e in self.expiry.values_mut() {
            if matches!(e.tif, TimeInForce::Day) && e.day_anchor.is_none() {
                e.day_anchor = Some(now_ms);
            }
        }
        let due: Vec<String> = self
            .expiry
            .iter()
            .filter(|(_, e)| e.is_expired(now_ms))
            .map(|(coid, _)| coid.clone())
            .collect();
        for coid in due {
            self.expiry.shift_remove(&coid);
            // only a still-RESTING order expires; anything already gone emits nothing. A resting
            // COMBO expires as one order too (its whole leg set leaves the book together).
            let before = self.pending.len() + self.combos.len();
            self.pending.retain(|(c, _)| c != &coid);
            self.combos.retain(|(c, _)| c != &coid);
            if self.pending.len() + self.combos.len() == before {
                continue;
            }
            self.contingency.remove(&coid);
            self.events.push_back(Event::OrderExpired(OrderExpired {
                client_order_id: coid.clone(),
                ts: now_ms,
            }));
            self.expire_children_of(&coid, now_ms);
        }
        self.drop_marks_if_idle(); // the expired order may have been the last resting combo
    }

    /// Cascade-cancel the held OTO children of an order that just EXPIRED (transitively, so a
    /// child-of-a-child goes too).
    ///
    /// A held exit only ever arms on its parent's FILL (`apply_contingency`), so once the parent
    /// has expired its children can never arm and can never fill — left alone they would rest
    /// inactive and invisible-but-live for the rest of the run, inflating the open-order set and
    /// never terminalizing. They are canceled with the distinct reason **`"parent-expired"`** (vs
    /// `"user"` / `"oco"`): the child did not itself reach a deadline, its parent did.
    ///
    /// Only reachable from `expire_due`, i.e. only when an expiring-TIF order was resting, so the
    /// `Gtc` path never runs it.
    fn expire_children_of(&mut self, parent_coid: &str, ts: i64) {
        let mut queue: Vec<String> = vec![parent_coid.to_string()];
        while let Some(parent) = queue.pop() {
            let children = self.contingency.children_of(&parent);
            for child in children {
                self.contingency.remove(&child);
                self.expiry.shift_remove(&child);
                let before = self.pending.len();
                self.pending.retain(|(c, _)| c != &child);
                if self.pending.len() != before {
                    self.events.push_back(Event::OrderCanceled(OrderCanceled {
                        client_order_id: child.clone(),
                        reason: "parent-expired".to_string().into(),
                        ts,
                    }));
                }
                queue.push(child);
            }
        }
    }

    /// OTO arm-on-fill + OCO cancel-sibling, applied after `filled_coid` fills. A no-op for plain
    /// orders (no contingency entry). The DECISION — which held children arm, which linked
    /// siblings cancel — is the shared [`vike_exec::ContingencyBook::on_fill`] resolver (ONE law
    /// with the backtest SimBroker's lanes, trigger-law wave 2); this method keeps the book
    /// mechanics: cancel each returned sibling still resting here and emit its `OrderCanceled`.
    fn apply_contingency(&mut self, filled_coid: &str, ts: i64) {
        for sib in self.contingency.on_fill(filled_coid) {
            if self.pending.iter().any(|(coid, _)| coid == &sib) {
                self.pending.retain(|(coid, _)| coid != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    client_order_id: sib,
                    reason: "oco".to_string().into(),
                    ts,
                }));
            } else if self.combos.iter().any(|(coid, _)| coid == &sib) {
                // An OCO sibling that is a resting COMBO cancels as ONE order too (all legs at
                // once), same as `cancel`. Only reachable from a PLAIN order's fill during the
                // single-symbol pass — a combo can never carry links itself (`submit` rejects
                // them), so this never runs inside `fill_combos`' mem::take window. Marks are NOT
                // dropped here: `fill_combos` runs right after this pass and owns that hygiene.
                self.combos.retain(|(coid, _)| coid != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    client_order_id: sib,
                    reason: "oco".to_string().into(),
                    ts,
                }));
            }
        }
    }

    /// Price every leg of `combo` off the recorded marks, or `None` if ANY leg is still unmarked
    /// **or marked staler than [`Self::combo_mark_staleness_ms`] as of `now_ts`** (the triggering
    /// pass's bar ts). A stale mark is treated exactly like no mark at all.
    ///
    /// This all-or-nothing return is the FIRST half of combo atomicity: a combo whose legs are not
    /// all priceable AT THE SAME MOMENT produces no quote at all, so it can neither half-fill on
    /// the legs that happen to have ticked nor trigger off a net whose leg prices never coexisted.
    ///
    /// Side selection mirrors [`vike_model::combo_net_from_legs`] exactly: the combo pays the ASK
    /// on a leg it buys and receives the BID on a leg it sells, i.e. `want_ask = side · ratio > 0`.
    /// A bar without an explicit bid/ask falls back to its close for both sides.
    fn quote_legs(&self, combo: &RestingCombo, now_ts: i64) -> Option<Vec<LegQuote>> {
        let mut quotes: Vec<LegQuote> = Vec::with_capacity(combo.legs.len());
        for leg in &combo.legs {
            let bar = self.leg_marks.get(&leg.symbol)?;
            // The engine's staleness convention verbatim (strict `age > bound`; bound 0 = only a
            // mark stamped at this very pass's ts). The mark's ts IS its bar's ts — bar streams
            // are the clock here, exactly as in the engine's stale-price wait lane.
            if vike_fills::staleness::is_stale(Some(bar.ts), now_ts, self.combo_mark_staleness_ms) {
                return None;
            }
            let want_ask = combo.side.signum() * leg.ratio.signum() > 0;
            let px =
                if want_ask { bar.ask.unwrap_or(bar.close) } else { bar.bid.unwrap_or(bar.close) };
            quotes.push((leg.ratio, px));
        }
        Some(quotes)
    }

    /// Combo-book hygiene: `leg_marks` exists ONLY to price a resting combo's legs, so the moment
    /// the last combo leaves the book (filled / canceled / expired / OCO-canceled) every recorded
    /// mark is dropped. This keeps the field's contract literal — marks live only while a combo is
    /// resting — and makes mark RESURRECTION structurally impossible: a combo submitted later can
    /// never price off a mark recorded during an earlier combo's life, no matter how permissive
    /// `combo_mark_staleness_ms` is. (The freshness bound alone already refuses old marks; this is
    /// the belt to that suspenders, and it also frees the map on every combo-free run.)
    ///
    /// Never called from inside `emit_combo_fills`: `fill_combos` drains `self.combos` via
    /// `mem::take` while iterating, so the book is transiently empty there and clearing marks
    /// mid-pass would starve the other combos in the same pass.
    fn drop_marks_if_idle(&mut self) {
        if self.combos.is_empty() && !self.leg_marks.is_empty() {
            self.leg_marks.clear();
        }
    }

    /// The combo twin of the single-symbol fill pass: for each resting combo, price its legs, test
    /// the COMBO's net against its net limit, and fill **every leg or none**.
    ///
    /// The trigger is the aggregate net only ([`vike_model::combo_net_cross`], which owns both the
    /// `Σ(ratio · px)` sign law and the buy-`net ≤ limit` / sell-`net ≥ limit` crossing law). The
    /// net is SIGNED and is never compared as a magnitude: a credit spread's net and limit are both
    /// negative and cross exactly like a debit's, which is why nothing here takes an `abs()`.
    ///
    /// A combo MARKET (`net_limit == None`) has no trigger — it fills as soon as every leg carries
    /// a FRESH mark (see `quote_legs`; freshness is the same gate the limit trigger runs behind).
    ///
    /// Each leg then fills at its OWN price; the net is only the trigger, never a fill price. This
    /// is deliberate: downstream accounting must see real per-instrument fills, not one synthetic
    /// blended fill on a symbol that does not exist.
    ///
    /// **The net limit binds PRE-slippage.** The trigger compares the RAW leg quotes; per-leg
    /// adverse slippage is applied afterwards in `emit_combo_fills`, so with `slippage > 0` the
    /// EXECUTED net lands through the limit by slippage × gross(Σ|ratio·px|). This is not an
    /// oversight — it is the same law as this book's single-symbol path (`BarFillModel` triggers
    /// off raw bar prices; `emit_fill` then applies `adverse_fill_price`) and as LEAN's
    /// `ComboLimitFill`, which also crosses on raw quotes. Folding slippage into the trigger here
    /// would make a combo limit STRICTER than a plain limit under the identical cost model.
    /// Pinned by `slippage_is_adverse_per_leg_side`.
    fn fill_combos(&mut self, ts: i64) {
        if self.combos.is_empty() {
            // the book may have emptied since the last pass (cancel / OCO) — drop dead marks
            self.drop_marks_if_idle();
            return;
        }
        let mut still: Vec<(String, RestingCombo)> = Vec::new();
        for (coid, combo) in std::mem::take(&mut self.combos) {
            let Some(quotes) = self.quote_legs(&combo, ts) else {
                still.push((coid, combo)); // a leg is unmarked or stale — cannot fill any leg
                continue;
            };
            // `None` limit = combo market: no crossing test at all. `Some` = LEAN's
            // `ComboLimitFill` aggregate crossing law, sign-preserving.
            if let Some(limit) = combo.net_limit
                && combo_net_cross(combo.side, limit, &quotes).is_none()
            {
                still.push((coid, combo)); // net has not crossed — the whole combo rests on
                continue;
            }
            self.emit_combo_fills(&coid, &combo, &quotes, ts);
        }
        self.combos = still;
        self.drop_marks_if_idle();
    }

    /// Emit the per-leg fills of ONE crossed combo — the SECOND half of atomicity: this runs only
    /// after `fill_combos` has proved every leg priceable and the net crossed, and it emits all
    /// `n` legs unconditionally, so there is no path on which a subset of legs fills.
    ///
    /// Event shape follows `emit_fill`'s dual-publish rule per leg (bare `Fill` first, then the FSM
    /// wrap) and keeps the ONE-order invariant: legs `0..n-1` wrap as `OrderPartiallyFilled` and
    /// the LAST leg as `OrderFilled`, so the combo's single coid reaches exactly one terminal
    /// event, exactly as a single-symbol order does.
    ///
    /// **The consuming engine must admit every LEG symbol** — via `ExecutionEngine::extra_symbols`
    /// (the mount-side lowering wires this at combo registration; it also carries leg
    /// Funding/PositionLiquidated events, which fill-routing cannot), AND, since combo gate 4,
    /// the engine's own `owns_fill_symbol` coid-ownership route folds a bare leg fill whose
    /// `client_order_id` names the registered combo even when the leg was never admitted — the
    /// safety net that retired the old failure shape (order Filled via the coid-routed wraps,
    /// Account flat, strategy blind). Both are proven by `tests/combo_engine_fold.rs`.
    ///
    /// **`ManagedOrder` accounting is a cross-instrument AGGREGATE for a combo.** The FSM wraps
    /// accumulate per-leg fills under the one coid, so `filled_qty` ends at `Σ|ratio| × qty`
    /// across legs — e.g. 9 for a 1×2 ratio spread with `request.qty` 3 — and `avg_fill_px` is a
    /// qty-weighted VWAP across DIFFERENT instruments: display-only, a price on no tradable
    /// instrument, and NOT the per-unit net (which `combo_net` computes from the leg fills).
    /// Surfaces as-is in `OrderView`, the journal's `exec_order` rows and any order table; per-leg
    /// truth lives in the bare `Fill`s (Account) — pinned by `tests/combo_engine_fold.rs`.
    /// Making the wraps count combo units instead would require the wrap's embedded `FillEvent`
    /// to disagree with the bare `Fill` the Account folds (same struct, dual-published), rippling
    /// into vike-exec's accumulate/journal semantics — documented instead, deliberately.
    ///
    /// **Fill prices are the raw quotes worsened by per-leg adverse slippage** — the net limit the
    /// trigger enforced binds pre-slippage (see `fill_combos`).
    ///
    /// `self.position` is deliberately NOT moved: it tracks this book's own single `symbol` for
    /// `resolve_intrabar_fills`' bracket guard, while combo legs are other instruments. Folding
    /// foreign-instrument quantities into it would corrupt the single-symbol fill pass.
    fn emit_combo_fills(&mut self, coid: &str, combo: &RestingCombo, quotes: &[LegQuote], ts: i64) {
        // a filled combo has left the resting book, so it can never expire afterwards
        self.expiry.shift_remove(coid);
        // maker/taker by ORDER KIND, matching `emit_fill`: a net-limit combo books maker.
        let is_maker = combo.net_limit.is_some();
        let n = combo.legs.len();
        for (i, leg) in combo.legs.iter().enumerate() {
            let (ratio, raw_px) = quotes[i];
            // selling the combo flips every leg (the venue convention `ComboLeg` documents)
            let leg_side = combo.side.signum() * ratio.signum();
            let leg_qty = f64::from(ratio.abs()) * combo.qty;
            let px = adverse_fill_price(raw_px, leg_side, self.slippage);
            // Each leg fills on its OWN instrument, so the underlying source (if wired) is queried
            // per LEG symbol; a non-`PercentOfUnderlying`/no-source combo is byte-identical.
            let commission = self.commission_for(is_maker, leg_qty, px, &leg.symbol, ts);
            self.fill_n += 1;
            let fill = FillEvent {
                // Same minted namespace as the single-symbol path above — `fill_n` is shared, so
                // combo legs and plain fills draw from one sequence and stay byte-identical.
                trade_id: TradeId::prefixed("paper-", self.fill_n),
                client_order_id: coid.to_string(),
                venue: self.venue.clone().into(),
                // the LEG's own instrument — never the (empty) combo symbol
                symbol: leg.symbol.clone().into(),
                side: leg_side,
                last_qty: leg_qty,
                last_px: px,
                commission,
                commission_asset: String::new().into(),
                liquidity_side: if is_maker { "maker" } else { "taker" }.to_string().into(),
                ts,
                mark_price: None,
                position_side: "BOTH".to_string().into(),
            };
            self.fills.lock().unwrap().push(PaperFill {
                ts,
                side: leg_side,
                qty: leg_qty,
                px,
                fee: commission,
                is_maker,
            });
            self.events.push_back(Event::Fill(fill.clone()));
            let wrap = if i + 1 == n {
                Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts })
            } else {
                Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts,
                })
            };
            self.events.push_back(wrap);
        }
        self.apply_contingency(coid, ts);
    }
}

impl ExecutionClient for PaperExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        // HALT kill-switch, when this book was mounted with one (`with_halt_path`; `None` on every
        // simulation path short-circuits above without a syscall). While the sentinel exists, refuse
        // a submit that OPENS or ADDS risk and synthesize the terminal rejection through the normal
        // event path, so the intent never silently vanishes — the same emitter-split contract a live
        // adapter honours, and checked here BEFORE the combo/contingency arms so a halted mount
        // refuses every shape of opening order identically.
        //
        // ⚠ A REDUCING SUBMIT IS ADMITTED (`vike_exec::halt::halt_admits_submit`, shared verbatim
        // with `ExecActor` and hyperliquid): a halt must never trap an operator in a position, and
        // `OrderIntent::Flatten` mints exactly this shape, so `market-exit` still works on a halted
        // paper mount. Cancel is never gated at all — it does not pass through here.
        if self.halt_engaged() && !vike_exec::halt::halt_admits_submit(request) {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: vike_exec::halt::HALT_REJECT_REASON.to_string().into(),
                ts: request.ts,
            }));
            return;
        }
        // A COMBO carrying contingency links is REJECTED terminally, not accepted-and-ignored:
        // the OTO-hold check lives in the single-symbol fill pass only (`fill_combos` never
        // consults `contingency.active`, so a "held" combo child would fill immediately) and the
        // expire-cascade sweeps `pending` only. Silently recording the links while enforcing none
        // of them is the one thing this book must never do — the dead-path rule says reject loud.
        // (`Submitted -> Rejected` with no `OrderAccepted`, same shape as the multi-router's
        // no-book arm.) A combo may still act as an OTO PARENT of plain children — the parent
        // itself carries no link fields — and a PLAIN order may OCO-link a combo (enforced in
        // `apply_contingency`).
        if !request.combo_legs.is_empty()
            && (request.parent_order_id.is_some()
                || !request.linked_order_ids.is_empty()
                || request.contingency_type.is_some())
        {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: "combo orders do not support contingency links (OTO/OCO)"
                    .to_string()
                    .into(),
                ts: request.ts,
            }));
            return;
        }
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: Some(request.client_order_id.clone().into()),
            ts: request.ts,
        }));
        // A COMBO (non-empty `combo_legs`) rests in its own book, never in `pending`: its `symbol`
        // is empty until a venue resolves the combo instrument and its `price` is a signed NET, so
        // lowering it to a `WorkingOrder` would hand the single-symbol fill primitives a limit
        // order on no instrument at a possibly-negative price. Empty legs = not a combo = every
        // pre-existing order, which takes the original path below byte-identically.
        if !request.combo_legs.is_empty() {
            self.combos.push((
                request.client_order_id.clone(),
                RestingCombo {
                    side: request.side,
                    qty: request.qty,
                    // `price` carries the SIGNED net limit verbatim (`build_combo`); a market
                    // combo carries `None`. Neither is clamped or absolute-valued here.
                    net_limit: request.price,
                    legs: request.combo_legs.clone(),
                },
            ));
        } else {
            self.pending.push((request.client_order_id.clone(), order_request_to_working(request)));
        }
        // record a TIF deadline for an EXPIRING time-in-force only — `Gtc` (the default) records
        // nothing, so the resting book behaves exactly as it did before. See `expire_due`.
        if matches!(request.time_in_force, TimeInForce::Gtd | TimeInForce::Day) {
            self.expiry.insert(
                request.client_order_id.clone(),
                Expiry {
                    tif: request.time_in_force,
                    gtd_expiry: request.gtd_expiry,
                    // anchored on the first bar, NOT on `request.ts` (which nothing stamps).
                    day_anchor: None,
                },
            );
        }
        // record OTO/OCO linkage for a bracket leg — plain orders add nothing, so the fill path
        // stays byte-identical (contingency map empty ⇒ `apply_contingency` is a no-op).
        if request.parent_order_id.is_some()
            || !request.linked_order_ids.is_empty()
            || request.contingency_type.is_some()
        {
            // a held exit (has a parent) arms on the parent's fill; an entry is active now —
            // the shared resolver's own insert law (`ContingencyBook::insert`).
            self.contingency.insert(
                request.client_order_id.clone(),
                request.parent_order_id.clone(),
                request.linked_order_ids.clone(),
            );
        }
    }

    fn cancel(&mut self, client_order_id: &str) {
        let before = self.pending.len() + self.combos.len();
        self.pending.retain(|(coid, _)| coid != client_order_id);
        // a resting combo cancels as ONE order (all legs at once) — it never half-cancels.
        self.combos.retain(|(coid, _)| coid != client_order_id);
        self.drop_marks_if_idle(); // last combo gone => its marks go with it
        self.contingency.remove(client_order_id);
        self.expiry.shift_remove(client_order_id);
        if self.pending.len() + self.combos.len() != before {
            self.events.push_back(Event::OrderCanceled(OrderCanceled {
                client_order_id: client_order_id.to_string(),
                reason: "user".to_string().into(),
                ts: 0,
            }));
        }
    }

    /// Modify a resting order in place (RUST-NATIVE; no Python twin — `sim_exchange.py` has no
    /// modify). Updates the resting `WorkingOrder`'s size/price and emits `OrderModified`; the fill
    /// on the next bar then uses the new terms. No-op if the order already filled/canceled (it is
    /// no longer in `pending`).
    ///
    /// ⚠ **`resting.size = q` MAKES THE AMEND'S QTY THE ORDER'S REMAINING SIZE**, which is neither
    /// venue convention — see [`ExecutionClient::amend_semantics`] below and
    /// `vike_model::AmendSemantics`. Whatever executed before the amend is not deducted from `q`
    /// here and `WorkingOrder` carries no execution history at all, so `q` is exactly what the next
    /// `on_bar` will fill. This is what the declaration on this client must keep saying.
    ///
    /// ⚠ **HALT BLOCKS A MODIFY OUTRIGHT — there is no reducing exemption here, unlike
    /// [`ExecutionClient::submit`].** A modify can add size or chase price, and "does this reduce?"
    /// cannot be inferred from the request: `order.reduce_only` describes the ORIGINAL order, and
    /// `new_qty` is neither reliably a delta nor reliably a remainder (see [`Self::amend_semantics`]
    /// — this book's own answer is `InPlaceRemaining`, and the venues disagree). So the exit path
    /// under a halt is CANCEL, which is never gated, and never modify.
    ///
    /// This arm exists because the arm in [`ExecutionClient::submit`] did and this one did not, and
    /// a paper mount that refused new risk while still admitting an amend that ADDS it is not the
    /// same kill switch a live venue has. `crates/vike-bridge-core/src/exec_actor.rs`'s `modify` is
    /// the copy this mirrors.
    ///
    /// ⚠ **It mirrored it down to the SILENCE, and that is what changed.** Both returned with no
    /// event at all, so a halted amend was byte-indistinguishable from one the adapter had lost —
    /// `docs/ops/kill-switches.md`'s gap 5, open on three clients of four. All four now emit the
    /// same NON-terminal [`Event::OrderModifyRejected`] carrying
    /// [`vike_exec::halt::HALT_REJECT_REASON`]. Non-terminal is load-bearing: the FSM maps it back
    /// to MODIFIABLE (`vike_exec::order`), so the resting order still keeps its terms and the
    /// VERDICT — a modify is refused outright, with no reducing exemption on any client — is
    /// untouched. The fix landed for all three at once for the reason this doc already gave: the
    /// whole point of the arm is that paper and live agree.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if self.halt_engaged() {
            self.events.push_back(Event::OrderModifyRejected(
                vike_model::events::OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                    ts: order.ts,
                },
            ));
            return;
        }
        let coid = order.client_order_id.as_str();
        if let Some((_, resting)) = self.pending.iter_mut().find(|(c, _)| c == coid) {
            if let Some(q) = new_qty {
                resting.size = q;
            }
            if let Some(p) = new_price {
                resting.price = Some(p); // WorkingOrder.price is the resting limit/stop level
            }
            self.events.push_back(Event::OrderModified(OrderModified {
                client_order_id: coid.to_string(),
                venue_order_id: Some(coid.to_string().into()),
                new_qty,
                new_price,
                ts: 0,
            }));
        }
    }

    /// ⚠ **THE PAPER BOOK IS NOT THE VENUE IT IS MOUNTED UNDER.** `vike_mount::make_engine` builds
    /// the `ExecutionEngine` with the REAL venue string even when the mount fell back to this client
    /// (a `paper` ceiling, a bridge's `ExecOutcome::Paper`, or a venue this build does not compile),
    /// and `vike_mount::build_paper_maker_core` does the same with its profile's venue — so without this declaration a paper mount on binance/okx/bybit would have its amends
    /// judged under `vike_model::AmendSemantics::InPlaceTotal`, i.e. the gate would assume
    /// `q − filled_qty` can still execute while [`PaperExecutionClient::modify`] above puts the whole
    /// `q` back on the book. That under-states projected exposure and under-charges margin, in the
    /// crate whose whole job is that "backtest == paper == live" is checkable.
    ///
    /// It is inert TODAY only by accident — a partially filled paper order leaves `pending` and
    /// never re-rests, so `modify` finds nothing and no amend of a partially filled order exists to
    /// mis-judge. Accidents are not gates, and this crate is the wrong place to keep one.
    fn amend_semantics(&self) -> Option<vike_model::AmendSemantics> {
        Some(vike_model::AmendSemantics::InPlaceRemaining)
    }

    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// The engine's `fill_pending` twin over the resting book.
    fn on_bar(&mut self, bar: &Bar) {
        // simulated time advanced to `bar.ts` — retire any resting order whose TIF deadline has
        // passed BEFORE filling (an expired order must not fill on the bar that expires it).
        // No-op unless an expiring-TIF order is resting.
        self.expire_due(bar.ts);
        // Record this bar as the mark for its symbol — ONLY while a combo is resting, so a plain
        // run neither allocates nor changes behavior. A symbol-less bar marks this book's own
        // symbol (the single-book compatibility case `MultiPaperExecutionClient::on_bar` relies on).
        if !self.combos.is_empty() {
            let sym = bar.symbol.clone().unwrap_or_else(|| self.symbol.clone());
            self.leg_marks.insert(sym, bar.clone());
        }
        let mut triggered: Vec<(String, WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<(String, WorkingOrder)> = Vec::new();
        for (coid, mut order) in std::mem::take(&mut self.pending) {
            // a held OTO exit (inactive) never fills until its parent arms it — keep it resting.
            if self.contingency.is_held(&coid) {
                still.push((coid, order));
                continue;
            }
            match BarFillModel.fill_price(&mut order, bar) {
                None => still.push((coid, order)),
                Some(px) => triggered.push((coid, order, px)),
            }
        }
        self.pending = still;
        // adverse-first intrabar resolution (the engine's exact path when >1 trigger)
        if triggered.len() > 1 {
            let orders: Vec<(WorkingOrder, f64)> =
                triggered.iter().map(|(_, o, p)| (o.clone(), *p)).collect();
            let (resolved, _both) = resolve_intrabar_fills(orders, self.position);
            // resolve preserves order objects; re-pair with coids by identity of the
            // (side, kind, size) — the engine keys by object identity, we key by index:
            // resolve_intrabar_fills returns the SAME orders (possibly size-capped),
            // in adverse-first order. Re-associate by matching original index.
            let mut used = vec![false; triggered.len()];
            for (order, px) in resolved {
                let idx = triggered
                    .iter()
                    .enumerate()
                    .position(|(i, (_, o, p))| {
                        !used[i]
                            && o.side == order.side
                            && o.kind == order.kind
                            && (*p - px).abs() < 1e-12
                    })
                    .expect("resolved order maps back");
                used[idx] = true;
                let (coid, original, _) = &triggered[idx];
                if order.size <= 1e-12 {
                    // capped to ~0 by the bracket guard: the order has already left `pending`
                    // (the triggered/still partition) and never re-rests, so drop its deadline
                    // too — the map must track `pending` exactly or `expire_due` loses its
                    // empty-map fast path for the rest of the run.
                    self.expiry.shift_remove(coid);
                    continue;
                }
                let requested = original.size;
                let coid = coid.clone();
                self.emit_fill(&coid, requested, &order, px, bar.ts);
                self.apply_contingency(&coid, bar.ts);
            }
        } else if let Some((coid, order, px)) = triggered.pop() {
            if order.size > 1e-12 {
                let requested = order.size;
                let coid = coid.clone();
                self.emit_fill(&coid, requested, &order, px, bar.ts);
                self.apply_contingency(&coid, bar.ts);
            } else {
                // size-capped to ~0: gone from `pending` without a fill — drop its deadline too
                // (see the same branch above).
                self.expiry.shift_remove(&coid);
            }
        }
        // the combo book, AFTER the single-symbol pass — so the existing path is untouched (and a
        // no-op early return when no combo rests, i.e. on every pre-combo run).
        self.fill_combos(bar.ts);
    }
}

/// The BacktestDataClient (plan R7): replay a bar list through the SAME lossless ingest
/// lane a live feed uses. The core fills + strategy-steps each close deterministically.
///
/// Takes the lane itself rather than the core handle it came from. That is the whole of what this
/// function ever used (`CoreHandle::bar_sender()` is a cheap `Clone` of exactly this sender), and
/// [`vike_exec::BarSender`] is the producer-side type venue bridges already hold — so the R7 replay
/// driver no longer names a `vike_core` type, which is what lets vike-backtest keep vike-core as a
/// DEV-dependency instead of a real one (see this crate's Cargo.toml). Live callers pass
/// `&handle.bar_sender()`; a test may equally pass the `vike_exec::event_channel()` twin's sender.
pub fn replay_bars(
    sender: &vike_exec::BarSender,
    venue: &str,
    symbol: &str,
    interval: &str,
    bars: &[Bar],
) {
    for bar in bars {
        sender
            .close(vike_exec::BarUpdate {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                interval: interval.to_string(),
                bar: bar.clone(),
            })
            .expect("core alive during replay");
    }
}

/// Phase D remainder: N per-symbol paper books behind ONE `ExecutionClient` — the
/// multi-symbol paper twin. Each book keeps `PaperExecutionClient`'s single-symbol
/// engine-primitive fill semantics untouched (r7 law); this wrapper only ROUTES:
/// submits by `request.symbol` (a miss synthesizes the terminal `OrderRejected` — the
/// dead-path rule: no order silently vanishes), bars by `bar.symbol` (None = every book,
/// the single-book compatibility case), cancels/modifies broadcast (books no-op silently
/// on unknown coids).
///
/// **Combos are NOT routed here** (combo PR-3): a combo's `symbol` is empty and its legs span
/// several instruments, so there is no single book to route it to. It therefore falls into the
/// no-book arm below and is terminally `OrderRejected` — loudly, per the dead-path rule, never
/// silently dropped or half-routed to one leg's book. Drive a combo through a single
/// `PaperExecutionClient` fed symbol-tagged bars for every leg instead; that book prices legs from
/// `leg_marks` regardless of its own `symbol`.
#[derive(Default)]
pub struct MultiPaperExecutionClient {
    /// (symbol, book) in registration order — few books, linear scan, no map dep
    books: Vec<(String, PaperExecutionClient)>,
    events: VecDeque<Event>,
}

impl MultiPaperExecutionClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one per-symbol book (keyed by its `symbol`; re-adding replaces).
    pub fn add_book(&mut self, book: PaperExecutionClient) {
        let symbol = book.symbol.clone();
        if let Some(slot) = self.books.iter_mut().find(|(s, _)| *s == symbol) {
            slot.1 = book;
        } else {
            self.books.push((symbol, book));
        }
    }

    pub fn book(&self, symbol: &str) -> Option<&PaperExecutionClient> {
        self.books.iter().find(|(s, _)| s == symbol).map(|(_, b)| b)
    }
}

impl ExecutionClient for MultiPaperExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        match self.books.iter_mut().find(|(s, _)| *s == request.symbol).map(|(_, b)| b) {
            Some(book) => book.submit(request),
            None => {
                // emitter-split rule: a dead route must synthesize a terminal
                self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: request.client_order_id.clone(),
                    ts: request.ts,
                }));
                self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: format!("no paper book for symbol {}", request.symbol).into(),
                    ts: request.ts,
                }));
            }
        }
    }

    fn cancel(&mut self, client_order_id: &str) {
        for (_, book) in self.books.iter_mut() {
            book.cancel(client_order_id); // no-op + no event on books that don't hold it
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if let Some((_, book)) = self.books.iter_mut().find(|(s, _)| *s == order.symbol) {
            book.modify(order, new_qty, new_price);
        }
    }

    /// The same declaration its per-symbol books make — this client IS N of them, and routing an
    /// amend to one of them does not change what that amend means. Spelled out rather than
    /// inherited from the trait default, for the reason the boxed `ExecutionClient` impl gives:
    /// a default would silently shadow the book's own override.
    fn amend_semantics(&self) -> Option<vike_model::AmendSemantics> {
        Some(vike_model::AmendSemantics::InPlaceRemaining)
    }

    fn poll_events(&mut self) -> Option<Event> {
        if let Some(ev) = self.events.pop_front() {
            return Some(ev);
        }
        for (_, book) in self.books.iter_mut() {
            if let Some(ev) = book.poll_events() {
                return Some(ev);
            }
        }
        None
    }

    fn on_bar(&mut self, bar: &Bar) {
        match &bar.symbol {
            Some(sym) => {
                if let Some((_, book)) = self.books.iter_mut().find(|(s, _)| s == sym) {
                    book.on_bar(bar);
                }
            }
            // symbol-less bar: the single-book compatibility case — every book sees it
            None => {
                for (_, book) in self.books.iter_mut() {
                    book.on_bar(bar);
                }
            }
        }
    }

    /// Phase-one teardown seam, fanned out to every book — the same shape `detach` below already
    /// has. Spelled out for the reason `amend_semantics` above gives: this wrapper delegates every
    /// other method, and a wrapper that delegates `detach` but inherits the trait's `begin_detach`
    /// no-op is exactly the shape `crates/vike-exec/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach` warns about. Today every book is a `PaperExecutionClient`,
    /// whose own `begin_detach` is the default no-op (an in-process book owns no thread and has no
    /// flag to raise), so this buys nothing yet — it costs nothing either, and the delegation is
    /// what keeps the trap closed the day a book grows a thread.
    fn begin_detach(&mut self) {
        for (_, book) in self.books.iter_mut() {
            book.begin_detach();
        }
    }

    fn detach(&mut self) {
        for (_, book) in self.books.iter_mut() {
            book.detach();
        }
    }
}

#[cfg(test)]
mod modify_tests;

#[cfg(test)]
mod fee_schedule_tests;

#[cfg(test)]
mod tif_expiry_tests;

#[cfg(test)]
mod bracket_tests;

#[cfg(test)]
mod combo_tests;
