//! Live demo PROBE: what does IG's close endpoint (`POST /positions/otc` + `_method: DELETE`)
//! actually DO with `orderType: LIMIT` (and `STOP`) and a `level`?
//!
//!     cargo test -p vike-ig --test ig_limit_close_probe -- --ignored --nocapture
//!
//! The question `exec.rs`'s `CLOSE_MARKET_ONLY` refusal left open (PR #1431): does a LIMIT close
//! (1) REST until the level trades, (2) reject when the level is not marketable, or (3) execute at
//! market regardless? Only the venue can answer, so this places REAL orders on the IG DEMO account
//! and prints every request body and reply VERBATIM (IG demo data is not secret; credentials never
//! reach stdout — nothing here prints the config).
//!
//! ⚠ **The answer is MEASURED (demo, 2026-08-22): (2) — the close endpoint cannot rest.** A
//! non-marketable LIMIT close is REJECTED (`LIMIT_ORDER_WRONG_SIDE_OF_MARKET`) with nothing
//! resting and the position untouched; a marketable one executes immediately at level-or-better;
//! `orderType: STOP` is refused at the gateway (400 `invalid.request.orderType`). The verdict and
//! its verbatim wire table live in `tests/close_limit_never_rests.rs` (over
//! `fixtures/confirm_close_limit_rejected.json`); this probe stays in the tree as the way to
//! RE-ASK the venue if IG's close endpoint ever grows a resting arm.
//!
//! Sequence, leaving the account FLAT:
//!   1. find a TRADEABLE epic (EURUSD mini on a weekday; IG's weekend/crypto markets otherwise —
//!      FX is shut at the weekend and the probe must run regardless),
//!   2. OPEN a minimum-size position via the production `build_request` bytes,
//!   3. PROBE A — LIMIT close at a NON-marketable level (a SELL close levelled ABOVE the offer:
//!      restable, not immediately executable, well inside any level tolerance),
//!      then read `/confirms`, `/workingorders` and `/positions` to see what it did,
//!   4. PROBE C — STOP close (IG's close endpoint documents no STOP arm; capture the refusal),
//!   5. PROBE B — LIMIT close at a MARKETABLE level (SELL levelled BELOW the bid), which under
//!      execute-at-level-or-better semantics is also the cleanup close,
//!   6. GUARANTEE FLAT: market-close any remaining size via the production `build_close_request`
//!      bytes, cancel anything the probe left resting, and assert the open-deal count is back to
//!      where it started.
//!
//! Triple-gated like `ig_close_position_smoke.rs`: absent `IG_DEMO_*` creds → skip; failed demo
//! login → skip; no TRADEABLE market → skip (reported honestly, never forced).

use serde_json::{json, Value};
use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_ig::{build_close_request, build_request, load_ig_config_from, IgSession};
use vike_model::OrderRequest;

/// Preferred epic on a weekday. `find_tradeable` falls back to searching IG's weekend/crypto
/// markets when this one is shut.
const WEEKDAY_EPIC: &str = "CS.D.EURUSD.MINI.IP";

fn cfg() -> Option<vike_ig::IgConfig> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    load_ig_config_from(Environment::Demo, &vars)
}

fn market_order(coid: &str, epic: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ig".into(),
        symbol: epic.into(),
        side,
        qty,
        order_type: "market".into(),
        reduce_only,
        ts: 1,
        ..Default::default()
    }
}

/// One tradeable market, with everything the probe needs read off `GET /markets/{epic}` v3.
struct Market {
    epic: String,
    bid: f64,
    offer: f64,
    min_size: f64,
    decimals: usize,
    currency: String,
}

fn market_details(session: &IgSession, epic: &str) -> Option<Market> {
    let m = session.get(&format!("/markets/{epic}"), "3", "").ok()?;
    let status = m.pointer("/snapshot/marketStatus").and_then(Value::as_str).unwrap_or("");
    let bid = m.pointer("/snapshot/bid").and_then(Value::as_f64);
    let offer = m.pointer("/snapshot/offer").and_then(Value::as_f64);
    let min_size = m
        .pointer("/dealingRules/minDealSize/value")
        .and_then(Value::as_f64)
        .unwrap_or(f64::INFINITY);
    let decimals = m
        .pointer("/snapshot/decimalPlacesFactor")
        .and_then(Value::as_u64)
        .map(|d| d.min(5) as usize)
        .unwrap_or(2);
    let currency = m
        .pointer("/instrument/currencies/0/code")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    println!(
        "  {epic}: status={status} bid={bid:?} offer={offer:?} minSize={min_size} \
         decimals={decimals} currency={currency}"
    );
    if status != "TRADEABLE" {
        return None;
    }
    Some(Market { epic: epic.to_string(), bid: bid?, offer: offer?, min_size, decimals, currency })
}

/// The first TRADEABLE market: the weekday FX epic, else IG's weekend/crypto listings by search.
fn find_tradeable(session: &IgSession) -> Option<Market> {
    println!("looking for a TRADEABLE market:");
    if let Some(m) = market_details(session, WEEKDAY_EPIC) {
        return Some(m);
    }
    for term in ["weekend", "bitcoin", "ether"] {
        let Ok(found) = session.get("/markets", "1", &format!("searchTerm={term}")) else {
            continue;
        };
        let empty = Vec::new();
        let rows = found.get("markets").and_then(Value::as_array).unwrap_or(&empty);
        println!("  search '{term}': {} results", rows.len());
        for row in rows.iter().take(8) {
            let epic = row.get("epic").and_then(Value::as_str).unwrap_or("");
            let status = row.get("marketStatus").and_then(Value::as_str).unwrap_or("");
            let name = row.get("instrumentName").and_then(Value::as_str).unwrap_or("");
            println!("    {epic} [{status}] {name}");
            if status != "TRADEABLE" || epic.is_empty() {
                continue;
            }
            if let Some(m) = market_details(session, epic) {
                // `build_request` hardcodes `currencyCode: "USD"`, so the OPEN needs a
                // USD-denominated market.
                if m.currency == "USD" && m.min_size.is_finite() {
                    return Some(m);
                }
            }
        }
    }
    None
}

fn fmt_level(level: f64, decimals: usize) -> String {
    format!("{level:.decimals$}")
}

/// Net open size in `epic` (positive long), plus the raw deal count — read from `GET /positions`
/// v2, the same fetch `recon_client::parse_positions` aggregates.
fn open_state(session: &IgSession, epic: &str) -> (f64, usize) {
    let body = session.get("/positions", "2", "").expect("GET /positions");
    let empty = Vec::new();
    let rows = body.get("positions").and_then(Value::as_array).unwrap_or(&empty);
    let mut net = 0.0;
    let mut count = 0;
    for r in rows {
        if r.pointer("/market/epic").and_then(Value::as_str) != Some(epic) {
            continue;
        }
        count += 1;
        let size = r.pointer("/position/size").and_then(Value::as_f64).unwrap_or(0.0);
        let dir = r.pointer("/position/direction").and_then(Value::as_str).unwrap_or("");
        net += if dir == "SELL" { -size } else { size };
    }
    (net, count)
}

/// Working orders resting in `epic` right now: `(dealId, type, level, direction, size)` rows.
fn working_orders(session: &IgSession, epic: &str) -> Vec<(String, String, f64, String, f64)> {
    let body = session.get("/workingorders", "2", "").expect("GET /workingorders");
    let empty = Vec::new();
    let rows = body.get("workingOrders").and_then(Value::as_array).unwrap_or(&empty);
    rows.iter()
        .filter(|r| r.pointer("/marketData/epic").and_then(Value::as_str) == Some(epic))
        .map(|r| {
            (
                r.pointer("/workingOrderData/dealId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                r.pointer("/workingOrderData/orderType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                r.pointer("/workingOrderData/orderLevel").and_then(Value::as_f64).unwrap_or(0.0),
                r.pointer("/workingOrderData/direction")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                r.pointer("/workingOrderData/orderSize").and_then(Value::as_f64).unwrap_or(0.0),
            )
        })
        .collect()
}

/// POST one close-shaped body via `_method: DELETE`, print request + reply + confirm verbatim.
/// Returns `(deal_ref, confirm)` on a 2xx, `None` on an HTTP error (also printed verbatim).
fn probe_close(session: &IgSession, label: &str, body: &Value) -> Option<(String, Value)> {
    println!("{label}:");
    println!("  -> POST+_method:DELETE /positions/otc (Version 1)");
    println!("     body: {body}");
    match session.post_method_delete("/positions/otc", "1", body) {
        Ok(resp) => {
            println!("     reply: {resp}");
            let deal_ref = resp.get("dealReference").and_then(Value::as_str)?.to_string();
            std::thread::sleep(std::time::Duration::from_millis(1500));
            match session.get(&format!("/confirms/{deal_ref}"), "1", "") {
                Ok(confirm) => {
                    println!("     confirm: {confirm}");
                    Some((deal_ref, confirm))
                }
                Err(e) => {
                    println!("     confirm FAILED: status={} body={}", e.status, e.message);
                    Some((deal_ref, Value::Null))
                }
            }
        }
        Err(e) => {
            println!("     HTTP ERROR: status={} body={}", e.status, e.message);
            None
        }
    }
}

#[test]
#[ignore = "live DEMO; PLACES REAL ORDERS — needs IG_DEMO_* creds and an open market"]
fn ig_limit_close_probe() {
    let Some(c) = cfg() else {
        eprintln!("skip: no IG_DEMO_* creds in the credential store");
        return;
    };
    let Ok(session) = IgSession::login(&c) else {
        eprintln!("skip: IG demo login failed (rotated password?) — not a red");
        return;
    };
    println!("logged in: account {}", session.account_id);

    let Some(m) = find_tradeable(&session) else {
        eprintln!(
            "skip: no TRADEABLE USD market found (weekend + no weekend listings?) — not a red"
        );
        return;
    };
    let epic = m.epic.clone();
    let size = m.min_size;
    println!(
        "probing on {epic}: bid={} offer={} size={size} ({} decimals)",
        m.bid, m.offer, m.decimals
    );

    let (net_before, deals_before) = open_state(&session, &epic);
    let wo_before = working_orders(&session, &epic);
    println!("state before: net={net_before} deals={deals_before} workingOrders={wo_before:?}");

    // ---- OPEN: production bytes, minimum size ------------------------------------------------
    println!("OPEN {size} BUY (production build_request):");
    let (path, version, body) = build_request(&market_order("probe-open", &epic, 1, size, false));
    println!("  -> POST {path} (Version {version})");
    println!("     body: {body}");
    let open_resp = match session.post(&path, version, &body) {
        Ok(r) => r,
        Err(e) => {
            // An un-openable market (margin, currency) is a skip, not a red — but say why.
            eprintln!(
                "skip: OPEN failed: status={} body={} — cannot probe a close",
                e.status, e.message
            );
            return;
        }
    };
    println!("     reply: {open_resp}");
    let open_ref = open_resp
        .get("dealReference")
        .and_then(Value::as_str)
        .expect("open answered no dealReference")
        .to_string();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let open_confirm =
        session.get(&format!("/confirms/{open_ref}"), "1", "").expect("GET /confirms open");
    println!("     confirm: {open_confirm}");
    if open_confirm.get("dealStatus").and_then(Value::as_str) != Some("ACCEPTED") {
        eprintln!("skip: the OPEN was not accepted — nothing to probe a close against");
        return;
    }

    // Everything after the open runs inside a closure so the flat-guarantee below is
    // unconditional — a panicking assertion can never strand the demo position.
    let probes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // ---- PROBE A: LIMIT close at a NON-marketable level ----------------------------------
        // Closing a LONG is a SELL; a sell limit ABOVE the offer is restable but not immediately
        // executable (0.3% above: far outside the spread, inside any sane tolerance).
        let a_level = fmt_level(m.offer * 1.003, m.decimals);
        let a_body = json!({
            "epic": epic,
            "expiry": "-",
            "direction": "SELL",
            "size": size,
            "orderType": "LIMIT",
            "level": a_level,
        });
        let a = probe_close(&session, "PROBE A — LIMIT close, NON-marketable level", &a_body);

        let (net_a, deals_a) = open_state(&session, &epic);
        let wo_a = working_orders(&session, &epic);
        println!("state after A: net={net_a} deals={deals_a} workingOrders={wo_a:?}");

        // If A left something resting, try the normal working-order cancel against it.
        for (deal_id, otype, level, dir, wsize) in &wo_a {
            if wo_before.iter().any(|(id, ..)| id == deal_id) {
                continue; // not ours
            }
            println!(
                "PROBE A rested a working order: dealId={deal_id} type={otype} level={level} \
                 {dir} {wsize} — cancelling via DELETE /workingorders/otc/{{dealId}}"
            );
            match session.delete(&format!("/workingorders/otc/{deal_id}"), "2") {
                Ok(r) => println!("     cancel reply: {r}"),
                Err(e) => println!("     cancel FAILED: status={} body={}", e.status, e.message),
            }
        }

        // ---- PROBE C: STOP close (endpoint documents no STOP arm — capture the refusal) ------
        let c_level = fmt_level(m.bid * 0.997, m.decimals);
        let c_body = json!({
            "epic": epic,
            "expiry": "-",
            "direction": "SELL",
            "size": size,
            "orderType": "STOP",
            "level": c_level,
        });
        let _c = probe_close(&session, "PROBE C — STOP close", &c_body);
        let (net_c, deals_c) = open_state(&session, &epic);
        println!("state after C: net={net_c} deals={deals_c}");

        // ---- PROBE B: LIMIT close at a MARKETABLE level --------------------------------------
        // A sell limit BELOW the bid is immediately executable at bid-or-better. Only meaningful
        // while the position is still open.
        let (net_now, _) = open_state(&session, &epic);
        if net_now > net_before {
            let b_level = fmt_level(m.bid * 0.997, m.decimals);
            let b_body = json!({
                "epic": epic,
                "expiry": "-",
                "direction": "SELL",
                "size": size,
                "orderType": "LIMIT",
                "level": b_level,
            });
            let _b = probe_close(&session, "PROBE B — LIMIT close, MARKETABLE level", &b_body);
            let (net_b, deals_b) = open_state(&session, &epic);
            println!("state after B: net={net_b} deals={deals_b}");
        } else {
            println!("PROBE B skipped: position already closed by an earlier probe");
        }
        a
    }));

    // ---- GUARANTEE FLAT — runs whatever the probes did -----------------------------------------
    let (net_after, _) = open_state(&session, &epic);
    if (net_after - net_before).abs() > 1e-12 {
        let residual = net_after - net_before;
        let side = vike_model::closing_side(residual);
        println!("residual {residual} — market-closing via production build_close_request:");
        let req = market_order("probe-flatten", &epic, side, residual.abs(), true);
        let (cpath, cver, cbody) = build_close_request(&req);
        println!("  -> POST+_method:DELETE {cpath} (Version {cver})");
        println!("     body: {cbody}");
        match session.post_method_delete(&cpath, cver, &cbody) {
            Ok(r) => {
                println!("     reply: {r}");
                if let Some(fref) = r.get("dealReference").and_then(Value::as_str) {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    match session.get(&format!("/confirms/{fref}"), "1", "") {
                        Ok(cf) => println!("     confirm: {cf}"),
                        Err(e) => {
                            println!("     confirm FAILED: status={} body={}", e.status, e.message)
                        }
                    }
                }
            }
            Err(e) => println!("     flatten FAILED: status={} body={}", e.status, e.message),
        }
    }
    // ...and sweep any working order the probes left behind.
    for (deal_id, otype, level, dir, wsize) in working_orders(&session, &epic) {
        if wo_before.iter().any(|(id, ..)| id == &deal_id) {
            continue;
        }
        println!("sweeping leftover working order {deal_id} ({otype} {dir} {wsize} @ {level})");
        match session.delete(&format!("/workingorders/otc/{deal_id}"), "2") {
            Ok(r) => println!("     cancel reply: {r}"),
            Err(e) => println!("     cancel FAILED: status={} body={}", e.status, e.message),
        }
    }

    let (net_final, deals_final) = open_state(&session, &epic);
    let wo_final = working_orders(&session, &epic);
    println!(
        "FINAL state: net={net_final} deals={deals_final} workingOrders={wo_final:?} \
         (before: net={net_before} deals={deals_before})"
    );
    assert!(
        (net_final - net_before).abs() < 1e-12,
        "THE ACCOUNT MUST BE LEFT FLAT — net before={net_before} after={net_final}"
    );
    assert_eq!(
        wo_final.len(),
        wo_before.len(),
        "no probe working order may survive: before={wo_before:?} after={wo_final:?}"
    );
    if let Err(p) = probes {
        std::panic::resume_unwind(p);
    }
    println!("account flat; probe complete — read the verbatim replies above for the verdict");
}
