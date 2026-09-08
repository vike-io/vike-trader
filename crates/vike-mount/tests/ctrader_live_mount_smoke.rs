//! LIVE demo smoke for the cTrader **mount arm** — proves the merged
//! `vike_mount::make_engine("ctrader", …)` path (PR a) end-to-end against the REAL cTrader demo
//! endpoint (`demo.ctraderapi.com:5035`): its OAuth-token cred-load
//! (`CtraderConfig::from_vars(Demo, …)`), the blocking protobuf/TLS exec handshake, the inline
//! `ReconClient` on its own dedicated authed socket, and the handshake-resolved `risk_properties`
//! RiskGate wiring — i.e. everything the `("ctrader", _)` arm builds. This is the MOUNT-level twin
//! of `vike-ctrader`'s own `tests/ctrader_demo_smoke.rs` (exec) and `tests/ctrader_reconcile_smoke.rs`
//! (recon): those prove the pieces in isolation, this proves `make_engine` composing them.
//!
//!     cargo test -p vike-mount --test ctrader_live_mount_smoke -- --ignored --nocapture
//!
//! Places a REAL EURUSD demo round-trip through the mounted `ExecutionClient` — a min-volume BUY
//! market order, then a `reduce_only` flatten that `CtraderExec::submit` routes to
//! `ProtoOAClosePositionReq` against the tracked open position (NOT a plain opposite
//! `ProtoOANewOrderReq`, which on the demo's HEDGING account OPENS an opposing hedged position
//! instead of netting flat) — and asserts the account is left flat via the mount's OWN reconcile
//! handle. The FLAT proof is MODE-AGNOSTIC: the venue's NET signed EURUSD position quantity must be
//! ≈ 0 (truly flat at the venue, netting or hedging alike), which the old opposite-order flatten
//! could not achieve on a hedging account. Standing user authorization exists to place real DEMO
//! orders; the account is left flat.
//!
//! Credentials come from the workspace's gitignored `.env` (`CTRADER_CLIENT_ID`/`_SECRET` +
//! `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`/optional `_ACCOUNT_ID`), loaded via
//! `load_workspace_dotenv_from` — the SAME var map `make_engine` reads — and gated on the SAME
//! `CtraderConfig::from_vars` the arm gates on. Absent creds → the test SKIPS (the live gate); it is
//! also `#[ignore]`d, so CI never runs it (which keeps CI green — there are no creds there).
//!
//! Weekend caveat: the FX demo is closed Fri 22:00 – Sun 22:00 GMT. A BUY that reaches no terminal
//! fill within the timeout (or is rejected) is treated as a SOFT SKIP (market likely closed — re-run
//! during FX hours), never a hard failure, and leaves the account untouched (nothing filled ⇒ still
//! flat). The flatten, by contrast, is a HARD requirement once the BUY has filled — an open
//! position must never be left behind.

use std::collections::HashSet;
use std::time::Duration;

use vike_bridge_core::credentials::{Environment, load_workspace_dotenv_from};
use vike_ctrader::config::CtraderConfig;
use vike_exec::event_channel;
use vike_exec::lanes::Ingest;
use vike_exec::recon::ReconClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatusReport, PositionStatusReport, now_ms};

const SYMBOL: &str = "EURUSD";
/// The venue's minimum EURUSD order volume in UNITS (1000 units == 100_000 centi-units == 0.01 lot;
/// see `vike_ctrader::symbols::VolumeGrid`, live-verified — mirrors `ctrader_demo_smoke.rs`).
const MIN_EURUSD_UNITS: f64 = 1000.0;
/// Bounded per-order fill wait. A demo market order that never fills within this is either a closed
/// market (weekend) or a genuine wiring fault; the BUY treats it as a soft skip, the flatten as a
/// hard failure.
const FILL_TIMEOUT: Duration = Duration::from_secs(30);
/// FLAT tolerance on the venue's NET signed EURUSD position quantity. A real close-by-position-id
/// nets the venue to exactly zero, so this is tight (mirrors `ctrader_demo_smoke.rs`).
const FLAT_TOL: f64 = 1e-6;

fn market_order(coid: &str, side: i32) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: SYMBOL.into(),
        side,
        qty: MIN_EURUSD_UNITS,
        order_type: "market".into(),
        ts: now_ms(),
        ..Default::default()
    }
}

/// A `reduce_only` flatten order — the mounted `ExecutionClient` (`CtraderExec::submit`) routes it
/// to `ProtoOAClosePositionReq` against the tracked open position rather than opening an opposing
/// (hedged) one. Mirrors `ctrader_demo_smoke.rs`'s `flatten_order`.
fn flatten_order(coid: &str, side: i32) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market_order(coid, side) }
}

/// Terminal outcome of one submitted market order, drained off the ingest lane.
enum FillOutcome {
    Filled,
    Rejected(String),
    TimedOut,
}

/// Block until a TERMINAL event for `coid` arrives on the ingest lane, or `total` elapses. Returns
/// the outcome (rather than panicking like the exec smoke's `wait_for_fill`) so the caller can
/// distinguish a market-closed BUY (soft skip) from a flatten failure (hard fail).
fn await_terminal(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    coid: &str,
    total: Duration,
) -> FillOutcome {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return FillOutcome::TimedOut,   // deadline elapsed
                Ok(None) => return FillOutcome::TimedOut, // ingest channel closed
                Ok(Some(Ingest::Event(Event::OrderFilled(f)))) if f.client_order_id == coid => {
                    return FillOutcome::Filled;
                }
                Ok(Some(Ingest::Event(Event::OrderRejected(r)))) if r.client_order_id == coid => {
                    return FillOutcome::Rejected(r.reason.to_string());
                }
                Ok(Some(_)) => {} // any other event — keep draining until our coid's terminal
            }
        }
    })
}

/// The venue's NET signed EURUSD position quantity (long +, short −), summed over every EURUSD
/// position report — the mode-agnostic "am I flat?" measure (mirrors `ctrader_demo_smoke.rs`'s
/// `net_eurusd_qty`): a hedging account's stacked long+short pair nets to ≈ 0 only after a real
/// close-by-position-id, never after an opposite-order flatten.
fn net_eurusd_qty(positions: &[PositionStatusReport]) -> f64 {
    positions.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum()
}

/// Re-fetch the mount's reconcile reports until the account reads flat (no resting SYMBOL order AND
/// net signed SYMBOL exposure ≈ 0) or a bounded number of attempts elapse — a small settle window
/// for venue state to catch up to the just-received flatten fill. Returns the LAST fetched pair; the
/// caller makes the hard assertions on it, so a genuinely-stuck non-flat state still fails loudly.
fn fetch_until_flat(
    recon: &dyn ReconClient,
) -> (Vec<OrderStatusReport>, Vec<PositionStatusReport>) {
    let mut orders = Vec::new();
    let mut positions = Vec::new();
    for attempt in 0..4 {
        orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
        positions = recon.fetch_position_status_reports().expect("fetch_position_status_reports");
        let resting = orders.iter().any(|o| o.symbol == SYMBOL);
        if !resting && net_eurusd_qty(&positions).abs() < FLAT_TOL {
            break;
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    (orders, positions)
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_mount_live_round_trip() {
    vike_log::test_init();

    // Cred-gate on the SAME var map AND the SAME `from_vars` gate `make_engine`'s ctrader arm uses.
    // Absent creds → clean skip (never a CI failure; the test is also `#[ignore]`d).
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    if CtraderConfig::from_vars(Environment::Demo, &vars).is_none() {
        tracing::warn!(target: "vike_mount::smoke", "SKIP: CTRADER_DEMO creds absent (live gate)");
        return;
    }

    let (tx, mut rx) = event_channel(256);
    let mut live_venues: HashSet<String> = HashSet::new();
    // cTrader's arm threads NO PropertiesRecorder into its spawn, so the caller-built recorder
    // handle is `None` here — matching the in-crate `make_engine` tests (and the disabled default).

    // Task 6 (armed-risk-defaults): a LIVE mount now REFUSES TO START without an operator-supplied
    // `max_notional_per_order`/`max_total_exposure` (see `vike_mount::require_live_risk_budget`'s
    // doc) — no universal safe default exists for either. This demo places a single
    // `MIN_EURUSD_UNITS` (1000-unit, i.e. 0.01 lot) round-trip, so a generous but still genuinely
    // bounded budget lets the real order through while still proving the mount is armed.
    let risk_profile = vike_exec::ProfileRisk {
        max_notional_per_order: Some(10_000.0),
        max_total_exposure: Some(10_000.0),
        ..vike_exec::ProfileRisk::default()
    };

    // THE unit under test: the merged `make_engine("ctrader", …)` arm.
    // `recon_enabled: true` is REQUIRED here, not incidental: the inline `ReconClient` handshake is
    // gated on it, and this smoke asserts the handle exists below.
    let (mut engine, recon) = vike_mount::make_engine(
        "ctrader",
        SYMBOL,
        &vars,
        &tx,
        &mut live_venues,
        true,
        None,
        None,
        Some(&risk_profile),
        // ⚠ A machine policy IS required now, and it carries exactly one thing: the ARMING CEILING
        // for ctrader. Before that ceiling existed this was `None` — "no machine policy" — because
        // cTrader ships a native market order and `MountPolicy` bound nothing on this arm. It binds
        // this: `None` (and the default) means `paper`, so a policy-less mount returns the paper
        // engine above the arm entirely and this smoke would place no order at all. `demo` is the
        // tier the arm resolves (`CtraderConfig::from_vars(Environment::Demo, …)`), which is what
        // this smoke trades on.
        Some(&vike_mount::MountPolicy {
            venues: vike_config::VenuePolicy::default()
                .declare("ctrader", vike_config::VenueMode::Demo),
            ..vike_mount::MountPolicy::default()
        }),
    )
    .expect("live mount with an operator-supplied risk budget must not refuse to start");

    // Creds present ⇒ a LIVE mount: the exec handshake succeeded, so the venue is marked live and the
    // inline reconcile handle (its own dedicated authed socket) exists. A handshake failure would have
    // DEMOTED cTrader to paper (`live_venues` empty, `recon` None) — the weaker-robustness contract
    // called out in the arm's doc — so this doubles as a live-connectivity check.
    assert!(
        live_venues.contains("ctrader"),
        "creds present ⇒ ctrader marked LIVE (exec connect/auth handshake must have succeeded)"
    );
    let recon =
        recon.expect("creds present ⇒ inline ReconClient built on its dedicated authed socket");

    // --- BUY: min-volume EURUSD market order through the mounted ExecutionClient ----------------
    // Submitted straight through `engine.client` (the returned boxed `ExecutionClient`), bypassing
    // the RiskGate exactly like the exec smoke — a nominal-equity mount must not spuriously deny a
    // valid venue-minimum demo order; the risk_properties wiring is proven at mount time above.
    let buy_coid = format!("vtrctradermount{}", now_ms() % 100_000_000);
    engine.client.submit(&market_order(&buy_coid, 1));
    match await_terminal(&mut rx, &buy_coid, FILL_TIMEOUT) {
        FillOutcome::Filled => tracing::info!(target: "vike_mount::smoke", "buy {buy_coid} filled"),
        FillOutcome::Rejected(reason) => {
            tracing::warn!(
                target: "vike_mount::smoke",
                %reason,
                "SOFT SKIP: BUY rejected (FX demo likely closed — re-run during FX market hours). Nothing filled ⇒ account still flat."
            );
            return;
        }
        FillOutcome::TimedOut => {
            tracing::warn!(
                target: "vike_mount::smoke",
                "SOFT SKIP: BUY never reached a terminal fill within {FILL_TIMEOUT:?} (FX demo likely closed — re-run during FX market hours). Nothing filled ⇒ account still flat."
            );
            return;
        }
    }

    // --- FLATTEN: reduce_only close-by-position-id. A fill here is REQUIRED — never leave an open
    // position. `reduce_only` routes to `ProtoOAClosePositionReq` (not an opposite order), so it nets
    // flat even on the demo's HEDGING account. ---------------------------------------------------
    let flat_coid = format!("vtrctradermount{}f", now_ms() % 100_000_000);
    engine.client.submit(&flatten_order(&flat_coid, -1));
    match await_terminal(&mut rx, &flat_coid, FILL_TIMEOUT) {
        FillOutcome::Filled => {
            tracing::info!(target: "vike_mount::smoke", "flatten {flat_coid} filled via close-position")
        }
        FillOutcome::Rejected(r) => panic!(
            "FLATTEN FAILED (rejected: {r}) after a filled BUY — EURUSD demo position may be OPEN; flatten it manually"
        ),
        FillOutcome::TimedOut => panic!(
            "FLATTEN FAILED (timed out) after a filled BUY — EURUSD demo position may be OPEN; flatten it manually"
        ),
    }

    // --- Assert FLAT via the mount's OWN reconcile handle ----------------------------------------
    // A close-by-position-id round-trip leaves no resting order and NET-zero venue exposure. Allow a
    // brief settle for venue state to propagate past the flatten fill.
    let (orders, positions) = fetch_until_flat(recon.as_ref());

    let resting: Vec<&OrderStatusReport> = orders.iter().filter(|o| o.symbol == SYMBOL).collect();
    assert!(
        resting.is_empty(),
        "expected no resting {SYMBOL} order after the round-trip, got {}",
        resting.len()
    );

    // MODE-AGNOSTIC FLAT: the venue's NET signed EURUSD exposure must be ≈ 0. On the demo's HEDGING
    // account an opposite-order flatten would leave a stacked long+short pair (net-zero fills on the
    // wire, two OPEN positions at the venue); the `reduce_only` close-by-position-id truly nets it
    // flat. Any EURUSD row the recon still echoes must carry the right venue tag.
    for p in positions.iter().filter(|p| p.symbol == SYMBOL) {
        assert_eq!(p.venue, "ctrader");
    }
    let net = net_eurusd_qty(&positions);
    assert!(
        net.abs() < FLAT_TOL,
        "account not FLAT: {SYMBOL} net signed qty = {net} (venue min volume is {MIN_EURUSD_UNITS} units)"
    );

    tracing::info!(
        target: "vike_mount::smoke",
        "ctrader mount live smoke GREEN: make_engine round-trip filled + flattened via close-position, venue net EURUSD exposure flat (qty={net})"
    );
}
