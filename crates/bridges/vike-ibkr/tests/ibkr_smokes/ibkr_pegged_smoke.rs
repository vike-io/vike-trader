//! LIVE PAPER smoke - the wave-5 native pegged order through the bridge, on the IB Gateway socket: one row per anchor,
//! each a real paper BUY of 1 AAPL pegged FAR below the market (a passive offset of [`OFFSET`]), placed, read back
//! through the restart reconcile's own client, and cancelled. NO ROW TRADES: each rests 50 below its anchor, or IB
//! refuses the passive offset, and the table carries IB's words.
//!
//! ⚠ **FIRST RUN 2026-11-09 (paper #12):** `PEG MID` was accepted, read back and cancelled; `PEG MKT` got no order status
//! in 20 s (its cancel was answered); `REL` and `PEG BEST` got no answer to the submit or the cancel. Since then the BRIDGE
//! refuses `PEG MKT`, `REL` and `PEG BEST` by name (`src/execution/order.rs`'s
//! `a_mid_peg_is_peg_mid_with_its_offset_in_ibs_sign_and_every_other_anchor_is_refused`), so a run now shows those rows
//! REFUSED with its words and nothing is sent for them. Offline as well: `src/transport/socket.rs`'s
//! `the_mid_peg_is_built_as_mapped`, `src/reconcile/cpapi.rs`'s
//! `every_order_type_spelling_maps_to_its_twin_and_stop_limit_reads_as_a_stop_limit`. The `ibkr` row lists `pegged`
//! behind this pass, in its own commit, for the mid anchor alone: the other three are the bridge's refusals.
//!
//! What only a gateway can answer, one line each in the printed `PEGGED` table:
//!
//! 1. **TWS takes each form**: `PEG MID` (the offset negated as `auxPrice`, no `lmtPrice`): `OrderAccepted`, or an
//!    `OrderRejected` carrying IB's words; an unlisted rejection code is the core watchdog's, and this smoke reports it as
//!    NO ANSWER (the bridge logs IB's notice on the order at INFO). The `PEG MKT`, `REL` and `PEG BEST` rows read REFUSED
//!    with the bridge's words while it refuses them.
//! 2. **The reconcile reads each back**: the socket recon client's open orders list the order client id as `Pegged` (`READ BACK`),
//!    or say what they list instead.
//! 3. **The cancel ends each**: `OrderCanceled`.
//!
//! It dials the exec client under a client id of its own (from the clock, never one the daemon dials) and the
//! reconcile client two ids above it; the order client ids are `b2w5…`. The only hard failures are a target that is not a paper
//! `DU...` account and a run in which no submit was answered at all. A run that prints no `PEGGED` table tested nothing
//! (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES AND CANCELS REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. On a the CI box lane (the gateway is on the box), on
//! 2026-11-09 or later, in AAPL's regular session:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_pegged_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_execution::recon::ReconcileClient;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::connect_execution_client;
use vike_ibkr::reconcile::socket::ReconcileClientIbkrSocket;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderPeg, OrderPegAnchor, OrderRequest, OrderType};
use vike_venues::IbkrBackend;

/// The user name (decision 0290); the box's kept ibkr row (`AAPL.SMART.USD`) is what the bridge sends.
const SYMBOL: &str = "AAPL";
/// The same stock routed to IBKR ATS, the one venue IB takes a `PEG BEST` on. ⚠ Under decision 0290 the bridge refuses
/// it (a wire name is no user name, and a second row of `AAPL` would make the name name two rows): until the route is
/// an order attribute, not a name, that leg answers a refusal (deferred with the SYM-NAMES rename).
const ATS_SYMBOL: &str = "AAPL.IBKRATS.USD.STK";
/// Passive (a buy pegged this far BELOW its anchor): far from any fill in a run.
const OFFSET: f64 = -50.0;

/// A client id this gateway has not seen from us: from the clock (the daemon's ids are small and configured).
fn fresh_id() -> i32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::common::SMOKE_CLIENT_ID_BASE + 8_000 + (secs % 8_000) as i32
}

type Verdict = (&'static str, String);

/// The first event that `pick` answers, within `window`: `(verdict, detail)`, or NO ANSWER.
fn wait_for(
    rx: &mut Receiver<Ingest>,
    window: Duration,
    pick: impl Fn(&Event) -> Option<Verdict>,
) -> Verdict {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                if let Some(answer) = pick(&e) {
                    return answer;
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    ("NO ANSWER", format!("nothing within {} s", window.as_secs()))
}

fn placed(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(20), |e| match e {
        Event::OrderAccepted(a) if a.order_client_id == order_client_id => {
            Some(("ACCEPTED", "OrderAccepted".into()))
        }
        Event::OrderFilled(f) if f.order_client_id == order_client_id => {
            Some(("FILLED", "OrderFilled: IT TRADED, sell 1 AAPL back by hand".into()))
        }
        Event::OrderRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderRejected: {}", r.reason)))
        }
        _ => None,
    })
}

fn cancelled(rx: &mut Receiver<Ingest>, order_client_id: &str) -> Verdict {
    wait_for(rx, Duration::from_secs(10), |e| match e {
        Event::OrderCanceled(c) if c.order_client_id == order_client_id => {
            Some(("CANCELED", "OrderCanceled".into()))
        }
        Event::OrderCancelRejected(r) if r.order_client_id == order_client_id => {
            Some(("REFUSED", format!("OrderCancelRejected: {}", r.reason)))
        }
        _ => None,
    })
}

/// What the reconcile's open-order read says of `order_client_id`: `READ BACK` when it lists it as a pegged order.
fn read_back(recon: Option<&ReconcileClientIbkrSocket>, order_client_id: &str) -> Verdict {
    let Some(recon) = recon else {
        return ("NO READ", "the recon client did not connect".into());
    };
    match recon.fetch_order_status_reports(0) {
        Ok(rows) => {
            match rows.iter().find(|r| r.order_client_id.as_deref() == Some(order_client_id)) {
                Some(r) if r.order_type == OrderType::Pegged => {
                    ("READ BACK", format!("{:?}, {:?}, qty {}", r.order_type, r.status, r.qty))
                }
                Some(r) => ("WRONG KIND", format!("listed as {:?}, wanted Pegged", r.order_type)),
                None => (
                    "NOT LISTED",
                    format!("{} open orders, none under {order_client_id}", rows.len()),
                ),
            }
        }
        Err(e) => ("NO READ", format!("fetch_order_status_reports: {e}")),
    }
}

#[test]
#[ignore = "live paper, Monday 2026-11-09 or later: places, reads back and cancels real paper pegged orders (PEG MID, PEG MKT, REL, PEG BEST) pegged far below the market; needs a running Gateway logged into a DU... account"]
fn ibkr_pegged_place_read_back_cancel() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut config) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    if config.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; a pegged order goes through the IB Gateway socket only"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    config.client_id = fresh_id();
    config.reconcile_client_id = config.client_id + 2;
    let (events, mut rx) = event_channel(256);
    let mut client = match connect_execution_client(&config, events) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("skip: the bridge could not connect (client id {}): {e}", config.client_id);
            return;
        }
    };
    let recon = ReconcileClientIbkrSocket::connect(&config, SYMBOL)
        .map_err(|e| eprintln!("the recon client did not connect: {e}"))
        .ok();

    let pegged = |order_client_id: &str, symbol: &str, anchor| OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor, offset: OFFSET }),
        ..Default::default()
    };
    let rows = [
        ("PEG MID buy", pegged("b2w5pmid", SYMBOL, OrderPegAnchor::Mid)),
        ("PEG MKT buy", pegged("b2w5pmkt", SYMBOL, OrderPegAnchor::Market)),
        ("REL buy", pegged("b2w5prel", SYMBOL, OrderPegAnchor::Primary)),
        ("PEG BEST buy, IBKRATS", pegged("b2w5pbst", ATS_SYMBOL, OrderPegAnchor::Best)),
    ];

    let mut table: Vec<(String, Verdict)> = Vec::new();
    for (row, order) in &rows {
        let order_client_id = order.order_client_id.as_str();
        client.submit(order);
        let verdict = placed(&mut rx, order_client_id);
        let accepted = verdict.0 == "ACCEPTED";
        table.push((format!("{row}: submit"), verdict));
        if accepted {
            std::thread::sleep(Duration::from_secs(1)); // the gateway's open-order table catches up
            table.push((format!("{row}: reconcile"), read_back(recon.as_ref(), order_client_id)));
        }
        // Cancel whatever the answer was: an order IB holds must not be left resting.
        client.cancel(order_client_id);
        table.push((format!("{row}: cancel"), cancelled(&mut rx, order_client_id)));
    }
    client.detach();

    eprintln!("PEGGED (AAPL, paper; buy 1, offset {OFFSET}: passive, below each anchor)");
    for (step, (verdict, detail)) in &table {
        eprintln!("  {step:<32}  {verdict:<11}  {detail}");
    }
    assert!(
        table
            .iter()
            .any(|(step, (verdict, _))| step.ends_with("submit") && *verdict != "NO ANSWER"),
        "no submit was answered: nothing was measured"
    );
}
