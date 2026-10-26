//! LIVE demo smoke (network + fxPractice creds, REAL ORDERS on the practice account): what ONE UNIT of an
//! OANDA instrument is worth, per class. Run by hand, one test at a time:
//!     cargo test -p vike-oanda --test oanda_unit_semantics_smoke -- --ignored --nocapture --test-threads=1 \
//!         oanda_unit_semantics_on_eur_usd
//!     cargo test -p vike-oanda --test oanda_unit_semantics_smoke -- --ignored --nocapture --test-threads=1 \
//!         oanda_unit_semantics_of_the_cfd_classes
//!
//! **Why it exists.** The engine values an order as `qty x price` in the instrument's quote currency (times
//! a journaled rate), and the bridge refuses a metal, an index or any other CFD by name
//! (`vike_oanda::grid_refusal`) until one unit of that class is MEASURED to be worth exactly that. The
//! measurement is a tiny market round trip per class: the venue's own fill transactions carry the realized
//! P&L in the ACCOUNT currency and the initial margin, so `pl / (units x price move x quote->account
//! rate)` IS the contract multiplier, and `initialMarginRequired / (units x price x marginRate x rate)`
//! IS the margin convention. Each class prints `FINDING` lines; the assertions fail when a multiplier is
//! not 1, because that is the number the engine's ceilings assume.
//!
//! **What it found on 2026-10-26 (practice account 14):** the account's instrument list holds 68
//! CURRENCY pairs and NO metal, index, commodity or bond, and OANDA refuses an order on `XAU_USD` and
//! `SPX500_USD` with `INSTRUMENT_NOT_TRADEABLE` ("The instrument specified is not tradeable by the
//! Account"), so `oanda_unit_semantics_of_the_cfd_classes` SKIPS there, naming what the list holds. Run it
//! on a practice account whose list carries the classes (another OANDA division) to lift the fence.
//!
//! * `oanda_unit_semantics_on_eur_usd` is the CONTROL: the same harness on the one class every practice
//!   account can trade (1000 units of `EUR_USD`, a multiplier of exactly 1 by definition), so a green
//!   control proves the arithmetic that the CFD test would apply. Measured the same day: bought 1.11976,
//!   sold 1.11962, realized pl -0.14 USD (= 1000 x -0.00014), initial margin 22.3952 (= 1000 x 1.11976 x
//!   marginRate 0.02), implied multiplier 1.0000 and margin multiplier 1.0000.
//! * Both refuse to run on an account that holds a trade or a pending order, and both end FLAT (a venue
//!   `positions/{instrument}/close` backs up the bridge's own closing order, and a non-flat end FAILS).
//!
//! Credentials: the default DEMO oanda account's `API_KEY` / `ACCOUNT_ID` fields via
//! `vike_secrets::smoke_directory`; absent -> SKIP. Not wired into `.github/workflows/live-smokes.yml`:
//! it places orders that FILL, which that lane admits for nothing.

use std::time::Duration;

use serde_json::{Value, json};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, TimeInForce, now_ms};
use vike_oanda::{OandaConfig, OandaExecutionClient, OandaRest, load_oanda_config_from};

/// The smallest representative of each class the fence refuses, in the order the plan names them: a metal,
/// an index quoted in USD, an index quoted in another currency, a commodity.
const CFD_CLASSES: [&str; 4] = ["XAU_USD", "SPX500_USD", "DE30_EUR", "BCO_USD"];

/// How far from 1.0 a measured multiplier may sit before the engine's `qty x price` is wrong for the class:
/// the quote->account rate is read at the moment of the trade and OANDA's `accountGain`/`accountLoss`
/// factors carry a spread of about 1%, so this is a measurement tolerance, not a tolerance for the rule.
const MULTIPLIER_TOLERANCE: f64 = 0.03;

fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    total: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> Result<Event, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .map_err(|_| "timed out waiting for a matching event".to_string())?
                .ok_or("ingest channel closed")?;
            if let Ingest::Event(ev) = ingest
                && pred(&ev)
            {
                return Ok(ev);
            }
        }
    })
}

/// A v20 decimal (a JSON string) at `pointer`, or NaN.
fn num(v: &Value, pointer: &str) -> f64 {
    v.pointer(pointer)
        .and_then(|x| x.as_str().map(str::to_owned).or_else(|| x.as_f64().map(|n| n.to_string())))
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::NAN)
}

struct Account {
    config: OandaConfig,
    rest: OandaRest,
}

impl Account {
    fn open() -> Option<Self> {
        vike_log::test_init();
        let dir = vike_secrets::smoke_directory();
        let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
        let Some(config) = load_oanda_config_from(AccountTier::Demo, &creds) else {
            tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
            return None;
        };
        let rest = OandaRest::new(config.api_token.clone());
        Some(Self { config, rest })
    }

    fn get(&self, path: &str, query: &str) -> Value {
        let path = format!("/v3/accounts/{}{path}", self.config.account_id);
        self.rest.get(&self.config.rest_base, &path, query).unwrap_or_else(|e| {
            panic!("GET {} failed: {e}", path.replace(&self.config.account_id, "<acct>"))
        })
    }

    fn summary(&self) -> Value {
        self.get("/summary", "")
    }

    /// Fail unless the account holds no trade and no pending order: the harness nets against the account's
    /// own position, so a held one would make `pl` the sum of two things.
    fn require_flat(&self) {
        let s = self.summary();
        let open = s.pointer("/account/openTradeCount").and_then(Value::as_i64).unwrap_or(-1);
        let pending = s.pointer("/account/pendingOrderCount").and_then(Value::as_i64).unwrap_or(-1);
        assert!(
            open == 0 && pending == 0,
            "the practice account holds {open} trade(s) and {pending} pending order(s): this smoke \
             measures against a flat account; flatten it first"
        );
    }

    /// The account's instrument rows (`GET /instruments`), keyed by name.
    fn instruments(&self) -> Vec<Value> {
        self.get("/instruments", "")
            .get("instruments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Close whatever `instrument` still holds at the VENUE (the backstop of the bridge's own closing order).
    fn flatten(&self, instrument: &str) {
        let path = format!("/v3/accounts/{}/positions/{instrument}/close", self.config.account_id);
        let _ = self.rest.put_json(
            &self.config.rest_base,
            &path,
            &json!({ "longUnits": "ALL", "shortUnits": "ALL" }),
        );
    }

    /// How many units of `instrument` the account holds, signed (0 when flat).
    fn held(&self, instrument: &str) -> f64 {
        let body = self.get("/openPositions", "");
        let Some(positions) = body.get("positions").and_then(Value::as_array) else { return 0.0 };
        positions
            .iter()
            .filter(|p| p.get("instrument").and_then(Value::as_str) == Some(instrument))
            .map(|p| num(p, "/long/units").max(0.0) + num(p, "/short/units").min(0.0))
            .sum()
    }

    /// Whether the venue prices `instrument` as tradeable right now (an index or a metal has its own hours;
    /// a closed market refuses the order and the smoke would fail for that, not for the unit).
    fn tradeable(&self, instrument: &str) -> bool {
        let body = self.get("/pricing", &format!("instruments={instrument}"));
        body.pointer("/prices/0/tradeable").and_then(Value::as_bool).unwrap_or(false)
    }

    /// The venue's own factor for one unit of `currency` in the account currency (`positionValue` of the
    /// `homeConversions` entry), asked for `instrument`'s pricing.
    fn home_factor(&self, instrument: &str, currency: &str) -> f64 {
        let body =
            self.get("/pricing", &format!("instruments={instrument}&includeHomeConversions=true"));
        body.get("homeConversions")
            .and_then(Value::as_array)
            .and_then(|all| {
                all.iter().find(|c| c.get("currency").and_then(Value::as_str) == Some(currency))
            })
            .map_or(f64::NAN, |c| num(c, "/positionValue"))
    }
}

/// One market order through the REAL exec client and its fill. `Err` carries what the venue said.
fn market(
    client: &mut OandaExecutionClient,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    instrument: &str,
    side: i32,
    qty: f64,
) -> Result<(f64, f64), String> {
    let coid = format!("vtunits{}", now_ms() % 1_000_000_000);
    client.submit(&OrderRequest {
        client_order_id: coid.clone(),
        venue: "oanda".into(),
        symbol: instrument.into(),
        side,
        qty,
        order_type: OrderType::Market,
        time_in_force: TimeInForce::Ioc,
        ts: now_ms(),
        ..Default::default()
    });
    let ev = wait_for(rx, Duration::from_secs(30), |ev| {
        matches!(ev, Event::OrderFilled(f) if f.client_order_id == coid)
            || matches!(ev, Event::OrderRejected(r) if r.client_order_id == coid)
            || matches!(ev, Event::OrderCanceled(c) if c.client_order_id == coid)
    })?;
    match ev {
        Event::OrderFilled(f) => Ok((f.fill.last_px, f.fill.last_qty)),
        Event::OrderRejected(r) => Err(format!("REJECTED: {}", r.reason)),
        other => Err(format!("ended without a fill: {other:?}")),
    }
}

/// The measurement of ONE instrument: a market BUY of `qty`, a market SELL of the same, then the venue's
/// own fill transactions. Prints `FINDING` lines and returns `(multiplier, margin_multiplier)`.
fn measure(account: &Account, row: &Value, qty: f64) -> (f64, f64) {
    let instrument = row["name"].as_str().expect("row name").to_string();
    let quote = instrument.split('_').nth(1).expect("BASE_QUOTE name").to_string();
    let margin_rate = num(row, "/marginRate");
    account.require_flat();
    let floor = account
        .summary()
        .pointer("/lastTransactionID")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();

    let (events, mut rx) = event_channel(64);
    let mut client = OandaExecutionClient::spawn(account.config.clone(), events);
    let run = (|| {
        let (buy_px, buy_qty) = market(&mut client, &mut rx, &instrument, 1, qty)?;
        let rate = account.home_factor(&instrument, &quote);
        let (sell_px, sell_qty) = market(&mut client, &mut rx, &instrument, -1, qty)?;
        Ok::<_, String>((buy_px, buy_qty, sell_px, sell_qty, rate))
    })();
    // The backstop first, the verdict after: nothing here may leave the account holding the instrument.
    if account.held(&instrument).abs() > 0.0 {
        account.flatten(&instrument);
    }
    drop(client);
    let (buy_px, buy_qty, sell_px, sell_qty, rate) =
        run.unwrap_or_else(|why| panic!("{instrument}: the round trip failed: {why}"));
    assert_eq!(account.held(&instrument), 0.0, "{instrument}: the account is not flat");
    account.require_flat();

    // The venue's own word: every ORDER_FILL of this instrument since the floor.
    let txs = account.get("/transactions/sinceid", &format!("id={floor}"));
    let fills: Vec<&Value> = txs["transactions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|t| t["type"] == "ORDER_FILL" && t["instrument"] == instrument.as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        fills.len(),
        2,
        "{instrument}: expected the entry and the exit fill, got {}",
        fills.len()
    );
    let (open, close) = (fills[0], fills[1]);
    let pl = num(close, "/pl");
    let margin = num(open, "/tradeOpened/initialMarginRequired");
    let entry = num(open, "/price");
    let exit = num(close, "/price");
    let multiplier = pl / (qty * (exit - entry) * rate);
    let margin_multiplier = margin / (qty * entry * margin_rate * rate);
    println!(
        "FINDING {instrument}: units {buy_qty} in / {sell_qty} out, bridge fills {buy_px} -> {sell_px}, \
         venue fills {entry} -> {exit}, realized pl {pl} (account currency), quote {quote} -> account \
         {rate}, initial margin {margin} at marginRate {margin_rate}"
    );
    println!(
        "FINDING {instrument}: implied contract multiplier {multiplier:.4} (pl / (units x move x rate)), \
         implied margin multiplier {margin_multiplier:.4} (margin / (units x price x marginRate x rate))"
    );
    (multiplier, margin_multiplier)
}

/// The control: 1000 units of `EUR_USD`, the one class every practice account lists. Its multiplier is 1 by
/// the definition of a currency pair, so a result away from 1 means the HARNESS is wrong, not the venue.
#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials; places a REAL market round trip"]
fn oanda_unit_semantics_on_eur_usd() {
    let Some(account) = Account::open() else { return };
    let rows = account.instruments();
    let row = rows.iter().find(|r| r["name"] == "EUR_USD").expect("the account lists EUR_USD");
    let (multiplier, margin_multiplier) = measure(&account, row, 1000.0);
    assert!(
        (multiplier - 1.0).abs() <= MULTIPLIER_TOLERANCE,
        "the control's multiplier is {multiplier}: the harness's arithmetic is wrong"
    );
    assert!(
        (margin_multiplier - 1.0).abs() <= MULTIPLIER_TOLERANCE,
        "the control's margin multiplier is {margin_multiplier}"
    );
}

/// The point: one representative per class the fence refuses, at the instrument's minimum size. SKIPS (naming
/// what the account's list holds) for every class the account does not list.
#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials; places REAL market round trips"]
fn oanda_unit_semantics_of_the_cfd_classes() {
    let Some(account) = Account::open() else { return };
    let rows = account.instruments();
    let kinds: std::collections::BTreeMap<&str, usize> =
        rows.iter().fold(Default::default(), |mut m, r| {
            *m.entry(r["type"].as_str().unwrap_or("?")).or_default() += 1;
            m
        });
    let mut measured = 0;
    for name in CFD_CLASSES {
        let Some(row) = rows.iter().find(|r| r["name"] == name) else {
            println!("SKIP {name}: not in this account's instrument list (it lists {kinds:?})");
            continue;
        };
        if !account.tradeable(name) {
            println!(
                "SKIP {name}: the venue prices it as not tradeable now (its market is closed)"
            );
            continue;
        }
        let qty = num(row, "/minimumTradeSize");
        assert!(qty.is_finite() && qty > 0.0, "{name}: no minimumTradeSize in the row: {row}");
        let (multiplier, margin_multiplier) = measure(&account, row, qty);
        assert!(
            (multiplier - 1.0).abs() <= MULTIPLIER_TOLERANCE,
            "{name}: one unit is worth {multiplier} x (units x price): the engine's notional \
             (qty x price) is wrong for this class, so the fence must carry that multiplier"
        );
        println!("FINDING {name}: margin multiplier {margin_multiplier:.4}");
        measured += 1;
    }
    if measured == 0 {
        println!(
            "SKIP: no CFD class is listed for this account ({kinds:?}); nothing to measure, the fence stays"
        );
    }
}
