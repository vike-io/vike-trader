//! Live demo smoke: this bridge OPENS a position and then CLOSES it, and the account ends FLAT.
//!
//!     cargo test -p vike-ig --test ig_close_position_smoke -- --ignored --nocapture
//!
//! ⚠ **This one PLACES REAL ORDERS on the IG DEMO account** — unlike the crate's other two smokes,
//! which are read-only. It is the only proof that exists for the close path: `POST /positions/otc`
//! + `_method: DELETE` is a wire shape no offline test can validate, and IG's own behaviour around
//!   it is not what the documentation suggests (a real HTTP `DELETE` with the same body answers 400
//!   `validation.null-not-allowed.request` — see `IgSession::post_method_delete`).
//!
//! It drives the PRODUCTION bytes: [`vike_ig::build_request`] and [`vike_ig::build_close_request`]
//! build the bodies, [`vike_ig::IgSession`] sends them, [`vike_ig::map_confirm`] maps the replies.
//! A smoke that hand-rolled its own JSON would prove IG's endpoint works and nothing about this
//! bridge.
//!
//! **It leaves the account FLAT**, and says so out loud: the position count is read before opening
//! and asserted equal afterwards, and the close is attempted even if an assertion about the open
//! already failed, so a mid-test panic cannot strand a live position.
//!
//! Double-gated like every other `*_smoke.rs` (absent `IG_DEMO_*` creds, then a failed demo login —
//! a rotated password must not read as a red), plus a THIRD gate this one needs: the market must be
//! TRADEABLE. FX is shut at the weekend, and a smoke that reddens on a Sunday is one people learn
//! to ignore.

use serde_json::Value;
use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_ig::{build_close_request, build_request, load_ig_config_from, IgSession};
use vike_model::events::Event;
use vike_model::OrderRequest;

/// EUR/USD mini — present on the demo gateway, and the smallest dealable size IG offers there
/// (`dealingRules.minDealSize` = 0.1, read live on 2026-08-21).
const EPIC: &str = "CS.D.EURUSD.MINI.IP";
const SIZE: f64 = 0.1;

fn cfg() -> Option<vike_ig::IgConfig> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    load_ig_config_from(Environment::Demo, &vars)
}

fn market_order(coid: &str, side: i32, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ig".into(),
        symbol: EPIC.into(),
        side,
        qty: SIZE,
        order_type: "market".into(),
        reduce_only,
        ts: 1,
        ..Default::default()
    }
}

/// How many open deals the account holds in `EPIC` right now.
fn deals_open(session: &IgSession) -> usize {
    let body = session.get("/positions", "2", "").expect("GET /positions");
    body.get("positions")
        .and_then(|p| p.as_array())
        .map(|rows| {
            rows.iter()
                .filter(|r| r.pointer("/market/epic").and_then(Value::as_str) == Some(EPIC))
                .count()
        })
        .unwrap_or(0)
}

/// Send a built deal request and resolve its confirm. `method_delete` picks the close verb.
fn deal(
    session: &IgSession,
    built: (String, &'static str, Value),
    method_delete: bool,
) -> (String, Value) {
    let (path, version, body) = built;
    println!(
        "  -> {} {path} (Version {version})",
        if method_delete { "POST+_method:DELETE" } else { "POST" }
    );
    println!("     body: {body}");
    let posted = if method_delete {
        session.post_method_delete(&path, version, &body)
    } else {
        session.post(&path, version, &body)
    };
    let resp = posted.unwrap_or_else(|e| panic!("{path} failed: {e}"));
    let deal_ref = resp
        .get("dealReference")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no dealReference in {resp}"))
        .to_string();
    println!("     dealReference: {deal_ref}");
    // IG needs a beat to process a reference; the production path retries for this same reason.
    std::thread::sleep(std::time::Duration::from_millis(800));
    let confirm = session
        .get(&format!("/confirms/{deal_ref}"), "1", "")
        .unwrap_or_else(|e| panic!("GET /confirms/{deal_ref} failed: {e}"));
    println!("     confirm: {confirm}");
    (deal_ref, confirm)
}

fn bare_fill(events: &[Event]) -> &vike_model::events::FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a bare Fill among {events:?}"))
}

#[test]
#[ignore = "live DEMO; PLACES REAL ORDERS — needs IG_DEMO_* creds and an open FX market"]
fn ig_open_then_close_leaves_the_account_flat() {
    let Some(c) = cfg() else {
        eprintln!("skip: no IG_DEMO_* creds in the credential store");
        return;
    };
    let Ok(session) = IgSession::login(&c) else {
        eprintln!("skip: IG demo login failed (rotated password?) — not a red");
        return;
    };
    println!("logged in: account {}", session.account_id);

    let market = session.get(&format!("/markets/{EPIC}"), "3", "").expect("GET /markets");
    let status = market.pointer("/snapshot/marketStatus").and_then(Value::as_str).unwrap_or("");
    if status != "TRADEABLE" {
        eprintln!(
            "skip: {EPIC} is {status}, not TRADEABLE (FX is shut at the weekend) — not a red"
        );
        return;
    }

    let before = deals_open(&session);
    println!("open deals in {EPIC} before: {before}");

    // ---- OPEN: the production body, forceOpen: true, its own deal ---------------------------
    println!("OPEN {SIZE} BUY:");
    let (_open_ref, open_confirm) =
        deal(&session, build_request(&market_order("smoke-open", 1, false)), false);
    let open_events = vike_ig::map_confirm("smoke-open", 1, true, &open_confirm);
    let open_fill = bare_fill(&open_events).clone();

    // ---- CLOSE: this is the whole point ------------------------------------------------------
    // Attempted UNCONDITIONALLY, before any assertion about the open, so a failed expectation can
    // never leave a live position behind.
    println!("CLOSE {SIZE} SELL (reduce_only):");
    let (close_ref, close_confirm) =
        deal(&session, build_close_request(&market_order("smoke-close", -1, true)), true);
    let close_events = vike_ig::map_confirm("smoke-close", 2, true, &close_confirm);

    let after = deals_open(&session);
    println!("open deals in {EPIC} after: {after}");

    // ---- now assert -------------------------------------------------------------------------
    assert_eq!(after, before, "THE ACCOUNT MUST BE LEFT FLAT — open deals before != after");
    assert_eq!(
        close_confirm.get("dealStatus").and_then(Value::as_str),
        Some("ACCEPTED"),
        "the close was accepted: {close_confirm}"
    );
    assert_eq!(
        close_confirm.get("status").and_then(Value::as_str),
        Some("CLOSED"),
        "and IG reports the position CLOSED, not opened: {close_confirm}"
    );

    let close_fill = bare_fill(&close_events);
    assert_eq!(close_fill.side, -1, "the close books as a SELL");
    assert!((close_fill.last_qty - SIZE).abs() < 1e-12, "for the size closed");
    // ⚠ The live-proven trap: IG reuses the closed deal's `dealId` on the close confirm, so this is
    // the assertion that the close will actually BOOK rather than being deduped into its own open.
    assert_ne!(
        open_fill.trade_id,
        close_fill.trade_id,
        "open dealId={:?} close dealId={:?} — the close fill must not collide with the open's",
        open_confirm.get("dealId"),
        close_confirm.get("dealId")
    );
    assert_eq!(close_fill.trade_id, close_ref.as_str(), "keyed on the close's own dealReference");
    println!(
        "OK: open trade_id={} close trade_id={} — distinct, account flat",
        open_fill.trade_id.as_str(),
        close_fill.trade_id.as_str()
    );
}
