//! **Four branches of one reconcile pass that no other test of this crate asserted**, each driven
//! through a real spawned core and a `Command::ReconcileReports`, and each read off what an operator
//! can see: the published `CoreSnapshot` (positions, balance, held alerts) and its recent-events
//! ring. The pass is `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports`, run as
//! `reconcile_compute` -> `apply_recon_balance` -> the event fold -> `hold_recon_alerts`.
//!
//! 1. **The instance-origin stamp** (`reconcile_compute`). The core stamps the tag its OWN
//!    `ClientOrderIdGenerator` mints under onto the pass's `ReconPolicy::local_instance_origin`, and
//!    `crates/vike-exec/src/recon/resolve.rs`'s `mode_applies_divergence` then HOLDS a divergence
//!    whose evidence names a different instance, under every policy. Pinned four ways: a foreign
//!    fill is held while an own-tagged and an untagged fill fold (every policy that folds a
//!    `MissingFill`); the tag is the generator's, not the configured one; an identity the pass
//!    already carries is not overwritten; a core minting untagged ids treats nothing as foreign.
//! 2. **A balance crossing under a HOLDING policy** leaves no `absorbed` ring note: the drift is held
//!    instead. The positive case (a folding policy) is
//!    `crates/vike-core/tests/recon/recon_balance_crossing.rs`.
//! 3. **`BalanceCheck::NotReported`** with cash reconcile ON: a pass whose venue reported no balance
//!    writes nothing, on a cold account and on an anchored one.
//! 4. **A held divergence that clears and returns** keeps its row and its confirm id (the refresh
//!    path of `hold_recon_alerts`, reached with the announcer having forgotten the identity), while a
//!    new identity gets its own row. The WARN half of the same path is a log line, so it is pinned in
//!    the white-box `runtime/tests/recon_held.rs` suite, which can capture logs on its own thread.
//!
//! Every pass here carries `route_key: None` — what `vike_core::ReconLeg::sole_account_of` produces
//! for a single-account venue — and every core is single-account, so the Class E refusal is
//! unreachable by construction.

use std::sync::Arc;

use vike_core::{CoreConfig, CoreHandle, CoreSnapshot, ReconConfig, spawn_core};
use vike_exec::recon::{BalanceTol, DivergenceKind, POLICY_NAMES, ReconPolicy, mode_applies};
use vike_exec::testing::RecordingClient;
use vike_exec::{BalanceMode, Command, ReconcileReports};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, InstanceOrigin, PositionStatusReport};

use crate::kit::engines::{dyn_engine, test_config};
use crate::kit::handle::wait_for_snapshot;

// -------------------------------------------------------------------------------------------
// Shared builders. Each pins the field that is its test's subject by hand, so it stays visible.
// -------------------------------------------------------------------------------------------

/// A binance/BTCUSDT core whose coid generator is built from `origin` (the configured
/// `CoreConfig::instance_origin`) and `session` (a RESUMED `CoreConfig::coid_session`, which wins
/// over the configured origin — `CoreConfig::instance_origin`'s doc says why).
fn core_minting_under(origin: Option<&str>, session: Option<&str>) -> CoreHandle {
    let primary = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let config = CoreConfig {
        instance_origin: origin.map(|o| InstanceOrigin::parse(o).expect("a legal origin tag")),
        coid_session: session.map(|s| (s.to_string(), 0)),
        ..test_config(10_000.0)
    };
    spawn_core(primary, config)
}

/// A venue fill report local state has never folded (its trade id is new), echoing `coid` as the
/// client order id. The coid is the subject, so it is a parameter rather than a default.
fn fill_echoing(trade_id: &'static str, coid: &str, qty: f64) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: trade_id.into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

/// A venue position row for `symbol` at `qty` — with no local counterpart it is a
/// `PositionOnlyExternal`, which `quarantine` holds.
fn position(symbol: &str, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts: 5,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// One pass carrying `fills` and `positions` under `policy`, with cash reconcile OFF.
fn pass(
    policy: ReconPolicy,
    fills: Vec<FillReport>,
    positions: Vec<PositionStatusReport>,
) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills,
        positions,
        policy,
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
    }))
}

/// One balance-only pass, built from `cfg` field by field exactly as the reconcile manager builds
/// it (the shape `recon_balance_crossing.rs` uses).
fn balance_pass(cfg: &ReconConfig, balance: Option<f64>) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: cfg.policy.clone(),
        balance,
        generate_missing_orders: cfg.generate_missing_orders,
        reconcile_balance: cfg.reconcile_balance,
        balance_tol: cfg.balance_tol,
        route_key: None,
    }))
}

/// The published BTCUSDT position size of the primary account, `0.0` when there is none.
fn btc_size(snap: &CoreSnapshot) -> f64 {
    snap.portfolio
        .venues
        .first()
        .and_then(|v| v.positions.iter().find(|p| p.symbol == "BTCUSDT"))
        .map_or(0.0, |p| p.size)
}

/// The primary account's published balance, or NaN before the first block exists.
fn balance(snap: &CoreSnapshot) -> f64 {
    snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN)
}

fn ring_lines_containing(snap: &CoreSnapshot, needle: &str) -> Vec<String> {
    snap.recent_events.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

fn is(x: f64, want: f64) -> bool {
    (x - want).abs() < 1e-9
}

/// Every policy an operator can NAME for which `mode_applies` answers `want` for `kind` — derived
/// from the authority rather than listed, so a policy change re-scopes the test instead of
/// silently leaving it pinned to a stale set.
fn policies_where(kind: DivergenceKind, want: bool) -> Vec<&'static str> {
    POLICY_NAMES
        .iter()
        .copied()
        .filter(|n| {
            let p = ReconPolicy::from_policy_name(n).expect("every listed name resolves");
            mode_applies(&p, kind) == want
        })
        .collect()
}

// -------------------------------------------------------------------------------------------
// 1. THE INSTANCE-ORIGIN STAMP.
// -------------------------------------------------------------------------------------------

/// This instance's tag.
const OWN: &str = "ap1";
/// An id THIS instance minted: `<origin>V<8-hex session><seq>`.
const OWN_COID: &str = "ap1Vcafef00d0";
/// An id carrying no origin claim at all: eight lowercase hex plus a sequence.
const UNTAGGED_COID: &str = "cafef00d1";
/// An id another instance (`bx2`) minted.
const FOREIGN_COID: &str = "bx2Vdeadbeef0";

/// Distinct powers of two, so the folded total names exactly which fills folded.
const OWN_QTY: f64 = 1.0;
const UNTAGGED_QTY: f64 = 2.0;
const FOREIGN_QTY: f64 = 4.0;

fn own_untagged_and_foreign_fills() -> Vec<FillReport> {
    vec![
        fill_echoing("t-own", OWN_COID, OWN_QTY),
        fill_echoing("t-untagged", UNTAGGED_COID, UNTAGGED_QTY),
        fill_echoing("t-foreign", FOREIGN_COID, FOREIGN_QTY),
    ]
}

/// Send one fills pass and return the snapshot it published. A pass folds or holds every fill it
/// carries inside ONE dispatch, and a snapshot is published only between dispatches, so the first
/// snapshot showing either a position or a held alert shows the whole pass.
fn run_fills_pass(
    handle: &CoreHandle,
    policy: ReconPolicy,
    fills: Vec<FillReport>,
) -> Arc<CoreSnapshot> {
    let cell = handle.snapshot_cell();
    handle.send_command(pass(policy, fills, Vec::new()));
    wait_for_snapshot(&cell, 5, "the fills pass completed", |s| {
        btc_size(s) > 0.0 || !s.recon.alerts.is_empty()
    });
    cell.load_full()
}

/// **1a — a fill naming ANOTHER instance is held under every policy that folds a `MissingFill`,
/// while this instance's own fill and an untagged fill fold beside it.** The pass carries no
/// identity of its own (`local_instance_origin: None`, what every `ReconPolicy` constructor builds),
/// so the core's stamp is the only place the hold can come from.
#[test]
fn a_fill_naming_another_instance_is_held_under_every_policy_that_folds_missing_fills() {
    // PRECONDITIONS: the three ids classify as their names say, relative to OWN.
    let own = InstanceOrigin::parse(OWN).expect("a legal origin tag");
    assert!(vike_model::instance_origin::coid_is_foreign(FOREIGN_COID, Some(&own)));
    assert!(!vike_model::instance_origin::coid_is_foreign(OWN_COID, Some(&own)));
    assert!(!vike_model::instance_origin::coid_is_foreign(UNTAGGED_COID, Some(&own)));
    let folding = policies_where(DivergenceKind::MissingFill, true);
    assert!(
        folding.contains(&"hybrid") && folding.contains(&"synthesize"),
        "PREMISE: the policies this test is about fold a MissingFill by KIND: {folding:?}"
    );

    for name in folding {
        let policy = ReconPolicy::from_policy_name(name).expect("a nameable policy");
        assert!(policy.local_instance_origin.is_none(), "PREMISE: {name} carries no identity");
        let handle = core_minting_under(Some(OWN), None);
        let snap = run_fills_pass(&handle, policy, own_untagged_and_foreign_fills());
        handle.shutdown_and_join();

        assert!(
            is(btc_size(&snap), OWN_QTY + UNTAGGED_QTY),
            "{name}: the own and untagged fills fold and the foreign one does not; size {}",
            btc_size(&snap)
        );
        assert_eq!(snap.recon.alerts.len(), 1, "{name}: exactly the foreign fill is held");
        let held = &snap.recon.alerts[0];
        assert_eq!(held.kind, format!("{:?}", DivergenceKind::MissingFill), "{name}");
        assert!(
            held.detail.contains("placed by ANOTHER INSTANCE (origin=bx2, this instance is ap1)"),
            "{name}: the held row names both instances: {}",
            held.detail
        );
        assert_eq!(held.proposed_event_count, 1, "{name}: the fill alone (its coid is not empty)");
    }
}

/// **1b — the identity is the tag the generator MINTS under, not the configured one.** A resumed
/// session keeps minting under its persisted tag (`ap1`) while configuration now says `zz`; the
/// pass judges by what is on the wire, so a `zz` id is the stranger and an `ap1` id is our own.
#[test]
fn the_stamped_identity_is_the_generators_tag_not_the_configured_one() {
    let handle = core_minting_under(Some("zz"), Some("ap1Vcafef00d"));
    let snap = run_fills_pass(
        &handle,
        ReconPolicy::hybrid(),
        vec![
            fill_echoing("t-own", OWN_COID, OWN_QTY),
            fill_echoing("t-zz", "zzVdeadbeef0", FOREIGN_QTY),
        ],
    );
    handle.shutdown_and_join();

    assert!(
        is(btc_size(&snap), OWN_QTY),
        "the generator's own (ap1) fill folds and the configured tag's (zz) fill is held; size {}",
        btc_size(&snap)
    );
    assert_eq!(snap.recon.alerts.len(), 1, "{:?}", snap.recon.alerts);
    assert!(
        snap.recon.alerts[0].detail.contains("(origin=zz, this instance is ap1)"),
        "{}",
        snap.recon.alerts[0].detail
    );
}

/// **1c — an identity the pass already carries is not overwritten by the stamp.**
/// `ReconPolicy::local_instance_origin` is settable so a caller can supply the identity explicitly;
/// the core stamps only an EMPTY one. With `bx2` supplied, `bx2`'s fill is the pass's own and the
/// core's `ap1` fill is the stranger.
#[test]
fn an_identity_the_pass_already_carries_is_not_overwritten_by_the_stamp() {
    let handle = core_minting_under(Some(OWN), None);
    let mut policy = ReconPolicy::hybrid();
    policy.local_instance_origin = Some(InstanceOrigin::parse("bx2").expect("a legal origin tag"));
    let snap = run_fills_pass(
        &handle,
        policy,
        vec![
            fill_echoing("t-ap1", OWN_COID, OWN_QTY),
            fill_echoing("t-bx2", FOREIGN_COID, FOREIGN_QTY),
        ],
    );
    handle.shutdown_and_join();

    assert!(
        is(btc_size(&snap), FOREIGN_QTY),
        "the supplied identity (bx2) decides: its fill folds, the ap1 fill is held; size {}",
        btc_size(&snap)
    );
    assert_eq!(snap.recon.alerts.len(), 1, "{:?}", snap.recon.alerts);
    assert!(
        snap.recon.alerts[0].detail.contains("(origin=ap1, this instance is bx2)"),
        "{}",
        snap.recon.alerts[0].detail
    );
}

/// **1d — a core minting UNTAGGED ids treats no fill as foreign**, tagged or not: with nothing to
/// compare against there is no stranger (`crates/vike-model/src/instance_origin.rs`'s
/// `coid_is_foreign` answers `false` for a `None` local origin), so the pass is exactly what it was
/// before origins existed.
#[test]
fn a_core_minting_untagged_ids_treats_no_fill_as_foreign() {
    let handle = core_minting_under(None, None);
    let snap = run_fills_pass(&handle, ReconPolicy::hybrid(), own_untagged_and_foreign_fills());
    handle.shutdown_and_join();

    assert!(
        is(btc_size(&snap), OWN_QTY + UNTAGGED_QTY + FOREIGN_QTY),
        "every fill folds; size {}",
        btc_size(&snap)
    );
    assert!(snap.recon.alerts.is_empty(), "nothing is held: {:?}", snap.recon.alerts);
}

// -------------------------------------------------------------------------------------------
// 2. A BALANCE CROSSING UNDER A HOLDING POLICY.
// -------------------------------------------------------------------------------------------

/// The figure the first pass ADOPTS (the account is still `Delta`, so nothing is diffed).
const ADOPTED: f64 = 9_999.0;
/// The figure the second pass reports: +251 against the adopted anchor, far past the default band.
const CROSSED: f64 = 10_250.0;

/// **2 — under a policy that HOLDS a `BalanceDrift` the crossing is an alert, and the `absorbed`
/// ring note does not fire.** The note is read off the resolved alerts, so a held drift suppresses
/// it; the local figure stays at the anchor (the roll-forward writes `expected`, not the venue's
/// number). The holding set is derived from `mode_applies`, the authority.
#[test]
fn a_balance_crossing_under_a_holding_policy_is_held_and_leaves_no_absorbed_note() {
    let holding = policies_where(DivergenceKind::BalanceDrift, false);
    assert!(holding.contains(&"hybrid"), "PREMISE: hybrid holds a BalanceDrift: {holding:?}");
    let tol = ReconConfig::default().balance_tol;
    let band = tol.abs_floor.max(tol.rel_frac * CROSSED.abs());
    assert!(
        (CROSSED - ADOPTED).abs() > band,
        "PREMISE: the second pass is BEYOND the default band"
    );

    for name in holding {
        let cfg = ReconConfig {
            policy: ReconPolicy::from_policy_name(name).expect("a nameable policy"),
            reconcile_balance: true,
            ..ReconConfig::default()
        };
        let handle = core_minting_under(None, None);
        let cell = handle.snapshot_cell();

        handle.send_command(balance_pass(&cfg, Some(ADOPTED)));
        wait_for_snapshot(&cell, 5, "pass 1 adopted the venue figure", |s| {
            s.balance_mode == BalanceMode::Authoritative && is(balance(s), ADOPTED)
        });
        handle.send_command(balance_pass(&cfg, Some(CROSSED)));
        wait_for_snapshot(&cell, 5, "pass 2 held the drift", |s| !s.recon.alerts.is_empty());
        let snap = cell.load_full();
        handle.shutdown_and_join();

        assert_eq!(
            ring_lines_containing(&snap, "absorbed"),
            Vec::<String>::new(),
            "{name}: a HELD crossing leaves no absorbed note; ring = {:?}",
            snap.recent_events
        );
        assert_eq!(snap.recon.alerts.len(), 1, "{name}: {:?}", snap.recon.alerts);
        let held = &snap.recon.alerts[0];
        assert_eq!(held.kind, format!("{:?}", DivergenceKind::BalanceDrift), "{name}");
        assert_eq!(
            held.detail,
            "balance drift binance USDT: venue 10250 vs expected 9999 (+251 unexplained)",
            "{name}"
        );
        assert_eq!(held.proposed_event_count, 1, "{name}: the correcting AccountState, held");
        assert_eq!(
            ring_lines_containing(&snap, "RECON alert"),
            vec![
                "RECON alert #1 BalanceDrift: balance drift binance USDT: venue 10250 vs expected \
                 9999 (+251 unexplained) (awaiting confirm) [account binance]"
                    .to_string()
            ],
            "{name}: the raise note is what the ring says instead"
        );
        assert!(
            is(balance(&snap), ADOPTED),
            "{name}: the local figure stays at the anchor, not the venue's; got {}",
            balance(&snap)
        );
    }
}

// -------------------------------------------------------------------------------------------
// 3. NO BALANCE REPORTED, WITH CASH RECONCILE ON.
// -------------------------------------------------------------------------------------------

/// The ring note a first sync of [`ADOPTED`] leaves on the binance account.
const ADOPTED_NOTE: &str = "RECON balance binance: adopted authoritative 9999 [account binance]";

/// **3 — `BalanceCheck::NotReported` writes nothing.** A pass whose venue reported no balance (not
/// fetched, or the fetch failed) is evidence of neither agreement nor disagreement: on a COLD
/// account it is not a first sync, and on an ANCHORED account it is neither a roll nor a drift. No
/// note, no alert, the balance and its mode untouched.
#[test]
fn a_pass_reporting_no_balance_writes_nothing_with_cash_reconcile_on() {
    let cfg = ReconConfig {
        policy: ReconPolicy::hybrid(),
        reconcile_balance: true,
        ..ReconConfig::default()
    };
    let handle = core_minting_under(None, None);
    let cell = handle.snapshot_cell();

    // Pass 1, on the COLD (`Delta`) account: no balance. Pass 2 then adopts ADOPTED. The ingest
    // queue is FIFO, so once pass 2's adopt is visible pass 1 has run — and had pass 1 written
    // anything, pass 2 would no longer be a plain first sync.
    handle.send_command(balance_pass(&cfg, None));
    handle.send_command(balance_pass(&cfg, Some(ADOPTED)));
    wait_for_snapshot(&cell, 5, "pass 2 adopted the venue figure", |s| {
        s.balance_mode == BalanceMode::Authoritative && is(balance(s), ADOPTED)
    });
    let snap = cell.load_full();
    assert_eq!(
        ring_lines_containing(&snap, "RECON balance"),
        vec![ADOPTED_NOTE.to_string()],
        "the only balance note is pass 2's adopt; ring = {:?}",
        snap.recent_events
    );
    assert!(snap.recon.alerts.is_empty(), "{:?}", snap.recon.alerts);

    // Pass 3, on the ANCHORED account: no balance again.
    let pass2_ts = snap.recon.last_pass_ts;
    handle.send_command(balance_pass(&cfg, None));
    wait_for_snapshot(&cell, 5, "pass 3 completed", |s| s.recon.last_pass_ts > pass2_ts);
    let snap = cell.load_full();
    handle.shutdown_and_join();

    assert_eq!(snap.balance_mode, BalanceMode::Authoritative);
    assert!(is(balance(&snap), ADOPTED), "balance untouched; got {}", balance(&snap));
    assert_eq!(
        ring_lines_containing(&snap, "RECON balance"),
        vec![ADOPTED_NOTE.to_string()],
        "pass 3 added no note; ring = {:?}",
        snap.recent_events
    );
    assert!(snap.recon.alerts.is_empty(), "pass 3 held nothing: {:?}", snap.recon.alerts);
}

// -------------------------------------------------------------------------------------------
// 4. A HELD DIVERGENCE THAT CLEARS AND RETURNS.
// -------------------------------------------------------------------------------------------

/// **4 — a held divergence the venue stops reporting and then reports again is REFRESHED in its
/// surviving row**: same confirm id, no new row, no new ring note. The announcer forgot the
/// identity when it cleared, so this is the one way into the refresh path with `announce` set; the
/// row must still not be duplicated. A genuinely new identity (a second symbol) still gets its own
/// row, id and note.
#[test]
fn a_held_divergence_that_clears_and_returns_keeps_its_row_and_its_confirm_id() {
    let quarantine = || ReconPolicy::from_policy_name("quarantine").expect("a nameable policy");
    let handle = core_minting_under(None, None);
    let cell = handle.snapshot_cell();
    let raise_notes = |s: &CoreSnapshot| ring_lines_containing(s, "RECON alert").len();

    // Raise.
    handle.send_command(pass(quarantine(), Vec::new(), vec![position("BTCUSDT", 0.5)]));
    wait_for_snapshot(&cell, 5, "pass 1 held the position", |s| s.recon.alerts.len() == 1);
    let snap = cell.load_full();
    let first_id = snap.recon.alerts[0].id;
    assert_eq!(raise_notes(&snap), 1);

    // Clear: the venue stops reporting it. The row survives for the operator.
    let ts = snap.recon.last_pass_ts;
    handle.send_command(pass(quarantine(), Vec::new(), Vec::new()));
    wait_for_snapshot(&cell, 5, "pass 2 completed", |s| s.recon.last_pass_ts > ts);
    let snap = cell.load_full();
    assert_eq!(
        snap.recon.alerts.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![first_id],
        "PREMISE: a divergence the venue stops reporting keeps its row"
    );

    // Return: the same divergence again.
    let ts = snap.recon.last_pass_ts;
    handle.send_command(pass(quarantine(), Vec::new(), vec![position("BTCUSDT", 0.5)]));
    wait_for_snapshot(&cell, 5, "pass 3 completed", |s| s.recon.last_pass_ts > ts);
    let snap = cell.load_full();
    assert_eq!(
        snap.recon.alerts.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![first_id],
        "the return refreshes the surviving row under its original confirm id"
    );
    assert_eq!(
        raise_notes(&snap),
        1,
        "...and writes no ring note; ring = {:?}",
        snap.recent_events
    );

    // A new identity beside it.
    let ts = snap.recon.last_pass_ts;
    handle.send_command(pass(
        quarantine(),
        Vec::new(),
        vec![position("BTCUSDT", 0.5), position("ETHUSDT", 3.0)],
    ));
    wait_for_snapshot(&cell, 5, "pass 4 completed", |s| s.recon.last_pass_ts > ts);
    let snap = cell.load_full();
    handle.shutdown_and_join();

    assert_eq!(
        snap.recon.alerts.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![first_id, first_id + 1],
        "a new identity gets its own row and the next confirm id"
    );
    assert_eq!(raise_notes(&snap), 2, "...and its own raise note; ring = {:?}", snap.recent_events);
    assert!(
        ring_lines_containing(&snap, "RECON alert")[1]
            .starts_with(&format!("RECON alert #{} PositionOnlyExternal:", first_id + 1)),
        "{:?}",
        snap.recent_events
    );
}
