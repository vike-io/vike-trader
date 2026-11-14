//! **A coin-margined MOUNT** (decision 0258's bybit amendment), offline, over answers the Bybit DEMO host gave
//! (`tests/fixtures/classes/`; the `BTCUSD` far limit, the coin-scoped BTC wallet and the measured fee-rate
//! refusal are `tests/bybit_smokes/bybit_inverse_smoke.rs`'s captures of 2026-11-08): the exec plane no longer
//! refuses it, its reconcile client reads the orders, the position and the settle coin's wallet in the inverse
//! book, and its funding poll asks the inverse log in the coin. A linear mount's requests are unchanged. The fee-rate
//! read of a SPOT mount, and what a failed fee-rate read says, ride the same harness.
//!
//! Only public seams, so every test here also runs on the code before the amendment; where a test says "base", it
//! FAILED there (the assertion differs), it did not fail to compile.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::ReconcileClientBybit;
use vike_bybit::account::leverage::PATH_ACCOUNT_INFO;
use vike_bybit::execution::funding::{BybitFundingPoller, LOG_PATH};
use vike_bybit::execution::rest::{
    PATH_ACCOUNT, PATH_ORDER_REALTIME, PATH_POSITION_LIST, VenueRestBybit,
};
use vike_bybit::history::Category;
use vike_bybit::signer::BybitV5Signer;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{OrderType, SymbolProperties};
use vike_secrets::credentials::Credentials;

const FEE_RATE: &str = "/v5/account/fee-rate";

type Asked = Arc<Mutex<Vec<(String, Vec<(String, Value)>)>>>;

/// Records every signed request and plays the venue for each path.
struct Venue {
    asked: Asked,
}

fn recorded(body: &str) -> Value {
    let full: Value = serde_json::from_str(body).expect("a captured fixture");
    full["result"].clone()
}

/// The `result` the demo host answered for `path`, or a synthetic one where the comment says so.
fn answer(path: &str, params: &[(&str, Value)]) -> Result<Value, VenueApiError> {
    let asked = |k: &str| params.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());
    Ok(match path {
        PATH_ORDER_REALTIME => {
            recorded(include_str!("../fixtures/classes/order_realtime_inverse_far_limit.json"))
        }
        PATH_POSITION_LIST => {
            recorded(include_str!("../fixtures/classes/position_inverse_perp_open.json"))
        }
        // MEASURED on the demo account (the CI box's `INV` row, 2026-11-09, `bybit_venue_read_smoke`): it states UTA 1.0
        // and refuses the separate inverse wallet, with or without `coin`.
        PATH_ACCOUNT_INFO => json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": 3}),
        PATH_ACCOUNT if asked("accountType") == Some(json!("CONTRACT")) => {
            return Err(VenueApiError {
                code: 10001,
                msg: "accountType only support UNIFIED.".to_string(),
            });
        }
        // The coin-scoped read (`coin=BTC`), recorded on the demo account: a dust BTC wallet beside a unified
        // account of 174,676 USD. Synthetic: the unscoped read's USDT row.
        PATH_ACCOUNT if asked("coin") == Some(json!("BTC")) => {
            recorded(include_str!("../fixtures/classes/wallet_balance_btc_coin_scoped.json"))
        }
        PATH_ACCOUNT => json!({"list": [{"accountType": "UNIFIED", "coin": [
            {"coin": "USDT", "walletBalance": "174659.63815995"}
        ]}]}),
        // MEASURED: the demo host answers `10001` with an empty message to every inverse spelling of the fee-rate
        // read (`category=inverse` with `symbol=BTCUSD`, with nothing, with `baseCoin=BTC`; and `category=linear`
        // with `symbol=BTCUSD`, the request before the amendment). Synthetic: a linear symbol's answer.
        FEE_RATE if asked("category") == Some(json!("inverse")) => {
            return Err(VenueApiError { code: 10001, msg: String::new() });
        }
        // RECORDED on the demo host 2026-11-08 (the default demo account, read-only): a pair is asked by its bare name.
        FEE_RATE if asked("category") == Some(json!("spot")) => {
            assert_eq!(asked("symbol"), Some(json!("DOGEUSDT")), "the bare pair, never `.S`");
            json!({"category": "spot", "list": [
                {"baseCoin": "", "makerFeeRate": "0.001", "symbol": "DOGEUSDT", "takerFeeRate": "0.001"}
            ]})
        }
        // Synthetic: a linear symbol's answer (the demo host refused `category=linear&symbol=BTCUSDT` with `10001` too,
        // measured 2026-11-08, so a demo linear mount keeps the static row as well).
        FEE_RATE => json!({"list": [
            {"symbol": "BTCUSDT", "makerFeeRate": "0.0002", "takerFeeRate": "0.00055"}
        ]}),
        // Synthetic: the documented row of a settlement (the demo account's inverse log was EMPTY, measured: it held
        // no inverse position), in the coin the log was asked for.
        LOG_PATH => json!({"list": [{
            "id": "f1", "type": "SETTLEMENT", "symbol": "BTCUSD", "currency": "BTC",
            "funding": "-0.00000012", "feeRate": "0.0001", "transactionTime": "1000"
        }]}),
        other => panic!("not a read this suite plays: {other}"),
    })
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let owned = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.asked.lock().unwrap().push((path.to_string(), owned));
        let result = answer(path, params)?;
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": result}))
    }
}

fn signer() -> BybitV5Signer {
    BybitV5Signer::new(&Credentials::for_test("k", "s", None), || 0)
}

fn recon(symbol: &str) -> (ReconcileClientBybit<Venue>, Asked) {
    let asked: Asked = Arc::default();
    let client = ReconcileClientBybit::new(
        signer(),
        Venue { asked: Arc::clone(&asked) },
        "https://unused.invalid",
        symbol,
    );
    (client, asked)
}

/// The value of `key` in the LAST request to `path`.
fn param(asked: &Asked, path: &str, key: &str) -> Option<Value> {
    let asked = asked.lock().unwrap();
    let (_, params) = asked.iter().rev().find(|(p, _)| p == path)?;
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// The exec plane signs a coin-margined perpetual and dated future on the mount (the venue's own words).
///
/// Base: both were refused (`non_linear_perpetual_refusal` answered a message).
#[test]
fn a_coin_margined_mount_is_not_refused() {
    for (symbol, word) in [("BTCUSD", "InversePerpetual"), ("BTCUSDZ26", "InverseFutures")] {
        assert_eq!(
            vike_bybit::execution::non_linear_perpetual_refusal(symbol, Some(word)),
            None,
            "{symbol}"
        );
    }
}

/// **The mount's reconcile balance is its SETTLE COIN's wallet** (decision 0258: the engine keeps its books in the
/// coin and adopts this figure): the client of `BTCUSD` asks the wallet for BTC alone and reports its
/// `walletBalance`, the demo account's 0.0000004 BTC; a linear mount still asks the whole wallet and reports USDT,
/// with no `account/info` read.
///
/// Base: the inverse mount asked the whole wallet and reported its USDT row (174,659.6 "BTC").
#[test]
fn an_inverse_mount_reports_its_settle_coins_wallet() {
    let (client, asked) = recon("BTCUSD");
    assert_eq!(client.fetch_balance(), Ok(Some(0.0000004)), "the BTC row's walletBalance");
    assert_eq!(param(&asked, PATH_ACCOUNT, "coin"), Some(json!("BTC")));
    assert_eq!(param(&asked, PATH_ACCOUNT, "accountType"), Some(json!("UNIFIED")));

    let (linear, asked) = recon("BTCUSDT");
    assert_eq!(linear.fetch_balance(), Ok(Some(174_659.638_159_95)), "a linear mount reads USDT");
    assert_eq!(param(&asked, PATH_ACCOUNT, "coin"), None, "and asks the whole wallet, as before");
    assert_eq!(paths(&asked), [PATH_ACCOUNT], "one request, as before");
}

/// The paths asked, in order.
fn paths(asked: &Asked) -> Vec<String> {
    asked.lock().unwrap().iter().map(|(p, _)| p.clone()).collect()
}

/// One account as `account/info` and its two wallets answer (`Err` = the venue's refusal): the unified wallet holds
/// the demo account's 0.0099904 BTC (measured 2026-11-09), the `CONTRACT` wallet, where it answers, another figure.
struct Account {
    asked: Asked,
    info: Result<Value, VenueApiError>,
    contract: Result<Value, VenueApiError>,
}

impl RestTransportJson for Account {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let owned: Vec<_> = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let contract = owned.iter().any(|(k, v)| k == "accountType" && v == "CONTRACT");
        self.asked.lock().unwrap().push((path.to_string(), owned));
        let result = match path {
            PATH_ACCOUNT_INFO => self.info.clone()?,
            PATH_ACCOUNT if contract => self.contract.clone()?,
            PATH_ACCOUNT => json!({"list": [{"accountType": "UNIFIED", "coin": [
                {"coin": "BTC", "walletBalance": "0.0099904", "equity": "0.0099904"}
            ]}]}),
            other => panic!("not a read this suite plays: {other}"),
        };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": result}))
    }
}

fn status(status: i64) -> Result<Value, VenueApiError> {
    Ok(json!({"marginMode": "REGULAR_MARGIN", "unifiedMarginStatus": status}))
}

/// A `CONTRACT` wallet holding 0.5 BTC (the documented shape, the unified wallet's: not captured).
fn contract_wallet() -> Result<Value, VenueApiError> {
    Ok(json!({"list": [{"accountType": "CONTRACT", "coin": [
        {"coin": "BTC", "walletBalance": "0.5", "equity": "0.52"}
    ]}]}))
}

fn inverse_on(
    info: Result<Value, VenueApiError>,
    contract: Result<Value, VenueApiError>,
) -> (ReconcileClientBybit<Account>, Asked) {
    let asked: Asked = Arc::default();
    let account = Account { asked: Arc::clone(&asked), info, contract };
    (ReconcileClientBybit::new(signer(), account, "https://unused.invalid", "BTCUSD"), asked)
}

/// The `accountType` of every wallet read, in order (each asks `coin=BTC`).
fn wallets_asked(asked: &Asked) -> Vec<Value> {
    let asked = asked.lock().unwrap();
    asked
        .iter()
        .filter(|(p, _)| p == PATH_ACCOUNT)
        .map(|(_, params)| {
            let value = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            assert_eq!(value("coin"), Some(json!("BTC")), "{params:?}");
            value("accountType").expect("an accountType")
        })
        .collect()
}

/// **A classic or UTA 1.0 account's inverse cash is the CONTRACT wallet's** (`bybit-exchange.github.io/docs/v5/acct-mode`:
/// inverse contracts trade in a separate account there), never the unified BTC beside it; the version is read once
/// and kept, so the next pass asks the wallet alone.
///
/// Base: the UNIFIED wallet's 0.0099904 BTC was reported, which is not the inverse book's money on such an account.
#[test]
fn a_uta1_accounts_inverse_cash_comes_from_the_contract_wallet() {
    for version in [1, 3, 4] {
        let (client, asked) = inverse_on(status(version), contract_wallet());
        assert_eq!(client.fetch_balance(), Ok(Some(0.5)), "unifiedMarginStatus {version}");
        assert_eq!(client.fetch_balance(), Ok(Some(0.5)));
        assert_eq!(wallets_asked(&asked), [json!("CONTRACT"), json!("CONTRACT")]);
        assert_eq!(
            paths(&asked),
            [PATH_ACCOUNT_INFO, PATH_ACCOUNT, PATH_ACCOUNT],
            "the version is read once"
        );
    }
}

/// **A UTA 2.0 account's inverse cash is the UNIFIED wallet's** (CONTRACT is not supported there): the `CONTRACT`
/// wallet is never asked.
#[test]
fn a_uta2_accounts_inverse_cash_comes_from_the_unified_wallet() {
    for version in [5, 6] {
        let (client, asked) = inverse_on(status(version), contract_wallet());
        assert_eq!(client.fetch_balance(), Ok(Some(0.0099904)), "unifiedMarginStatus {version}");
        assert_eq!(wallets_asked(&asked), [json!("UNIFIED")]);
    }
}

/// The demo host's account (measured 2026-11-09): it states `3` but answers `CONTRACT` with "accountType only
/// support UNIFIED.", so the venue itself names the one wallet; the inverse cash is the unified BTC, and the next pass
/// asks the unified wallet straight away.
#[test]
fn an_account_that_keeps_the_unified_wallet_alone_is_read_there() {
    let refused =
        VenueApiError { code: 10001, msg: "accountType only support UNIFIED.".to_string() };
    let (client, asked) = inverse_on(status(3), Err(refused));
    assert_eq!(client.fetch_balance(), Ok(Some(0.0099904)));
    assert_eq!(client.fetch_balance(), Ok(Some(0.0099904)));
    assert_eq!(wallets_asked(&asked), [json!("CONTRACT"), json!("UNIFIED"), json!("UNIFIED")]);
}

/// **An unknown or unreadable account version refuses the read, in words** (no wallet is guessed), and so does any
/// other refusal of the `CONTRACT` read; a refused version is not kept, so the next pass asks again.
///
/// Base: every inverse mount read the unified wallet, whatever the account's version.
#[test]
fn an_unknown_account_version_refuses_the_inverse_balance() {
    let info_down = Err(VenueApiError { code: 10002, msg: "request expired".to_string() });
    for info in [status(2), status(7), Ok(json!({"marginMode": "REGULAR_MARGIN"})), info_down] {
        let (client, asked) = inverse_on(info.clone(), contract_wallet());
        let why = client.fetch_balance().unwrap_err();
        assert!(why.contains("not guessed"), "{info:?}: {why}");
        assert!(wallets_asked(&asked).is_empty(), "no wallet is read: {info:?}");
        let _ = client.fetch_balance();
        assert_eq!(paths(&asked), [PATH_ACCOUNT_INFO, PATH_ACCOUNT_INFO], "nothing kept");
    }
    let other = VenueApiError { code: 10003, msg: "API key is invalid.".to_string() };
    let (client, asked) = inverse_on(status(3), Err(other));
    assert_eq!(client.fetch_balance(), Err("API key is invalid.".to_string()));
    assert_eq!(wallets_asked(&asked), [json!("CONTRACT")], "never the unified wallet instead");
}

/// The fee rate of an inverse mount is asked in the inverse book (the documented spelling: `category=inverse`,
/// `symbol`), a linear mount's in the linear one, as before. The demo host refuses every inverse spelling with
/// `10001` (measured), and the start then keeps the static `bybit` fee row (`resolve_fee_schedule` is fail-soft).
///
/// Base: the inverse mount's fee rate was asked in `linear`.
#[test]
fn an_inverse_mounts_fee_rate_is_asked_in_the_inverse_book() {
    let (client, asked) = recon("BTCUSD");
    assert!(client.fetch_fee_rates().is_err(), "the demo host's 10001 reaches the caller");
    assert_eq!(param(&asked, FEE_RATE, "category"), Some(json!("inverse")));

    let (linear, asked) = recon("BTCUSDT");
    assert!(linear.fetch_fee_rates().expect("the linear read").is_some());
    assert_eq!(param(&asked, FEE_RATE, "category"), Some(json!("linear")), "as before");
}

/// **A spot mount's fee rate is the SPOT book's** (FEES-AUDIT 2026-11-08): `DOGEUSDT.S` is asked as
/// `category=spot&symbol=DOGEUSDT` and the demo account's recorded 10/10 bps reach the schedule, not the
/// linear 2/5.5.
///
/// Base: the `.S` mount was asked `category=linear&symbol=DOGEUSDT.S`, a name no book lists.
#[test]
fn a_spot_mounts_fee_rate_is_asked_in_the_spot_book() {
    let (client, asked) = recon("DOGEUSDT.S");
    assert_eq!(
        client.fetch_fee_rates(),
        Ok(Some(vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 }))
    );
    assert_eq!(param(&asked, FEE_RATE, "category"), Some(json!("spot")));
    assert_eq!(param(&asked, FEE_RATE, "symbol"), Some(json!("DOGEUSDT")));
}

/// **A failed fee-rate read is said at WARN, once per client** (the caller keeps the static row and logs
/// the error at debug only): the demo host's `10001` on the inverse read reaches the caller as before, and
/// a second failed read on the same client says nothing more.
///
/// Base: the failure produced no line at all from the bridge.
#[test]
fn a_failed_fee_rate_read_warns_once() {
    let (client, _asked) = recon("BTCUSD");
    let ((first, second), log) =
        vike_log::capture::captured(|| (client.fetch_fee_rates(), client.fetch_fee_rates()));
    assert!(first.is_err() && second.is_err(), "the refusal still reaches the caller");
    let warns: Vec<_> = log
        .iter()
        .filter(|e| {
            e.level == tracing::Level::WARN && e.message.contains("fee rate could not be read")
        })
        .collect();
    assert_eq!(warns.len(), 1, "{log:?}");
    assert_eq!(warns[0].field("symbol"), Some("BTCUSD"));
    assert_eq!(warns[0].field("category"), Some("inverse"));
}

/// The mounted inverse perpetual's orders and position are read in its own book, in CONTRACTS (USD): the far limit
/// the demo rested is a live `Limit` of 8 contracts, and the recorded position a long of 8 at the harmonic entry.
/// (Already true before the amendment: the book came from the name; pinned because the coin ledger books exactly
/// these sizes.)
#[test]
fn an_inverse_mounts_orders_and_position_are_contracts_in_the_inverse_book() {
    vike_bybit::history::kept_list_for_tests();
    let (client, asked) = recon("BTCUSD");
    let orders = client.fetch_order_status_reports(0).expect("order/realtime");
    assert_eq!(param(&asked, PATH_ORDER_REALTIME, "category"), Some(json!("inverse")));
    let order = orders.iter().find(|o| o.symbol == "BTCUSD").expect("the far limit");
    assert_eq!((order.order_type.clone(), order.qty, order.side), (OrderType::Limit, 8.0, 1));
    assert!(order.status.is_live(), "{order:?}");

    let rows = client.fetch_position_status_reports().expect("position/list");
    assert_eq!(param(&asked, PATH_POSITION_LIST, "category"), Some(json!("inverse")));
    let row = rows.iter().find(|r| r.symbol == "BTCUSD").expect("the BTCUSD row");
    assert_eq!(row.qty, 8.0, "whole USD contracts, long");
    assert!((row.average_price - 76_790.170_858_13).abs() < 1e-6, "{row:?}");
}

/// **An inverse mount's funding is the inverse log, in its settle coin** (decision 0258 books a stated coin amount
/// as is): the poll asks `category=inverse&currency=BTC` (the demo host answered that query), so another coin's
/// settlement never reaches this engine; the event carries the coin amount the venue states.
///
/// Base: the poll asked `category=linear` whatever the mount's book, so an inverse mount's funding was never read.
#[test]
fn an_inverse_mounts_funding_poll_reads_the_inverse_log_in_its_coin() {
    let asked: Asked = Arc::default();
    let rest = VenueRestBybit {
        category: Category::Inverse,
        signer: signer(),
        transport: Venue { asked: Arc::clone(&asked) },
        base_url: "https://unused.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSD".to_string(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    };
    let events = BybitFundingPoller::new(&rest, "BTCUSD", 0).poll().expect("the log answered");
    assert_eq!(param(&asked, LOG_PATH, "category"), Some(json!("inverse")));
    assert_eq!(param(&asked, LOG_PATH, "currency"), Some(json!("BTC")));
    match events.as_slice() {
        [Event::Funding(f)] => {
            assert_eq!(f.symbol, "BTCUSD");
            assert_eq!(f.amount, -0.00000012, "the coin amount as the venue states it");
        }
        other => panic!("expected one funding event, got {other:?}"),
    }
}
