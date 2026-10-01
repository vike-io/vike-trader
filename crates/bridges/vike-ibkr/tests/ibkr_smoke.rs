//! Live PAPER smoke — places and cancels ONE real order on the IBKR PAPER account (DUQ186573).
//! Double-gated: `#[ignore]` (never in the default run) AND self-skips unless a DEMO config exists
//! AND a Gateway/TWS is reachable. NEVER targets the live account. Run explicitly:
//!   cargo test -p vike-ibkr --features "ibkr test-support" --test ibkr_smoke -- --ignored --nocapture
//!
//! This is the vehicle for a human to live-verify four Task-9/10 items that cannot be exercised
//! against a `FakeTransport` (see `tests/ibkr_lifecycle.rs`) because they depend on real Gateway
//! behaviour:
//!
//!   1. **Hard-rejection delivery channel.** `event_mapper::on_error`'s `OrderRejection` arm
//!      (`src/event_mapper.rs`) notes that ibapi's global `order_update_stream` drops the order id
//!      from order-rejection Notices — an ASYNC hard rejection (delivered AFTER `submit_order`
//!      returned `Ok`) arrives as `on_error(code, 0, msg)`, an id-less path attributed to the
//!      order only when exactly one order is unacked. Watch stderr/trace for `unroutable IBKR
//!      order-rejection notice` if this smoke ever races a rejection; none is expected for a
//!      resting limit far from the market, but a human should know to look.
//!   2. **Fill timestamp/symbol echo.** If a fill arrives (it should not, given the far-off limit),
//!      confirm `FillEvent.ts`/`.symbol` reflect the Gateway's own execDetails, not a synthesized
//!      local clock/symbol.
//!      ⚠ **This item cannot be satisfied by running this test, and never could.** The order is
//!      priced far from the market precisely so it cannot fill, so "if a fill arrives" is a branch
//!      this smoke is built never to take. It is left here because the QUESTION is real and still
//!      open; what would actually answer it — a marketable order, on a box that can reach an IB
//!      Gateway, during an open session — is spelled out in `src/transport/socket.rs`'s module doc.
//!      Do not record this item as confirmed on the strength of a green run.
//!   3. **`OpenOrder.order_ref` (coid) round-trip.** The accepted order's `client_order_id` on the
//!      emitted `OrderAccepted`/`OrderCanceled` events must be the SAME coid this test submitted —
//!      proving `orderRef` survives the Gateway round-trip intact (the reconnect resync path in
//!      `tests/ibkr_lifecycle.rs` depends on this same field being populated correctly on replay).
//!   4. **`AccountsReady` / readiness.** The bridge does not accept `submit` before the Gateway has
//!      signalled account readiness; confirm no hang/deadlock waiting on that handshake against a
//!      real Gateway (the FakeTransport always scripts it immediately, which cannot prove the real
//!      transport's timing is sane).
//!      ⚠ **This item asked for a gate that DID NOT EXIST until 2026-08-23**, so anyone who
//!      "confirmed" it confirmed nothing: `src/id_registry.rs`'s `is_connected` was
//!      `#[allow(dead_code)]` with no caller, and `run_exec`'s submit arm allocated an id from the
//!      101 floor and placed immediately. The gate is now real —
//!      `src/exec.rs`'s `submit_refusal` — and it is a BOUNDED wait (`READINESS_WAIT`) that
//!      terminally rejects rather than blocking, which is what makes the hang this item asks about
//!      structurally impossible. What a live run still adds: whether a real Gateway ever takes long
//!      enough over the handshake to reach that bound, which no `FakeTransport` can tell you.
use std::time::{Duration, Instant};

use tokio::sync::mpsc::error::TryRecvError;
use vike_bridge_core::credentials::Environment;
use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_exec::ExecutionClient;
use vike_exec::lanes::{Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::OrderRequest;
use vike_model::events::Event;

#[test]
#[ignore = "live paper: needs a running Gateway logged into DUQ186573"]
fn paper_place_and_cancel() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let Some(cfg) = load_ibkr_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(settings_dir.as_deref()),
        &ibkr_settings(settings_dir.as_deref()),
    ) else {
        eprintln!("skip: no IBKR_DEMO_* config in .env");
        return;
    };
    // HARD SAFETY: refuse to run against anything but a paper account. This assert is the ONE
    // check in this file that must remain a hard failure, never a diagnostic — it is the
    // structural guarantee that this smoke cannot touch the live U-account.
    assert!(
        cfg.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );

    let (events, mut rx) = event_channel(256);
    let mut client = match vike_ibkr::IbkrExecutionClient::connect(&cfg, events) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: Gateway unreachable: {e}");
            return;
        }
    };

    // A far-from-market LMT BUY on a liquid symbol: tiny qty so it can never move the paper
    // account meaningfully, and priced miles below the market so it rests instead of filling.
    let coid = format!("ibkr-smoke-{}", std::process::id());
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

    // Deadline-bounded drain: `blocking_recv` has no internal timeout, and the exec-actor thread
    // can hold a live `EventSender` clone (channel never closes) while stalled on a degraded
    // Gateway connection — a bare `blocking_recv` would then block past the 10s deadline
    // indefinitely. `try_recv` + a short sleep, with the deadline checked every iteration, keeps
    // this smoke genuinely bounded.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_submitted = false;
    let mut saw_accepted = false;
    while Instant::now() < deadline && !(saw_submitted && saw_accepted) {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderSubmitted(e))) => {
                println!("got OrderSubmitted: {e:?}");
                if e.client_order_id == coid {
                    saw_submitted = true;
                } else {
                    eprintln!("note: OrderSubmitted for a different coid: {}", e.client_order_id);
                }
            }
            Ok(Ingest::Event(Event::OrderAccepted(e))) => {
                println!("got OrderAccepted: {e:?}");
                if e.client_order_id == coid {
                    saw_accepted = true;
                } else {
                    eprintln!("note: OrderAccepted for a different coid: {}", e.client_order_id);
                }
            }
            Ok(Ingest::Event(Event::OrderRejected(e))) => {
                eprintln!("got OrderRejected (item 1 — hard rejection): {e:?}");
                break;
            }
            Ok(Ingest::Event(other)) => {
                println!("note: other event while waiting for accept: {other:?}");
            }
            Ok(_) => {} // non-Event Ingest (Market/Command/etc.) — ignore
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                eprintln!("skip: event channel closed while waiting for accept");
                client.detach();
                return;
            }
        }
    }
    if !saw_submitted || !saw_accepted {
        eprintln!(
            "diagnostic: did not observe both OrderSubmitted and OrderAccepted within 10s \
             (submitted={saw_submitted}, accepted={saw_accepted}) — inspect --nocapture output \
             above; this is a smoke, not asserting hard failure"
        );
    } else {
        println!("OK: submit -> accept round-trip observed for coid {coid}");
    }

    // Cancel regardless of whether we confirmed acceptance — a resting order should still be
    // cancelable, and this also exercises teardown cleanly if the accept wait timed out.
    println!("cancelling {coid}");
    client.cancel(&coid);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_canceled = false;
    while Instant::now() < deadline && !saw_canceled {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderCanceled(e))) => {
                println!("got OrderCanceled: {e:?}");
                if e.client_order_id == coid {
                    saw_canceled = true;
                } else {
                    eprintln!("note: OrderCanceled for a different coid: {}", e.client_order_id);
                }
            }
            Ok(Ingest::Event(other)) => {
                println!("note: other event while waiting for cancel: {other:?}");
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => {
                eprintln!("skip: event channel closed while waiting for cancel");
                break;
            }
        }
    }
    if saw_canceled {
        println!("OK: cancel round-trip observed for coid {coid} (item 3: coid survived intact)");
    } else {
        eprintln!(
            "diagnostic: did not observe OrderCanceled within 10s for coid {coid} — inspect \
             --nocapture output above; check the paper account's TWS/Gateway order log directly"
        );
    }

    client.detach();
}

/// The venue's gateway rows — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` (decision 0095) — read from
/// the settings database in the same settings directory the credential read above resolves.
///
/// ⚠ LOUD, never a quiet default: a gateway setting left in the credential store under its old
/// name (which the daemons refuse to start on) or a database that will not read would otherwise
/// hand this smoke the built-in gateway — the self-skip that prints `ok` having tested nothing.
fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    let stranded: Vec<(String, String)> =
        vike_secrets::venue_setting::stranded_venue_setting_names(
            load_workspace_dotenv_from(settings_dir_override).keys().map(String::as_str),
        )
        .into_iter()
        .filter(|(_, key)| key.starts_with("venue.ibkr."))
        .collect();
    assert!(
        stranded.is_empty(),
        "the credential store holds IBKR gateway settings under their old names, which nothing \
         reads: {stranded:?} — run `vike-cli secrets move-venue-config` first"
    );
    vike_model::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("ibkr"))
    .unwrap_or_default()
}
