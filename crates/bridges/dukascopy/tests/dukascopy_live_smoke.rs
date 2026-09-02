//! LIVE Dukascopy JForex smoke (network + demo creds + built sidecar jar — run manually):
//!
//! ```text
//! cargo test -p vike-dukascopy --test dukascopy_live_smoke -- --ignored --nocapture dukascopy_jforex_login
//! cargo test -p vike-dukascopy --test dukascopy_live_smoke -- --ignored --nocapture dukascopy_jforex_ladder
//! ```
//!
//! Requirements: the sidecar jar installed at `<project>/bin/jforex/jforex-bridge.jar` (it is
//! committed there; `crates/bridges/dukascopy/scripts/provision-jforex.sh` also supplies a JRE at
//! `<project>/bin/jre/`), a JVM from that directory or `JAVA_HOME` or `PATH`, and
//! `DUKASCOPY_DEMO1_LOGIN/PASSWORD` in the credential store (absent creds → the test self-skips,
//! the standard live gate).
//!
//! ⚠ **This file is the COMPOSITION ROOT for the two runtime tool paths**, because it is the only
//! caller of `DukascopyExecutionClient::spawn` in the workspace — `vike_mount::make_engine` has no
//! dukascopy arm (the venue is exec-built but not live-mounted; see
//! `crates/bridges/dukascopy/src/recon_client.rs` for the same "built, not wired" status on the
//! reconcile side). So the env sweep, the project walk and the `resolve_dukascopy_tools` call all
//! happen HERE, in [`live_tools`], exactly as they would in a `make_engine` arm: the library takes
//! the answer as a parameter and resolves nothing. When that arm is written, it inherits the same
//! two lines and this file stops being the only root.
//!
//! Run the two tests separately — each spawns its own sidecar login and Dukascopy may
//! reject concurrent sessions on one demo account.
//!
//! The ladder is weekend-tolerant: FX is closed Fri 22:00 → Sun 22:00 GMT, and a
//! market order then comes back venue-rejected. That still proves the full
//! submit→sidecar→venue→event round trip, so the test logs it and passes; the
//! fill/net-close/cancel legs only run against an open market.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_dukascopy::{
    load_dukascopy_config_from, resolve_dukascopy_tools, DukascopyAccount, DukascopyConfig,
    DukascopyExecutionClient, DukascopyTools,
};
use vike_exec::{event_channel, ExecutionClient, Ingest};
use vike_model::events::Event;
use vike_model::OrderRequest;

/// DEMO1 creds from the workspace `.env`; `None` (skip) when absent — the live gate.
/// `server` is force-blanked: the `.env` SERVER var holds the web-platform login URL,
/// not a JNLP — the client's built-in demo JNLP default is the correct connect target.
/// (The client now also self-defends: a non-`.jnlp` server value is warned about and
/// replaced by the default demo JNLP; blanking here just keeps the smoke explicit.)
fn live_config() -> Option<DukascopyConfig> {
    // DUKASCOPY_SMOKE_ACCOUNT=demo2 switches to the EU demo account (differential
    // testing: separate login counter + server backend from the Swiss DEMO1).
    let account = if std::env::var("DUKASCOPY_SMOKE_ACCOUNT").as_deref() == Ok("demo2") {
        DukascopyAccount::Demo2
    } else {
        DukascopyAccount::Demo1
    };
    let mut cfg = load_dukascopy_config_from(
        account,
        &load_workspace_dotenv_from(settings_dir_override().as_deref()),
    )?;
    cfg.server = String::new();
    Some(cfg)
}

/// `$VIKE_SETTINGS_DIR`, the ONE fact this root pulls out of the process environment by name.
///
/// Spent twice, deliberately: on the credential store above and on the project walk below. Reading
/// it once and threading it is what keeps the two answers from disagreeing — the split
/// `vike_model::state_path::project_bin_dir_from` exists to close, and the one a unit file that
/// relocates the project would otherwise hit.
fn settings_dir_override() -> Option<String> {
    std::env::var("VIKE_SETTINGS_DIR").ok()
}

/// The composition root's one impure act: sweep the environment, walk for the project, and hand
/// `resolve_dukascopy_tools` both. See this file's module doc for why it lives in a test.
///
/// A `make_engine` arm would spell exactly this — its `vars` map plus the same walk — which is the
/// point: the resolution is the CALLER's, and the library takes the result.
fn live_tools() -> DukascopyTools {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let bin =
        vike_model::state_path::project_bin_dir_from(settings_dir_override().as_deref(), &cwd);
    let tools = resolve_dukascopy_tools(&env, bin.as_deref());
    tracing::info!(
        target: "vike_dukascopy",
        jar = %tools.bridge_jar.display(),
        java = %tools.java,
        "resolved sidecar tools"
    );
    tools
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn order(coid: &str, side: i32, order_type: &str, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side,
        qty: 1000.0, // Dukascopy FX minimum
        order_type: order_type.into(),
        price,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: now_ms(),
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

/// Next event off the ingest lane within `secs` (venue round trips are seconds, not ms).
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await })
        .expect("timed out waiting for venue event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

/// LIVE: real JForex login on DEMO1 — spawn returns Ok only after the sidecar's `ready`
/// handshake (account id + balance logged on stderr). Proves jar + java + JNLP + creds.
#[test]
#[ignore = "live: network + demo creds + <project>/bin/jforex/jforex-bridge.jar"]
fn dukascopy_jforex_login() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no DEMO1 creds in workspace .env — skipping");
        return;
    };
    let (events, _rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn(cfg, &live_tools(), events)
        .expect("live JForex login (ready)");
    tracing::info!(target: "vike_dukascopy", "LOGIN OK — JForex session established; detaching");
    client.detach();
}

/// LIVE ladder: market → fill → net-close → resting limit → cancel. Weekend-tolerant:
/// a market-closed rejection ends the test early (round trip still proven).
#[test]
#[ignore = "live: network + demo creds + <project>/bin/jforex/jforex-bridge.jar; full fills need an open market"]
fn dukascopy_jforex_ladder() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no DEMO1 creds in workspace .env — skipping");
        return;
    };
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn(cfg, &live_tools(), events)
        .expect("live JForex login (ready)");
    tracing::info!(target: "vike_dukascopy", "LOGIN OK — starting ladder");

    // Leg 1: market BUY 1000 EURUSD.
    let coid1 = format!("smk{}", now_ms());
    client.submit(&order(&coid1, 1, "market", None));
    match recv_event(&mut rx, 30) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, coid1),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match recv_event(&mut rx, 60) {
        Event::OrderRejected(e) => {
            // Weekend / closed market: the venue answered — full round trip proven.
            tracing::warn!(target: "vike_dukascopy", "VENUE REJECTED (acceptable when market closed): {}", e.reason);
            client.detach();
            return;
        }
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, coid1);
            tracing::info!(target: "vike_dukascopy", "ACCEPTED venue_order_id={:?}", e.venue_order_id);
        }
        other => panic!("expected OrderAccepted or OrderRejected, got {other:?}"),
    }
    // Dual-publish contract (Proto.orderFilled, proven by the dukascopy_exec.rs fake): a
    // fill emits BOTH the bare FillEvent — which the core Account folds into position/PnL —
    // FIRST, then the wrapping OrderFilled the order FSM applies, both carrying the same
    // fill. The bare Fill is therefore the event that arrives here, before the wrap.
    let fill = match recv_event(&mut rx, 60) {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, coid1);
            assert_eq!(fill.venue, "dukascopy");
            assert_eq!(fill.symbol, "EURUSD");
            fill
        }
        other => panic!("expected bare Fill, got {other:?}"),
    };
    match recv_event(&mut rx, 60) {
        Event::OrderFilled(e) => assert_eq!(e.client_order_id, coid1),
        other => panic!("expected OrderFilled wrap, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "FILLED {} @ {}", fill.last_qty, fill.last_px);

    // Leg 2: opposite market SELL nets the position flat (bridge close emulation).
    // Contract: synthetic OrderAccepted(venue_order_id="net:<order>") first, then
    // OrderPartiallyFilled per non-final close leg, and exactly ONE terminal
    // OrderFilled. NOTE the netting may close a STALE opposing position left by an
    // earlier crashed run before this leg's — that's correct netting behavior.
    let coid2 = format!("smk{}", now_ms());
    client.submit(&order(&coid2, -1, "market", None));
    loop {
        match recv_event(&mut rx, 60) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(e.client_order_id, coid2, "net-accept under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-ACCEPTED {:?}", e.venue_order_id);
                continue;
            }
            Event::Fill(f) => {
                // Dual-publish: the bare FillEvent precedes each partial/terminal wrap.
                assert_eq!(f.client_order_id, coid2, "net-close fill under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE FILL {} @ {}", f.last_qty, f.last_px);
                continue;
            }
            Event::OrderPartiallyFilled(e) => {
                assert_eq!(e.client_order_id, coid2);
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE LEG {} @ {}", e.fill.last_qty, e.fill.last_px);
                continue;
            }
            Event::OrderFilled(e) => {
                assert_eq!(e.client_order_id, coid2, "close fill under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-CLOSED {} @ {}", e.fill.last_qty, e.fill.last_px);
                break;
            }
            other => panic!("expected net-close lifecycle event, got {other:?}"),
        }
    }

    // Leg 3: resting limit far below market, then cancel.
    let coid3 = format!("smk{}", now_ms());
    client.submit(&order(&coid3, 1, "limit", Some(0.9000)));
    loop {
        match recv_event(&mut rx, 60) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(e.client_order_id, coid3);
                tracing::info!(target: "vike_dukascopy", "LIMIT RESTING — canceling");
                break;
            }
            other => panic!("expected limit OrderAccepted, got {other:?}"),
        }
    }
    client.cancel(&coid3);
    match recv_event(&mut rx, 60) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, coid3),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "LADDER GREEN: login -> market fill -> net close -> limit rest -> cancel");
    client.detach();
}
