//! Netting-truth re-anchor (law-map A7): make the Rust fold see what the Java sidecar did.
//!
//! JForex is position-per-order; the sidecar's `NettingPlan` closes opposing FILLED orders in
//! candidate-list order, realizing each close at THAT order's own entry price. The core `Account`
//! blends everything into one net average (`vike_model::compute_fill`), so after a PARTIAL netted
//! close the two agree on net size but drift in (realized, remaining avg) — e.g. short 1M@100
//! (A) and 1M@110 (B), buy 1M@108: the venue realized −8/unit and keeps the remainder at 110;
//! the blend realized −3/unit and keeps 105. Dukascopy has no reconcile client, so nothing
//! re-anchored.
//!
//! ## Design decision (the two routes from the brief, evaluated in code)
//!
//! **Rejected — per-fill basis attribution (`closed_entry_px` reshaping the close fill):** the
//! blended fold's realized on a reduce is a function of the blended average, so no stream of
//! TRUTHFUL fills can reproduce the venue's per-order attribution; a zero-qty "avg adjustment"
//! fill is a no-op through `compute_fill` (the add branch leaves avg unchanged at qty 0), and
//! anything stronger needs either fabricated prices or a per-order sub-position ledger — a second
//! fold law. Both violate the one-fold-law mandate.
//!
//! **Rejected — the literal recon machinery:** `vike_exec::recon::diff` detects `PositionDrift`
//! on qty ONLY; the netting drift has EQUAL qty and drifted avg/realized, so it is invisible to
//! recon by construction (and dukascopy has no `ReconClient` to feed it).
//!
//! **Chosen — venue-authoritative position line + re-anchor legs through the ONE fold:** the
//! sidecar emits its authoritative `(size, avg_px)` after every fill it reports
//! (`Envelope::Position`); the exec client keeps a shadow of the blended fold (this module —
//! literally `compute_fill`, the same primitive `Account::fold` calls, so the shadow can never
//! drift from the core) and, when the venue line shows an avg drift at matching size, synthesizes
//! two ORDINARY fills at the venue's remaining average — close the whole blended position at
//! `venue_avg`, reopen `venue_size` at `venue_avg`. Folding that pair books EXACTLY the
//! attribution difference as realized PnL ((venue_avg − blended_avg) · sign · size) and leaves
//! the remaining basis at the venue's truth — the recon `synth_position_legs` doctrine
//! (#497/#468: synthesized fills through the one shared fold, policy per caller), applied
//! venue-locally. No new `Event` arms, no fold change, no second law.
//!
//! Guards: a SIZE mismatch is never auto-healed (that is a missed/spurious fill — real recon
//! territory with different failure modes); it is surfaced loudly and skipped. A position line
//! for a symbol the shadow has never folded (process restart with an open venue position, or
//! pre-session external activity) adopts the line as baseline without synthesizing anything —
//! the session-crossing case belongs to journal restore + a future recon client, not here.
//!
//! **No dedup on the shadow's own fold**: `ShadowBook::fold_fill` folds every bare `Event::Fill`
//! it is handed unconditionally — unlike `ExecutionEngine::on_event`, which guards its own
//! `Account::apply_fill` fold with the always-on `seen_trade_ids` reconnect-replay dedup (see
//! `vike-exec/src/execution_engine/mod.rs`), this module has no equivalent set and shares none of
//! the engine's. A replayed duplicate fill would therefore desync the shadow from the blended
//! `Account` it mirrors — bounded, not silent: the next `reanchor` sees a size that no longer
//! matches and surfaces a loud `SizeMismatch` (never auto-healed) rather than corrupting state
//! quietly. This is largely theoretical for this venue: the JForex sidecar process survives
//! reconnects (unlike the crypto venues' WS user-data pumps) and its `Envelope::Fill`/`Position`
//! stream is fresh-seq per delta, not replayed — but the gap is real if that assumption ever
//! breaks.

use vike_model::compute_fill;
use vike_model::events::{Event, FillEvent, LiquiditySide, PositionSide, TradeId};

/// Absolute size tolerance in UNITS. Fills are ≥1000 units (the venue minimum is 1000);
/// float dust from the Java millions ↔ Rust units scaling is bounded far below this.
const QTY_TOL: f64 = 1e-3;
/// Avg-px tolerance: relative to the price scale, absorbing ulp dust between the sidecar's
/// weighted-average arithmetic and the fold's incremental blend. A real netting drift is
/// pips (≥1e-5 relative) — orders of magnitude above.
const PX_REL_TOL: f64 = 1e-9;

/// Reserved coid/trade-id namespace for re-anchor legs (mirrors recon's `EXT-` convention).
const REANCHOR_PREFIX: &str = "NETRA";

/// What a venue position line resolved to (the caller logs / forwards accordingly).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reanchor {
    /// First sight of this symbol with no folded fills — venue state adopted as baseline.
    Baseline,
    /// Shadow already matches the venue truth (within tolerance).
    InSync,
    /// Avg drift at matching size — the close+reopen legs to pump into ingest, in order.
    Corrected(Vec<Event>),
    /// Net size disagrees — NOT auto-healed (missed/spurious fill class); caller warns loudly.
    SizeMismatch { local_size: f64, venue_size: f64 },
}

/// One shadow position: the same (size, avg_px) pair `Account::fold` tracks, advanced by the
/// same `compute_fill` law over the same fill stream this client emits.
#[derive(Debug, Clone, Copy, Default)]
struct Shadow {
    size: f64,
    avg_px: f64,
}

/// Per-symbol shadow of the core's blended dukascopy positions, plus the re-anchor decision.
/// Lives on the bridge reader thread (single-threaded — no interior locking needed).
#[derive(Debug, Default)]
pub(crate) struct ShadowBook {
    positions: std::collections::HashMap<String, Shadow>,
}

impl ShadowBook {
    /// Fold one emitted fill (bare `Event::Fill` lane ONLY — the `OrderFilled` wrap carries the
    /// same fill and must not double-fold, exactly like `Account`).
    pub(crate) fn fold_fill(&mut self, fill: &FillEvent) {
        let entry = self.positions.entry(fill.symbol.to_string()).or_default();
        // Multiplier 1.0: it scales realized PnL only — size/avg (all the shadow tracks) are
        // multiplier-independent, and the synthesized legs let the Account apply its OWN grid.
        let out =
            compute_fill(entry.size, entry.avg_px, fill.side, fill.last_qty, fill.last_px, 1.0);
        entry.size = out.new_size;
        entry.avg_px = out.new_avg_px;
    }

    /// Resolve a sidecar `position` line against the shadow. On `Corrected`, the legs have
    /// already been folded back into the shadow (it ends at the venue truth), so a repeated
    /// identical line is `InSync` — idempotent by construction.
    pub(crate) fn reanchor(
        &mut self,
        symbol: &str,
        venue_size: f64,
        venue_avg: f64,
        ts: i64,
    ) -> Reanchor {
        let Some(local) = self.positions.get(symbol).copied() else {
            // Never folded a fill for this symbol: adopt the venue state as baseline.
            self.positions
                .insert(symbol.to_string(), Shadow { size: venue_size, avg_px: venue_avg });
            return Reanchor::Baseline;
        };
        if (local.size - venue_size).abs() > QTY_TOL {
            return Reanchor::SizeMismatch { local_size: local.size, venue_size };
        }
        let flat = local.size.abs() <= QTY_TOL && venue_size.abs() <= QTY_TOL;
        let px_tol = PX_REL_TOL * venue_avg.abs().max(1.0);
        if flat || (local.avg_px - venue_avg).abs() <= px_tol {
            return Reanchor::InSync;
        }
        // Close the whole blended position at venue_avg, reopen venue_size at venue_avg. The
        // close leg realizes (venue_avg − blended_avg)·sign·|size| — the exact attribution
        // difference — and the reopen re-bases the remainder at the venue's truth.
        let sign = if local.size > 0.0 { 1 } else { -1 };
        let close = self.leg(symbol, -sign, local.size.abs(), venue_avg, ts);
        let open = self.leg(symbol, sign, venue_size.abs(), venue_avg, ts);
        for f in [&close, &open] {
            self.fold_fill(f);
        }
        Reanchor::Corrected(vec![Event::Fill(close), Event::Fill(open)])
    }

    /// One synthesized re-anchor leg: an ordinary bare `FillEvent` (no FSM wrap — there is no
    /// order behind it; the engine folds bare fills into the Account unconditionally, deduped by
    /// trade_id, without touching the order registry).
    ///
    /// ## Why the id is CONTENT-derived and not `self.seq`
    ///
    /// It used to be `{REANCHOR_PREFIX}-{symbol}:{seq}` off an in-process counter, which is the
    /// MIRROR-IMAGE defect of an empty id: not a duplicate waved through, but two DISTINCT fills
    /// collapsed into one. `seq` restarts at 0 every process, while the engine's `seen_trade_ids`
    /// does NOT — it is restored across a restart (`EngineSnapshot::seen_trade_ids`, and vike-app
    /// seeds it from `vike_data::exec_index::recent_seen_trade_ids`). So: session A re-anchors
    /// EURUSD and mints `NETRA-EURUSD:1`, which is journalled; the process restarts; session B
    /// re-anchors EURUSD for a genuinely DIFFERENT attribution difference, mints `NETRA-EURUSD:1`
    /// again, and the engine drops it as a replay. The shadow book folds it (it has no dedup — see
    /// the module doc), the real `Account` does not, and they diverge silently until the next
    /// `reanchor` reports a `SizeMismatch`, which is never auto-healed. The legs DO reach the engine
    /// (`exec.rs` pumps them), so this was reachable rather than theoretical.
    ///
    /// The id is now a pure function of the leg's own content — symbol, side, qty, px and the
    /// SIDECAR-SUPPLIED position-frame `ts` (`Envelope::Position`'s own field, never a wall clock).
    /// Every input is replay-stable, so re-decoding the same position frame re-derives the same id
    /// and dedups, while two genuinely different re-anchors differ in at least one input. `side` is
    /// load-bearing in that key: a `Corrected` pair is only ever emitted when the sizes already
    /// agree, so the close and open legs share qty AND px and are distinguished by side alone.
    fn leg(&self, symbol: &str, side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
        FillEvent {
            trade_id: TradeId::prefixed(
                REANCHOR_PREFIX,
                format_args!("-{symbol}:{ts}:{side}:{qty}:{px}"),
            ),
            client_order_id: format!("{REANCHOR_PREFIX}-{symbol}"),
            venue: "dukascopy".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Unknown,
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(side: i32, qty: f64, px: f64) -> FillEvent {
        fill_sym("EURUSD", side, qty, px)
    }

    /// `fill` with the symbol as a parameter — the shadow book is keyed per symbol, so a
    /// cross-symbol test needs to fold onto a second key.
    fn fill_sym(symbol: &'static str, side: i32, qty: f64, px: f64) -> FillEvent {
        FillEvent {
            trade_id: "t".into(),
            client_order_id: "c".into(),
            venue: "dukascopy".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 1,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }

    fn legs(r: Reanchor) -> Vec<FillEvent> {
        match r {
            Reanchor::Corrected(events) => events
                .into_iter()
                .map(|e| match e {
                    Event::Fill(f) => f,
                    other => panic!("re-anchor legs must be bare fills, got {other:?}"),
                })
                .collect(),
            other => panic!("expected Corrected, got {other:?}"),
        }
    }

    /// Drive one book to the brief's A/B state and return its re-anchor legs. Used to replay the
    /// IDENTICAL history on a FRESH book — i.e. to simulate a process restart.
    fn ab_scenario_legs() -> Vec<FillEvent> {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 100.0));
        book.fold_fill(&fill(-1, 1000.0, 110.0));
        book.fold_fill(&fill(1, 1000.0, 108.0));
        legs(book.reanchor("EURUSD", -1000.0, 110.0, 3))
    }

    /// THE RESTART COLLISION. The id used to be `NETRA-{symbol}:{seq}` off `ShadowBook.seq`, an
    /// IN-PROCESS counter that restarts at 0 every process — while the engine's `seen_trade_ids` does
    /// NOT (it is restored from `EngineSnapshot::seen_trade_ids`, and vike-app seeds it from
    /// `vike_data::exec_index::recent_seen_trade_ids`). So session A minted `NETRA-EURUSD:1` and
    /// journalled it; after a restart session B minted `NETRA-EURUSD:1` for a genuinely DIFFERENT
    /// attribution difference and the engine DROPPED it as a replay. That is the mirror image of the
    /// empty-id defect: not a duplicate waved through, but two distinct fills collapsed into one —
    /// and it is silent, because the shadow book has no dedup of its own (module doc) so it folds the
    /// leg the `Account` refused, and the two diverge until a `SizeMismatch` that is never auto-healed.
    ///
    /// The fix makes the id a pure function of the leg's own content, so this test asserts BOTH
    /// halves: the same frame re-derives the same id (dedup still works across a restart), and a
    /// different re-anchor gets a different id (distinct fills stay distinct).
    #[test]
    fn a_reanchor_id_is_stable_across_a_restart_and_distinct_between_different_reanchors() {
        // Two independent books = two process lifetimes over the same venue history.
        let first = ab_scenario_legs();
        let second = ab_scenario_legs();
        let ids = |v: &[FillEvent]| v.iter().map(|f| f.trade_id.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ids(&first),
            ids(&second),
            "a re-anchor id must be REPLAY-STABLE: the same venue position frame must re-derive the \
             same trade_id after a restart, or the engine's dedup cannot recognise it"
        );

        // ...and the two legs of one pair stay distinct. They are only ever emitted when the sizes
        // already agree, so close and open share qty AND px — `side` is what separates them.
        assert_ne!(first[0].trade_id, first[1].trade_id, "close and open legs must differ");

        // A DIFFERENT re-anchor (different venue basis) must not collide with the first.
        let mut other = ShadowBook::default();
        other.fold_fill(&fill(-1, 1000.0, 100.0));
        other.fold_fill(&fill(-1, 1000.0, 120.0));
        other.fold_fill(&fill(1, 1000.0, 108.0));
        let different = legs(other.reanchor("EURUSD", -1000.0, 120.0, 3));
        for a in &first {
            for b in &different {
                assert_ne!(
                    a.trade_id, b.trade_id,
                    "two genuinely different re-anchors must never share a trade_id — the engine \
                     would drop the second as a replay and silently desync the shadow"
                );
            }
        }

        // The counter is gone: nothing about the id may depend on how many legs a session has minted.
        // Folding an unrelated symbol's re-anchor FIRST must not shift EURUSD's ids.
        let mut interleaved = ShadowBook::default();
        interleaved.fold_fill(&fill_sym("USDJPY", -1, 1000.0, 150.0));
        let _ = interleaved.reanchor("USDJPY", -1000.0, 155.0, 1);
        interleaved.fold_fill(&fill(-1, 1000.0, 100.0));
        interleaved.fold_fill(&fill(-1, 1000.0, 110.0));
        interleaved.fold_fill(&fill(1, 1000.0, 108.0));
        assert_eq!(
            ids(&legs(interleaved.reanchor("EURUSD", -1000.0, 110.0, 3))),
            ids(&first),
            "an id must not depend on session-local leg ORDER — that was the counter's whole defect"
        );
    }

    /// The brief's A/B example: short 1000@100 (A) + 1000@110 (B) → blend 105; buy 1000@108
    /// netted against A. Venue truth: realized −8/unit, remainder 1000 short @110. The re-anchor
    /// legs close 1000@110 (realizing the −5/unit attribution difference on top of the blend's
    /// −3/unit) and reopen 1000 short @110.
    #[test]
    fn partial_netted_close_reanchors_to_the_venue_basis() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 100.0));
        assert_eq!(book.reanchor("EURUSD", -1000.0, 100.0, 1), Reanchor::InSync);
        book.fold_fill(&fill(-1, 1000.0, 110.0));
        // Signed-weighted venue avg of two whole orders == the blend: still in sync.
        assert_eq!(book.reanchor("EURUSD", -2000.0, 105.0, 2), Reanchor::InSync);
        // The netted close leg (buy 1000@108) — blend realizes at 105, venue at A's 100.
        book.fold_fill(&fill(1, 1000.0, 108.0));
        let legs = legs(book.reanchor("EURUSD", -1000.0, 110.0, 3));
        assert_eq!(legs.len(), 2);
        // Close the blended short 1000@105 at 110 → realized (110−105)·(−1000) = −5000.
        assert_eq!((legs[0].side, legs[0].last_qty, legs[0].last_px), (1, 1000.0, 110.0));
        // Reopen short 1000 at the venue's true remaining basis 110.
        assert_eq!((legs[1].side, legs[1].last_qty, legs[1].last_px), (-1, 1000.0, 110.0));
        assert_ne!(legs[0].trade_id, legs[1].trade_id);
        assert!(legs[0].trade_id.starts_with("NETRA-EURUSD:"), "{}", legs[0].trade_id);
        // Idempotent: the shadow now sits at the venue truth; the same line is a no-op.
        assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 4), Reanchor::InSync);
    }

    /// A FULL netted close has no attribution drift (the blend's total equals the per-order
    /// sum), so the flat position line must synthesize nothing.
    #[test]
    fn full_close_is_in_sync_flat() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 100.0));
        book.fold_fill(&fill(-1, 1000.0, 110.0));
        book.fold_fill(&fill(1, 2000.0, 108.0));
        assert_eq!(book.reanchor("EURUSD", 0.0, 0.0, 3), Reanchor::InSync);
    }

    /// The long-side mirror: long 1000@100 + 1000@110, sell 1000@108 netted against the @100
    /// order → venue remainder long 1000@110; the close leg realizes +5000 over the blend.
    #[test]
    fn long_side_mirror_reanchors() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(1, 1000.0, 100.0));
        book.fold_fill(&fill(1, 1000.0, 110.0));
        book.fold_fill(&fill(-1, 1000.0, 108.0));
        let legs = legs(book.reanchor("EURUSD", 1000.0, 110.0, 3));
        assert_eq!((legs[0].side, legs[0].last_qty, legs[0].last_px), (-1, 1000.0, 110.0));
        assert_eq!((legs[1].side, legs[1].last_qty, legs[1].last_px), (1, 1000.0, 110.0));
    }

    #[test]
    fn size_mismatch_is_surfaced_never_healed() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 100.0));
        match book.reanchor("EURUSD", -2000.0, 105.0, 2) {
            Reanchor::SizeMismatch { local_size, venue_size } => {
                assert_eq!(local_size, -1000.0);
                assert_eq!(venue_size, -2000.0);
            }
            other => panic!("expected SizeMismatch, got {other:?}"),
        }
        // The shadow is untouched: the mismatch keeps surfacing until real fills explain it.
        match book.reanchor("EURUSD", -2000.0, 105.0, 3) {
            Reanchor::SizeMismatch { .. } => {}
            other => panic!("expected persistent SizeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn first_line_without_fills_adopts_baseline() {
        let mut book = ShadowBook::default();
        assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 1), Reanchor::Baseline);
        // Adopted: the same line is now in sync, no synthesis.
        assert_eq!(book.reanchor("EURUSD", -1000.0, 110.0, 2), Reanchor::InSync);
    }

    /// Ulp-level avg dust (Java weighted-average vs Rust incremental blend arithmetic) must not
    /// trigger a correction — the tolerance is far below a real drift (pips) but above dust.
    #[test]
    fn ulp_dust_is_in_sync() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 1.1));
        book.fold_fill(&fill(-1, 1000.0, 1.2));
        let blended = book.positions["EURUSD"].avg_px;
        let dusted = blended + blended * 1e-13;
        assert_eq!(book.reanchor("EURUSD", -2000.0, dusted, 2), Reanchor::InSync);
    }

    /// Per-symbol independence: a drift on one symbol never touches another's shadow.
    #[test]
    fn symbols_are_independent() {
        let mut book = ShadowBook::default();
        book.fold_fill(&fill(-1, 1000.0, 100.0));
        let mut jpy = fill(-1, 1000.0, 155.0);
        jpy.symbol = "USDJPY".into();
        book.fold_fill(&jpy);
        assert_eq!(book.reanchor("USDJPY", -1000.0, 155.0, 2), Reanchor::InSync);
        assert_eq!(book.reanchor("EURUSD", -1000.0, 100.0, 2), Reanchor::InSync);
    }
}
