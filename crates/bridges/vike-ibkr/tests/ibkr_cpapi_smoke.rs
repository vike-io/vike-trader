//! Live PAPER smoke for the **cpapi** backend — places and cancels ONE real order on the IBKR PAPER
//! account (DUQ186573) through a running, browser-authenticated Client Portal Gateway. Double-gated:
//! `#[ignore]` (never in the default run) AND self-skips unless a DEMO config exists AND the gateway
//! is reachable. NEVER targets the live account. Run explicitly (with the CP Gateway up + logged in):
//!   cargo test -p vike-ibkr --features ibkr --test ibkr_cpapi_smoke -- --ignored --nocapture
//!
//! This is the vehicle for a human to LIVE-VERIFY the cpapi wire shapes that are unverified guesses
//! in `transport/cpapi/*` (endpoint paths, the WS URL + `sor` subscribe frame, the reply-confirm
//! JSON, the cancel-key identity, conId resolution). If submit→accept→cancel round-trips here, those
//! guesses hold; if not, the `--nocapture` output + the gateway's own order log show what to correct.
#![cfg(feature = "ibkr")]

use std::time::{Duration, Instant};

use tokio::sync::mpsc::error::TryRecvError;
use vike_bridge_core::credentials::Environment;
use vike_exec::lanes::{event_channel, Ingest};
use vike_exec::ExecutionClient;
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::load_workspace_dotenv_from;
use vike_ibkr::IbkrBackend;
use vike_model::events::Event;
use vike_model::OrderRequest;

#[test]
#[ignore = "live paper: needs a running, authenticated CP Gateway for DUQ186573"]
fn cpapi_paper_place_and_cancel() {
    vike_log::test_init();

    let Some(mut cfg) = load_ibkr_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    ) else {
        eprintln!("skip: no IBKR_DEMO_* config in .env");
        return;
    };
    cfg.backend = IbkrBackend::Cpapi; // force cpapi regardless of IBKR_DEMO_BACKEND
                                      // HARD SAFETY: refuse to run against anything but a paper account — the structural guarantee
                                      // this smoke can never touch the live U-account.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );

    // HARD SAFETY (cpapi-specific): the `.env`-string check above cannot see which account the CP
    // Gateway is actually LOGGED INTO — cpapi routes orders by the gateway's authenticated SESSION,
    // not `cfg.account`. So verify the SESSION account is a paper `DU…` via a read-only
    // `GET /iserver/accounts` before placing anything. A live `U…` session → refuse (hard fail); an
    // unreachable/unauthenticated gateway → skip. (Live-verified 2026-07-15 the gateway can be on the
    // LIVE account even when `.env` says DU.)
    let rest = vike_ibkr::transport::CpapiRest::new(&cfg.cpapi_url, &cfg.account);
    match rest.accounts() {
        Ok(v) => {
            let session_acct = v
                .get("accounts")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .unwrap_or("");
            assert!(
                session_acct.starts_with("DU"),
                "cpapi gateway SESSION is on '{session_acct}', not a paper DU\u{2026} account — \
                 refusing to place an order (log the gateway into the paper account first)"
            );
            eprintln!("session-account guard OK: gateway session is {session_acct}");
        }
        Err(e) => {
            eprintln!(
                "skip: could not read gateway /iserver/accounts (unreachable/unauth?): {e:?}"
            );
            return;
        }
    }

    let (events, mut rx) = event_channel(256);
    let mut client = match vike_ibkr::IbkrExecutionClient::connect(&cfg, events) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: CP Gateway unreachable/unauthenticated: {e}");
            return;
        }
    };

    // A far-from-market LMT BUY: tiny qty, priced miles below market so it rests instead of filling.
    let coid = format!("ibkr-cpapi-smoke-{}", std::process::id());
    let order = OrderRequest {
        client_order_id: coid.clone(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1, // buy
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0), // absurdly far below market so it rests, never fills
        ..Default::default()
    };
    println!("submitting {order:?}");
    client.submit(&order);

    // Deadline-bounded drain (blocking_recv has no internal timeout; a degraded gateway must not
    // hang the smoke past its deadline).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_submitted = false;
    let mut saw_accepted = false;
    while Instant::now() < deadline && !(saw_submitted && saw_accepted) {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderSubmitted(e))) if e.client_order_id == coid => {
                println!("got OrderSubmitted: {e:?}");
                saw_submitted = true;
            }
            Ok(Ingest::Event(Event::OrderAccepted(e))) if e.client_order_id == coid => {
                println!("got OrderAccepted: {e:?}");
                saw_accepted = true;
            }
            Ok(Ingest::Event(Event::OrderRejected(e))) => {
                eprintln!(
                    "got OrderRejected: {e:?} — inspect the gateway's reply-confirm behaviour"
                );
                break;
            }
            Ok(Ingest::Event(other)) => println!("note: other event: {other:?}"),
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                eprintln!("skip: event channel closed while waiting for accept");
                client.detach();
                return;
            }
        }
    }
    if saw_submitted && saw_accepted {
        println!("OK: cpapi submit -> accept round-trip observed for coid {coid}");
    } else {
        eprintln!(
            "diagnostic: did not observe both OrderSubmitted and OrderAccepted within 15s \
             (submitted={saw_submitted}, accepted={saw_accepted}) — the cpapi wire shapes likely \
             need correcting; inspect --nocapture output + the gateway order log"
        );
    }

    println!("cancelling {coid}");
    client.cancel(&coid);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_canceled = false;
    while Instant::now() < deadline && !saw_canceled {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderCanceled(e))) if e.client_order_id == coid => {
                println!("got OrderCanceled: {e:?}");
                saw_canceled = true;
            }
            Ok(Ingest::Event(other)) => println!("note: other event: {other:?}"),
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    if saw_canceled {
        println!("OK: cpapi cancel round-trip observed for coid {coid}");
    } else {
        eprintln!("diagnostic: no OrderCanceled within 15s for {coid} — check the cancel-key identity guess");
    }

    client.detach();
}
