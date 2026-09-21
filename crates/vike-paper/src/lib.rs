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
//! The payoff is on the consumer side. `vike-mount` and `vike-run` referenced `vike-backtest` for
//! this client and NOTHING else (12 and 4 sites, every one of them `vike_backtest::paper::…`), so
//! both were compiling the entire simulator — engine, harness, analytics — to obtain an
//! `ExecutionClient`. They depend on this crate instead. `vike_backtest::paper` remains as a
//! re-export, so every historical path still resolves.
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
/// mirroring `vike_backtest::engine::EngineParams::properties`' `(venue, symbol, ts) -> Option<_>` shape
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
    /// today's premium-only (understated) commission. See the module note in `vike_model::fees`.
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
    /// `vike_run::build_paper_maker_core_with`), and it is the SIMULATION primitive
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
    /// `vike_backtest::engine::EngineParams::properties`' `(venue, symbol, ts) -> Option<_>` seam.
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
    /// REHEARSES the kill switch, and before this existed `touch $VIKE_HALT_FILE` on a paper mount
    /// did nothing at all, silently — the switch appeared armed and was not. A switch that works on
    /// some mounts and not others is worse than one that works nowhere, because the operator cannot
    /// tell which they have. `vike-backtest` and the r7 gate never call this, so the equivalence law
    /// is untouched (see [`Self::halt_path`]'s field doc).
    ///
    /// ⚠ It takes a RESOLVED path instead of reading `VIKE_HALT_FILE` here, for two reasons. This
    /// workspace's standing rule is that libraries take configuration as parameters and only
    /// binaries read the environment (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`
    /// ratchets that down and would refuse a new library read); and the resolution is not a plain
    /// variable lookup but a three-rung precedence with an armability probe and a once-per-process
    /// report, which lives — deliberately, once — in `crates/vike-bridge-core/src/halt.rs`'s
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
    /// `crates/vike-run/src/xemm.rs`'s `build_paper_xemm_core` was ALREADY that third seam, unarmed.
    /// An assertion inside seam A is evidence about A. Nothing was looking at the set, so nothing
    /// could fail. The two halves now in place:
    ///
    /// - **the roster gate** — `crates/vike-ops/tests/paper_mount_arming_gate.rs` walks every
    ///   crate's `src/` for calls to this client's constructors and fails on a file it does not
    ///   classify as MOUNT (must arm) or SIMULATION (must not). A FOURTH seam reddens there before
    ///   anyone has to notice it is a mount;
    /// - **the per-seam assertions**, which answer the question the roster gate cannot — is THIS
    ///   book armed: `vike_mount`'s `the_paper_fallback_every_venue_arm_uses_is_halt_armed`,
    ///   `vike_run`'s `the_paper_maker_mount_is_halt_armed_on_both_fee_paths` and
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
        // engine's `dispatch_fill`. See the classification caveat in `vike_model::fees` — it flips
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
    /// the `ExecutionEngine` with the REAL venue string even when the absent-credentials gate fell
    /// back to this client, and `vike_run::build_paper_maker_core` does the same with its profile's
    /// venue — so without this declaration a paper mount on binance/okx/bybit would have its amends
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
mod modify_tests {
    use super::*;

    fn limit_req(coid: &str, side: i32, qty: f64, price: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty,
            order_type: "limit".into(),
            price: Some(price),
            ..Default::default()
        }
    }

    #[test]
    fn modify_reprices_the_resting_order_in_place() {
        let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        c.submit(&limit_req("o1", 1, 1.0, 90.0)); // rest a buy limit: qty 1 @ 90
        c.modify(&limit_req("o1", 1, 1.0, 90.0), Some(2.0), Some(95.0)); // re-quote: qty 2 @ 95

        // the modify surfaces as a canonical OrderModified
        assert!(
            std::iter::from_fn(|| c.poll_events()).any(|e| matches!(e, Event::OrderModified(_))),
            "modify emits OrderModified"
        );
        // and the resting order is updated IN PLACE (not canceled + re-created)
        let (_, order) = c
            .pending
            .iter()
            .find(|(coid, _)| coid == "o1")
            .expect("order still resting after modify");
        assert_eq!(order.size, 2.0, "resting qty modified");
        assert_eq!(order.price, Some(95.0), "resting price modified");
    }

    #[test]
    fn modify_of_unknown_or_filled_order_is_a_noop() {
        let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        c.modify(&limit_req("ghost", 1, 1.0, 90.0), Some(2.0), Some(95.0)); // never submitted
        assert!(c.poll_events().is_none(), "modifying an unknown order emits nothing");
    }
}

#[cfg(test)]
mod fee_schedule_tests {
    use super::*;

    fn market_req(coid: &str, side: i32, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty,
            order_type: "market".into(),
            ..Default::default()
        }
    }

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// The `with_fee_schedule` path applies `FeeSchedule::commission` (taker for a market fill).
    #[test]
    fn schedule_path_charges_taker_bps_on_a_market_fill() {
        let sched = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
        let mut c = PaperExecutionClient::with_fee_schedule("binance", "BTCUSDT", 0.0, sched);
        c.submit(&market_req("m1", 1, 2.0));
        c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0)); // market fills at next open = 100
        let fills = c.fills.lock().unwrap();
        assert_eq!(fills.len(), 1);
        // taker 10 bps of 2 * 100 = 0.2
        assert_eq!(fills[0].fee, sched.commission(false, 2.0, 100.0));
        assert_eq!(fills[0].fee, 2.0 * 100.0 * (10.0 / 10_000.0));
    }

    /// The schedule path handles [`FeeSchedule::ProbabilityScaled`] (pm-economics lane): a taker
    /// market fill at probability price p is charged `qty × taker_rate × p × (1−p)`.
    #[test]
    fn schedule_path_charges_probability_curve_on_a_market_fill() {
        let sched = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.0,
            maker_rebate_share: 0.0,
        };
        let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
        c.submit(&market_req("p1", 1, 100.0));
        c.on_bar(&bar(1, 0.5, 0.55, 0.45, 0.5)); // market fills at next open = 0.5 (curve peak)
        let fills = c.fills.lock().unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fee, sched.commission(false, 100.0, 0.5));
        assert_eq!(fills[0].fee, 100.0 * 0.02 * (0.5 * (1.0 - 0.5))); // = 0.5
    }

    /// A resting-limit (maker) fill under a rebate-bearing [`FeeSchedule::ProbabilityScaled`]
    /// carries a NEGATIVE commission — the maker rebate flows through the paper fill path.
    #[test]
    fn schedule_path_pays_probability_curve_maker_rebate() {
        let sched = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.0,
            maker_rebate_share: 0.25,
        };
        let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
        // rest a buy limit at 0.5; the bar opens above and trades through it → maker fill @ 0.5
        let req = OrderRequest {
            client_order_id: "m1".into(),
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            side: 1,
            qty: 100.0,
            order_type: "limit".into(),
            price: Some(0.5),
            ..Default::default()
        };
        c.submit(&req);
        c.on_bar(&bar(1, 0.6, 0.65, 0.4, 0.45));
        let fills = c.fills.lock().unwrap();
        assert_eq!(fills.len(), 1);
        assert!(fills[0].is_maker, "a resting limit fill is maker");
        assert_eq!(fills[0].fee, sched.commission(true, 100.0, 0.5));
        assert!(fills[0].fee < 0.0, "maker rebate = negative commission, got {}", fills[0].fee);
        assert_eq!(fills[0].fee, -(0.25 * (100.0 * 0.02 * (0.5 * (1.0 - 0.5)))));
        // = −0.125
    }

    /// End-to-end through the client (not just `commission()` in isolation): SLIPPAGE on a
    /// near-certain fill pushes the traded price OUT of the `[0,1]` probability domain, and the
    /// curve's clamp still yields a well-defined, non-negative fee — a `p > 1` would otherwise make
    /// `p·(1−p)` negative and turn a taker fee into a phantom rebate.
    ///
    /// Only the upper bound is reachable this way: adverse slippage on a BUY multiplies the price
    /// UP (`raw × (1 + slippage)`), while the sell side multiplies DOWN and cannot cross zero for
    /// any `slippage < 1`. The `p < 0` half of the clamp is covered on `commission()` directly.
    #[test]
    fn schedule_path_clamps_a_slippage_pushed_price_into_the_probability_domain() {
        let sched = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.0,
            maker_rebate_share: 0.25,
        };
        // 5% adverse slippage on a buy at 0.99 → 1.0395, outside the probability domain
        let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.05, sched);
        c.submit(&market_req("clamp", 1, 100.0));
        c.on_bar(&bar(1, 0.99, 0.99, 0.99, 0.99));
        let fills = c.fills.lock().unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].px, 0.99 * (1.0 + 0.05));
        assert!(fills[0].px > 1.0, "the fill price really did leave [0,1]: {}", fills[0].px);
        // clamped to p = 1 → curve = 0 → no fee, and crucially NOT a negative (phantom-rebate) one
        assert_eq!(fills[0].fee, sched.commission(false, 100.0, fills[0].px));
        assert_eq!(fills[0].fee, 0.0);
    }

    /// The `new` (flat-rate) path is unchanged — byte-identical to `broker_sim::fee` (r7 gate).
    #[test]
    fn new_path_is_unchanged_flat_rate() {
        let mut c = PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0002, 0.0007);
        c.submit(&market_req("m2", 1, 2.0));
        c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
        let fills = c.fills.lock().unwrap();
        // taker flat 0.0007 of 2 * 100 = 0.14
        assert_eq!(fills[0].fee, vike_fills::broker_sim::fee(2.0, 100.0, 0.0007, 1.0));
    }

    fn deribit_req(coid: &str, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "deribit".into(),
            symbol: "BTC-25JUL-60000-C".into(),
            side: 1,
            qty,
            order_type: "market".into(),
            ..Default::default()
        }
    }

    /// Fee model follow-up 2 — the deliberate behavior change. A Deribit-options
    /// `PercentOfUnderlying` fill degrades to 0.03%-of-PREMIUM at the paper site WITHOUT an
    /// underlying source, but books the accurate 0.03%-of-UNDERLYING (premium-cap-bounded) WHEN one
    /// is supplied. With premium 3000 / underlying 60_000 the accurate fee is exactly 20x the
    /// premium-only approximation — the ~20x understatement this change fixes.
    ///
    /// FAIL-BEFORE / PASS-AFTER: before `with_underlying_source` existed the `deribit` book had no
    /// way to reach `commission_with_underlying` from the fill site, so this 20x assertion could not
    /// hold; the seam is what makes it reachable.
    #[test]
    fn deribit_option_uses_underlying_fee_when_source_supplied() {
        let sched = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };

        // (a) NO underlying source: today's premium-only approximation, byte-identical to before.
        let mut premium_only =
            PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, sched);
        premium_only.submit(&deribit_req("o1", 1.0));
        premium_only.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0)); // fills at open = premium 3000
        let premium_fee = premium_only.fills.lock().unwrap()[0].fee;
        // 3 bps of the PREMIUM notional: 1 * 3000 * 0.0003 = 0.9
        assert_eq!(premium_fee, 1.0 * 3000.0 * (3.0 / 10_000.0));
        assert_eq!(premium_fee, sched.commission(false, 1.0, 3000.0));

        // (b) WITH an underlying source pricing the 60_000 index: the accurate underlying fee.
        let src: UnderlyingSource = Arc::new(|venue: &str, symbol: &str, _ts: i64| {
            assert_eq!(venue, "deribit");
            assert_eq!(symbol, "BTC-25JUL-60000-C");
            Some(60_000.0)
        });
        let mut with_underlying =
            PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, sched)
                .with_underlying_source(src);
        with_underlying.submit(&deribit_req("o2", 1.0));
        with_underlying.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
        let underlying_fee = with_underlying.fills.lock().unwrap()[0].fee;
        // 3 bps of the UNDERLYING notional (< 12.5% of premium, so the cap does not bind):
        // 1 * 60_000 * 0.0003 = 18.0
        assert_eq!(underlying_fee, 1.0 * 60_000.0 * (3.0 / 10_000.0));
        assert_eq!(underlying_fee, sched.commission_with_underlying(1.0, 3000.0, 60_000.0));
        // the accurate fee is exactly 20x the premium-only approximation (the fixed understatement)
        assert!(
            (underlying_fee / premium_fee - 20.0).abs() < 1e-9,
            "underlying fee {underlying_fee} should be ~20x premium-only fee {premium_fee}"
        );
    }

    /// The seam is inert unless BOTH conditions hold. A wired source that returns `None` (no index
    /// price for this instrument at this ts), and a NON-`PercentOfUnderlying` schedule even with a
    /// source, both keep the previous commission bit-for-bit — the byte-identical fallback contract.
    #[test]
    fn underlying_source_is_inert_without_a_price_or_on_other_shapes() {
        // (a) source returns None -> premium-only PercentOfUnderlying number, unchanged.
        let deribit = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
        let none_src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| None);
        let mut c =
            PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, deribit)
                .with_underlying_source(none_src);
        c.submit(&deribit_req("o3", 1.0));
        c.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
        assert_eq!(c.fills.lock().unwrap()[0].fee, deribit.commission(false, 1.0, 3000.0));

        // (b) a non-underlying schedule ignores the source entirely (delegates on the premium).
        let crypto = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
        let src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| Some(60_000.0));
        let mut c2 = PaperExecutionClient::with_fee_schedule("binance", "BTCUSDT", 0.0, crypto)
            .with_underlying_source(src);
        c2.submit(&market_req("o4", 1, 2.0));
        c2.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
        assert_eq!(c2.fills.lock().unwrap()[0].fee, crypto.commission(false, 2.0, 100.0));
    }
}

#[cfg(test)]
mod tif_expiry_tests {
    use super::*;
    // The day helpers now live in the model (dedup A4) — the local aliases were removed.
    use vike_model::{MS_PER_DAY, utc_day};

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// A resting buy limit at 90 that only fills if the bar trades down to it.
    fn limit(coid: &str, tif: TimeInForce, gtd_expiry: Option<i64>, ts: i64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(90.0),
            time_in_force: tif,
            gtd_expiry,
            ts,
            ..Default::default()
        }
    }

    fn client() -> PaperExecutionClient {
        PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0)
    }

    /// Drain every queued event (helpers below project out of the drained list, so a test can
    /// inspect expiries AND cancels from one drain).
    fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
        std::iter::from_fn(|| c.poll_events()).collect()
    }

    /// (coid, ts) of every `OrderExpired` — the model's purpose-built expiry variant, which is
    /// what a venue emits and what the OMS FSM terminalizes as `OrderStatus::Expired`.
    fn expired_in(events: &[Event]) -> Vec<(String, i64)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::OrderExpired(x) => Some((x.client_order_id.clone(), x.ts)),
                _ => None,
            })
            .collect()
    }

    /// (coid, reason) of every `OrderCanceled` — expiry itself must NOT use this vocabulary.
    fn canceled_in(events: &[Event]) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::OrderCanceled(x) => Some((x.client_order_id.clone(), x.reason.to_string())),
                _ => None,
            })
            .collect()
    }

    fn expired(c: &mut PaperExecutionClient) -> Vec<(String, i64)> {
        expired_in(&drain(c))
    }

    /// GTD expires on the first bar whose OPEN ts is at or past its deadline — not on the bar
    /// before it — and that bar's fill pass no longer sees the order.
    #[test]
    fn gtd_expires_on_the_first_bar_at_or_past_its_deadline() {
        let mut c = client();
        c.submit(&limit("g1", TimeInForce::Gtd, Some(2_000), 0));

        // a bar strictly BEFORE the deadline: still resting, nothing expired.
        c.on_bar(&bar(1_999, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "not yet expired one ms before the deadline");
        assert_eq!(c.pending.len(), 1, "still resting");

        // the bar AT the deadline expires it, stamped with that ts.
        c.on_bar(&bar(2_000, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c), vec![("g1".to_string(), 2_000)]);
        assert!(c.pending.is_empty(), "expired order left the resting book");
    }

    /// Expiry fires exactly ONCE — later bars re-emit nothing.
    #[test]
    fn gtd_expires_only_once() {
        let mut c = client();
        c.submit(&limit("g2", TimeInForce::Gtd, Some(1_000), 0));
        c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c).len(), 1, "expires on the deadline bar");
        for ts in [2_000, 3_000, 4_000] {
            c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
        }
        assert!(expired(&mut c).is_empty(), "never expires a second time");
        assert!(c.expiry.is_empty(), "the deadline entry is dropped with the order");
    }

    /// The expiry sweep runs BEFORE the fill pass: an order whose deadline this bar carries it
    /// past cannot fill on that bar, even though the bar's range would trigger it.
    #[test]
    fn an_expiring_order_does_not_fill_on_the_bar_that_expires_it() {
        let mut c = client();
        c.submit(&limit("g3", TimeInForce::Gtd, Some(2_000), 0));
        // low 85 trades THROUGH the buy limit at 90 — it would fill if it were still alive.
        c.on_bar(&bar(2_000, 100.0, 101.0, 85.0, 95.0));
        assert!(c.fills.lock().unwrap().is_empty(), "expired order does not fill");
        assert_eq!(expired(&mut c).len(), 1, "it is terminalized as expired instead");
    }

    /// Expiry cancels resting orders WITHOUT touching fills or position: an order that fills
    /// before its deadline is untouched by the sweep.
    #[test]
    fn a_fill_before_the_deadline_is_unaffected() {
        let mut c = client();
        c.submit(&limit("g4", TimeInForce::Gtd, Some(9_000), 0));
        c.on_bar(&bar(1_000, 100.0, 101.0, 85.0, 95.0)); // trades through 90 → fills
        assert_eq!(c.fills.lock().unwrap().len(), 1, "fills normally before the deadline");
        assert_eq!(c.position, 1.0);
        c.on_bar(&bar(9_000, 100.0, 101.0, 95.0, 100.0)); // past the deadline
        assert!(expired(&mut c).is_empty(), "a filled order never expires afterwards");
        assert_eq!(c.fills.lock().unwrap().len(), 1, "no extra fill");
        assert_eq!(c.position, 1.0, "expiry never moves position");
    }

    /// A `Gtd` with no `gtd_expiry` has no deadline and rests like GTC.
    #[test]
    fn gtd_without_a_deadline_never_expires() {
        let mut c = client();
        c.submit(&limit("g5", TimeInForce::Gtd, None, 0));
        c.on_bar(&bar(i64::MAX / 2, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty());
        assert_eq!(c.pending.len(), 1);
    }

    /// Day expires at the UTC-day boundary of its ANCHOR BAR: it survives every bar on that day
    /// and dies on the first bar of the next one.
    #[test]
    fn day_expires_at_the_utc_day_boundary() {
        let mut c = client();
        // submitted mid-day on UTC day 0 (the request ts is NOT the anchor — the first bar is)
        c.submit(&limit("d1", TimeInForce::Day, None, 12 * 3_600_000));
        // last ms of UTC day 0 — this bar anchors the session, so it is still alive
        c.on_bar(&bar(MS_PER_DAY - 1, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "alive through the end of its own UTC day");
        assert_eq!(c.pending.len(), 1);
        // first ms of the next UTC day: expired
        c.on_bar(&bar(MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c), vec![("d1".to_string(), MS_PER_DAY)]);
        assert!(c.pending.is_empty());
    }

    /// GTC — the default on every existing order — never expires, and records no state at all,
    /// which is what keeps the r7 gate and every existing caller byte-identical.
    #[test]
    fn gtc_never_expires_and_records_nothing() {
        let mut c = client();
        c.submit(&limit("k1", TimeInForce::Gtc, Some(1), 0)); // deadline ignored for GTC
        assert!(c.expiry.is_empty(), "GTC records no expiry state");
        for ts in [1, 1_000, 10 * MS_PER_DAY] {
            c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
        }
        assert!(expired(&mut c).is_empty(), "GTC never expires");
        assert_eq!(c.pending.len(), 1, "still resting after ten days");
    }

    /// A FULLY default-constructed request (the `..Default::default()` every in-tree producer
    /// builds) is GTC, so it records nothing AND its event stream carries no expiry/cancel at all
    /// — the byte-identity property the r7 gate depends on, asserted on the stream rather than on
    /// the guard's own spelling.
    #[test]
    fn a_default_request_is_inert() {
        let mut c = client();
        let req = OrderRequest { client_order_id: "z1".into(), ..Default::default() };
        assert!(
            matches!(req.time_in_force, TimeInForce::Gtc),
            "the model default must stay Gtc — an expiring default would move the parity fixtures"
        );
        c.submit(&req);
        assert!(c.expiry.is_empty(), "a default request records no deadline");
        for ts in [0, MS_PER_DAY, 10 * MS_PER_DAY] {
            c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
        }
        let events = drain(&mut c);
        assert!(expired_in(&events).is_empty(), "no expiry on the default path");
        assert!(canceled_in(&events).is_empty(), "no cancel on the default path");
    }

    /// IOC/FOK are immediate-execution semantics, not a resting deadline — this sweep leaves them
    /// alone (documented on `Expiry`), so they cannot silently start disappearing.
    #[test]
    fn ioc_and_fok_are_untouched_by_the_deadline_sweep() {
        for tif in [TimeInForce::Ioc, TimeInForce::Fok] {
            let mut c = client();
            c.submit(&limit("i1", tif, Some(1), 0));
            assert!(c.expiry.is_empty(), "{tif:?} records no deadline");
            c.on_bar(&bar(10 * MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
            assert!(expired(&mut c).is_empty(), "{tif:?} is not expired by this check");
        }
    }

    /// Cancel-then-expire cannot double-emit: a user cancel drops the deadline with the order.
    #[test]
    fn a_user_cancel_removes_the_deadline() {
        let mut c = client();
        c.submit(&limit("c1", TimeInForce::Gtd, Some(1_000), 0));
        c.cancel("c1");
        assert!(c.expiry.is_empty(), "cancel drops the deadline entry");
        c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "an already-canceled order never expires");
    }

    /// Expiry uses the model's `OrderExpired` variant, NOT `OrderCanceled{reason:"expired"}` —
    /// the distinction a strategy reacting to `OrderEventKind::Expired` depends on, and the reason
    /// a paper trade log and a live trade log agree on status for the identical order.
    #[test]
    fn expiry_emits_order_expired_not_a_cancel() {
        let mut c = client();
        c.submit(&limit("v1", TimeInForce::Gtd, Some(1_000), 0));
        c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
        let events = drain(&mut c);
        assert_eq!(expired_in(&events), vec![("v1".to_string(), 1_000)]);
        assert!(canceled_in(&events).is_empty(), "expiry must not use the cancel vocabulary");
    }

    /// RESOLUTION CAVEAT, pinned deliberately: `Bar.ts` is the bar's OPEN, so a deadline lying
    /// strictly INSIDE a bar interval is only noticed at the NEXT open — the order survives, and
    /// can fill, for up to one interval past its deadline. This is inherent to bar-resolution
    /// simulation; the test exists so the overshoot cannot silently change.
    #[test]
    fn a_deadline_inside_a_bar_overshoots_to_the_next() {
        let mut c = client();
        // 1m bars; deadline 30s into the 12:00:00 bar.
        let open = 12 * 3_600_000;
        c.submit(&limit("o1", TimeInForce::Gtd, Some(open + 30_000), 0));
        // the bar that CONTAINS the deadline: its open is still before it, so the order lives —
        // and the bar's full range fills it, 30s "after" it should have been dead.
        c.on_bar(&bar(open, 100.0, 101.0, 85.0, 95.0));
        assert!(expired(&mut c).is_empty(), "an interior deadline is not seen at the bar open");
        assert_eq!(c.fills.lock().unwrap().len(), 1, "it fills off the containing bar's range");

        // and with no fill available, expiry lands on the next open instead.
        let mut c2 = client();
        c2.submit(&limit("o2", TimeInForce::Gtd, Some(open + 30_000), 0));
        c2.on_bar(&bar(open, 100.0, 101.0, 95.0, 100.0)); // never trades down to 90
        assert!(expired(&mut c2).is_empty(), "still resting through the containing bar");
        c2.on_bar(&bar(open + 60_000, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c2), vec![("o2".to_string(), open + 60_000)], "expires next open");
    }

    /// A `Day` order whose request ts was never stamped (`OrderRequest::ts` defaults to 0, and
    /// nothing in the write path writes it) must NOT be born expired: the session anchors on the
    /// first BAR, so it lives out that bar's UTC day.
    #[test]
    fn an_unstamped_day_order_is_not_born_expired() {
        let mut c = client();
        c.submit(&limit("u1", TimeInForce::Day, None, 0)); // ts = 0 ⇒ utc_day 0 (1970)
        let today = 20_650 * MS_PER_DAY; // a plausible modern bar clock
        c.on_bar(&bar(today, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "an unstamped Day order survives its first bar");
        assert_eq!(c.pending.len(), 1);
        c.on_bar(&bar(today + MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c).len(), 1, "and expires on the next UTC day as normal");
    }

    /// Same guarantee under submitter-vs-bar CLOCK SKEW (the classic seconds-vs-ms mixup): the
    /// deadline never depends on the submitter's clock, only on the bar clock.
    #[test]
    fn a_day_order_is_immune_to_submitter_clock_skew() {
        let mut c = client();
        let today = 20_650 * MS_PER_DAY;
        c.submit(&limit("u2", TimeInForce::Day, None, today / 1_000)); // ts in SECONDS
        c.on_bar(&bar(today, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "a skewed submit ts cannot kill the order early");
        c.on_bar(&bar(today + MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
        assert_eq!(expired(&mut c).len(), 1);
    }

    /// An expiring OTO PARENT cascade-cancels its held children: they can only ever arm on the
    /// parent's FILL, so leaving them resting-but-inactive would strand them live for the rest of
    /// the run.
    #[test]
    fn an_expiring_bracket_parent_cancels_its_held_children() {
        let mut c = client();
        let mut entry = limit("e1", TimeInForce::Gtd, Some(1_000), 0);
        entry.contingency_type = Some("OTO".into());
        c.submit(&entry);
        for child in ["sl", "tp"] {
            let mut leg = limit(child, TimeInForce::Gtc, None, 0);
            leg.side = -1;
            leg.parent_order_id = Some("e1".to_string());
            leg.linked_order_ids = vec!["sl".to_string(), "tp".to_string()];
            c.submit(&leg);
        }
        assert_eq!(c.pending.len(), 3, "entry + two held exits resting");

        c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
        let events = drain(&mut c);
        assert_eq!(expired_in(&events), vec![("e1".to_string(), 1_000)], "the parent expires");
        let mut cancels = canceled_in(&events);
        cancels.sort();
        assert_eq!(
            cancels,
            vec![
                ("sl".to_string(), "parent-expired".to_string()),
                ("tp".to_string(), "parent-expired".to_string()),
            ],
            "both held children are cascade-canceled, distinctly reasoned"
        );
        assert!(c.pending.is_empty(), "no orphaned leg left resting");
        assert!(c.contingency.is_empty(), "and no orphaned linkage left behind");
    }

    /// Per-book expiry under `MultiPaperExecutionClient`: a symbol-less bar advances EVERY book
    /// (so an order expires off the shared clock), while a symbol-tagged bar advances only its own
    /// book — expiry is driven by the bar stream the book actually receives.
    #[test]
    fn multi_book_expiry_follows_each_books_own_bar_stream() {
        let mut multi = MultiPaperExecutionClient::new();
        multi.add_book(PaperExecutionClient::new("sim", "AAA", 0.0, 0.0, 0.0));
        multi.add_book(PaperExecutionClient::new("sim", "BBB", 0.0, 0.0, 0.0));
        let mut a = limit("a1", TimeInForce::Gtd, Some(1_000), 0);
        a.symbol = "AAA".into();
        multi.submit(&a);

        // a bar for the OTHER book only: book AAA never advances, so nothing expires.
        let mut b_bar = bar(1_000, 100.0, 101.0, 95.0, 100.0);
        b_bar.symbol = Some("BBB".to_string());
        multi.on_bar(&b_bar);
        assert_eq!(multi.book("AAA").expect("book AAA").pending.len(), 1, "AAA untouched");

        // a symbol-less bar fans out to every book, expiring the AAA order.
        multi.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
        assert!(multi.book("AAA").expect("book AAA").pending.is_empty(), "expired off the fanout");
        let expiries: Vec<String> = std::iter::from_fn(|| multi.poll_events())
            .filter_map(|e| match e {
                Event::OrderExpired(x) => Some(x.client_order_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            expiries,
            vec!["a1".to_string()],
            "and surfaces through the router exactly once"
        );
    }

    #[test]
    fn utc_day_floors_across_the_epoch() {
        assert_eq!(utc_day(0), 0);
        assert_eq!(utc_day(MS_PER_DAY - 1), 0);
        assert_eq!(utc_day(MS_PER_DAY), 1);
        assert_eq!(utc_day(-1), -1, "pre-epoch floors down, so days stay monotone");
    }
}

#[cfg(test)]
mod bracket_tests {
    use super::*;
    use vike_model::{BracketSpec, build_bracket};

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn filled(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                Event::OrderFilled(f) => Some(f.client_order_id.clone()),
                _ => None,
            })
            .collect()
    }
    fn canceled(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                Event::OrderCanceled(x) => Some(x.client_order_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bracket_oto_arms_entry_then_oco_cancels_sibling() {
        let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        // long bracket: market entry, SL 90, TP 110
        let spec = BracketSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 90.0,
            take_profit: 110.0,
        };
        for leg in build_bracket(&spec, "entry", "sl", "tp") {
            c.submit(&leg);
        }
        // bar 1: entry (market) fills at open; the exits are HELD (armed by this fill, not filled).
        c.on_bar(&bar(1, 100.0, 105.0, 95.0, 100.0));
        // bar 2: rally to 112 → TP (limit sell 110) fills; SL must be OCO-canceled.
        c.on_bar(&bar(2, 105.0, 112.0, 104.0, 110.0));

        let evs: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
        let f = filled(&evs);
        assert!(f.contains(&"entry".to_string()), "entry fills first");
        assert!(f.contains(&"tp".to_string()), "TP fills on the rally");
        assert!(!f.contains(&"sl".to_string()), "SL never fills");
        assert!(canceled(&evs).contains(&"sl".to_string()), "SL is OCO-canceled when TP fills");
    }

    #[test]
    fn held_exit_does_not_fill_before_entry() {
        // a limit entry that never fills → the exits stay held even where price would trigger them.
        let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        let spec = BracketSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(50.0), // far below the bar → entry never fills
            stop_loss: 90.0,
            take_profit: 110.0,
        };
        for leg in build_bracket(&spec, "e2", "sl2", "tp2") {
            c.submit(&leg);
        }
        // range 95..112 would trigger TP(110) if active, but the entry (limit 50) never fills.
        c.on_bar(&bar(1, 100.0, 112.0, 95.0, 108.0));
        let evs: Vec<Event> = std::iter::from_fn(|| c.poll_events()).collect();
        assert!(filled(&evs).is_empty(), "no leg fills while the entry is unfilled");
    }
}

#[cfg(test)]
mod combo_tests {
    use super::*;
    use vike_model::{ComboSpec, build_combo};

    /// A bar carrying an explicit bid/ask for `symbol` — combos price legs off the quote side they
    /// would actually trade, so the spread has to be expressible.
    fn quoted(ts: i64, symbol: &str, bid: f64, ask: f64) -> Bar {
        Bar {
            ts,
            open: bid,
            high: ask,
            low: bid,
            close: (bid + ask) / 2.0,
            volume: 0.0,
            funding: None,
            bid: Some(bid),
            ask: Some(ask),
            symbol: Some(symbol.to_string()),
        }
    }

    fn client() -> PaperExecutionClient {
        PaperExecutionClient::new("deribit", "COMBO", 0.0, 0.0, 0.0)
    }

    /// A two-leg vertical: buy `NEAR`, sell `FAR`, one unit.
    fn spread(side: i32, net_limit: Option<f64>) -> ComboSpec {
        ComboSpec {
            venue: "deribit".into(),
            side,
            qty: 1.0,
            legs: vec![
                ComboLeg { symbol: "NEAR".into(), ratio: 1 },
                ComboLeg { symbol: "FAR".into(), ratio: -1 },
            ],
            net_limit,
            time_in_force: TimeInForce::Gtc,
        }
    }

    fn submit_combo(c: &mut PaperExecutionClient, coid: &str, spec: &ComboSpec) {
        c.submit(&build_combo(spec, coid).expect("valid combo spec"));
    }

    fn fills_of(c: &PaperExecutionClient) -> Vec<PaperFill> {
        c.fills.lock().unwrap().clone()
    }

    fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
        std::iter::from_fn(|| c.poll_events()).collect()
    }

    /// A DEBIT spread (net > 0): buying it fills once the achievable net reaches the limit, and
    /// each leg fills at its OWN price — the net is only the trigger.
    #[test]
    fn buying_a_debit_spread_fills_every_leg_at_its_own_price() {
        let mut c = client();
        // buy NEAR at its ask, sell FAR at its bid => net = 100 - 94 = 6, limit 6 => crosses.
        submit_combo(&mut c, "cb", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        assert!(fills_of(&c).is_empty(), "one leg marked is not enough to fill anything");
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));

        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2, "both legs filled");
        assert_eq!((fills[0].side, fills[0].qty, fills[0].px), (1, 1.0, 100.0), "buy NEAR at ask");
        assert_eq!((fills[1].side, fills[1].qty, fills[1].px), (-1, 1.0, 94.0), "sell FAR at bid");
        // and crucially NOT one blended fill at the net
        assert!(fills.iter().all(|f| f.px != 6.0), "no synthetic fill at the net price");
    }

    /// The defining property: a combo fills ALL legs or NONE. One tick short of the limit leaves
    /// the whole structure resting — not one leg done and one working.
    #[test]
    fn a_combo_short_of_its_limit_fills_no_leg_at_all() {
        let mut c = client();
        submit_combo(&mut c, "cb", &spread(1, Some(5.0))); // needs net <= 5
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0)); // net = 6 > 5
        assert!(fills_of(&c).is_empty(), "net has not crossed => zero legs fill");
        assert_eq!(c.combos.len(), 1, "the whole combo rests on");

        // FAR bid rises to 95 => net = 5, exactly at the limit. BOTH legs print at ts=2 — under
        // the default freshness bound (0 = same-ts) a lone FAR print could not trigger against
        // NEAR's ts=1 mark (see `a_leg_that_stops_printing_stops_the_combo`).
        c.on_bar(&quoted(2, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(2, "FAR", 95.0, 96.0));
        assert_eq!(fills_of(&c).len(), 2, "all legs fill together, on the same bar");
        assert!(c.combos.is_empty(), "and the combo leaves the book");
    }

    /// A CREDIT spread's net is NEGATIVE. Buying one must work with a negative limit — nothing on
    /// this path may clamp, abs() or sign-assume.
    #[test]
    fn buying_a_credit_spread_works_with_a_negative_net() {
        let mut c = client();
        // buy NEAR@10 / sell FAR@25 => net = 10 - 25 = -15 (a credit). Limit -15 => crosses.
        submit_combo(&mut c, "cr", &spread(1, Some(-15.0)));
        c.on_bar(&quoted(1, "NEAR", 9.0, 10.0));
        c.on_bar(&quoted(1, "FAR", 25.0, 26.0));
        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2, "a negative net limit fills exactly like a positive one");
        assert_eq!(fills[0].px, 10.0);
        assert_eq!(fills[1].px, 25.0);
    }

    /// A credit combo that is not yet rich enough does NOT fill: proof the negative-side comparison
    /// is a real ordered test, not "negative => always crosses".
    #[test]
    fn a_credit_combo_still_respects_its_limit() {
        let mut c = client();
        submit_combo(&mut c, "cr", &spread(1, Some(-20.0))); // wants net <= -20
        c.on_bar(&quoted(1, "NEAR", 9.0, 10.0));
        c.on_bar(&quoted(1, "FAR", 25.0, 26.0)); // net = -15, which is > -20
        assert!(fills_of(&c).is_empty(), "-15 does not cross a -20 buy limit");
    }

    /// SELLING a debit spread: the sell side flips every leg AND flips the crossing direction
    /// (`net >= limit`), and each leg trades the opposite side of the book.
    #[test]
    fn selling_a_debit_spread_flips_every_leg_and_the_crossing_test() {
        let mut c = client();
        // sell the spread: sell NEAR at its BID (99), buy FAR at its ASK (95) => net = 99 - 95 = 4.
        submit_combo(&mut c, "sd", &spread(-1, Some(4.0))); // sell fills when net >= 4
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2);
        assert_eq!((fills[0].side, fills[0].px), (-1, 99.0), "the +1 leg SELLS at the bid");
        assert_eq!((fills[1].side, fills[1].px), (1, 95.0), "the -1 leg BUYS at the ask");
    }

    /// A combo is ONE order downstream: one coid, exactly one terminal event, with the earlier legs
    /// wrapped as partials — the `ManagedOrder` FSM law.
    #[test]
    fn a_combo_emits_one_terminal_event_for_its_single_coid() {
        let mut c = client();
        submit_combo(&mut c, "one", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        let evs = drain(&mut c);

        let bare: Vec<&FillEvent> = evs
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(bare.len(), 2, "one bare Fill per leg (Account folds these)");
        assert_eq!(bare[0].symbol.as_str(), "NEAR", "fills carry the LEG symbol");
        assert_eq!(bare[1].symbol.as_str(), "FAR");
        assert!(bare.iter().all(|f| f.client_order_id == "one"), "all under the one combo coid");

        let partials = evs.iter().filter(|e| matches!(e, Event::OrderPartiallyFilled(_))).count();
        let terminal = evs.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count();
        assert_eq!(partials, 1, "legs 0..n-1 wrap as partials");
        assert_eq!(terminal, 1, "exactly ONE terminal event for the combo");
    }

    /// Leg RATIOS scale each leg's quantity (`|ratio| x units`) and weight its price in the net.
    #[test]
    fn ratios_scale_leg_quantities_and_weight_the_net() {
        let mut c = client();
        // a 1x2 ratio spread, 3 units: buy 1xNEAR, sell 2xFAR => net = 100 - 2*94 = -88.
        let spec = ComboSpec {
            venue: "deribit".into(),
            side: 1,
            qty: 3.0,
            legs: vec![
                ComboLeg { symbol: "NEAR".into(), ratio: 1 },
                ComboLeg { symbol: "FAR".into(), ratio: -2 },
            ],
            net_limit: Some(-88.0),
            time_in_force: TimeInForce::Gtc,
        };
        submit_combo(&mut c, "rr", &spec);
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2);
        assert_eq!((fills[0].qty, fills[0].side), (3.0, 1), "|1| x 3 units");
        assert_eq!((fills[1].qty, fills[1].side), (6.0, -1), "|-2| x 3 units");
    }

    /// A combo MARKET (no net limit) has no crossing trigger — it fills as soon as every leg
    /// carries a FRESH mark (the freshness gate applies to market combos too; see
    /// `a_filled_combos_marks_never_resurrect_for_a_later_combo` for the stale side).
    #[test]
    fn a_market_combo_fills_once_every_leg_is_marked() {
        let mut c = client();
        submit_combo(&mut c, "mk", &spread(1, None));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        assert!(fills_of(&c).is_empty(), "still missing a leg mark");
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        assert_eq!(fills_of(&c).len(), 2, "no crossing test — fills on the mark");
        assert!(fills_of(&c).iter().all(|f| !f.is_maker), "a market combo books taker");
    }

    /// Slippage is applied per leg, adversely to that LEG's own side (the buy leg pays up, the sell
    /// leg receives less) — not to the net. And the net LIMIT binds PRE-slippage: the trigger
    /// compares raw quotes (raw net 6 == limit 6 fills), so the EXECUTED net lands through the
    /// limit by slippage × gross — the documented convention (see `fill_combos`), identical to the
    /// single-symbol path (`BarFillModel` triggers on raw bar prices, `emit_fill` slips after) and
    /// to LEAN's `ComboLimitFill`. This test PINS that convention: if slippage ever moves into the
    /// trigger, the fill here disappears and this fails — make that change deliberately.
    #[test]
    fn slippage_is_adverse_per_leg_side() {
        let mut c = PaperExecutionClient::new("deribit", "COMBO", 0.01, 0.0, 0.0);
        submit_combo(&mut c, "sl", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2, "raw net 6 == limit => fills, slippage NOT in the trigger");
        assert_eq!(fills[0].px, 100.0 * 1.01, "the BUY leg slips up");
        assert_eq!(fills[1].px, 94.0 * 0.99, "the SELL leg slips down");
        // executed net = 101 - 93.06 = 7.94: through the 6 limit by slippage × gross (1.94), the
        // pre-slippage-binding law made visible.
        let executed_net = fills[0].px - fills[1].px;
        assert!((executed_net - 7.94).abs() < 1e-9, "executed net {executed_net} != 7.94");
        assert!(executed_net > 6.0, "the executed net is through the pre-slippage limit");
    }

    /// A resting combo cancels as ONE order — all legs at once, one cancel event.
    #[test]
    fn cancel_removes_the_whole_combo() {
        let mut c = client();
        submit_combo(&mut c, "cx", &spread(1, Some(6.0)));
        c.cancel("cx");
        assert!(c.combos.is_empty(), "the whole combo left the book");
        let cancels = drain(&mut c).iter().filter(|e| matches!(e, Event::OrderCanceled(_))).count();
        assert_eq!(cancels, 1, "one order, one cancel");
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        assert!(fills_of(&c).is_empty(), "a canceled combo never fills");
    }

    /// A resting combo honours an expiring TIF, and expires as one order (no leg survives).
    #[test]
    fn a_combo_expires_as_one_order() {
        let mut c = client();
        let mut req = build_combo(&spread(1, Some(6.0)), "ex").expect("valid");
        req.time_in_force = TimeInForce::Gtd;
        req.gtd_expiry = Some(1_000);
        c.submit(&req);
        c.on_bar(&quoted(1_000, "NEAR", 99.0, 100.0));
        let expiries = drain(&mut c).iter().filter(|e| matches!(e, Event::OrderExpired(_))).count();
        assert_eq!(expiries, 1, "the combo expires exactly once");
        assert!(c.combos.is_empty(), "no leg left resting");
    }

    /// OFF-PATH BYTE-IDENTITY: an ordinary (non-combo) order records NO combo state at all, so the
    /// combo path cannot perturb the r7-gated single-symbol semantics.
    #[test]
    fn a_non_combo_order_records_no_combo_state() {
        let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        let req = OrderRequest {
            client_order_id: "plain".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            ..Default::default()
        };
        assert!(req.combo_legs.is_empty(), "the default request is not a combo");
        c.submit(&req);
        assert!(c.combos.is_empty(), "no combo recorded");
        c.on_bar(&Bar {
            ts: 1,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        assert!(c.leg_marks.is_empty(), "no marks recorded on a combo-free run");
        assert_eq!(c.pending.len(), 0, "and the ordinary order filled as before");
        assert_eq!(fills_of(&c).len(), 1);
    }

    /// A bar with no explicit bid/ask falls back to its close for both sides — combos still work
    /// on plain OHLC series.
    #[test]
    fn legs_fall_back_to_close_without_a_quote() {
        let mut c = client();
        submit_combo(&mut c, "fb", &spread(1, Some(6.0)));
        for (sym, close) in [("NEAR", 100.0), ("FAR", 94.0)] {
            c.on_bar(&Bar {
                ts: 1,
                open: close,
                high: close,
                low: close,
                close,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(sym.to_string()),
            });
        }
        let fills = fills_of(&c);
        assert_eq!(fills.len(), 2, "net = 100 - 94 = 6 crosses the 6 limit off closes alone");
        assert_eq!(fills[0].px, 100.0);
        assert_eq!(fills[1].px, 94.0);
    }

    /// `MultiPaperExecutionClient` has no book for a combo (empty symbol, legs across instruments),
    /// so it REJECTS terminally rather than dropping or half-routing it — the dead-path rule.
    #[test]
    fn the_multi_router_rejects_a_combo_terminally() {
        let mut multi = MultiPaperExecutionClient::new();
        multi.add_book(PaperExecutionClient::new("deribit", "NEAR", 0.0, 0.0, 0.0));
        multi.submit(&build_combo(&spread(1, Some(6.0)), "mc").expect("valid"));
        let evs: Vec<Event> = std::iter::from_fn(|| multi.poll_events()).collect();
        assert!(
            evs.iter().any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "mc")),
            "an unroutable combo terminalizes as OrderRejected, never vanishes: {evs:?}"
        );
    }

    // ---- leg-mark freshness discipline (adversarial-review MAJOR fix) ----

    /// RESURRECTION is dead: a filled combo's marks are dropped with it, so a later combo on the
    /// same legs can NEVER fill off prices recorded during the earlier combo's life — no matter
    /// how many bars pass in between and regardless of which symbol's bar triggers the pass
    /// (`fill_combos` runs on every bar). It fills only once EVERY leg has printed fresh.
    #[test]
    fn a_filled_combos_marks_never_resurrect_for_a_later_combo() {
        let mut c = client();
        submit_combo(&mut c, "a", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        assert_eq!(fills_of(&c).len(), 2, "combo A fills");
        assert!(c.leg_marks.is_empty(), "the book emptied => every mark dropped with it");

        // many bars pass with NO combo resting — nothing is recorded (and nothing lingers)
        for ts in [100_000, 200_000, 300_000] {
            c.on_bar(&quoted(ts, "NEAR", 99.0, 100.0));
            c.on_bar(&quoted(ts, "FAR", 94.0, 95.0));
        }
        assert!(c.leg_marks.is_empty(), "no combo resting => no marks accumulate");

        // a combo MARKET on the same legs, ~1000 bars after A's life. The next bar — ANY
        // symbol — must not fill it off A-era (or idle-era) prices.
        submit_combo(&mut c, "b", &spread(1, None));
        c.on_bar(&quoted(1_000_000, "OTHER", 1.0, 2.0));
        assert_eq!(fills_of(&c).len(), 2, "no fill off dead marks");
        c.on_bar(&quoted(1_000_000, "NEAR", 99.0, 100.0));
        assert_eq!(fills_of(&c).len(), 2, "one fresh leg is not enough — FAR has not printed");
        c.on_bar(&quoted(1_000_000, "FAR", 94.0, 95.0));
        assert_eq!(fills_of(&c).len(), 4, "fills exactly when every leg has a FRESH print");
    }

    /// WITHIN-LIFE SKEW is dead: a resting combo whose leg goes dark stops triggering — the other
    /// leg's prints cannot cross against the dark leg's old mark, because `A(t) + B(t−N)` is a net
    /// that never coexisted. Under the default bound (0 = same-ts) one stale pass is already too
    /// many.
    #[test]
    fn a_leg_that_stops_printing_stops_the_combo() {
        let mut c = client();
        submit_combo(&mut c, "sk", &spread(1, Some(5.0))); // needs net <= 5
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0)); // net = 100 - 94 = 6 > 5 — rests
        assert!(fills_of(&c).is_empty());

        // NEAR goes dark; FAR keeps printing at a bid that WOULD cross against NEAR's ts=1 mark
        // (100 - 95 = 5 <= 5). It must never trigger.
        for ts in 2..8 {
            c.on_bar(&quoted(ts, "FAR", 95.0, 96.0));
            assert!(fills_of(&c).is_empty(), "no fill off a dark leg (pass ts {ts})");
        }
        assert_eq!(c.combos.len(), 1, "the whole combo just rests");

        // NEAR prints again — on the pass where both marks share the current ts, it may trigger.
        c.on_bar(&quoted(8, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(8, "FAR", 95.0, 96.0));
        assert_eq!(fills_of(&c).len(), 2, "coexisting fresh legs => the combo fills");
    }

    /// `combo_mark_staleness_ms` mirrors the engine's `max_price_staleness_ms` convention: strict
    /// `age > bound`, so a mark exactly at the bound is still fresh and one past it is not.
    #[test]
    fn a_widened_staleness_bound_admits_recent_marks_and_refuses_older_ones() {
        // bound 10: NEAR marked at ts=1, FAR triggers the pass at ts=11 — age 10 == bound, fresh.
        let mut c = client();
        c.combo_mark_staleness_ms = 10;
        submit_combo(&mut c, "w1", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(11, "FAR", 94.0, 95.0));
        assert_eq!(fills_of(&c).len(), 2, "age == bound is fresh (strict >)");

        // same shape, FAR at ts=12 — NEAR's age 11 > 10, stale => the whole combo rests.
        let mut c = client();
        c.combo_mark_staleness_ms = 10;
        submit_combo(&mut c, "w2", &spread(1, Some(6.0)));
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(12, "FAR", 94.0, 95.0));
        assert!(fills_of(&c).is_empty(), "age > bound is stale — no leg fills");
        assert_eq!(c.combos.len(), 1);
    }

    // ---- contingency links on combos are rejected loudly (adversarial-review fix) ----

    /// A combo carrying ANY contingency link (OTO parent, OCO links, or an explicit contingency
    /// type) is terminally REJECTED at submit: the hold/cascade machinery enforces against the
    /// single-symbol book only, and silently recording unenforced links is the failure mode this
    /// guards. No `OrderAccepted`, nothing rests, nothing ever fills.
    #[test]
    fn a_combo_carrying_contingency_links_is_rejected_terminally() {
        type Mutator = fn(&mut OrderRequest);
        let cases: Vec<(&str, Mutator)> = vec![
            ("parent_order_id", |r| r.parent_order_id = Some("entry".into())),
            ("linked_order_ids", |r| r.linked_order_ids = vec!["sib".into()]),
            ("contingency_type", |r| r.contingency_type = Some("oco".into())),
        ];
        for (field, mutate) in cases {
            let mut c = client();
            let mut req = build_combo(&spread(1, Some(6.0)), "cl").expect("valid");
            mutate(&mut req);
            c.submit(&req);
            let evs = drain(&mut c);
            assert!(
                evs.iter()
                    .any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "cl")),
                "{field}: a linked combo terminalizes as OrderRejected: {evs:?}"
            );
            assert!(
                !evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
                "{field}: never accepted"
            );
            assert!(c.combos.is_empty(), "{field}: nothing rests");
            assert!(c.contingency.is_empty(), "{field}: no unenforceable link is recorded");
            assert!(c.expiry.is_empty(), "{field}: no deadline is recorded");
            c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
            c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
            assert!(fills_of(&c).is_empty(), "{field}: a rejected combo never fills");
        }
    }

    /// The one contingency direction that IS enforced against the combo book: a PLAIN order
    /// OCO-linked to a resting combo cancels the combo — as ONE order, all legs at once — when the
    /// plain order fills. (The combo itself carries no links, so it was legitimately accepted.)
    #[test]
    fn a_plain_oco_fill_cancels_its_resting_combo_sibling() {
        let mut c = client();
        submit_combo(&mut c, "cb", &spread(1, Some(6.0)));
        let plain = OrderRequest {
            client_order_id: "pl".into(),
            venue: "deribit".into(),
            symbol: "COMBO".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            linked_order_ids: vec!["cb".into()],
            ..Default::default()
        };
        c.submit(&plain);
        // the plain market order fills on the first bar (single-symbol pass), which OCO-cancels
        // the combo BEFORE fill_combos runs — even though this same bar marks NEAR.
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        assert!(c.combos.is_empty(), "the combo sibling left the book with all its legs");
        let evs = drain(&mut c);
        assert!(
            evs.iter().any(|e| matches!(e, Event::OrderCanceled(x)
                if x.client_order_id == "cb" && x.reason.as_str() == "oco")),
            "one OrderCanceled(oco) for the combo: {evs:?}"
        );
        // and it can never fill afterwards, even once both legs print
        c.on_bar(&quoted(2, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(2, "FAR", 94.0, 95.0));
        assert_eq!(fills_of(&c).len(), 1, "only the plain order's own fill exists");
        assert!(c.leg_marks.is_empty(), "no combo resting => marks were dropped");
    }
}
