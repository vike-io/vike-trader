//! Dukascopy `ReconClient` (recon-breadth → Dukascopy) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) for the one venue with NO REST API. Every other venue's
//! reconcile client opens a dedicated signed transport and PULLS reports on demand; Dukascopy
//! cannot — its only trading API is the JForex Java sidecar, spoken over JSON-lines stdio, and
//! that channel is owned end-to-end by [`crate::exec`]'s reader thread. So this client does not
//! query the venue at all: it reads a SHARED SNAPSHOT the reader thread already fills from the
//! sidecar's authoritative `position` push stream.
//!
//! ## Source: the existing `position` push line (netting-truth law A7), NOT a new query
//! The sidecar emits [`crate::proto::Envelope::Position`] — its authoritative per-instrument net
//! `(size, avg_px)`, computed on the Java side from `engine.getOrders()`'s FILLED orders
//! (`StrategyBridge.emitPositionState`) — right after every fill it reports. The reader thread
//! already consumes that line to re-anchor the blended fold (`crate::netting`); this seam adds one
//! thing: the reader captures the RAW venue `(size, avg_px, ts)` into [`PositionSnapshot`] BEFORE
//! the re-anchor consumes it, so the snapshot is pure venue truth even in the netting
//! `SizeMismatch` case the re-anchor deliberately refuses to auto-heal. `fetch_position_status_reports`
//! maps that snapshot to [`PositionStatusReport`]s. No new protocol command, no sidecar/jar change
//! — this works against the SHIPPED `jforex-bridge.jar` as-is.
//!
//! ## What it deliberately does NOT cover (and why the two empty fetches are honest, not lazy)
//! `fetch_order_status_reports` and `fetch_fill_reports` return `Ok(vec![])`. The current stdio
//! protocol carries no order-status or fill-history QUERY — those would need a new
//! command + response envelope answered from JForex `engine.getOrders()` / `IHistory` on the Java
//! side, i.e. a `StrategyBridge`/`Proto` change and therefore a gated `jforex-bridge.jar` rebuild
//! (full JDK; `.github/workflows/jforex-bridge.yml`'s reproducibility gate, and a RELEASE before
//! anyone receives the new jar). That is a separate, larger PR (see this crate's recon design
//! note / the PR body).
//!
//! ## NOT wired into `vike_mount::make_engine` — and MUST NOT be until orders are covered
//! Like the OANDA (#579) and IG (#578) seams, this client is the trait impl + factory only; live
//! wiring is a deliberate follow-up. For Dukascopy the follow-up is GATED on real order coverage:
//! `vike_exec::recon::diff` treats an order present in local state but absent from the order report
//! as an `OrphanLocalOrder`, so wiring this position-only client (its `fetch_order_status_reports`
//! reports nothing) against a live account holding resting orders would orphan EVERY one of them,
//! every pass. ⚠ That is a REPORTING defect, not a destructive one, and this paragraph used to say
//! otherwise: it claimed the `hybrid` policy "AUTO-SYNTHESIZES its cancellation" so the wiring
//! "would wrongly cancel every one". `resolve` synthesizes no `OrderCanceled` for any kind under
//! any policy (`crates/vike-exec/tests/recon/recon_policy_pin.rs`); what an orphaned book actually costs
//! is one permanent, un-actionable operator alert saying the local book disagrees with a venue we
//! never asked about orders. The gate stands — only its stated cost was wrong. The position seam
//! is still valuable STANDALONE: it surfaces the
//! netting `SizeMismatch` (which `crate::netting` logs but refuses to auto-heal) as a first-class
//! `PositionDrift`/`PositionOnlyExternal` divergence for operator review.
//!
//! ## Residual limitations (documented, not hidden)
//! - The snapshot is refreshed on the sidecar's `position` PUSH (after each fill), not by an
//!   on-demand re-query: it is the venue net position AS OF THE LAST FILL. Between fills it is
//!   stale-but-correct — fine for a periodic reconcile pass.
//! - A symbol only appears once a fill has touched it THIS session; a position carried across a
//!   process restart is absent until its next fill (the same session-crossing gap `crate::netting`
//!   handles as `Reanchor::Baseline`). Cross-session restore belongs to the journal + the future
//!   query slice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_exec::recon::ReconClient;
use vike_model::events::PositionSide;
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

/// The venue key stamped on every report row.
pub const VENUE: &str = "dukascopy";

/// One venue-authoritative net position line, captured verbatim from the sidecar's `position`
/// envelope (raw venue truth — the signed net size in UNITS and the signed-amount-weighted average
/// of the remaining orders' open prices, exactly as [`crate::proto::Envelope::Position`] carries).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VenuePosition {
    /// Signed net size in UNITS: `>0` long, `<0` short, `0` flat.
    pub size: f64,
    /// Signed-amount-weighted average open price; `0.0` when flat.
    pub avg_px: f64,
    /// Sidecar event timestamp of the `position` line.
    pub ts: i64,
}

/// The reader-thread-updated snapshot of the sidecar's last `position` line per canonical symbol.
/// Shared (`Arc`) between [`crate::exec::DukascopyExecutionClient`]'s reader thread (writer) and
/// every [`DukascopyReconClient`] derived from it (reader). The `Mutex` guards a short critical
/// section (one `HashMap` insert / a clone-out), never held across I/O.
pub type PositionSnapshot = Arc<Mutex<HashMap<String, VenuePosition>>>;

/// Map one captured venue net position → a normalized [`PositionStatusReport`]. Dukascopy is a
/// NET / one-way FX account (like OANDA / binance-perp / ibkr): `position_side` is
/// [`PositionSide::Both`] and the SIGN of `qty` carries direction. No isolated wallet → the
/// `margin_mode`/`isolated_margin` defaults (Cross / `None`). Pure — fixture-tested below.
pub fn to_position_report(symbol: &str, p: &VenuePosition) -> PositionStatusReport {
    PositionStatusReport {
        venue: VENUE.to_string(),
        symbol: symbol.to_string(),
        position_side: PositionSide::Both,
        qty: p.size,
        avg_px: p.avg_px,
        ts: p.ts,
        margin_mode: MarginMode::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// The Dukascopy reconcile client: a thin reader over the shared [`PositionSnapshot`]. Built from a
/// running [`crate::exec::DukascopyExecutionClient`] via its `recon_client()` — it CANNOT be
/// constructed from config alone (there is no REST endpoint to connect to; the venue truth arrives
/// only through the live sidecar's stdio the exec client owns).
pub struct DukascopyReconClient {
    positions: PositionSnapshot,
}

impl DukascopyReconClient {
    /// Wrap a shared position snapshot (the handle `DukascopyExecutionClient::recon_client` clones).
    pub fn new(positions: PositionSnapshot) -> Self {
        Self { positions }
    }
}

impl ReconClient for DukascopyReconClient {
    /// EMPTY by design — the stdio protocol carries no order-status query (see the module doc).
    /// `Ok(vec![])`, never `Err`: an empty order report is honest ("this seam does not cover
    /// orders"), and returning it keeps the seam inert rather than failing a pass.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }

    /// EMPTY by design — no fill-history query in the stdio protocol (see the module doc). Live
    /// fills already reach the core through [`crate::exec`]'s event lane; a historical fill QUERY
    /// is the deferred Java/jar-gated slice.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }

    /// The sidecar's last authoritative net position per symbol, mapped from the shared snapshot.
    /// Sorted by symbol for deterministic ordering (the diff engine's golden-fixture discipline).
    /// Flat (`qty == 0`) rows are KEPT: they let `recon::diff` detect a stale LOCAL position the
    /// venue has since closed.
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let snap = self.positions.lock().map_err(|e| format!("position snapshot poisoned: {e}"))?;
        let mut reports: Vec<PositionStatusReport> =
            snap.iter().map(|(sym, p)| to_position_report(sym, p)).collect();
        reports.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        Ok(reports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(entries: &[(&str, VenuePosition)]) -> PositionSnapshot {
        let map: HashMap<String, VenuePosition> =
            entries.iter().map(|(s, p)| (s.to_string(), *p)).collect();
        Arc::new(Mutex::new(map))
    }

    #[test]
    fn maps_a_short_position_signed() {
        let p = VenuePosition { size: -1000.0, avg_px: 1.1, ts: 42 };
        let r = to_position_report("EURUSD", &p);
        assert_eq!(r.venue, "dukascopy");
        assert_eq!(r.symbol, "EURUSD");
        assert_eq!(r.position_side, PositionSide::Both);
        assert_eq!(r.qty, -1000.0);
        assert_eq!(r.avg_px, 1.1);
        assert_eq!(r.ts, 42);
        assert_eq!(r.margin_mode, MarginMode::default());
        assert_eq!(r.isolated_margin, None);
    }

    #[test]
    fn positions_are_sorted_and_include_flat_rows() {
        let client = DukascopyReconClient::new(snapshot(&[
            ("USDJPY", VenuePosition { size: 2000.0, avg_px: 155.25, ts: 7 }),
            ("EURUSD", VenuePosition { size: 0.0, avg_px: 0.0, ts: 9 }),
        ]));
        let reports = client.fetch_position_status_reports().unwrap();
        // Sorted by symbol; the flat EURUSD row is kept (diff needs it to spot a stale local pos).
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].symbol, "EURUSD");
        assert_eq!(reports[0].qty, 0.0);
        assert_eq!(reports[1].symbol, "USDJPY");
        assert_eq!(reports[1].qty, 2000.0);
    }

    #[test]
    fn order_and_fill_fetches_are_empty_not_errors() {
        let client = DukascopyReconClient::new(snapshot(&[]));
        assert_eq!(client.fetch_order_status_reports(0), Ok(Vec::new()));
        assert_eq!(client.fetch_fill_reports(0), Ok(Vec::new()));
        assert_eq!(client.fetch_position_status_reports(), Ok(Vec::new()));
        // Balance stays the trait default (not wired for this venue).
        assert_eq!(client.fetch_balance(), Ok(None));
    }
}
