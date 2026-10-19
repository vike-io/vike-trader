//! Terminal settlement: binary-resolution and option-expiry checks and their end-of-run sweeps.
//!
//! # Option expiry reads the ONE `vike_model::OptionSpec`
//!
//! The option-expiry lane ([`EngineParams::option_specs`]) is answered with the model's own
//! [`vike_model::OptionSpec`] — the `Contract::Option` payload a catalogued instrument carries
//! (decision 0126). The settlement reads `strike`, `expiry_ms` (epoch ms, the engine's timestamp
//! unit), `right` and `underlying`; nothing else in the spec changes the cash payout.
//!
//! ⚠ **An option whose spec has `underlying: None` is UNSETTLEABLE, and the engine says so.** The
//! model allows `None` because some venues do not name the underlying on the option row, but
//! settlement needs the engine's own mark for the underlying symbol, and there is no symbol to read
//! it from. Guessing one (from the option's name, from the run's only other symbol) would settle
//! against a price nobody declared. So such a spec is treated as unusable: the option is never
//! settled, its position stays held and is marked at its own last price, exactly like an option
//! whose `underlying` names a symbol the run did not register — and, unlike that case, the engine
//! emits ONE `tracing::warn!` per symbol (`SimBroker::option_payout` raises it, the
//! `option_no_underlying_reported` latch keeps it to one) so the defect is visible in the run's
//! log rather than only in a suspiciously un-flattened book. The order-intake latch is NOT set:
//! the option keeps trading, because nothing about it has expired as far as the engine can prove.

use vike_model::Strategy;

use super::{RESOLUTION_PROBE_SENTINEL, SimBroker, StrategyEngine};

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Binary-resolution settlement sweep over ALL symbols at this step's bar timestamps (the
    /// opt-in `EngineParams::resolution` source, pm-economics lane). A no-op — zero calls, zero
    /// allocation — when no source is configured, so the default path is byte-identical. Runs
    /// BEFORE the step's stop/fill phase so a just-resolved symbol can neither fill resting
    /// orders nor keep a position past resolution.
    pub(super) fn check_resolutions(&mut self, cur_idx: usize) {
        if self.core.resolution.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            let ts = self.core.bars[si][cur_idx].ts;
            self.check_resolution_at(si, ts);
        }
    }

    /// One symbol's IN-LOOP resolution check: probe AND settle at this event's own `ts`
    /// (shared by the bar-step sweep and the tick path).
    #[inline]
    pub(super) fn check_resolution_at(&mut self, si: usize, ts: i64) {
        self.resolve_at(si, ts, ts);
    }

    /// One symbol's resolution check: ask the source at `probe_ts`, settle at `settle_ts` (the two
    /// differ only in the END-OF-RUN sweep, see [`Self::final_resolution_sweep`]).
    /// When the source reports `Some(payout)`: LATCH the symbol resolved, cancel everything
    /// resting (pending + tagged + protective stop — a resolved market can no longer trade, and
    /// the fill phase runs right after this), then close any open position at the payout via the
    /// fee-free settlement fill ([`SimBroker::settle_at_payout`]); the strategy learns of it
    /// through `on_fill`. Idempotent: once flat with nothing resting, later checks are no-ops.
    ///
    /// The latch comes FIRST, before the clear and before the settlement fires `on_fill`, because
    /// `on_fill` re-entry is the hole it closes: a strategy that re-enters from inside `on_fill`
    /// (re-hedge / re-enter on close) would otherwise push a fresh order into `pending`/`tagged`
    /// AFTER this clear but BEFORE this same step's fill phase, and fill it against the resolution
    /// bar — manufacturing fee-free PnL every time the market settled again. With the latch set,
    /// `SimBroker::push_pending` / `submit_limit_tagged` refuse those orders outright.
    fn resolve_at(&mut self, si: usize, probe_ts: i64, settle_ts: i64) {
        let payout =
            self.core.resolution.as_ref().and_then(|f| f(&self.core.symbols[si], probe_ts));
        let Some(payout) = payout else {
            return;
        };
        self.core.mark_resolved(si);
        self.core.sym[si].pending.clear();
        self.core.sym[si].tagged.clear();
        self.core.sym[si].stop = None;
        if let Some(fill) = self.core.settle_at_payout(si, payout, settle_ts) {
            self.fire_on_fill(fill);
        }
    }

    /// END-OF-RUN resolution sweep — the counterpart to the in-loop checks, run once after the
    /// event loop in BOTH `run` and `run_ticks`. Returns whether anything settled.
    ///
    /// Why it exists: a real prediction-market tape STOPS at the trading halt and the resolution
    /// posts later, so every recorded event carries `ts < res_ts` and the in-loop probes (which
    /// only ever ask at event timestamps) never see the market resolve. Without this sweep the
    /// canonical use case — hold YES to expiry over a recorded tape — silently no-ops: no
    /// settlement, no closed trade in the metrics, and `final_equity` marking the position at the
    /// last traded price (0.97) instead of the payout (1.0).
    ///
    /// So the sweep probes at [`EngineParams::resolution_end_ts`], defaulting to
    /// [`RESOLUTION_PROBE_SENTINEL`] ("did this market EVER resolve?"), while stamping the
    /// settlement at that same timestamp when set and otherwise at the symbol's LAST event ts —
    /// never at the sentinel, which would poison the closed trade's `exit_ts`. A no-op — zero
    /// calls — when no source is configured.
    pub(super) fn final_resolution_sweep(&mut self, last_ts: &[i64]) -> bool {
        if self.core.resolution.is_none() {
            return false;
        }
        let before = self.core.settlements.len();
        let probe_ts = self.core.resolution_end_ts.unwrap_or(RESOLUTION_PROBE_SENTINEL);
        for si in 0..self.core.symbols.len() {
            let settle_ts = self.core.resolution_end_ts.unwrap_or(last_ts[si]);
            self.resolve_at(si, probe_ts, settle_ts);
        }
        self.core.settlements.len() != before
    }

    /// Option-expiry settlement sweep over ALL symbols at this step's bar timestamps (the opt-in
    /// [`EngineParams::option_specs`] source, options lane). A no-op — zero calls, zero allocation —
    /// when no source is configured, so the default path is byte-identical. Runs BEFORE the step's
    /// stop/fill phase (right after [`Self::check_resolutions`]) so a just-expired option can neither
    /// fill resting orders nor keep a position past expiry.
    pub(super) fn check_option_expiries(&mut self, cur_idx: usize) {
        if self.core.option_specs.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            let ts = self.core.bars[si][cur_idx].ts;
            self.check_option_expiry_at(si, ts);
        }
    }

    /// One symbol's IN-LOOP option-expiry check: probe AND settle at this event's own `ts` (shared
    /// by the bar-step sweep and the tick path).
    #[inline]
    pub(super) fn check_option_expiry_at(&mut self, si: usize, ts: i64) {
        self.expire_at(si, ts, ts);
    }

    /// One symbol's option-expiry check: if `symbols[si]` is a configured option whose `expiry_ms`
    /// is at/before `probe_ts` (see [`SimBroker::option_payout`]), settle any open position to CASH
    /// at its intrinsic value, stamped at `settle_ts` (the two differ only in the END-OF-RUN sweep,
    /// see [`Self::final_option_expiry_sweep`]). The mechanism MIRRORS [`Self::resolve_at`] exactly:
    /// LATCH the symbol resolved FIRST (so the settlement's own `on_fill` re-entry is refused), then
    /// cancel everything resting (pending + tagged + protective stop — an expired option can no
    /// longer trade), then close the position at the intrinsic via the fee-free
    /// [`SimBroker::settle_at_payout`]; the strategy learns of it through `on_fill`. Idempotent:
    /// once flat with nothing resting, later checks add no settlement.
    fn expire_at(&mut self, si: usize, probe_ts: i64, settle_ts: i64) {
        let Some(payout) = self.core.option_payout(si, probe_ts) else {
            return;
        };
        self.core.mark_resolved(si);
        self.core.sym[si].pending.clear();
        self.core.sym[si].tagged.clear();
        self.core.sym[si].stop = None;
        if let Some(fill) = self.core.settle_at_payout(si, payout, settle_ts) {
            self.fire_on_fill(fill);
        }
    }

    /// END-OF-RUN option-expiry sweep — the counterpart to the in-loop checks, run once after the
    /// event loop in BOTH `run` and `run_ticks`. Settles any option still open at run end whose
    /// `expiry_ms` is at/before the as-of ([`EngineParams::option_expiry_end_ts`], defaulting to
    /// each symbol's LAST event ts — a safe idempotent no-op that never force-settles a still-live
    /// option; set a FUTURE as-of to settle one expiring just past the tape). Returns whether
    /// anything settled. A no-op — zero calls — when no source is configured.
    pub(super) fn final_option_expiry_sweep(&mut self, last_ts: &[i64]) -> bool {
        if self.core.option_specs.is_none() {
            return false;
        }
        let before = self.core.settlements.len();
        for si in 0..self.core.symbols.len() {
            let as_of = self.core.option_expiry_end_ts.unwrap_or(last_ts[si]);
            self.expire_at(si, as_of, as_of);
        }
        self.core.settlements.len() != before
    }
}
