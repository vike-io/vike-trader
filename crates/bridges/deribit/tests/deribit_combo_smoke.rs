//! Combo-book LIVE smokes (combo orders PR-4 + the deribit `supports_combo` gate flip):
//!
//!     cargo test -p vike-deribit --test deribit_combo_smoke -- --ignored --nocapture
//!
//! `#[ignore]`d and double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! `DERIBIT_DEMO_*` creds in the workspace `.env`; self-skips with a `tracing::warn!` when absent).
//! TWO tests, run separately by name if needed (the binance reconcile smoke's read/write split):
//!
//! 1. [`deribit_combo_details_parse_smoke`] — **STRICTLY READ-ONLY.** Calls only the two PUBLIC
//!    combo readers (`public/get_combo_ids`, `public/get_combo_details`) against Deribit TESTNET
//!    and runs the real venue JSON through the PURE parser/mapper in `src/combo.rs`. Closes the
//!    one gap fixtures cannot close: whether `parse_combo` matches the shape the venue actually
//!    emits today. Places no orders, registers no combos.
//!
//! 2. [`deribit_combo_order_lifecycle_smoke`] — **PLACES A REAL (far-from-market, non-fillable)
//!    DEMO COMBO ORDER** through the WHOLE production stack, proving the `supports_combo` gate
//!    flip end to end: `ComboSpec` → `Command::Order(OrderIntent::Combo)` → the vike-core
//!    lowering (caps gate + `check_combo`) → `DeribitRest::submit_order` → `private/create_combo`
//!    → the orientation solve → `private/buy|sell` on the combo id — then verifies the venue's
//!    OWN acknowledgment (open order direction/amount/price on the combo book) against the
//!    [`ComboMapping`] prediction, cancels, and asserts the account ends flat.
//!
//!    **The orientation proof is the point** (Deribit CANONICALIZES combos): the spec deliberately
//!    asks for the SHORT vertical `[-1·K1-call, +1·K2-call]` — the inverse of the canonical
//!    call-spread book (`CS` = buy-low/sell-high) — so if the venue hands back the inverse combo,
//!    our BUY must reach the wire as a venue SELL at the sign-flipped net, and the assertions
//!    check the venue-acked order against intent THROUGH the solved mapping `k`, not against a
//!    hard-coded side. The demanded net is a credit ≥ fair value + max(25% of strike width, 10
//!    ticks) — no rational counterparty crosses it, so the order rests until we cancel it.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::combo::{
    ComboMapping, build_combo_order, build_combo_order_params, build_create_combo_params,
    map_combo, parse_combo, parse_combo_grid,
};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, MarketTick, OrderIntent, OrderStatus, RiskGate,
    RiskLimits,
};
use vike_model::clock::now_ms;
use vike_model::{ComboLeg, ComboSpec, TimeInForce};

/// The double-gate: the read-only smoke touches only PUBLIC endpoints, but stays credential-gated
/// like its siblings so a credential-less CI/dev box never reaches the network here.
fn gated() -> bool {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    if load_credentials_from("deribit", Environment::Demo, &vars).is_none() {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return false;
    }
    true
}

#[test]
#[ignore = "network + demo creds — run manually; READ-ONLY, places no orders (see module doc)"]
fn deribit_combo_details_parse_smoke() {
    vike_log::test_init();
    if !gated() {
        return;
    }
    let http = UreqTransport::new("deribit");

    // 1) public/get_combo_ids — a bare array of combo instrument names.
    let ids_resp = http
        .public(TESTNET_REST, "/api/v2/public/get_combo_ids", &[("currency", "BTC".to_string())])
        .expect("public/get_combo_ids");
    let ids: Vec<String> = ids_resp
        .get("result")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    tracing::info!(target: "vike_deribit", n = ids.len(), "live BTC combo ids");
    if ids.is_empty() {
        tracing::warn!(target: "vike_deribit", "no live BTC combos on testnet right now — nothing to parse");
        return;
    }

    // 2) public/get_combo_details on each of the first few — the REAL result shape must feed the
    //    pure parser, and each parsed combo must be self-consistent (>=2 legs, no zero ratios).
    let mut parsed = 0usize;
    for id in ids.iter().take(5) {
        let resp = http
            .public(TESTNET_REST, "/api/v2/public/get_combo_details", &[("combo_id", id.clone())])
            .unwrap_or_else(|e| panic!("get_combo_details({id}) failed: {e:?}"));
        let result = resp.get("result").unwrap_or(&json!(null)).clone();
        let combo = parse_combo(&result)
            .unwrap_or_else(|| panic!("live combo {id} did NOT parse — shape drift: {result}"));

        assert_eq!(&combo.id, id, "parsed id must match the requested combo_id");
        assert!(combo.legs.len() >= 2, "a combo must have >=2 legs: {combo:?}");
        assert!(combo.legs.iter().all(|l| l.ratio != 0), "no zero ratios: {combo:?}");
        assert!(
            combo.legs.iter().all(|l| !l.symbol.is_empty()),
            "every leg names an instrument: {combo:?}"
        );
        assert!(
            combo.state == "active" || combo.state == "inactive",
            "state is the documented enum, got {:?}",
            combo.state
        );
        tracing::info!(target: "vike_deribit", id = %combo.id, state = %combo.state, legs = ?combo.legs, "parsed live combo");

        // 3) the mapper against the venue's OWN legs is the identity — a combo always reconciles
        //    with itself, which pins that `map_combo` agrees with real venue ratios (including
        //    real-world negative multipliers).
        assert_eq!(
            map_combo(&combo.legs, &combo.legs),
            Ok(ComboMapping::IDENTITY),
            "a live combo must reconcile with itself"
        );

        // 4) and the INVERSE of a live combo must map to sign = -1 — the orientation law that
        //    decides buy-vs-sell, checked against real venue ratios rather than a fixture.
        let inverse: Vec<ComboLeg> = combo
            .legs
            .iter()
            .map(|l| ComboLeg { symbol: l.symbol.clone(), ratio: -l.ratio })
            .collect();
        let m = map_combo(&combo.legs, &inverse).expect("the inverse must reconcile");
        assert_eq!(m.sign(), -1, "the inverse of a live combo is sign -1: {combo:?}");
        assert_eq!(m, ComboMapping { num: -1, den: 1 }, "gcd-reduced inverse is exactly -1/1");
        parsed += 1;
    }
    assert!(parsed > 0, "at least one live combo must have been parsed");
    tracing::info!(target: "vike_deribit", parsed, "live combo parse smoke OK (read-only, no orders)");
}

// ---------------------------------------------------------------------------------------------
// The order-placing lifecycle smoke (the `supports_combo` gate-flip proof) + its helpers.
// ---------------------------------------------------------------------------------------------

/// One authed JSON-RPC call on the VERIFICATION transport; panics with the venue's raw error
/// (the smoke's evidence discipline: capture the raw response, never guess past it).
fn rpc(t: &mut DeribitOrderTransport, method: &str, params: Value) -> Value {
    let resp =
        t.call(method, &params).unwrap_or_else(|e| panic!("{method} transport error: {e:?}"));
    if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
        panic!("{method} venue error: {err}");
    }
    resp.get("result").cloned().unwrap_or(Value::Null)
}

/// The account's nonzero BTC option positions, sorted — the flatness baseline/final comparison.
fn option_positions(t: &mut DeribitOrderTransport) -> Vec<(String, f64)> {
    let result = rpc(t, "private/get_positions", json!({"currency": "BTC", "kind": "option"}));
    let mut out: Vec<(String, f64)> = result
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|p| {
            (
                p.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                p.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0),
            )
        })
        .filter(|(_, s)| *s != 0.0)
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The venue's open BTC order carrying our label (currency-wide so the combo-instrument naming
/// never hides it), or `None` once it is gone.
fn open_order_with_label(t: &mut DeribitOrderTransport, label: &str) -> Option<Value> {
    let result = rpc(t, "private/get_open_orders_by_currency", json!({"currency": "BTC"}));
    result
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .find(|o| o.get("label").and_then(|l| l.as_str()) == Some(label))
        .cloned()
}

/// A call-option candidate leg parsed off the book summary.
#[derive(Debug, Clone)]
struct CallLeg {
    name: String,
    expiry: String,
    strike: f64,
    mark: f64,
}

/// Pick two same-expiry BTC calls for the vertical: the expiry with the most marked calls, then
/// the first two strikes ABOVE the index (OTM keeps the spread's fair value far below the credit
/// we demand), falling back to the two highest strikes.
fn pick_vertical_legs(public: &UreqTransport, index: f64) -> (CallLeg, CallLeg) {
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut cands: Vec<CallLeg> = Vec::new();
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        let parts: Vec<&str> = name.split('-').collect();
        if parts.len() != 4 || parts[3] != "C" {
            continue;
        }
        let Ok(strike) = parts[2].parse::<f64>() else { continue };
        let Some(mark) = row.get("mark_price").and_then(|m| m.as_f64()).filter(|m| *m > 0.0) else {
            continue;
        };
        cands.push(CallLeg { name: name.to_string(), expiry: parts[1].to_string(), strike, mark });
    }
    // the expiry with the most usable calls
    let mut expiries: Vec<(String, usize)> = Vec::new();
    for c in &cands {
        match expiries.iter_mut().find(|(e, _)| *e == c.expiry) {
            Some((_, n)) => *n += 1,
            None => expiries.push((c.expiry.clone(), 1)),
        }
    }
    let (best_expiry, n) =
        expiries.into_iter().max_by_key(|(_, n)| *n).expect("BTC calls with a mark on testnet");
    assert!(n >= 2, "expiry {best_expiry} has fewer than 2 marked calls");
    let mut legs: Vec<CallLeg> = cands.into_iter().filter(|c| c.expiry == best_expiry).collect();
    legs.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    let otm = legs.iter().position(|c| c.strike > index).unwrap_or(legs.len());
    let lo = if otm + 1 < legs.len() { otm } else { legs.len() - 2 };
    (legs[lo].clone(), legs[lo + 1].clone())
}

/// THE gate-flip proof (see module doc): a strategy-shaped `ComboSpec`, submitted through the
/// real core, reaches Deribit's combo book; the venue-acknowledged order matches intent through
/// the solved orientation mapping; cancel leaves the account flat.
#[test]
#[ignore = "network + demo creds — places a REAL far-from-market testnet combo order, then cancels it"]
fn deribit_combo_order_lifecycle_smoke() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("deribit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    // ---- instrument selection (public REST) ----
    let public = UreqTransport::new("deribit");
    let idx = public
        .public(TESTNET_REST, "/api/v2/public/get_index_price", &[("index_name", "btc_usd".into())])
        .expect("index price");
    let index = idx["result"]["index_price"].as_f64().expect("index_price");
    let (k1, k2) = pick_vertical_legs(&public, index);
    tracing::info!(
        target: "vike_deribit",
        index,
        k1 = %k1.name, k1_mark = k1.mark,
        k2 = %k2.name, k2_mark = k2.mark,
        "vertical legs picked"
    );

    // The spec: BUY the SHORT vertical [-1·K1, +1·K2] — deliberately the inverse of the
    // canonical CS book orientation, so the venue's canonicalization (if any) must be SOLVED,
    // not assumed, for the resting order to point the right way.
    let spec_legs = vec![
        ComboLeg { symbol: k1.name.clone(), ratio: -1 },
        ComboLeg { symbol: k2.name.clone(), ratio: 1 },
    ];

    // ---- verification transport: baseline, combo registration, prediction ----
    let mut verify =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    verify.connect().expect("verification order-WS auth");
    let baseline = option_positions(&mut verify);
    tracing::info!(target: "vike_deribit", ?baseline, "pre-test option positions");

    let create = rpc(&mut verify, "private/create_combo", build_create_combo_params(&spec_legs));
    tracing::info!(target: "vike_deribit", raw = %create, "private/create_combo result (RAW)");
    let mut venue_combo = parse_combo(&create)
        .unwrap_or_else(|| panic!("create_combo result did not parse: {create}"));
    // A brand-new combo book is born "inactive" and flips active moments later (observed live:
    // ~574 ms; the adapter's `wait_combo_active` covers the production path — this poll is the
    // pre-flight's own copy so the smoke also survives a first-ever combo registration).
    let deadline = Instant::now() + Duration::from_secs(10);
    while !venue_combo.is_active() {
        assert!(Instant::now() < deadline, "combo never activated: {venue_combo:?}");
        std::thread::sleep(Duration::from_millis(300));
        let details =
            rpc(&mut verify, "public/get_combo_details", json!({"combo_id": venue_combo.id}));
        venue_combo = parse_combo(&details)
            .unwrap_or_else(|| panic!("get_combo_details did not parse: {details}"));
    }
    let m = map_combo(&spec_legs, &venue_combo.legs).unwrap_or_else(|e| {
        panic!(
            "orientation UNSOLVABLE ({e}) — spec {spec_legs:?} vs venue {:?}; raw: {create}",
            venue_combo.legs
        )
    });
    tracing::info!(
        target: "vike_deribit",
        combo_id = %venue_combo.id, num = m.num, den = m.den, sign = m.sign(),
        "orientation solved (sign -1 = venue book is our spec's INVERSE)"
    );

    let grid_res = rpc(
        &mut verify,
        "public/get_instrument",
        json!({"instrument_name": venue_combo.id.clone()}),
    );
    let (tick, step) = parse_combo_grid(&grid_res).unwrap_or((0.0005, 0.1));

    // qty: two combo-book steps in venue units, expressed in SPEC units through |k|.
    let spec_qty = 2.0 * step * (m.num.abs() as f64) / (m.den.abs() as f64);
    // net: a credit no rational counterparty pays — fair value + max(25% width, 10 ticks).
    let width = (k2.strike - k1.strike) / index; // max spread value, BTC per unit
    let fair = (k1.mark - k2.mark).max(0.0); // spread fair-value estimate, BTC per unit
    let net_limit = -(fair + (0.25 * width).max(10.0 * tick));

    // The prediction runs the SAME pure functions the adapter runs — asserting the venue ack
    // against it is asserting the whole wire path (label aside, learned after the mint).
    let predicted = build_combo_order(&venue_combo.id, m, 1, spec_qty, Some(net_limit));
    let expected = build_combo_order_params(&predicted, "prediction", tick, step);
    let expected_dir = if predicted.side > 0 { "buy" } else { "sell" };
    let expected_amount = expected["amount"].as_f64().expect("predicted amount");
    let expected_price = expected["price"].as_f64().expect("predicted price");
    tracing::info!(
        target: "vike_deribit",
        combo_id = %venue_combo.id, spec_qty, net_limit, expected_dir, expected_amount, expected_price,
        "prediction (spec intent mapped through k)"
    );

    // ---- the production stack: engine + core, exactly as the app mounts deribit ----
    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let inst = parse_deribit_option_instruments(&info)[&k1.name].clone();
    let mut order_transport =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    order_transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(order_transport, &k1.name, inst.properties, "BTC");
    let engine = ExecutionEngine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "deribit",
        &k1.name,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();

    // `check_combo` DENIES a leg with no mark — feed both legs' real marks through the market
    // lane, exactly as a live feed would.
    let marks = handle.market_sender();
    for leg in [&k1, &k2] {
        marks.publish(MarketTick {
            venue: "deribit".into(),
            symbol: leg.name.clone(),
            px: leg.mark,
            ts: now_ms(),
        });
    }
    std::thread::sleep(Duration::from_millis(1500)); // conflated lane drains

    // ---- submit the strategy-shaped intent ----
    let spec = ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: spec_qty,
        legs: spec_legs.clone(),
        net_limit: Some(net_limit),
        time_in_force: TimeInForce::Gtc,
    };
    handle.try_command(Command::Order(OrderIntent::Combo(Box::new(spec)))).unwrap();

    // The minted coid surfaces as the one registry order with an EMPTY symbol (`build_combo`'s
    // contract — the adapter, not the model, names the combo instrument).
    let deadline = Instant::now() + Duration::from_secs(25);
    let coid = loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.symbol.is_empty()) {
            assert!(
                !matches!(o.status, OrderStatus::Rejected | OrderStatus::Denied),
                "combo terminalized before acceptance: {:?}; recent: {:?}",
                o.status,
                snap.recent_events
            );
            if o.status == OrderStatus::Accepted {
                break o.client_order_id.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "combo never reached Accepted; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    tracing::info!(target: "vike_deribit", %coid, "combo Accepted through the core");

    // ---- the venue's own acknowledgment vs intent, THROUGH the mapping ----
    let ack = open_order_with_label(&mut verify, &coid)
        .unwrap_or_else(|| panic!("no open venue order labeled {coid}"));
    tracing::info!(target: "vike_deribit", raw = %ack, "venue open order (RAW)");
    let ack_inst = ack["instrument_name"].as_str().unwrap_or("");
    let ack_dir = ack["direction"].as_str().unwrap_or("");
    let ack_amount = ack["amount"].as_f64().unwrap_or(f64::NAN);
    let ack_price = ack["price"].as_f64().unwrap_or(f64::NAN);
    assert_eq!(ack_inst, venue_combo.id, "the order must rest on the resolved combo book");
    assert_eq!(ack_dir, expected_dir, "venue side must match the mapping-predicted side");
    assert!(
        (ack_amount - expected_amount).abs() < 1e-9,
        "venue amount {ack_amount} != predicted {expected_amount}"
    );
    assert!(
        (ack_price - expected_price).abs() < 1e-9,
        "venue net price {ack_price} != predicted {expected_price}"
    );
    // The orientation LAW against intent, off the venue's own fields: sign(k)·venue_side must
    // recover our spec side (+1 = we BOUGHT the short vertical), and the venue price's sign must
    // be sign(k)·sign(net) — a credit intent stays a credit in spec space.
    let ack_side = if ack_dir == "buy" { 1 } else { -1 };
    assert_eq!(
        m.sign() * ack_side,
        1,
        "mapped-back venue side must equal the spec side (BUY): k sign {} dir {ack_dir}",
        m.sign()
    );
    assert_eq!(
        ack_price < 0.0,
        (f64::from(m.sign()) * net_limit) < 0.0,
        "venue price sign must be sign(k)·sign(net): price {ack_price}, k {} net {net_limit}",
        m.sign()
    );
    assert_eq!(ack["order_state"].as_str().unwrap_or(""), "open", "must be RESTING, not filled");
    assert_eq!(ack["filled_amount"].as_f64().unwrap_or(-1.0), 0.0, "nothing may have filled");

    // ---- cancel + flatness ----
    handle.try_command(Command::Order(OrderIntent::Cancel(coid.clone()))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while open_order_with_label(&mut verify, &coid).is_some() {
        assert!(Instant::now() < deadline, "combo order still open at the venue after cancel");
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(cell.load().fault.is_none(), "no core fault through the whole lifecycle");
    let after = option_positions(&mut verify);
    assert_eq!(after, baseline, "the account must end flat vs its pre-test book");

    // ---- canonicalization evidence: the REVERSED legs resolve (idempotently) too ----
    let reversed: Vec<ComboLeg> =
        spec_legs.iter().map(|l| ComboLeg { symbol: l.symbol.clone(), ratio: -l.ratio }).collect();
    let re = rpc(&mut verify, "private/create_combo", build_create_combo_params(&reversed));
    tracing::info!(target: "vike_deribit", raw = %re, "reversed create_combo result (RAW)");
    if let Some(re_combo) = parse_combo(&re) {
        if re_combo.id == venue_combo.id {
            let rm = map_combo(&reversed, &re_combo.legs).expect("reversed spec must reconcile");
            assert_eq!(
                rm.sign(),
                -m.sign(),
                "one canonical book, opposite orientations, opposite mapping signs"
            );
            tracing::info!(
                target: "vike_deribit",
                combo_id = %re_combo.id,
                "CANONICALIZATION PROVEN LIVE: both orientations resolve to ONE combo book"
            );
        } else {
            tracing::warn!(
                target: "vike_deribit",
                ours = %venue_combo.id, theirs = %re_combo.id,
                "venue keeps orientation-distinct combo books — the mapping law covers both"
            );
        }
    }

    handle.shutdown_and_join();
    verify.close();
    tracing::info!(
        target: "vike_deribit",
        "combo lifecycle green: ComboSpec -> core lowering (caps+risk) -> create_combo -> \
         orientation solve -> resting venue order matching intent -> cancel -> flat"
    );
}
