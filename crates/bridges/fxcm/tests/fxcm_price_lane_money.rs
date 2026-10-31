//! **A foreign-quoted pair, end to end through a SCRIPTED FXCM session**: ONE docs-shaped Offers table behind the real
//! registry (`vike_fxcm::reads`) feeds the PRICE LANE (`vike_fxcm::quotes::process_desk`, the desk the gatewayhub's
//! stream listens on) and the CONVERSION RATE (`vike_fxcm::reads::quoted_facts`, what the gatewayhub's facts lane
//! answers), and an engine that admits USDJPY at that rate books a 10,000-yen move at 150.10 as 66.62 DOLLARS.
//!
//! The session is a fake: nothing here has met a ForexConnect session, and the Offers rows are shaped from the
//! ForexConnect documentation (`vendor/fcsdk/doc`: `ContractCurrency` is the BASE currency, `InstrumentType` 1 is
//! forex), the way `crates/bridges/fxcm/src/reads_tests.rs` shapes them. What it proves is the wiring and the
//! orientation: that the quote the price lane delivers and the rate the facts lane states come from the same
//! table, that a yen is a fraction of a dollar (`1/150`, never `150`), and that the engine's money follows.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate,
    SymbolAdmission,
};
use vike_fxcm::home_conversion::PropertiesRead;
use vike_fxcm::quotes::{PriceTick, process_desk};
use vike_fxcm::reads::{self, SessionReads};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{RiskLimits, SymbolGrid};

/// One row of `fc_offers`, in the shim's own key set.
fn row(name: &str, bid: f64, ask: f64, digits: i64) -> serde_json::Value {
    json!({
        "instrument": name, "bid": bid, "ask": ask, "digits": digits, "instrument_type": 1,
        "contract_currency": name.split('/').next().unwrap_or_default(), "contract_multiplier": 1.0,
        "subscription": "T", "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T",
        "base_unit": 1000, "quote_age_ms": 300
    })
}

fn table(usdjpy_bid: f64, usdjpy_ask: f64) -> String {
    serde_json::Value::Array(vec![
        row("EUR/USD", 1.0998, 1.1002, 5),
        row("USD/JPY", usdjpy_bid, usdjpy_ask, 3),
        row("GBP/USD", 1.3498, 1.3502, 5),
    ])
    .to_string()
}

/// The account's running session: its currency (`BASE_CRNCY`) and the Offers table as it stands now.
struct Session {
    offers: Mutex<String>,
}

impl SessionReads for Session {
    fn system_properties(&self) -> PropertiesRead {
        PropertiesRead::Json(json!({ "BASE_CRNCY": "USD" }).to_string())
    }

    fn offers(&self) -> Result<String, String> {
        Ok(self.offers.lock().unwrap().clone())
    }
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fill(trade_id: &'static str, side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: String::new(),
        venue: "fxcm".into(),
        symbol: "USDJPY".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// The admission the node builds from the facts the gatewayhub answered for USDJPY.
fn admission(facts: &vike_fxcm::home_conversion::ConvertedFacts) -> SymbolAdmission {
    let quote = facts.quote.as_ref().expect("a yen pair on a dollar account carries a rate");
    SymbolAdmission {
        route_key: "fxcm".into(),
        symbol: "USDJPY".into(),
        grid: SymbolGrid {
            tick_size: Some(facts.properties.tick_size),
            lot_size: Some(facts.properties.step_size),
            ..SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some(quote.quote_ccy.clone()),
        quote_rate: Some(quote.account_per_quote.get()),
        rate_as_of_ms: Some(quote.as_of_ms),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

#[test]
fn a_yen_move_at_150_10_books_66_62_dollars_from_one_scripted_session() {
    let session = Arc::new(Session { offers: Mutex::new(table(149.98, 150.02)) });
    let entry = reads::register("", Arc::clone(&session) as Arc<dyn SessionReads>);

    // 1. THE PRICE LANE: asking the pair starts the poll of the running session's table.
    let ticks: Arc<Mutex<Vec<PriceTick>>> = Arc::default();
    let sink = Arc::clone(&ticks);
    let listening =
        process_desk().listen(Arc::new(move |t: &PriceTick| sink.lock().unwrap().push(t.clone())));
    listening.want("USDJPY").expect("a session is registered");
    listening.want("GBPUSD").expect("a second pair costs no second request");
    wait_for("the first quote", || ticks.lock().unwrap().iter().any(|t| t.symbol == "USDJPY"));
    let first =
        ticks.lock().unwrap().iter().find(|t| t.symbol == "USDJPY").cloned().expect("a tick");
    assert_eq!((first.bid, first.ask), (149.98, 150.02));
    let open_px = (first.bid + first.ask) / 2.0;
    assert!((open_px - 150.00).abs() < 1e-9, "{open_px}");

    // 2. THE RATE, off the same table: a yen is a fraction of a dollar.
    let opening = reads::quoted_facts("", "USDJPY").expect("facts");
    let rate = opening.quote.as_ref().expect("a rate").account_per_quote.get();
    assert!((rate - 1.0 / 150.00).abs() < 1e-15, "{rate}");
    assert!(rate < 0.01, "a yen is worth well under a cent, never 150: {rate}");

    // 3. THE ENGINE: a dollar account that admitted the pair at that rate opens 100,000 units.
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "fxcm", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "fxcm",
        "EURUSD",
    );
    engine.set_account_ccy("USD").expect("a currency code");
    let as_of = opening.quote.as_ref().expect("a rate").as_of_ms;
    engine.now_ms = as_of;
    assert_eq!(engine.admit_symbol(&admission(&opening)), AdmitOutcome::Admitted);
    engine
        .on_event(&Event::Fill(fill("open", 1, 100_000.0, open_px, as_of)), &mut Outbox::default());
    let notional = engine.account.contract_value_of("USDJPY").gross_notional(100_000.0, open_px);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: 100,000 dollars, not 15,000,000 yen");

    // 4. THE MARKET MOVES 0.10: the lane prices the new quote, the facts restate the rate, the engine closes.
    *session.offers.lock().unwrap() = table(150.08, 150.12);
    wait_for("the moved quote", || ticks.lock().unwrap().iter().any(|t| t.bid == 150.08));
    let moved =
        ticks.lock().unwrap().iter().rfind(|t| t.symbol == "USDJPY").cloned().expect("a tick");
    let close_px = (moved.bid + moved.ask) / 2.0;
    assert!((close_px - 150.10).abs() < 1e-9, "{close_px}");
    let closing = reads::quoted_facts("", "USDJPY").expect("facts");
    let new_rate = closing.quote.as_ref().expect("a rate").account_per_quote.get();
    assert!((new_rate - 1.0 / 150.10).abs() < 1e-15, "{new_rate}");
    engine.now_ms = closing.quote.as_ref().expect("a rate").as_of_ms.max(as_of + 1);
    assert_eq!(engine.admit_symbol(&admission(&closing)), AdmitOutcome::Refreshed);
    engine.on_event(
        &Event::Fill(fill("close", -1, 100_000.0, close_px, engine.now_ms)),
        &mut Outbox::default(),
    );
    let realized = engine.account.realized_pnl;
    assert_eq!((realized * 100.0).round() / 100.0, 66.62, "{realized}");
    assert!(
        (realized - 10_000.0).abs() > 1.0,
        "NOT the 10,000 \"dollars\" an unconverted engine books"
    );

    drop(listening);
    reads::unregister("", &entry);
}
