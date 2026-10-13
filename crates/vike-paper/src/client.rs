//! The paper book itself: `PaperExecutionClient`, `PaperFill`, the constructors and the HALT seam.

mod combo;
mod contingency;
mod exec;
mod expiry;
mod fees;
mod fill;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use vike_exec::ContingencyBook;
use vike_marketdata::Bar;
use vike_model::FeeSchedule;
use vike_model::WorkingOrder;
use vike_model::events::Event;

use combo::RestingCombo;
use expiry::Expiry;
use fees::UnderlyingSource;

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
    /// `vike-sim`'s `tests/r7_gate.rs` drives to prove backtest == paper bit-for-bit. Reading a
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
    /// `vike_venues::fee_schedule_for(venue)`; the r7 gate and unit tests keep using [`Self::new`]
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
    /// binaries read the environment (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`
    /// ratchets that down and would refuse a new library read); and the resolution is not a plain
    /// variable lookup but a precedence (the declared project, else the exe directory) with an
    /// armability probe and a once-per-process report, which lives — deliberately, once — in
    /// `crates/vike-mount/src/halt.rs`'s `halt_path_from_env`. The mount, which owns that module,
    /// passes its answer in, so both mounts and every venue adapter consult ONE path.
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
    /// `crates/vike-mount/src/xemm/paper.rs`'s `build_paper_xemm_core` was ALREADY that third seam, unarmed.
    /// An assertion inside seam A is evidence about A. Nothing was looking at the set, so nothing
    /// could fail. The two halves now in place:
    ///
    /// - **the roster gate** — `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` walks every
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

#[cfg(test)]
mod test_support;
