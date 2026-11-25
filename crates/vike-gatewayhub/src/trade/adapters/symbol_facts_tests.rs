//! `SymbolPropertiesCache` over an injected instrument list: the three venues' spellings, the refusals, and the
//! listing cache. No network: the list is a counting closure over fixture rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_gatewayhub_client::proto::SymbolPropertiesRefusalCode;
use vike_model::{
    AccountPerQuote, Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
    ContractPerp, ContractValue, Instrument, MS_PER_DAY, MS_PER_HOUR, Margining, NotionalKind,
    QuoteRate, SymbolProperties,
};

use super::{
    AccountLister, CurrencyLister, InstrumentList, NOT_ANSWERED, PerAccount, RateSource,
    SymbolPropertiesCache, SymbolPropertiesCacheAccount, SymbolPropertiesCacheAccountListed,
    verdict,
};

use vike_gatewayhub_client::test_support::fake_trade_peer::grid;

/// oanda's account-listed facts: the generic per-account facts over the bridge's spec, the bridge's test kept list
/// held first (decision 0290: a USER name is looked up as its kept row's wire name).
fn oanda_facts() -> SymbolPropertiesCacheAccountListed<vike_oanda::SymbolPropertiesAccountSpecOanda>
{
    vike_oanda::test_support::kept_list_for_tests();
    SymbolPropertiesCacheAccountListed::default()
}

fn perp(venue: &str, symbol: &str, margining: Margining, p: SymbolProperties) -> Instrument {
    Instrument::new(
        venue,
        format!("{symbol}.P"),
        "BASE",
        "USDT",
        Contract::Perp(ContractPerp {
            margining,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
    .with_properties(p)
}

fn spot(venue: &str, symbol: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(venue, symbol, "BASE", "USDT", Contract::CryptoSpot).with_properties(p)
}

/// A list that serves `rows` for every venue and counts how often it was asked.
fn counting(rows: Vec<Instrument>) -> (InstrumentList, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let list: InstrumentList = Arc::new(move |_venue| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(rows.clone())
    });
    (list, calls)
}

fn binance_rows() -> Vec<Instrument> {
    vec![
        spot("binance", "SOLUSDT", grid(0.01, 0.001)),
        perp("binance", "SOLUSDT", Margining::Linear, grid(0.001, 1.0)),
        perp("binance", "COINUSD", Margining::Inverse, grid(0.1, 1.0)),
        perp("binance", "NOTICKUSDT", Margining::Linear, grid(0.0, 1.0)),
        perp("binance", "NOSTEPUSDT", Margining::Linear, grid(0.1, 0.0)),
    ]
}

#[test]
fn binance_and_aster_split_spot_from_the_perp_lane_by_the_p_suffix() {
    for venue in ["binance", "aster"] {
        let (list, _) = counting(binance_rows());
        let facts = SymbolPropertiesCache::new(list);
        let spot = facts.facts(venue, "SOLUSDT").expect("spot");
        assert_eq!((spot.properties.tick_size, spot.properties.step_size), (0.01, 0.001));
        assert_eq!(
            (spot.margin_mode, spot.margin_rate),
            (Some(vike_model::MarginMode::Cash), Some(1.0)),
            "{venue}: a spot pair is fully funded"
        );
        let perp = facts.facts(venue, "SOLUSDT.P").expect("perp");
        assert_eq!((perp.properties.tick_size, perp.properties.step_size), (0.001, 1.0));
        assert_eq!(perp.properties.min_notional, 5.0, "the row's own minimums travel");
        assert_eq!(perp.contract_value, None);
        assert_eq!(
            (perp.margin_mode, perp.margin_rate),
            (None, None),
            "{venue}: cross states nothing"
        );
        assert_eq!(facts.facts(venue, "solusdt.p").expect("case-insensitive"), perp);
    }
}

/// **A perpetual is its USER name `X.P`** (decision 0290): the linear `SOLUSDT.P` and the inverse `BTCUSD.P` are
/// answered from their own rows; the bare `SOLUSDT` is the spot pair, which this list does not carry.
#[test]
fn bybit_reads_a_perpetual_by_its_user_name() {
    let rows = vec![
        perp("bybit", "SOLUSDT", Margining::Linear, grid(0.001, 0.1)),
        Instrument::new("bybit", "BTCUSD.P", "BTC", "USD", perp_inverse())
            .with_venue_word("InversePerpetual")
            .with_properties(grid(0.5, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolPropertiesCache::new(list);
    let sol = facts.facts("bybit", "SOLUSDT.P").expect("linear perp");
    assert_eq!((sol.properties.tick_size, sol.properties.step_size), (0.001, 0.1));
    assert_eq!(sol.contract_value, None, "a linear perpetual is the default contract value");
    assert!(facts.facts("bybit", "SOLUSDT").is_err(), "the bare name is the spot pair");
    // Decision 0231: a coin-margined perpetual is admitted, as an INVERSE contract (one unit of quantity is one
    // USD of notional), where it used to be refused by name.
    let inverse = facts.facts("bybit", "BTCUSD.P").expect("a coin-margined perpetual is admitted");
    assert_eq!((inverse.properties.tick_size, inverse.properties.step_size), (0.5, 1.0));
    assert_eq!(
        inverse.contract_value.map(|v| (v.kind, v.per_unit)),
        Some((NotionalKind::Inverse, 1.0))
    );
    assert_eq!(inverse.margin_mode, None);
}

fn dated(symbol: &str, margining: Margining, expiry_ms: i64, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "bybit",
        symbol,
        "BASE",
        "USDT",
        Contract::Future(ContractFuture {
            margining,
            settle: "USDT".into(),
            value_currency: None,
            expiry_ms,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        }),
    )
    .with_venue_word(if margining == Margining::Linear {
        "LinearFutures"
    } else {
        "InverseFutures"
    })
    .with_properties(p)
}

/// **A dated future is admitted under its USER name** (decision 0290: `ETHUSDT_371127`, the kept row of the venue's
/// `ETHUSDT-27NOV37`, whose key the venue's row carries), the contract is linear or inverse by its margining, and a
/// contract already delivered (the day its user name states) is refused in words (the venue closes every position at
/// delivery and this adapter does not fold that). Inside its last day it is admitted close-only: the bridge refuses
/// each opening order at send. The venue's own dated name is no user name.
#[test]
fn bybit_admits_a_dated_future_by_its_user_name_and_refuses_one_delivered() {
    crate::testkit::fixtures::hold_bybit_kept_rows();
    let rows = vec![
        dated("ETHUSDT-27NOV37", Margining::Linear, 2_142_921_600_000, grid(0.01, 0.01)),
        dated("ETHUSDT-01NOV26", Margining::Linear, 1_793_520_000_000, grid(0.01, 0.01)),
        dated("ETHUSDT-01JAN20", Margining::Linear, 1_577_865_600_000, grid(0.01, 0.01)),
        dated("BTCUSDZ37", Margining::Inverse, 2_145_340_800_000, grid(0.5, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolPropertiesCache::new(list);
    let far = facts.facts("bybit", "ETHUSDT_371127").expect("a far dated linear future");
    assert_eq!((far.properties.tick_size, far.properties.step_size), (0.01, 0.01));
    assert_eq!(far.contract_value, None);
    assert_eq!(facts.facts("bybit", "ethusdt_371127").expect("case-insensitive"), far);

    let inverse = facts.facts("bybit", "BTCUSD_371225").expect("a far dated coin-margined future");
    assert_eq!(inverse.contract_value.map(|v| v.kind), Some(NotionalKind::Inverse));

    for symbol in ["ETHUSDT_261101", "ETHUSDT_200101"] {
        let err = facts.facts("bybit", symbol).expect_err("delivered");
        assert!(
            err.reason.contains("delivers in 0 h") && err.reason.contains("next one"),
            "{symbol}: {err}"
        );
    }
}

/// **A spot pair is the bare USER name** (decision 0290), answered `Cash` at a margin rate of 1 (fully funded, never
/// liquidated) on the SPOT row's grid; the perpetual of the same venue name is `DOGEUSDT.P`, its own row. A list that
/// carries only the perpetual has no pair to answer, and the old spot spelling `DOGEUSDT.S` is no name at all.
#[test]
fn bybit_reads_a_spot_pair_by_its_user_name_as_cash_and_keeps_the_perpetual_apart() {
    let rows = vec![
        spot("bybit", "DOGEUSDT", grid(0.00001, 0.1)),
        perp("bybit", "DOGEUSDT", Margining::Linear, grid(0.00001, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolPropertiesCache::new(list);
    let pair = facts.facts("bybit", "DOGEUSDT").expect("the spot pair");
    assert_eq!((pair.properties.tick_size, pair.properties.step_size), (0.00001, 0.1));
    assert_eq!(pair.margin_mode, Some(vike_model::MarginMode::Cash));
    assert_eq!(pair.margin_rate, Some(1.0));
    assert_eq!(pair.contract_value, None);
    let perpetual = facts.facts("bybit", "DOGEUSDT.P").expect("the perpetual of the same name");
    assert_eq!(perpetual.properties.step_size, 1.0, "the perpetual's own grid, not the pair's");
    assert_eq!(perpetual.margin_mode, None);

    let err = facts.facts("bybit", "NOPEUSDT").expect_err("an unlisted pair");
    assert_eq!(err.code, SymbolPropertiesRefusalCode::NoInstrument, "{err}");
    assert!(
        err.reason.starts_with("bybit: no instrument NOPEUSDT in the venue's instrument list"),
        "{err}"
    );
    assert!(facts.facts("bybit", "DOGEUSDT.S").is_err(), "the old spot spelling");
    // Only the perpetual listed: the pair's name keys no row.
    let (list, _) = counting(vec![perp("bybit", "DOGEUSDT", Margining::Linear, grid(0.1, 1.0))]);
    assert!(SymbolPropertiesCache::new(list).facts("bybit", "DOGEUSDT").is_err());
}

fn perp_inverse() -> Contract {
    Contract::Perp(ContractPerp {
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    })
}

#[test]
fn an_unknown_symbol_names_the_venue_the_symbol_and_the_lane() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolPropertiesCache::new(list);
    let err = facts.facts("binance", "NOPEUSDT.P").expect_err("unknown");
    assert!(
        err.reason.starts_with(
            "binance: no instrument NOPEUSDT.P in the venue's instrument list (spot, USDS-M \
             perpetuals (TradFi included) and dated USDS-M futures that are TRADING"
        ),
        "{err}"
    );
    assert_eq!(
        err.code,
        SymbolPropertiesRefusalCode::NoInstrument,
        "an unknown symbol is gone, not a retry"
    );
    // Aster's list is its perpetuals: the old words.
    let err = facts.facts("aster", "NOPEUSDT.P").expect_err("unknown");
    assert_eq!(
        err.reason,
        "aster: no instrument NOPEUSDT.P in the venue's instrument list (spot and USDS-M \
         perpetuals)"
    );
    // The perp lane is not the spot lane: a bare symbol that only exists as a perp is unknown.
    let rows = vec![perp("bybit", "SOLUSDT", Margining::Linear, grid(0.1, 1.0))];
    let (list, _) = counting(rows);
    let err = SymbolPropertiesCache::new(list).facts("bybit", "ETHUSDT").expect_err("unknown");
    assert!(
        err.reason.starts_with("bybit: no instrument ETHUSDT in the venue's instrument list"),
        "{err}"
    );
}

#[test]
fn a_perp_that_is_not_linear_is_refused_with_the_venues_word() {
    let (list, _) = counting(binance_rows());
    let err = SymbolPropertiesCache::new(list).facts("binance", "COINUSD.P").expect_err("inverse");
    assert_eq!(err.code, SymbolPropertiesRefusalCode::Verdict, "{err}");
    assert!(err.reason.contains("not a linear perpetual"), "{err}");
}

#[test]
fn a_missing_tick_or_step_is_refused_naming_which() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolPropertiesCache::new(list);
    let tick = facts.facts("binance", "NOTICKUSDT.P").expect_err("no tick");
    assert!(tick.reason.contains("no tick size"), "{tick}");
    let step = facts.facts("binance", "NOSTEPUSDT.P").expect_err("no step");
    assert!(step.reason.contains("no step size"), "{step}");
}

#[test]
fn another_venue_keeps_the_default_answer_and_asks_nothing() {
    let (list, calls) = counting(binance_rows());
    let facts = SymbolPropertiesCache::new(list);
    // oanda included: its fences (the account's currency, what the adapter writes) are its spec's,
    // so a PUBLIC source of oanda rows answers nothing.
    for venue in ["ctrader", "oanda"] {
        let err = facts.facts(venue, "EUR_USD").expect_err("not served");
        assert_eq!(err, verdict(NOT_ANSWERED), "{venue}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_fresh_listing_is_not_fetched_again_and_a_stale_one_is() {
    let (list, calls) = counting(binance_rows());
    let facts = SymbolPropertiesCache::with_ttl(list, Duration::from_secs(3600));
    for _ in 0..3 {
        facts.facts("binance", "SOLUSDT").expect("spot");
        facts.facts("binance", "SOLUSDT.P").expect("perp");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing served six asks");
    facts.facts("aster", "SOLUSDT").expect("another venue lists for itself");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let (list, calls) = counting(binance_rows());
    let stale = SymbolPropertiesCache::with_ttl(list, Duration::ZERO);
    stale.facts("binance", "SOLUSDT").expect("first");
    stale.facts("binance", "SOLUSDT").expect("second");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a listing past its keep time is fetched again");
}

#[test]
fn a_failed_listing_is_remembered_briefly_and_is_not_a_kept_listing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let list: InstrumentList = Arc::new(move |_venue| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("HTTP 503".to_string())
        } else {
            Ok(Vec::new())
        }
    });
    // A zero failure memory: every ask fetches, and neither answer is kept as a listing.
    let facts = SymbolPropertiesCache::with_ttls(list, Duration::from_secs(3600), Duration::ZERO);
    let failed = facts.facts("binance", "SOLUSDT").expect_err("failed");
    assert_eq!(
        failed.reason,
        "binance: the venue's instrument list could not be fetched: HTTP 503"
    );
    assert_eq!(
        failed.code,
        SymbolPropertiesRefusalCode::ListUnavailable,
        "a failed fetch is not a verdict on the symbol"
    );
    let empty = facts.facts("binance", "SOLUSDT").expect_err("empty");
    assert!(empty.reason.contains("came back empty"), "{empty}");
    assert_eq!(empty.code, SymbolPropertiesRefusalCode::ListEmpty, "{empty}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "neither answer was kept");
}

/// The negative cache: a venue that just failed is not asked again within [`FAILED_TTL`], however many
/// orders and previews arrive, and the next ask after it fetches afresh.
#[test]
fn a_flood_against_a_failing_venue_costs_one_fetch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let list: InstrumentList = Arc::new(move |_venue| {
        seen.fetch_add(1, Ordering::SeqCst);
        Err("HTTP 429".to_string())
    });
    let facts = SymbolPropertiesCache::new(list);
    for _ in 0..50 {
        assert_eq!(
            facts.facts("binance", "SOLUSDT").expect_err("rate limited").reason,
            "binance: the venue's instrument list could not be fetched: HTTP 429"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "fifty asks, one fetch");
    facts.facts("aster", "SOLUSDT").expect_err("another venue is asked for itself");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let patient = SymbolPropertiesCache::with_ttls(
        Arc::new(|_: &str| Err("down".to_string())),
        Duration::from_secs(60),
        Duration::ZERO,
    );
    patient.facts("binance", "SOLUSDT").expect_err("down");
    patient.facts("binance", "SOLUSDT").expect_err("down again");
}

#[test]
fn concurrent_askers_share_one_fetch() {
    let (list, calls) = counting(binance_rows());
    let slow: InstrumentList = {
        let inner = list;
        Arc::new(move |venue| {
            std::thread::sleep(Duration::from_millis(100));
            inner(venue)
        })
    };
    let facts = Arc::new(SymbolPropertiesCache::new(slow));
    let askers: Vec<_> = (0..4)
        .map(|_| {
            let facts = Arc::clone(&facts);
            std::thread::spawn(move || facts.facts("binance", "SOLUSDT").is_ok())
        })
        .collect();
    assert!(askers.into_iter().all(|a| a.join().expect("asker")));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---- okx: a SWAP's facts are the grid in BASE units, the one the mounted path uses ---------------

fn okx_row(
    instrument: &str,
    inst_type: &str,
    settle: &str,
    ct: (&str, &str, &str, &str),
    grid: (&str, &str, &str, &str),
) -> serde_json::Value {
    let (ct_val, ct_mult, contract_val_currency, contract_type) = ct;
    let (tick, lot, min, max) = grid;
    let uly = instrument.split('-').take(2).collect::<Vec<_>>().join("-");
    serde_json::json!({
        "instType": inst_type, "instId": instrument, "uly": uly, "instFamily": uly,
        "baseCcy": "", "quoteCcy": "", "settleCcy": settle,
        "ctVal": ct_val, "ctMult": ct_mult, "ctValCcy": contract_val_currency, "ctType": contract_type,
        // a future's delivery, 90 days from the test's clock: far outside the admission's last-day fence
        "expTime": if inst_type == "FUTURES" { in_ms(90 * 86_400_000) } else { String::new() },
        "alias": "", "lever": "50", "tickSz": tick, "lotSz": lot, "minSz": min, "maxMktSz": max,
        "state": "live",
    })
}

/// `now + ms` as the venue spells an epoch-millisecond field: a STRING of digits.
fn in_ms(ms: i64) -> String {
    (vike_model::now_ms() + ms).to_string()
}

/// A linear USDT future delivered in three hours: inside its last day, where it is admitted close-only.
fn near_delivery_future() -> serde_json::Value {
    let mut row = okx_row(
        "ETH-USDT-261226",
        "FUTURES",
        "USDT",
        ("0.1", "1", "ETH", "linear"),
        ("0.01", "1", "1", "5000"),
    );
    row["expTime"] = serde_json::Value::String(in_ms(3 * 3_600_000));
    row
}

/// The shapes OKX lists: three linear USDT SWAPs of different contract sizes, and every row the
/// gatewayhub must refuse (inverse, USDC-settled, contract counted in the quote, spot, dated).
fn okx_payload() -> serde_json::Value {
    let swap = |instrument, ct_val, currency, grid| {
        okx_row(instrument, "SWAP", "USDT", (ct_val, "1", currency, "linear"), grid)
    };
    serde_json::json!({ "code": "0", "msg": "", "data": [
        swap("BTC-USDT-SWAP", "0.01", "BTC", ("0.1", "0.01", "0.01", "35000")),
        swap("ETH-USDT-SWAP", "0.1", "ETH", ("0.01", "0.01", "0.01", "15000")),
        swap("DOGE-USDT-SWAP", "1000", "DOGE", ("0.00001", "1", "1", "100000")),
        swap("QUOTE-USDT-SWAP", "1", "USDT", ("0.1", "1", "1", "1000")),
        swap("NOCT-USDT-SWAP", "", "NOCT", ("0.1", "1", "1", "1000")),
        okx_row("BTC-USD-SWAP", "SWAP", "BTC", ("100", "1", "USD", "inverse"), ("0.1", "0.1", "0.1", "49000")),
        okx_row("BTC-USDC-SWAP", "SWAP", "USDC", ("0.001", "1", "BTC", "linear"), ("0.1", "0.1", "0.1", "5000")),
        okx_row("BTC-USDT-261225", "FUTURES", "USDT", ("0.01", "1", "BTC", "linear"), ("0.1", "0.01", "0.01", "5000")),
        // delivered in three hours: inside its last day, admitted close-only
        near_delivery_future(),
        okx_row("BTC-USD-261225", "FUTURES", "BTC", ("100", "1", "USD", "inverse"), ("0.1", "0.1", "0.1", "5000")),
        okx_row("BTC-USDC-261225", "FUTURES", "USDC", ("0.01", "1", "BTC", "linear"), ("0.1", "0.01", "0.01", "5000")),
        serde_json::json!({
            "instType": "SPOT", "instId": "BTC-USDT", "baseCcy": "BTC", "quoteCcy": "USDT",
            "tickSz": "0.1", "lotSz": "0.00000001", "minSz": "0.00001", "maxMktSz": "1000000",
            "state": "live",
        }),
        serde_json::json!({
            "instType": "SPOT", "instId": "BTC-USDC", "baseCcy": "BTC", "quoteCcy": "USDC",
            "tickSz": "0.1", "lotSz": "0.00000001", "minSz": "0.00001", "maxMktSz": "1000000",
            "state": "live",
        }),
        // the demo's BTC-USD option row, the fields the catalog reads
        serde_json::json!({
            "instType": "OPTION", "instId": "BTC-USD-261106-76000-C", "uly": "BTC-USD",
            "instFamily": "BTC-USD", "ctVal": "1", "ctMult": "0.01", "ctValCcy": "BTC",
            "ctType": "inverse", "settleCcy": "BTC", "optType": "C", "stk": "76000",
            "expTime": in_ms(7 * 86_400_000), "tickSz": "0.0001", "lotSz": "1", "minSz": "1",
            "maxMktSz": "5000", "state": "live",
        }),
    ]})
}

fn okx_facts() -> SymbolPropertiesCache {
    let rows = vike_okx::catalog::parse_instruments(&okx_payload());
    assert_eq!(rows.len(), 14, "every fixture row parses into the catalog");
    let (list, _) = counting(rows);
    SymbolPropertiesCache::new(list)
}

/// A USDT-quoted SPOT pair is admitted as a fully funded cash position (decision 0232): its grid as the
/// venue lists it (size counts the BASE coin already, nothing is scaled), margin mode `Cash`, margin rate 1.
#[test]
fn okx_admits_a_usdt_quoted_spot_pair_as_cash() {
    let facts = okx_facts();
    let got = facts.facts("okx", "BTC-USDT").expect("a USDT-quoted spot pair is admitted");
    assert_eq!(got.margin_mode, Some(vike_model::MarginMode::Cash));
    assert_eq!(got.margin_rate, Some(1.0));
    assert_eq!(got.contract_value, None, "a spot pair is linear 1.0");
    let p = got.properties;
    assert_eq!((p.tick_size, p.step_size, p.min_qty), (0.1, 0.00000001, 0.00001));
    assert_eq!(p.max_qty, 0.0, "the venue states a spot maxMktSz in the QUOTE currency");
    assert_eq!(p.contract_size, 0.0);
    // case is folded like the others
    assert!(facts.facts("okx", "btc-usdt").is_ok());
    // a SWAP and a dated future state no mode: they open in the engine's default
    for instrument in ["BTC-USDT-SWAP", "BTC-USDT-261225"] {
        let got = facts.facts("okx", instrument).expect(instrument);
        assert_eq!((got.margin_mode, got.margin_rate), (None, None), "{instrument}");
    }
}

/// **A dated USDT-linear future inside its last day is ADMITTED on its base-unit grid** (close-only: the engine books
/// no delivery settlement, so the bridge refuses each OPENING order at send, okx's rule for every venue,
/// `crates/vike-venues/src/venue_delivery_guard.rs`'s `delivery_verdict`; a delivered one is refused by the bridge's
/// own tests). The grid equality with the mounted path is `crates/bridges/okx/src/symbol_properties_tests.rs`'s.
#[test]
fn okx_admits_a_future_inside_its_last_day() {
    let facts = okx_facts();
    let got = facts.facts("okx", "ETH-USDT-261226").expect("admitted close-only");
    assert_eq!((got.contract_value, got.margin_mode), (None, None), "a linear dated future");
}

/// **A coin-margined okx SWAP or dated future is answered `Inverse(ctVal x ctMult)` on its CONTRACTS grid**
/// (decision 0258's okx amendment): one unit of quantity is one contract of 100 USD, so the grid is the venue's
/// as listed, equal to the mounted path's (`vike_okx::execution::mount_money` passes contracts through), and no
/// second multiplier rides `contract_size`. The node then admits it only into an engine of that kind and coin.
///
/// Base: both were refused ("not a linear perpetual or future").
#[test]
fn okx_answers_a_coin_margined_contract_inverse_on_its_contracts_grid() {
    let execution_rows = vike_okx::execution::rest::parse_okx_perp_instruments(&okx_payload());
    let facts = okx_facts();
    for instrument in ["BTC-USD-SWAP", "BTC-USD-261225"] {
        let got = facts.facts("okx", instrument).unwrap_or_else(|e| panic!("{instrument}: {e}"));
        assert_eq!(
            got.contract_value,
            Some(vike_model::ContractValue::inverse(100.0)),
            "{instrument}"
        );
        assert_eq!(
            got.properties, execution_rows[instrument].properties,
            "{instrument}: contracts, unscaled"
        );
        assert_eq!(got.properties.contract_size, 0.0, "{instrument}: the unit rides the value");
        assert_eq!((got.margin_mode, got.margin_rate), (None, None), "{instrument}");
    }
}

/// Only a linear USDT-settled or coin-margined SWAP or future and a USDT-quoted spot pair are admitted, and
/// each refusal is in the venue's own word.
#[test]
fn okx_refuses_what_is_not_a_linear_usdt_contract_or_a_usdt_pair_naming_why() {
    let facts = okx_facts();
    let why = |symbol: &str| facts.facts("okx", symbol).expect_err(symbol).reason;
    let option = why("BTC-USD-261106-76000-C");
    assert!(option.contains("option") && option.contains("51198"), "{option}");
    let spot_usdc = why("BTC-USDC");
    assert!(
        spot_usdc.contains("quoted in USDC") && spot_usdc.contains("ledger is USDT"),
        "{spot_usdc}"
    );
    let usdc = why("BTC-USDC-SWAP");
    assert!(usdc.contains("USDC") && usdc.contains("USDT-settled"), "{usdc}");
    let usdc_future = why("BTC-USDC-261225");
    assert!(usdc_future.contains("settles in USDC"), "{usdc_future}");
    let quote = why("QUOTE-USDT-SWAP");
    assert!(quote.contains("USDT") && quote.contains("not in the base asset"), "{quote}");
    let no_contract = why("NOCT-USDT-SWAP");
    assert!(no_contract.contains("no contract size"), "{no_contract}");
    let unknown = why("NOPE-USDT-SWAP");
    assert_eq!(
        unknown,
        "okx: no instrument NOPE-USDT-SWAP in the venue's instrument list (USDT-quoted spot pairs, \
         linear USDT-settled SWAP perpetuals and dated futures)"
    );
}

/// A spot pair and a dated future are read ONE at a time when a lister is set; a SWAP stays in the
/// bulk list (decision 0232). The lister is asked the venue slug and the upper-cased id.
#[test]
fn okx_reads_a_spot_pair_and_a_future_one_at_a_time_and_a_swap_from_the_list() {
    let rows = vike_okx::catalog::parse_instruments(&okx_payload());
    let (list, list_calls) = counting(rows.clone());
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let lister: super::SymbolLister = Arc::new(move |venue, symbol| {
        seen.lock().unwrap().push(format!("{venue}/{symbol}"));
        Ok(rows.iter().filter(|r| r.raw_symbol == symbol).cloned().collect())
    });
    let facts = SymbolPropertiesCache::new(list).with_symbol_lister(lister);
    facts.facts("okx", "btc-usdt").expect("spot");
    facts.facts("okx", "BTC-USDT-261225").expect("future");
    assert_eq!(*asked.lock().unwrap(), ["okx/BTC-USDT", "okx/BTC-USDT-261225"]);
    assert_eq!(list_calls.load(Ordering::SeqCst), 0, "no bulk list for them");
    facts.facts("okx", "ETH-USDT-SWAP").expect("swap");
    assert_eq!(list_calls.load(Ordering::SeqCst), 1, "a SWAP is the bulk list's");
    assert_eq!(asked.lock().unwrap().len(), 2, "and no one-symbol read");
    // an id the venue does not list answers an empty list: the usual named refusal
    let why = facts.facts("okx", "NOPE-USDT").expect_err("unknown spot pair");
    assert_eq!(why.code, SymbolPropertiesRefusalCode::NoInstrument, "{why}");
    assert!(why.reason.contains("no instrument NOPE-USDT"), "{why}");
}

/// **The two OKX networks list different things**, and the asking account's network chooses (the proof of
/// 2026-10-31: three USDT dated futures exist on the demo alone, and the node refused them all because the facts
/// were read from mainnet). MAINNET here lists `DOGE-USDT-SWAP` and no `QTUM-USDT-261106`; the DEMO list is the
/// reverse. A `testnet` (demo) ask is answered from the demo list and a `mainnet` one, and the plain `facts`,
/// from mainnet's, exactly as before; the refusals by name are the same on both.
#[test]
fn an_okx_demo_account_is_answered_from_the_simulated_trading_list_and_a_live_one_from_mainnets() {
    let qtum = okx_row(
        "QTUM-USDT-261106",
        "FUTURES",
        "USDT",
        ("10", "1", "QTUM", "linear"),
        ("0.001", "1", "1", "1000"),
    );
    let mainnet_rows = vike_okx::catalog::parse_instruments(&okx_payload());
    let mut demo_payload = okx_payload();
    let data = demo_payload["data"].as_array_mut().expect("rows");
    data.retain(|row| row["instId"] != "DOGE-USDT-SWAP");
    data.push(qtum);
    let demo_rows = vike_okx::catalog::parse_instruments(&demo_payload);
    let rows_of = move |key: &str| match key {
        "okx" => Ok(mainnet_rows.clone()),
        k if k == vike_okx::symbol_properties::OKX_DEMO_LIST => Ok(demo_rows.clone()),
        other => Err(format!("{other}: no such okx list")),
    };
    let swaps = rows_of.clone();
    let list: InstrumentList = Arc::new(move |key| {
        swaps(key)
            .map(|rows| rows.into_iter().filter(|r| r.raw_symbol.ends_with("-SWAP")).collect())
    });
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let lister: super::SymbolLister = Arc::new(move |key, symbol| {
        seen.lock().expect("asked").push(format!("{key}/{symbol}"));
        rows_of(key).map(|rows| rows.into_iter().filter(|r| r.raw_symbol == symbol).collect())
    });
    let facts = SymbolPropertiesCache::new(list).with_symbol_lister(lister);

    // the demo-only dated future: admitted for the demo account, refused by name for the live one
    let future = facts.facts_on("okx", "testnet", "QTUM-USDT-261106").expect("on the demo list");
    assert_eq!(
        (future.margin_mode, future.margin_rate),
        (None, None),
        "a dated future opens as a swap"
    );
    assert_eq!(future.properties.step_size, 10.0, "1 contract of 10 QTUM, in base units");
    let live = facts
        .facts_on("okx", "mainnet", "QTUM-USDT-261106")
        .expect_err("mainnet lists none")
        .reason;
    assert!(
        live.contains("no instrument QTUM-USDT-261106") && !live.contains("simulated"),
        "{live}"
    );
    // a swap only mainnet lists: the demo account is refused, in words naming its network
    let swap = facts
        .facts_on("okx", "testnet", "DOGE-USDT-SWAP")
        .expect_err("not on the demo list")
        .reason;
    assert!(
        swap.contains("no instrument DOGE-USDT-SWAP") && swap.contains("simulated-trading"),
        "{swap}"
    );
    assert!(facts.facts_on("okx", "mainnet", "DOGE-USDT-SWAP").is_ok());
    assert!(facts.facts("okx", "DOGE-USDT-SWAP").is_ok(), "the plain ask is mainnet's, unchanged");
    // what both networks list still resolves on the demo: a swap, a dated future and a spot pair as cash
    for instrument in ["BTC-USDT-SWAP", "ETH-USDT-SWAP", "BTC-USDT-261225"] {
        facts
            .facts_on("okx", "testnet", instrument)
            .unwrap_or_else(|e| panic!("{instrument}: {e}"));
    }
    let spot = facts.facts_on("okx", "testnet", "btc-usdt").expect("spot");
    assert_eq!(spot.margin_mode, Some(vike_model::MarginMode::Cash));
    // the refusals by name do not depend on the network
    for (instrument, word) in [
        ("BTC-USDC", "quoted in USDC"),
        ("BTC-USD-261106-76000-C", "option"),
        ("BTC-USDC-SWAP", "USDT-settled"),
    ] {
        for network in ["testnet", "mainnet"] {
            let why = facts.facts_on("okx", network, instrument).expect_err(instrument);
            assert!(why.reason.contains(word), "{network} {instrument}: {why}");
        }
    }
    // each network's one-symbol reads are asked under its own key (own slots, own lookup budget)
    let asked = asked.lock().expect("asked");
    assert!(asked.iter().any(|a| a == "okx@demo/QTUM-USDT-261106"), "{asked:?}");
    assert!(asked.iter().any(|a| a == "okx/QTUM-USDT-261106"), "{asked:?}");
    assert!(
        !asked.iter().any(|a| a == "okx/BTC-USDT-261225"),
        "a demo ask never reads mainnet's rows"
    );
}

// ---------------------------------------------------------------------------------------------
// hyperliquid: a USER name keys its kept row's listing name, exactly as the venue lists it (decision 0290)
// ---------------------------------------------------------------------------------------------

/// A core perp row as the hyperliquid catalog builds it: the bare coin, linear, USDC-settled.
fn hl_perp(coin: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "hyperliquid",
        coin,
        coin,
        "USDC",
        Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_currency: None,
            funding_interval_ms: Some(3_600_000),
            settling_ms: None,
        }),
    )
    .with_properties(p)
}

fn hl_rows() -> Vec<Instrument> {
    vec![
        hl_perp("BTC", grid(0.1, 0.00001)),
        hl_perp("kPEPE", grid(0.000001, 1.0)),
        hl_perp("RLB", grid(0.0001, 1.0)).with_venue_word("onlyIsolated"),
        hl_perp("OLD", grid(0.01, 0.1)).with_venue_word("isDelisted"),
        hl_perp("NOTICK", grid(0.0, 1.0)),
        hl_perp("test:ABC", grid(0.01, 0.01)).with_wire(vike_model::InstrumentVenueIds {
            asset_id: Some(110_000),
            coin: Some("test:ABC".into()),
            group: Some("test".into()),
        }),
        hl_spot("HYPE/USDC", "USDC", grid(0.001, 0.01)),
        hl_spot("ABC/USDH", "USDH", grid(0.01, 1.0)),
    ]
}

/// A spot pair row as the hyperliquid catalog builds it: `BASE/QUOTE`, crypto spot.
fn hl_spot(symbol: &str, quote: &str, p: SymbolProperties) -> Instrument {
    let base = symbol.split('/').next().unwrap_or(symbol);
    Instrument::new("hyperliquid", symbol, base, quote, Contract::CryptoSpot).with_properties(p)
}

#[test]
fn hyperliquid_answers_a_core_perp_by_its_user_name() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let (list, _) = counting(hl_rows());
    let facts = SymbolPropertiesCache::new(list);
    let btc = facts.facts("hyperliquid", "BTCUSDC.P").expect("BTCUSDC.P");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (0.1, 0.00001));
    assert_eq!(btc.contract_value, None, "a linear perpetual sized in the coin: Linear, 1.0");
    // A user name is read in any case; the key is its kept row's `raw_symbol`, case kept (`kPEPE`).
    let pepe = facts.facts("hyperliquid", "kpepeusdc.p").expect("KPEPEUSDC.P");
    assert_eq!((pepe.properties.tick_size, pepe.properties.step_size), (0.000001, 1.0));
    // The coin names themselves are old spellings (decision 0290): no kept row answers them.
    for old in ["kPEPE", "KPEPE", "BTC", "btc"] {
        let err = facts.facts("hyperliquid", old).expect_err(old).reason;
        assert!(err.starts_with(NOT_ANSWERED) && err.contains("carries no row"), "{old}: {err}");
    }
}

#[test]
fn the_other_venues_still_upper_case_and_hyperliquid_does_not_leak_into_them() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolPropertiesCache::new(list);
    assert!(facts.facts("binance", "solusdt").is_ok(), "binance keeps upper-casing");
    assert!(facts.facts("binance", "solusdt.p").is_ok());
    let (list, _) = counting(vec![hl_perp("kPEPE", grid(0.1, 1.0))]);
    let err = SymbolPropertiesCache::new(list)
        .facts("bybit", "kPEPE")
        .expect_err("bybit looks up KPEPE, a name its list does not key");
    assert!(err.reason.starts_with("bybit: no instrument KPEPE in the venue's"), "{err}");
}

/// **A USDC spot pair is answered `Cash` at a margin rate of 1** (decision 0218): fully funded, never liquidated,
/// sized on the pair's own grid; a pair quoted in another token is refused by name, and the venue's wire name `@N`
/// is refused before any listing is read, naming the spelling to use.
#[test]
fn hyperliquid_answers_a_usdc_spot_pair_cash_and_refuses_the_other_quotes() {
    use vike_model::MarginMode;
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let (list, calls) = counting(hl_rows());
    let facts = SymbolPropertiesCache::new(list);
    let hype = facts.facts("hyperliquid", "HYPEUSDC").expect("HYPEUSDC");
    assert_eq!(hype.margin_mode, Some(MarginMode::Cash));
    assert_eq!(hype.margin_rate, Some(1.0), "a spot position margins at its whole notional");
    assert_eq!((hype.properties.tick_size, hype.properties.step_size), (0.001, 0.01));
    assert_eq!(hype.contract_value, None, "a spot size is the base token: Linear, 1.0");
    let usdh = facts.facts("hyperliquid", "ABCUSDH").expect_err("quoted in USDH").reason;
    assert!(usdh.contains("USDH") && usdh.contains("USDC") && usdh.contains("ABCUSDH"), "{usdh}");

    let before = calls.load(Ordering::SeqCst);
    // The pair's listing name is an old spelling (decision 0290), in any case.
    for old in ["HYPE/USDC", "hype/usdc"] {
        let why = facts.facts("hyperliquid", old).expect_err(old).reason;
        assert!(why.starts_with(NOT_ANSWERED) && why.contains("carries no row"), "{old}: {why}");
    }
    let wire = facts.facts("hyperliquid", "@107").expect_err("the wire name").reason;
    assert!(wire.starts_with(NOT_ANSWERED) && wire.contains("@107"), "{wire}");
    assert!(wire.contains("HYPEUSDC"), "it names the spelling to use: {wire}");
    assert_eq!(calls.load(Ordering::SeqCst), before, "and asks the venue nothing");
}

/// **An isolated-only perpetual is answered `Isolated`** (decision 0218) where it used to be refused; a
/// delisted one is still refused, a plain one states no mode, and a missing tick is still named.
#[test]
fn hyperliquid_answers_an_isolated_only_perp_isolated_and_still_refuses_a_delisted_one() {
    use vike_model::MarginMode;
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let (list, _) = counting(hl_rows());
    let facts = SymbolPropertiesCache::new(list);
    let iso = facts.facts("hyperliquid", "RLBUSDC.P").expect("isolated-only is answered");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    assert_eq!(iso.margin_rate, None, "the engine's own margin rate stands");
    assert_eq!(iso.properties.tick_size, 0.0001);
    let old = facts.facts("hyperliquid", "OLDUSDC.P").expect_err("delisted");
    assert_eq!(old.code, SymbolPropertiesRefusalCode::Verdict, "{old}");
    assert!(old.reason.contains("delisted") && old.reason.contains("OLDUSDC.P"), "{old}");
    let btc = facts.facts("hyperliquid", "BTCUSDC.P").expect("a plain perp");
    assert_eq!((btc.margin_mode, btc.margin_rate), (None, None), "cross states nothing");
    let tick = facts.facts("hyperliquid", "NOTICKUSDC.P").expect_err("no tick");
    assert!(tick.reason.contains("no tick size"), "{tick}");
    // The belt: a row of a kind the catalog may one day list is not answered (its kept row is the fixture's
    // perpetual `WEIRD`, so the name reaches the listing's index row).
    let (list, _) = counting(vec![
        Instrument::new("hyperliquid", "WEIRD", "WEIRD", "USDC", Contract::Index)
            .with_properties(grid(0.1, 1.0)),
    ]);
    let err = SymbolPropertiesCache::new(list)
        .facts("hyperliquid", "WEIRDUSDC.P")
        .expect_err("not a perp or spot");
    assert!(err.reason.starts_with(NOT_ANSWERED), "{err}");
}

/// **A builder-dex coin is answered from its listing, or ONE dex at a time** (decision 0218): a plain list that
/// carries the `dex:COIN` row (the HIP-3 flag's) answers its user name (`TEST:ABCUSDC.P`, decision 0290) with its own
/// mode; with a per-symbol source set it is read by its listing name with that dex alone, on the asking account's
/// network, and the whole list is never fetched.
#[test]
fn hyperliquid_answers_a_builder_dex_coin_alone_on_the_accounts_network() {
    use std::sync::Mutex;
    use vike_model::MarginMode;
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let dex_coin = |name: &str, word: Option<&str>| {
        let row = hl_perp(name, grid(0.01, 0.01)).with_wire(vike_model::InstrumentVenueIds {
            asset_id: Some(110_000),
            coin: Some(name.into()),
            group: Some("test".into()),
        });
        match word {
            Some(word) => row.with_venue_word(word),
            None => row,
        }
    };
    // From the plain listing the HIP-3 flag folds the dex into.
    let (list, _) = counting(hl_rows());
    let abc = SymbolPropertiesCache::new(list)
        .facts("hyperliquid", "TEST:ABCUSDC.P")
        .expect("TEST:ABCUSDC.P");
    assert_eq!(abc.margin_mode, None, "a cross-capable dex coin states no mode");

    // From one dex alone: the lister is told the network by the listing key.
    let (list, whole_calls) = counting(hl_rows());
    let asked = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let seen = Arc::clone(&asked);
    let facts =
        SymbolPropertiesCache::new(list).with_symbol_lister(Arc::new(move |venue, symbol| {
            seen.lock().unwrap().push((venue.to_string(), symbol.to_string()));
            Ok(vec![dex_coin("test:ABC", None), dex_coin("test:ISO", Some("onlyIsolated"))])
        }));
    let abc = facts.facts_on("hyperliquid", "testnet", "TEST:ABCUSDC.P").expect("TEST:ABCUSDC.P");
    assert_eq!(abc.margin_mode, None);
    let iso = facts.facts_on("hyperliquid", "mainnet", "test:isousdc.p").expect("TEST:ISOUSDC.P");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    let gone =
        facts.facts_on("hyperliquid", "mainnet", "TEST:GONEUSDC.P").expect_err("not on the dex");
    assert_eq!(gone.code, SymbolPropertiesRefusalCode::NoInstrument, "{gone}");
    assert!(
        gone.reason.starts_with("hyperliquid: no instrument TEST:GONEUSDC.P in the venue's"),
        "{gone}"
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [
            (
                vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST.to_string(),
                "test:ABC".to_string()
            ),
            ("hyperliquid".to_string(), "test:ISO".to_string()),
            ("hyperliquid".to_string(), "test:GONE".to_string()),
        ],
        "one read per coin, on the account's own network"
    );
    assert_eq!(whole_calls.load(Ordering::SeqCst), 0, "the universe list was never fetched");
    // A core coin still comes from the list, never the per-symbol source.
    assert!(facts.facts("hyperliquid", "BTCUSDC.P").is_ok());
    assert_eq!(whole_calls.load(Ordering::SeqCst), 1);
    assert_eq!(asked.lock().unwrap().len(), 3);
}

/// **A hyperliquid account is answered from ITS OWN network's universe** (the `demo` tier trades the testnet,
/// whose coin list, grids and asset indices are not mainnet's): a coin only mainnet lists is refused at the
/// door for a testnet account instead of spending an extra-symbol slot and dying at the adapter, a coin both
/// list is sized on the TESTNET's grid, the two listings are kept and fetched apart, and every other venue (and
/// the mainnet account) is answered exactly as `facts` answers.
#[test]
fn a_hyperliquid_testnet_account_is_answered_from_the_testnets_universe() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let key_of = |p: &str| p.to_string();
    let mainnet = vec![
        hl_perp("BTC", grid(0.1, 0.00001)),
        hl_perp("XRP", grid(0.0001, 1.0)),
        hl_perp("LINK", grid(0.001, 0.1)),
    ];
    let testnet = vec![hl_perp("BTC", grid(1.0, 0.001))];
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let list: InstrumentList = Arc::new(move |key| {
        seen.lock().expect("asked").push(key_of(key));
        match key {
            "hyperliquid" => Ok(mainnet.clone()),
            k if k == vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST => {
                Ok(testnet.clone())
            }
            other => Err(format!("{other}: not scripted")),
        }
    });
    let facts = SymbolPropertiesCache::new(list);

    // A testnet account: XRP is not on its network, BTC is, on the testnet's grid.
    let why = facts
        .facts_on("hyperliquid", "testnet", "XRPUSDC.P")
        .expect_err("XRP is not on the testnet");
    assert!(
        why.reason
            .starts_with("hyperliquid: no instrument XRPUSDC.P in the venue's instrument list")
            && why.reason.contains("testnet"),
        "{why}"
    );
    let btc = facts.facts_on("hyperliquid", "testnet", "BTCUSDC.P").expect("BTC is on both");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (1.0, 0.001));

    // A mainnet account (and the plain `facts`) keep the mainnet universe.
    for answer in [
        facts.facts_on("hyperliquid", "mainnet", "XRPUSDC.P"),
        facts.facts("hyperliquid", "XRPUSDC.P"),
        facts.facts_on("hyperliquid", "mainnet", "LINKUSDC.P"),
    ] {
        assert!(answer.is_ok(), "{answer:?}");
    }
    let btc = facts.facts_on("hyperliquid", "mainnet", "BTCUSDC.P").expect("BTC");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (0.1, 0.00001));

    // One fetch per network, however many asks (the listings are kept apart).
    let mut keys = asked.lock().expect("asked").clone();
    keys.sort();
    assert_eq!(
        keys,
        ["hyperliquid", vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST]
    );

    // The network word changes nothing for any other venue (binance's demo list is its own: below).
    let (list, calls) = counting(binance_rows());
    let other = SymbolPropertiesCache::new(list);
    assert!(other.facts_on("aster", "testnet", "SOLUSDT.P").is_ok());
    assert!(other.facts_on("aster", "mainnet", "SOLUSDT.P").is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one aster listing for both words");
}

/// A failed testnet read is the testnet's own failure: it is worded with the testnet key and does not poison
/// the mainnet slot.
#[test]
fn a_failed_testnet_listing_does_not_poison_the_mainnet_one() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let mainnet = vec![hl_perp("BTC", grid(0.1, 0.00001))];
    let list: InstrumentList = Arc::new(move |key| {
        if key == vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST {
            Err("timed out".to_string())
        } else {
            Ok(mainnet.clone())
        }
    });
    let facts = SymbolPropertiesCache::new(list);
    let why =
        facts.facts_on("hyperliquid", "testnet", "BTCUSDC.P").expect_err("the testnet read failed");
    assert!(
        why.reason.starts_with(vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST),
        "{why}"
    );
    assert_eq!(
        why.code,
        SymbolPropertiesRefusalCode::ListUnavailable,
        "a failed read is not a verdict on the coin: {why}"
    );
    assert!(facts.facts_on("hyperliquid", "mainnet", "BTCUSDC.P").is_ok());
}

// ---- deribit: perpetuals of the TESTNET book, inverse and linear, nothing else -----------------

fn deribit_perp(name: &str, margining: Margining, settle: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "deribit",
        name,
        "BTC",
        "USD",
        Contract::Perp(ContractPerp {
            margining,
            settle: settle.into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
    .with_properties(p)
}

fn deribit_rows() -> Vec<Instrument> {
    let future = Instrument::new(
        "deribit",
        "BTC-27MAR27",
        "BTC",
        "USD",
        Contract::Future(vike_model::ContractFuture {
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_currency: None,
            expiry_ms: 1_800_000_000_000,
            tenor: None,
            delivery: vike_model::ContractFutureDelivery::Unknown,
            family: vike_model::ContractFutureFamily::Crypto,
        }),
    )
    .with_properties(grid(0.5, 10.0));
    vec![
        deribit_perp("BTC-PERPETUAL", Margining::Inverse, "BTC", grid(0.5, 10.0)),
        deribit_perp("ETH-PERPETUAL", Margining::Inverse, "ETH", grid(0.05, 1.0)),
        deribit_perp("BTC_USDC-PERPETUAL", Margining::Linear, "USDC", grid(0.1, 0.0001)),
        deribit_perp("QUANTO-PERPETUAL", Margining::Quanto, "BTC", grid(0.5, 1.0)),
        deribit_perp("NOTICK-PERPETUAL", Margining::Inverse, "BTC", grid(0.0, 1.0)),
        future,
        spot("deribit", "BTC_USDC", grid(0.01, 0.0001)),
    ]
}

#[test]
fn deribit_answers_an_inverse_perpetual_as_inverse_and_a_linear_one_as_the_default() {
    let (list, _) = counting(deribit_rows());
    let facts = SymbolPropertiesCache::new(list);
    let btc = facts.facts("deribit", "BTC-PERPETUAL").expect("inverse perpetual");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (0.5, 10.0));
    assert_eq!(
        btc.contract_value,
        Some(vike_model::ContractValue::inverse(1.0)),
        "an inverse amount counts dollars: the kind travels, the lot stays a lot"
    );
    let eth = facts.facts("deribit", "eth-perpetual").expect("case-insensitive");
    assert_eq!((eth.properties.tick_size, eth.properties.step_size), (0.05, 1.0));
    assert_eq!(eth.contract_value, Some(vike_model::ContractValue::inverse(1.0)));
    let usdc = facts.facts("deribit", "BTC_USDC-PERPETUAL").expect("linear perpetual");
    assert_eq!((usdc.properties.tick_size, usdc.properties.step_size), (0.1, 0.0001));
    assert_eq!(usdc.contract_value, None, "linear is the default");
}

fn deribit_option(
    name: &str,
    margining: Margining,
    settle: &str,
    p: SymbolProperties,
) -> Instrument {
    Instrument::new(
        "deribit",
        name,
        "BTC",
        "USD",
        Contract::Option(vike_model::ContractOption {
            right: vike_marketdata::OptionKind::Call,
            strike: 77_000.0,
            expiry_ms: 1_800_000_000_000,
            style: vike_model::ContractOptionExerciseStyle::European,
            margining,
            settle: settle.into(),
            value_currency: None,
            underlying: Some("BTC".into()),
        }),
    )
    .with_properties(p)
}

/// **A dated future is answered like a perpetual of its margining** (decision 0233): the kind travels, the lot
/// stays a lot.
#[test]
fn deribit_answers_a_dated_future_by_its_margining() {
    let (list, _) = counting(deribit_rows());
    let facts = SymbolPropertiesCache::new(list);
    let future = facts.facts("deribit", "BTC-27MAR27").expect("dated inverse future");
    assert_eq!((future.properties.tick_size, future.properties.step_size), (0.5, 10.0));
    assert_eq!(future.contract_value, Some(vike_model::ContractValue::inverse(1.0)));
    assert_eq!(future.margin_rate, None, "a future keeps the engine's margin rate");
}

/// **A USDC option is read ONE at a time and answered at a margin rate of 1** (an added option is held long
/// only, so its margin is its premium); a coin-settled option and a spot pair are refused by name. An option
/// name keeps the venue's lower-case decimal `d`, and the whole futures list is never fetched for it. (The names deliver
/// in 2037: the delivery guard reads an option's day off its name against the real clock.)
#[test]
fn deribit_reads_a_usdc_option_alone_and_refuses_a_coin_option_and_a_spot_pair() {
    let (list, whole) = counting(deribit_rows());
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let facts =
        SymbolPropertiesCache::new(list).with_symbol_lister(Arc::new(move |venue, symbol| {
            seen.lock().unwrap().push(format!("{venue}/{symbol}"));
            Ok(match symbol {
                "BTC_USDC-6NOV37-77000-C" => {
                    vec![deribit_option(symbol, Margining::Linear, "USDC", grid(5.0, 0.01))]
                }
                "XRP_USDC-6NOV37-2d5-C" => {
                    vec![deribit_option(symbol, Margining::Linear, "USDC", grid(0.0005, 1000.0))]
                }
                "BTC-6NOV37-77000-C" => {
                    vec![deribit_option(symbol, Margining::Inverse, "BTC", grid(0.0001, 0.1))]
                }
                _ => Vec::new(),
            })
        }));
    let usdc = facts.facts("deribit", "BTC_USDC-6NOV37-77000-C").expect("a USDC option");
    assert_eq!((usdc.properties.tick_size, usdc.properties.step_size), (5.0, 0.01));
    assert_eq!(usdc.contract_value, None, "linear is the default");
    assert_eq!(usdc.margin_rate, Some(1.0), "a long option's margin is its premium");
    facts
        .facts("deribit", "XRP_USDC-6NOV37-2d5-C")
        .expect("a decimal strike keeps its lower-case d");
    let coin = facts.facts("deribit", "BTC-6NOV37-77000-C").expect_err("a coin-settled option");
    assert_eq!(coin.code, SymbolPropertiesRefusalCode::Verdict, "{coin}");
    assert!(coin.reason.contains("coin-settled option") && coin.reason.contains("BTC"), "{coin}");
    let gone = facts.facts("deribit", "ETH_USDC-6NOV37-1-C").expect_err("not listed");
    assert_eq!(gone.code, SymbolPropertiesRefusalCode::NoInstrument, "{gone}");
    assert!(gone.reason.contains("no instrument ETH_USDC-6NOV37-1-C"), "{gone}");
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "deribit/BTC_USDC-6NOV37-77000-C",
            "deribit/XRP_USDC-6NOV37-2d5-C",
            "deribit/BTC-6NOV37-77000-C",
            "deribit/ETH_USDC-6NOV37-1-C",
        ],
        "one read per option"
    );
    assert_eq!(whole.load(Ordering::SeqCst), 0, "the futures list was never fetched");
}

#[test]
fn deribit_refuses_a_spot_pair_a_quanto_row_and_a_missing_tick_by_name() {
    let (list, _) = counting(deribit_rows());
    let facts = SymbolPropertiesCache::new(list);
    let spot = facts.facts("deribit", "BTC_USDC").expect_err("spot pair");
    assert!(spot.reason.contains("no position"), "{spot}");
    let quanto = facts.facts("deribit", "QUANTO-PERPETUAL").expect_err("quanto");
    assert!(quanto.reason.contains("quanto"), "{quanto}");
    let tick = facts.facts("deribit", "NOTICK-PERPETUAL").expect_err("no tick");
    assert!(tick.reason.contains("no tick size"), "{tick}");
    let unlisted = facts.facts("deribit", "XYZ-PERPETUAL").expect_err("unlisted");
    assert!(unlisted.reason.contains("no instrument XYZ-PERPETUAL"), "{unlisted}");
}

// --- alpaca: a list behind the account's own credentials ----------------------------------------

fn equity(symbol: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "alpaca",
        symbol,
        symbol,
        "USD",
        Contract::Equity(vike_model::ContractEquity {
            exchange: Some("NASDAQ".into()),
            currency: Some("USD".into()),
        }),
    )
    .with_properties(p)
}

/// Two account keys (label and tier, as `TradeLaneBridge` composes them).
const ACCOUNT_A: &str = "DEFAULT/demo";
const ACCOUNT_B: &str = "second/demo";

fn alpaca_rows() -> Vec<Instrument> {
    vec![
        equity("AAPL", grid(0.01, 1.0)),
        equity("NOTICK", grid(0.0, 1.0)),
        Instrument::new("alpaca", "BTC/USD", "BTC", "USD", Contract::CryptoSpot)
            .with_properties(SymbolProperties { min_qty: 0.000016, ..grid(1.0, 0.000000001) }),
        Instrument::new("alpaca", "ETH/USD", "ETH", "USD", Contract::CryptoSpot)
            .with_properties(grid(0.1, 0.000000001)),
        Instrument::new("alpaca", "SHIB/USD", "SHIB", "USD", Contract::CryptoSpot)
            .with_properties(grid(0.000001, 1.0)),
    ]
}

/// A lister that serves the row named by the symbol asked (the venue's one-asset read: an unknown
/// symbol is an empty answer) and counts how often the venue was asked.
fn account_lister(rows: Vec<Instrument>) -> (AccountLister, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let lister: AccountLister = Arc::new(move |symbol| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(rows.iter().filter(|r| r.raw_symbol == symbol).cloned().collect())
    });
    (lister, calls)
}

/// alpaca answers an equity and a slashed crypto pair from its OWN rows, linear (`contract_value` None)
/// and on each instrument's own grid; each symbol is ONE asset request, kept for the next ask of it, and
/// an unknown asset is a named refusal (not a failure, not "the list came back empty").
#[test]
fn alpaca_answers_equities_and_crypto_pairs_one_asset_at_a_time() {
    let (lister, calls) = account_lister(alpaca_rows());
    let facts = SymbolPropertiesCacheAccount::new();
    let aapl = facts.facts("acct", "alpaca", "aapl", Arc::clone(&lister)).expect("equity");
    assert_eq!((aapl.properties.tick_size, aapl.properties.step_size), (0.01, 1.0));
    assert_eq!(aapl.contract_value, None, "a share is one unit: linear, multiplier 1");
    let btc = facts.facts("acct", "alpaca", "BTC/USD", Arc::clone(&lister)).expect("crypto pair");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (1.0, 0.000000001));
    assert_eq!(btc.properties.min_qty, 0.000016, "the row's own minimum travels");
    let unlisted =
        facts.facts("acct", "alpaca", "NOPE", Arc::clone(&lister)).expect_err("unlisted");
    assert!(unlisted.reason.starts_with("alpaca: unknown asset NOPE"), "{unlisted}");
    assert_eq!(
        unlisted.code,
        SymbolPropertiesRefusalCode::Verdict,
        "an unknown asset is a verdict: {unlisted}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3, "one asset request per symbol");
    for _ in 0..3 {
        facts.facts("acct", "alpaca", "AAPL", Arc::clone(&lister)).expect("kept");
        facts.facts("acct", "alpaca", "BTC/USD", Arc::clone(&lister)).expect("kept");
        facts.facts("acct", "alpaca", "NOPE", Arc::clone(&lister)).expect_err("kept");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3, "nothing was asked again inside the keep time");
}

/// Accounts keep their own state: one account's failure (or credentials) is never another's answer, and
/// each asset is read once PER ACCOUNT.
#[test]
fn alpaca_keeps_each_accounts_assets_and_failures_apart() {
    let (good, good_calls) = account_lister(alpaca_rows());
    let bad: AccountLister = Arc::new(|_| Err("no complete credentials".to_string()));
    let facts = SymbolPropertiesCacheAccount::new();
    let err = facts.facts("a/demo", "alpaca", "AAPL", bad).expect_err("a has no credentials");
    assert!(err.reason.contains("no complete credentials"), "{err}");
    facts
        .facts("b/demo", "alpaca", "AAPL", Arc::clone(&good))
        .expect("b is not handed a's failure");
    facts.facts("b/demo", "alpaca", "AAPL", Arc::clone(&good)).expect("kept for b");
    assert_eq!(good_calls.load(Ordering::SeqCst), 1);
    facts.facts("c/demo", "alpaca", "AAPL", good).expect("c reads for itself");
    assert_eq!(good_calls.load(Ordering::SeqCst), 2);
}

/// A row with no tick is refused naming it, and an ask with no account to list through (or a venue that
/// cannot be listed) is the lister's own words, never a guess.
#[test]
fn alpaca_refuses_a_missing_tick_and_a_failed_listing_in_words() {
    let (lister, _) = account_lister(alpaca_rows());
    let tick = SymbolPropertiesCacheAccount::new()
        .facts("acct", "alpaca", "NOTICK", lister)
        .expect_err("no tick");
    assert!(tick.reason.contains("no tick size"), "{tick}");
    let failing: AccountLister = Arc::new(|_| Err("HTTP 401".to_string()));
    let err = SymbolPropertiesCacheAccount::new()
        .facts("acct", "alpaca", "AAPL", failing)
        .expect_err("listing failed");
    assert!(err.reason.starts_with("alpaca: ") && err.reason.contains("HTTP 401"), "{err}");
    assert_eq!(
        err.code,
        SymbolPropertiesRefusalCode::ListUnavailable,
        "a failed fetch is transient, never a verdict: {err}"
    );
}

/// The adapter writes a price to the CENT, so a price grid that is not whole cents is refused by name
/// (a coin quoted at a millionth would otherwise rest at a different price than the engine judged), while
/// a grid of whole cents (a dime, a dollar) is admitted.
#[test]
fn alpaca_admits_only_a_price_grid_of_whole_cents() {
    let (lister, _) = account_lister(alpaca_rows());
    let facts = SymbolPropertiesCacheAccount::new();
    let shib =
        facts.facts("acct", "alpaca", "SHIB/USD", Arc::clone(&lister)).expect_err("sub-cent grid");
    assert!(shib.reason.contains("SHIB/USD") && shib.reason.contains("to the cent only"), "{shib}");
    assert_eq!(shib.code, SymbolPropertiesRefusalCode::Verdict, "a verdict, not a retry: {shib}");
    for ok in ["ETH/USD", "BTC/USD", "AAPL"] {
        facts
            .facts("acct", "alpaca", ok, Arc::clone(&lister))
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

/// `SymbolPropertiesCacheAccount` serves the venues `SymbolPropertiesCache` names and no others: a credentialed venue it was not
/// taught (ig) is the default answer, and its lister is never called.
#[test]
fn another_credentialed_venue_is_still_not_answered() {
    let (lister, calls) = account_lister(alpaca_rows());
    let err = SymbolPropertiesCacheAccount::new()
        .facts("acct", "ig", "EURUSD", lister)
        .expect_err("not served");
    assert_eq!(err, verdict(NOT_ANSWERED));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

// --- oanda: the account's own list, fenced to the account's currency -----------------------------

fn fx(name: &str, tick: f64, step: f64) -> Instrument {
    let (base, quote) = name.split_once('_').expect("an OANDA name has an underscore");
    Instrument::new(
        "oanda",
        name,
        base,
        quote,
        Contract::Fx(vike_model::ContractFx {
            pip_size: Some(0.0001),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        }),
    )
    .with_properties(SymbolProperties {
        tick_size: tick,
        step_size: step,
        min_qty: step,
        max_qty: 1e8,
        ..Default::default()
    })
}

fn oanda_rows() -> Vec<Instrument> {
    vec![
        fx("EUR_USD", 0.00001, 1.0),
        fx("GBP_USD", 0.00001, 1.0),
        fx("EUR_GBP", 0.00001, 1.0),
        fx("USD_JPY", 0.001, 1.0),
        fx("XAU_USD", 0.001, 1.0),
        fx("SPX500_USD", 0.1, 0.1),
        fx("FRACT_USD", 0.00001, 0.1),
        fx("NOTICK_USD", 0.0, 1.0),
    ]
}

/// A lister that serves `rows` for an account whose home currency is `currency`; counts the asks.
fn oanda_lister(currency: &str, rows: Vec<Instrument>) -> (CurrencyLister, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let currency = currency.to_string();
    let lister: CurrencyLister = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok((currency.clone(), rows.clone()))
    });
    (lister, calls)
}

/// A pair quoted in the account's currency is answered from the account's own rows, linear
/// (`contract_value` None), on its own grid, by its USER name in any case (decision 0290), and one listing
/// (the instruments and the currency) serves every symbol asked meanwhile. A name the kept list carries but this
/// account's list does not is the not-found sentence; the venue's own spelling and a name no kept row carries are
/// refused in the kept list's words before anything is read.
#[test]
fn oanda_answers_a_pair_quoted_in_the_accounts_currency_from_one_listing() {
    let (lister, calls) = oanda_lister("USD", oanda_rows());
    let facts = oanda_facts();
    for typed in ["GBPUSD", "gbpusd", "EURUSD"] {
        let got =
            facts.facts(typed, Arc::clone(&lister)).unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!((got.properties.tick_size, got.properties.step_size), (0.00001, 1.0), "{typed}");
        assert_eq!(got.contract_value, None, "{typed}: units x price in the quote IS the exposure");
    }
    let unlisted = facts.facts("XAGUSD", Arc::clone(&lister)).expect_err("unlisted");
    assert_eq!(unlisted.code, SymbolPropertiesRefusalCode::NoInstrument, "{unlisted}");
    assert_eq!(
        unlisted.reason,
        "oanda: no instrument XAGUSD in the venue's instrument list (the instruments this \
         account's own OANDA list carries; a CFD, metal or index is listed only if OANDA offers it \
         to the account)",
        "named as the engine spelled it"
    );
    for unkept in ["GBP_USD", "NOPEUSD"] {
        let why = facts.facts(unkept, Arc::clone(&lister)).expect_err(unkept).reason;
        assert!(why.contains(unkept) && why.contains("kept list"), "{unkept}: {why}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing for six asks");
}

/// **The currency fence**: USDJPY on a USD account is refused BY NAME (nothing converts a quote currency;
/// overstated PnL is the dangerous direction), a cross like EURGBP too, and the same cross is admitted on
/// a GBP account: the fence is the ACCOUNT's, one set of oanda facts per account.
#[test]
fn oanda_refuses_an_instrument_not_quoted_in_the_accounts_currency() {
    let (usd, _) = oanda_lister("USD", oanda_rows());
    let usd_facts = oanda_facts();
    let jpy = usd_facts.facts("USDJPY", Arc::clone(&usd)).expect_err("quoted in JPY").reason;
    assert!(
        jpy.contains("USD_JPY is quoted in JPY") && jpy.contains("account's currency is USD"),
        "{jpy}"
    );
    let cross = usd_facts.facts("EURGBP", Arc::clone(&usd)).expect_err("quoted in GBP");
    assert!(cross.reason.contains("EUR_GBP is quoted in GBP"), "{cross}");
    assert_eq!(cross.code, SymbolPropertiesRefusalCode::Verdict, "a verdict, not a retry: {cross}");

    let (gbp, _) = oanda_lister("GBP", oanda_rows());
    let gbp_facts = oanda_facts();
    gbp_facts.facts("EURGBP", Arc::clone(&gbp)).expect("quoted in the account's own currency");
    let usd_pair = gbp_facts.facts("GBPUSD", gbp).expect_err("a USD quote on a GBP account").reason;
    assert!(
        usd_pair.contains("quoted in USD") && usd_pair.contains("currency is GBP"),
        "{usd_pair}"
    );
}

/// The one rate record a source answers, `account_per_quote` account units per ONE `quote` unit.
fn quote_rate(quote: &str, account_per_quote: f64, as_of_ms: i64) -> QuoteRate {
    let account_per_quote = AccountPerQuote::new(account_per_quote).expect("a rate");
    QuoteRate { quote_currency: quote.to_string(), account_per_quote, as_of_ms }
}

/// A rate source over the account's home conversions as a table `(quote currency, account per quote)`: the
/// composition's closure in miniature (`TradeLaneBridge::oanda_facts_for`), so the contract the facts rely on is
/// the same: `Ok(None)` for a pair quoted in the home currency, `Ok(Some(rate))` for a priced one, `Err` for
/// a currency it holds no rate for. Counts the asks.
fn rates(table: &[(&str, f64)], as_of_ms: i64) -> (RateSource, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&asked);
    let table: Vec<(String, f64)> = table.iter().map(|(c, r)| ((*c).to_string(), *r)).collect();
    let source: RateSource = Arc::new(move |instrument: &str, home: &str| {
        seen.fetch_add(1, Ordering::SeqCst);
        let quote = instrument.split_once('_').map(|(_, q)| q).unwrap_or_default();
        if quote == home {
            return Ok(None);
        }
        match table.iter().find(|(c, _)| c == quote) {
            Some((c, r)) => Ok(Some(quote_rate(c, *r, as_of_ms))),
            None => Err(verdict(format!("oanda: no rate for {quote} in {home}"))),
        }
    });
    (source, asked)
}

/// **The quoted answer** (decision 0183, phase 1): a node that converts is answered a cross with its quote
/// currency and the rate, in the orientation the engine takes (account units per ONE quote unit: a pound is
/// MORE than a dollar), the rate asked NOW for each ask (the listing is kept, the rate is not), and a pair
/// quoted in the account's own currency is answered as before, with no rate and no question asked of the
/// rate source.
#[test]
fn oanda_answers_a_cross_with_its_quote_currency_and_rate_to_a_node_that_converts() {
    let (lister, calls) = oanda_lister("USD", oanda_rows());
    let (rate, asked) = rates(&[("GBP", 1.35), ("JPY", 1.0 / 150.0)], 1_700_000_000_000);
    let facts = oanda_facts();

    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("a priced cross");
    assert_eq!(cross.quote_currency.as_deref(), Some("GBP"));
    assert_eq!(cross.quote_rate, Some(1.35), "account units per ONE pound, never the inverse");
    assert_eq!(cross.rate_as_of_ms, Some(1_700_000_000_000));
    assert_eq!(cross.properties.tick_size, 0.00001, "the pair's own grid");
    assert_eq!(cross.contract_value, None, "the unit is 1: the engine multiplies the rate in");

    let own = facts.facts_quoted("GBPUSD", Arc::clone(&lister), &rate).expect("quoted in USD");
    assert_eq!((&own.quote_currency, own.quote_rate, own.rate_as_of_ms), (&None, None, None));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "the rate is asked for every ask, even a pair that needs none"
    );

    facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("again");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "a rate is never served from the listing's keep time"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing for all three asks");

    // The plain ask of the same account still refuses the cross: a node that does not convert is not handed it.
    let plain = facts.facts("EURGBP", lister).expect_err("the interim fence stands");
    assert!(plain.reason.contains("EUR_GBP is quoted in GBP"), "{plain}");
}

/// **No rate, no admission**: a currency the source holds no rate for, a pair that names no quote currency
/// and a source that fails each refuse the symbol, and the adapter's own fence still runs: USDJPY has a
/// three-decimal price grid the adapter cannot write, so it stays refused even WITH a rate.
#[test]
fn oanda_refuses_a_cross_it_cannot_price_and_keeps_the_adapters_own_fence() {
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let facts = oanda_facts();

    let (no_gbp, _) = rates(&[("JPY", 1.0 / 150.0)], 1);
    let why = facts.facts_quoted("EURGBP", Arc::clone(&lister), &no_gbp).expect_err("no GBP rate");
    assert!(why.reason.contains("no rate for GBP in USD"), "{why}");

    // The rate source's refusal is answered as it came, its code included.
    let failing: RateSource = Arc::new(|_: &str, _: &str| {
        Err(super::coded(SymbolPropertiesRefusalCode::RateUnavailable, "oanda: the venue is down"))
    });
    let down = facts.facts_quoted("EURGBP", Arc::clone(&lister), &failing).expect_err("down");
    assert_eq!(
        down,
        super::coded(SymbolPropertiesRefusalCode::RateUnavailable, "oanda: the venue is down")
    );

    let (with_jpy, _) = rates(&[("JPY", 1.0 / 150.0)], 1);
    let jpy = facts.facts_quoted("USDJPY", Arc::clone(&lister), &with_jpy).expect_err("grid");
    assert!(
        jpy.reason.contains("price grid of 0.001") && jpy.reason.contains("five decimals"),
        "{jpy}"
    );

    let unlisted = facts.facts_quoted("EURCHF", lister, &no_gbp).expect_err("unlisted");
    assert!(unlisted.reason.contains("no instrument EURCHF"), "{unlisted}");
}

/// The adapter writes five decimals and whole units, so an instrument with another price precision or a
/// fractional unit step is refused by name (the mounted symbol is not touched: this is the foreign path).
#[test]
fn oanda_refuses_what_the_adapter_cannot_write_on_the_wire() {
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let facts = oanda_facts();
    for (symbol, word) in
        [("XAUUSD", "five decimals"), ("SPX500.CFD", "five decimals"), ("FRACTUSD", "WHOLE units")]
    {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(
            why.reason.contains(word) && why.reason.contains("not admitted"),
            "{symbol}: {why}"
        );
        assert_eq!(
            why.code,
            SymbolPropertiesRefusalCode::Verdict,
            "{symbol}: a verdict, not a retry: {why}"
        );
    }
    // A row with no tick at all is the generic refusal, before either fence.
    let none = facts.facts("NOTICKUSD", lister).expect_err("no tick");
    assert!(none.reason.contains("no tick size"), "{none}");
}

/// A listing that fails (the summary or the instruments) is a transient refusal in the venue's own words,
/// and it is remembered: a flood of asks against a failing account costs ONE request pair.
#[test]
fn oanda_failed_listing_is_transient_and_remembered() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let failing: CurrencyLister = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Err("oanda /v3/summary: OANDA error 401: Insufficient authorization".to_string())
    });
    let facts = oanda_facts();
    for _ in 0..3 {
        let err = facts.facts("GBPUSD", Arc::clone(&failing)).expect_err("listing failed");
        assert!(err.reason.starts_with("oanda: ") && err.reason.contains("401"), "{err}");
        assert_eq!(err.code, SymbolPropertiesRefusalCode::ListUnavailable, "{err}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the failure is remembered, not retried per ask");
}

/// **A credential swap under one label is a different account** (an operator pastes another OANDA account's
/// id and key under the same label): the kept listing and currency are the OLD account's, and answering from
/// them for up to ten minutes would judge a pair against a currency the account no longer holds. The cache is
/// keyed by the account-id fingerprint, so the swap gets fresh oanda facts that list again, while the same
/// account (a rotated token, the same fingerprint) keeps its kept listing, and another label is its own entry.
#[test]
fn a_credential_swap_under_one_label_never_answers_from_the_old_accounts_listing() {
    vike_oanda::test_support::kept_list_for_tests();
    let by_account = PerAccount::<
        SymbolPropertiesCacheAccountListed<vike_oanda::SymbolPropertiesAccountSpecOanda>,
    >::default();
    let (usd, usd_calls) = oanda_lister("USD", oanda_rows());
    let (gbp, gbp_calls) = oanda_lister("GBP", oanda_rows());
    let (old_account, new_account) = (Some(1_u64), Some(2_u64));

    let facts = by_account.of("ALT", old_account);
    facts.facts("GBPUSD", Arc::clone(&usd)).expect("quoted in the USD account's currency");
    assert!(
        Arc::ptr_eq(&facts, &by_account.of("ALT", old_account)),
        "the same account keeps its facts"
    );
    by_account.of("ALT", old_account).facts("EURUSD", Arc::clone(&usd)).expect("kept listing");
    assert_eq!(usd_calls.load(Ordering::SeqCst), 1, "one listing for the account");

    // The swap: same label, another account (a GBP one). Fresh facts, and the fence is the NEW currency.
    let swapped = by_account.of("ALT", new_account);
    assert!(!Arc::ptr_eq(&facts, &swapped), "a changed fingerprint is a new account");
    let refused =
        swapped.facts("GBPUSD", Arc::clone(&gbp)).expect_err("USD-quoted on a GBP account").reason;
    assert!(
        refused.contains("GBP_USD is quoted in USD") && refused.contains("currency is GBP"),
        "{refused}"
    );
    swapped.facts("EURGBP", gbp).expect("quoted in the new account's currency");
    assert_eq!(gbp_calls.load(Ordering::SeqCst), 1, "the new account listed once");
    assert_eq!(usd_calls.load(Ordering::SeqCst), 1, "the old lister was not asked again");

    // The swap back is no cache hit either: one entry per label, never a growing history.
    assert!(!Arc::ptr_eq(&swapped, &by_account.of("ALT", old_account)));
    // Another label, and an account with no credentials, are their own entries.
    assert!(!Arc::ptr_eq(&by_account.of("OTHER", old_account), &by_account.of("ALT", old_account)));
    assert!(Arc::ptr_eq(&by_account.of("", None), &by_account.of("", None)));
}

/// **Every account keeps its OWN slot and lister.** An account with no credentials reads nothing and that
/// refusal is remembered for `FAILED_TTL` against ITS key only: a different account asking inside the window
/// is read with its own lister and answered, and the first account is not read again inside the window.
#[test]
fn one_accounts_failed_listing_is_not_served_to_another_account() {
    let down_calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&down_calls);
    let down: AccountLister = Arc::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Err("no complete credentials".to_string())
    });
    let (funded, funded_calls) = account_lister(alpaca_rows());
    let facts = SymbolPropertiesCacheAccount::new();
    let none = facts.facts(ACCOUNT_A, "alpaca", "AAPL", down).expect_err("no credentials");
    assert!(none.reason.contains("no complete credentials"), "{none}");
    // Inside FAILED_TTL: the same account is served its remembered refusal, another is not.
    let b =
        facts.facts(ACCOUNT_B, "alpaca", "AAPL", Arc::clone(&funded)).expect("B has credentials");
    assert_eq!(b.properties.tick_size, 0.01);
    assert_eq!(funded_calls.load(Ordering::SeqCst), 1, "B was read with B's lister");
    let a_again =
        facts.facts(ACCOUNT_A, "alpaca", "AAPL", funded).expect_err("A's refusal is kept");
    assert!(a_again.reason.contains("no complete credentials"), "{a_again}");
    assert_eq!(funded_calls.load(Ordering::SeqCst), 1, "A was not read with B's lister");
    assert_eq!(down_calls.load(Ordering::SeqCst), 1, "A was not read again inside FAILED_TTL");
}

/// A fetch runs with the credentials of the account it is for: asks of two accounts interleave and each
/// asset is fetched by its own lister, kept for the TTL per account, whatever asked last.
#[test]
fn each_account_reads_with_its_own_lister_and_keeps_its_own_assets() {
    let (lister_a, calls_a) = account_lister(vec![equity("ONLYA", grid(0.01, 1.0))]);
    let (lister_b, calls_b) = account_lister(vec![equity("ONLYB", grid(0.01, 1.0))]);
    let facts = SymbolPropertiesCacheAccount::new();
    facts.facts(ACCOUNT_A, "alpaca", "ONLYA", Arc::clone(&lister_a)).expect("A reads A's asset");
    facts.facts(ACCOUNT_B, "alpaca", "ONLYB", Arc::clone(&lister_b)).expect("B reads B's asset");
    // A asks again after B asked last: still A's kept asset, no second request.
    facts.facts(ACCOUNT_A, "alpaca", "ONLYA", Arc::clone(&lister_a)).expect("A's kept asset");
    let cross =
        facts.facts(ACCOUNT_A, "alpaca", "ONLYB", lister_a).expect_err("not in A's account");
    assert!(cross.reason.contains("unknown asset ONLYB"), "{cross}");
    assert_eq!((calls_a.load(Ordering::SeqCst), calls_b.load(Ordering::SeqCst)), (2, 1));
}

// --- ctrader: the account's own symbol list, fenced by its deposit currency ---------------------

/// A ctrader row: `symbol` quoted in `quote`, a forex pair (`fx`) or an index CFD, on a volume grid whose
/// minimum is `min`.
fn ctrader_row(symbol: &str, base: &str, quote: &str, fx: bool, min: f64) -> Instrument {
    use vike_model::{ContractCfdUnderlying, ContractFx};
    if fx {
        let fx = Contract::Fx(ContractFx {
            pip_size: Some(0.0001),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        });
        return ctrader_instrument(symbol, base, quote, fx, min);
    }
    ctrader_cfd_row(symbol, base, quote, ContractCfdUnderlying::Index, min)
}

/// A ctrader CFD row of the broker's asset class `kind` (a measured class or not).
fn ctrader_cfd_row(
    symbol: &str,
    base: &str,
    quote: &str,
    kind: vike_model::ContractCfdUnderlying,
    min: f64,
) -> Instrument {
    use vike_model::{ContractCfd, ContractCfdExpiry, ContractCfdProduct};
    let contract = Contract::Cfd(ContractCfd {
        underlying_kind: kind,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Unknown,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    });
    ctrader_instrument(symbol, base, quote, contract, min)
}

fn ctrader_instrument(
    symbol: &str,
    base: &str,
    quote: &str,
    contract: Contract,
    min: f64,
) -> Instrument {
    Instrument::new("ctrader", symbol, base, quote, contract).with_properties(SymbolProperties {
        tick_size: 0.00001,
        step_size: 1000.0,
        min_qty: min,
        max_qty: 100_000_000.0,
        ..Default::default()
    })
}

/// The account's listing, under the broker's names; the USER names the facts are asked by (decision 0290) are
/// `EURUSD`, `GBPJPY`, `NOMUSD` (the broker's `NOMIN`, a pair with no minimum), `US500.CFD` and `WHEAT.CFD`.
fn ctrader_rows() -> Vec<Instrument> {
    vec![
        ctrader_row("EURUSD", "EUR", "USD", true, 1000.0),
        ctrader_row("GBPJPY", "GBP", "JPY", true, 1000.0),
        ctrader_row("NOMIN", "NOM", "USD", true, 0.0),
        ctrader_row("US500", "US500", "USD", false, 1.0),
        // A dated futures wrapper (the broker's `Futures Commodities` class): a class nobody measured.
        ctrader_cfd_row("Wheat_H6", "WHEAT", "USD", vike_model::ContractCfdUnderlying::Other, 1.0),
    ]
}

/// **Hold the ctrader kept list every test of this binary reads** (`vike_catalog::hold_kept_list`): the bridge's spec
/// reads the USER name a fact is asked by through its kept row to the broker's name the listing keys (decision 0290).
/// [`ctrader_rows`] and the two crosses some tests add, the account's listing as the datahub keeps it. Process-wide and
/// the tests run as threads, so every caller holds THESE rows; a second call changes nothing.
fn hold_ctrader_kept_rows() {
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    rows.push(ctrader_row("EURGBP", "EUR", "GBP", true, 1000.0));
    vike_catalog::hold_kept_list("ctrader", rows);
}

/// A lister that serves `(deposit, rows)` and counts how often the account was asked.
fn ctrader_lister(deposit: &str, rows: Vec<Instrument>) -> (CurrencyLister, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let deposit = deposit.to_string();
    let lister: CurrencyLister = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok((deposit.clone(), rows.clone()))
    });
    (lister, calls)
}

/// ctrader's account-listed facts: the generic per-account facts over the bridge's spec (its plain fence is
/// `vike_ctrader::catalog::any_symbol_refusal`, its converting one `quoted_symbol_refusal`), with the kept list its
/// spec reads a USER name through held ([`hold_ctrader_kept_rows`]).
fn ctrader_facts() -> SymbolPropertiesCacheAccountListed<
    vike_ctrader::symbol_properties::SymbolPropertiesAccountSpecCtrader,
> {
    hold_ctrader_kept_rows();
    SymbolPropertiesCacheAccountListed::default()
}

/// A forex pair quoted in the account's deposit currency is answered from the account's OWN listing, on
/// its own grid, linear (`contract_value` None), asked by its USER name (decision 0290: read without case, the
/// broker's name off its kept row); the broker's own name of a CFD (`US500`, the user name `US500.CFD`) is no user
/// name and is refused in words; and one listing serves every ask meanwhile.
#[test]
fn ctrader_answers_a_deposit_quoted_forex_pair_by_its_user_name() {
    let (lister, calls) = ctrader_lister("USD", ctrader_rows());
    let facts = ctrader_facts();
    let eur = facts.facts("EURUSD", Arc::clone(&lister)).expect("a USD-quoted pair");
    assert_eq!((eur.properties.tick_size, eur.properties.step_size), (0.00001, 1000.0));
    assert_eq!(eur.properties.min_qty, 1000.0, "the row's own minimum travels");
    assert_eq!(eur.contract_value, None, "forex in the deposit currency: linear, multiplier 1");
    facts.facts("eurusd", Arc::clone(&lister)).expect("the user name, typed without case");
    let wire = facts.facts("US500", Arc::clone(&lister)).expect_err("the broker's name");
    assert!(wire.reason.contains("`US500` is no user name of a ctrader instrument"), "{wire}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing session for every ask");
}

/// **The lister holds the account's grant, so nothing keeps it past the ask**: after an answered ask, a
/// kept-listing ask and a failed ask, the only holder of what the lister captured is the test.
#[test]
fn ctrader_drops_the_lister_when_the_ask_returns() {
    let grant = Arc::new(());
    let holding = |inner: CurrencyLister| -> CurrencyLister {
        let grant = Arc::clone(&grant);
        Arc::new(move || {
            std::hint::black_box(&grant);
            inner()
        })
    };
    let (served, calls) = ctrader_lister("USD", ctrader_rows());
    let facts = ctrader_facts();
    facts.facts("EURUSD", holding(Arc::clone(&served))).expect("answered");
    assert_eq!(Arc::strong_count(&grant), 1, "the lister is dropped after a listing ask");
    facts.facts("EURUSD", holding(served)).expect("kept listing");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the second ask is served from the kept listing");
    assert_eq!(Arc::strong_count(&grant), 1, "and after an ask that read nothing");

    let failing: CurrencyLister = Arc::new(|| Err("ctrader symbols: auth refused".to_string()));
    ctrader_facts().facts("EURUSD", holding(failing)).expect_err("failed");
    assert_eq!(Arc::strong_count(&grant), 1, "and after a failed ask");
}

/// **A CFD of a MEASURED class is answered like a forex pair** (metals, indices, oil and commodities, crypto:
/// `vike_ctrader::catalog::MEASURED_CFD_CLASSES`): the account's own listing, the row's own grid and minimum,
/// linear with a multiplier of 1 (`contract_value` None) because one unit of volume is `volume / 100` of the
/// symbol's own unit (measured on the demo account, 2026-10-26). The same row on a different quote currency
/// is refused as a pair is. Asked by its USER name `US500.CFD` (decision 0290), the broker's `US500` off its kept row.
#[test]
fn ctrader_answers_a_measured_cfd_class_like_a_forex_pair() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = ctrader_facts();
    let us500 = facts.facts("US500.CFD", Arc::clone(&lister)).expect("an index CFD in USD");
    assert_eq!(us500.properties.min_qty, 1.0, "the row's own minimum travels");
    assert_eq!(us500.contract_value, None, "measured: linear, multiplier 1");
    assert_eq!(us500.quote_currency, None, "quoted in the deposit currency: nothing to convert");
    let (eur, _) = ctrader_lister("EUR", ctrader_rows());
    let why = ctrader_facts().facts("US500.CFD", eur).expect_err("USD index, EUR account");
    assert!(why.reason.contains("quoted in USD but this account's currency is EUR"), "{why}");
}

/// Every refusal names its cause, and none is transient (a verdict, not a retry): another quote currency,
/// a CFD of a class nobody measured (a mixed-case dated future, the broker's `Wheat_H6`), a missing minimum volume.
#[test]
fn ctrader_refuses_another_currency_an_unmeasured_cfd_and_a_missing_minimum_by_name() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = ctrader_facts();
    for (symbol, needle) in [
        ("GBPJPY", "quoted in JPY but this account's currency is USD"),
        ("WHEAT.CFD", "Wheat_H6 is a CFD of a class this adapter does not know"),
        ("NOMUSD", "no minimum volume"),
    ] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(why.reason.contains(needle), "{symbol}: {why}");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::Verdict, "{symbol}: a verdict: {why}");
    }
}

/// The fence follows the ACCOUNT's currency: on a EUR account the USD-quoted pair is refused, and the
/// EUR-quoted one is the admitted kind.
#[test]
fn ctrader_fences_on_the_accounts_own_deposit_currency() {
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let (lister, _) = ctrader_lister("EUR", rows);
    let facts = ctrader_facts();
    let why = facts.facts("EURUSD", Arc::clone(&lister)).expect_err("USD quote, EUR account");
    assert!(why.reason.contains("quoted in USD but this account's currency is EUR"), "{why}");
    facts.facts("GBPEUR", lister).expect("a EUR-quoted pair on a EUR account");
}

/// A failed listing is the lister's own words, transient (never a verdict about the instrument).
#[test]
fn ctrader_listing_failure_is_transient_and_a_public_list_never_answers_ctrader() {
    let failing: CurrencyLister = Arc::new(|| Err("ctrader symbols: auth refused".to_string()));
    let err = ctrader_facts().facts("EURUSD", failing).expect_err("failed");
    assert!(err.reason.starts_with("ctrader: ") && err.reason.contains("auth refused"), "{err}");
    assert_eq!(err.code, SymbolPropertiesRefusalCode::ListUnavailable, "{err}");

    // A PUBLIC source (no account behind it) must not answer ctrader from whatever rows it was given:
    // the currency fence is the account's.
    let (list, calls) = counting(ctrader_rows());
    let public =
        SymbolPropertiesCache::new(list).facts("ctrader", "EURUSD").expect_err("not served");
    assert_eq!(public, verdict(NOT_ANSWERED));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// **A session-costing source remembers its failure for the long keep time**: ctrader's listing is a whole
/// authenticated session, so an outage must not dial one every 30 s. The default is `SESSION_FAILED_TTL`
/// (five minutes, longer than the public sources' `FAILED_TTL`): two asks make ONE session; a zero keep
/// time (the plumbing's proof that the knob is the one in use) asks again.
#[test]
fn ctrader_remembers_a_failed_listing_for_the_session_keep_time() {
    use super::{FAILED_TTL, SESSION_FAILED_TTL};
    assert!(SESSION_FAILED_TTL >= Duration::from_secs(300) && SESSION_FAILED_TTL > FAILED_TTL);
    let sessions = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&sessions);
    let failing: CurrencyLister = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Err("ctrader symbols: auth refused".to_string())
    });
    let facts = ctrader_facts();
    facts.facts("EURUSD", Arc::clone(&failing)).expect_err("down");
    facts.facts("EURUSD", Arc::clone(&failing)).expect_err("the remembered failure");
    assert_eq!(sessions.load(Ordering::SeqCst), 1, "one session for two asks");

    let forgetful = SymbolPropertiesCacheAccountListed::<
        vike_ctrader::symbol_properties::SymbolPropertiesAccountSpecCtrader,
    >::with_failed_ttl(Duration::ZERO);
    forgetful.facts("EURUSD", Arc::clone(&failing)).expect_err("down");
    forgetful.facts("EURUSD", failing).expect_err("down again");
    assert_eq!(sessions.load(Ordering::SeqCst), 3, "a zero keep time dials again");
}

/// **A credential swap under one label is a different account**: the per-account map hands out the SAME
/// facts while the fingerprint is unchanged and FRESH ones (a new listing, a new currency) when it moves;
/// another label, and an account with no fingerprint, are their own entries.
#[test]
fn a_changed_credential_fingerprint_gets_fresh_facts() {
    hold_ctrader_kept_rows();
    let by_account = PerAccount::<
        SymbolPropertiesCacheAccountListed<
            vike_ctrader::symbol_properties::SymbolPropertiesAccountSpecCtrader,
        >,
    >::default();
    let (usd, usd_calls) = ctrader_lister("USD", ctrader_rows());
    let (eur, eur_calls) = ctrader_lister("EUR", ctrader_rows());
    let facts = by_account.of("ALT", Some(1));
    facts.facts("EURUSD", Arc::clone(&usd)).expect("a USD account");
    assert!(
        Arc::ptr_eq(&facts, &by_account.of("ALT", Some(1))),
        "the same account keeps its facts"
    );
    by_account.of("ALT", Some(1)).facts("EURUSD", Arc::clone(&usd)).expect("kept listing");
    assert_eq!(usd_calls.load(Ordering::SeqCst), 1, "one listing for the account");

    let swapped = by_account.of("ALT", Some(2));
    assert!(!Arc::ptr_eq(&facts, &swapped), "a changed fingerprint is a new account");
    let why = swapped.facts("EURUSD", eur).expect_err("USD quote on a EUR account");
    assert!(why.reason.contains("currency is EUR"), "{why}");
    assert_eq!(eur_calls.load(Ordering::SeqCst), 1, "the new account listed once");
    assert!(!Arc::ptr_eq(&by_account.of("OTHER", Some(1)), &by_account.of("ALT", Some(1))));
    assert!(Arc::ptr_eq(&by_account.of("", None), &by_account.of("", None)));
}

// --- ctrader, for a node that converts a quote currency (decision 0183, phase 1) ------------------------------

/// A rate source over `(quote currency, account per quote)`, the composition's closure in miniature
/// (`TradeLaneBridge::ctrader_facts_for`): it is called as `(quote currency, deposit currency)`, answers `Ok(None)`
/// for the deposit currency, `Ok(Some(rate))` for a priced one and `Err` for one it holds no rate for. Counts
/// the asks and remembers the last `(quote, deposit)` pair it was asked.
type LastAsked = Arc<std::sync::Mutex<(String, String)>>;

fn ctrader_rates(
    table: &[(&str, f64)],
    as_of_ms: i64,
) -> (RateSource, Arc<AtomicUsize>, LastAsked) {
    let asked = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(std::sync::Mutex::new((String::new(), String::new())));
    let (seen, seen_last) = (Arc::clone(&asked), Arc::clone(&last));
    let table: Vec<(String, f64)> = table.iter().map(|(c, r)| ((*c).to_string(), *r)).collect();
    let source: RateSource = Arc::new(move |quote: &str, deposit: &str| {
        seen.fetch_add(1, Ordering::SeqCst);
        *seen_last.lock().expect("last") = (quote.to_string(), deposit.to_string());
        if quote == deposit {
            return Ok(None);
        }
        match table.iter().find(|(c, _)| c == quote) {
            Some((c, r)) => Ok(Some(quote_rate(c, *r, as_of_ms))),
            None => Err(verdict(format!("ctrader: no rate for {quote} in {deposit}"))),
        }
    });
    (source, asked, last)
}

/// **The quoted answer**: a node that converts is answered a forex pair quoted in another currency with its
/// quote currency and the rate, in the orientation the engine takes (a yen is a FRACTION of a dollar), the
/// rate asked for EVERY ask (the listing is kept, the rate is not), the source called as `(the row's quote
/// currency, the deposit currency)`; a pair in the deposit currency is answered as before with no rate; and
/// the plain ask of the same account still refuses the foreign pair.
#[test]
fn ctrader_answers_a_foreign_quoted_pair_with_its_quote_currency_and_rate_to_a_node_that_converts()
{
    let (lister, calls) = ctrader_lister("USD", ctrader_rows());
    let (rate, asked, last) = ctrader_rates(&[("JPY", 1.0 / 150.0)], 1_700_000_000_000);
    let facts = ctrader_facts();

    let jpy =
        facts.facts_quoted("GBPJPY", Arc::clone(&lister), &rate).expect("a priced foreign pair");
    assert_eq!(jpy.quote_currency.as_deref(), Some("JPY"));
    assert_eq!(jpy.quote_rate, Some(1.0 / 150.0), "account units per ONE yen, never the inverse");
    assert_eq!(jpy.rate_as_of_ms, Some(1_700_000_000_000));
    assert_eq!(jpy.properties.min_qty, 1000.0, "the row's own grid travels");
    assert_eq!(jpy.contract_value, None, "the unit is 1: the engine multiplies the rate in");
    assert_eq!(*last.lock().expect("last"), ("JPY".to_string(), "USD".to_string()));

    let own = facts
        .facts_quoted("EURUSD", Arc::clone(&lister), &rate)
        .expect("quoted in the deposit currency");
    assert_eq!((&own.quote_currency, own.quote_rate, own.rate_as_of_ms), (&None, None, None));
    assert_eq!(asked.load(Ordering::SeqCst), 2, "the source is asked for every ask");
    facts.facts_quoted("GBPJPY", Arc::clone(&lister), &rate).expect("again");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "a rate is never served from the listing's keep time"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing session for all three asks");

    let plain = facts.facts("GBPJPY", lister).expect_err("the interim fence stands");
    assert!(plain.reason.contains("quoted in JPY but this account's currency is USD"), "{plain}");
}

/// The rate is judged against the ACCOUNT's deposit currency: on a EUR account the USD-quoted EURUSD is the
/// foreign one (rate asked as `(USD, EUR)`) and a EUR-quoted pair needs none.
#[test]
fn ctrader_converts_against_the_accounts_own_deposit_currency() {
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let (lister, _) = ctrader_lister("EUR", rows);
    let (rate, asked, last) = ctrader_rates(&[("USD", 0.92)], 1);
    let facts = ctrader_facts();
    let usd = facts
        .facts_quoted("EURUSD", Arc::clone(&lister), &rate)
        .expect("a USD quote on a EUR account");
    assert_eq!((usd.quote_currency.as_deref(), usd.quote_rate), (Some("USD"), Some(0.92)));
    assert_eq!(*last.lock().expect("last"), ("USD".to_string(), "EUR".to_string()));
    let own = facts.facts_quoted("GBPEUR", lister, &rate).expect("EUR quote");
    assert_eq!(own.quote_currency, None);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
}

/// **No rate, no admission**, and the fence's other clauses stand: a currency the source holds no rate for and
/// a source that fails each refuse the pair in the source's own words; a CFD of an unmeasured class and a missing
/// minimum volume are refused by name before the source is asked at all; the broker's own name is no user name and
/// names no row.
#[test]
fn ctrader_refuses_a_pair_it_cannot_price_and_keeps_the_other_clauses_of_the_fence() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = ctrader_facts();

    let (no_jpy, asked, _) = ctrader_rates(&[("GBP", 1.35)], 1);
    let why = facts.facts_quoted("GBPJPY", Arc::clone(&lister), &no_jpy).expect_err("no JPY rate");
    assert!(why.reason.contains("no rate for JPY in USD"), "{why}");
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    let failing: RateSource =
        Arc::new(|_: &str, _: &str| Err(verdict("ctrader: the venue is down")));
    let down = facts.facts_quoted("GBPJPY", Arc::clone(&lister), &failing).expect_err("down");
    assert_eq!(down, verdict("ctrader: the venue is down"));

    for (symbol, needle) in [("WHEAT.CFD", "Wheat_H6 is a CFD"), ("NOMUSD", "no minimum volume")] {
        let why = facts.facts_quoted(symbol, Arc::clone(&lister), &no_jpy).expect_err(symbol);
        assert!(why.reason.contains(needle), "{symbol}: {why}");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::Verdict, "{symbol}: a verdict: {why}");
    }
    assert_eq!(asked.load(Ordering::SeqCst), 1, "a refused row never reaches the rate source");

    // The broker's own name is no user name (decision 0290): read as one, upper case, it names no row.
    let wire = facts.facts_quoted("Wheat_H6", lister, &no_jpy).expect_err("the broker's name");
    assert!(wire.reason.contains("no instrument WHEAT_H6"), "{wire}");
}

/// **A deposit currency the broker's asset list does not name no longer refuses the WHOLE listing** (owner ruling
/// 2026-10-19: "the account currency is what the venue API states; if the API does not state it, or the read
/// fails: USD"): the lenient listing hands `None`, the account's currency is the ruling's USD, ASSUMED and said at
/// `warn` once, and the account LISTS (a USD-quoted pair is answered, which the strict listing refused with
/// everything else) and PRICES a cross (EURGBP: quoted in sterling, rate asked as `(GBP, USD)`). A broker that
/// DOES name its deposit (here EUR) is answered exactly as before: the golden, no assumption, no line.
#[test]
fn ctrader_with_an_unnamed_deposit_lists_and_prices_a_cross_on_an_assumed_usd_ledger() {
    use super::super::assumed_currency::AssumedCurrencySaid;
    use super::super::assumed_currency::listing_or_assumed;
    use vike_ctrader::symbol_properties::deposit_named;

    let said = Arc::new(AssumedCurrencySaid::default());
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("EURGBP", "EUR", "GBP", true, 1000.0));
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let lister = |deposit: Option<&'static str>, account: &'static str| -> CurrencyLister {
        let (said, rows) = (Arc::clone(&said), rows.clone());
        Arc::new(move || {
            listing_or_assumed(&said, "ctrader", account, || {
                Ok((deposit_named(deposit.map(str::to_string)), rows.clone()))
            })
        })
    };

    let unnamed = lister(None, "UNNAMED");
    let (rate, _, last) = ctrader_rates(&[("GBP", 1.35)], 7);
    let facts = ctrader_facts();
    let cross = facts
        .facts_quoted("EURGBP", Arc::clone(&unnamed), &rate)
        .expect("listed and priced on the assumed USD ledger");
    assert_eq!((cross.quote_currency.as_deref(), cross.quote_rate), (Some("GBP"), Some(1.35)));
    assert_eq!(*last.lock().expect("last"), ("GBP".to_string(), "USD".to_string()));
    facts
        .facts_quoted("EURUSD", Arc::clone(&unnamed), &rate)
        .expect("a USD-quoted pair is listed too");
    // ... and said at `warn`, once for the account, with the broker's own reason (read on THIS thread: the
    // lookups above may run the lister anywhere, and the account's line is said once, so a second account
    // is the one observed)
    let (listing, events) = vike_log::capture::captured(|| {
        listing_or_assumed(&said, "ctrader", "UNNAMED2", || {
            Ok((deposit_named(None), ctrader_rows()))
        })
    });
    assert_eq!(listing.expect("assumed").0, "USD");
    let warned: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(warned.len(), 1, "{events:?}");
    assert!(
        warned[0].starts_with("ctrader account currency not reported by the venue: assuming USD")
            && warned[0].contains("does not name this account's deposit currency"),
        "{warned:?}"
    );

    // the golden: the broker names EUR, and EUR it stays
    let named = lister(Some("EUR"), "NAMED");
    let (rate, _, last) = ctrader_rates(&[("USD", 0.92)], 7);
    let facts = ctrader_facts();
    let usd = facts
        .facts_quoted("EURUSD", Arc::clone(&named), &rate)
        .expect("a USD quote on a named EUR account");
    assert_eq!((usd.quote_currency.as_deref(), usd.quote_rate), (Some("USD"), Some(0.92)));
    assert_eq!(*last.lock().expect("last"), ("USD".to_string(), "EUR".to_string()));
    facts.facts_quoted("GBPEUR", named, &rate).expect("EUR-quoted: no rate");

    // a listing that could not be read at all is still an error (nothing to price, in any currency)
    let failing =
        listing_or_assumed(&said, "ctrader", "FAILED", || Err("ctrader symbols: timed out".into()));
    assert_eq!(failing.expect_err("no session"), "ctrader symbols: timed out");
}

/// **A converted answer STATES the ledger its rate is in** (review 2, HIGH-1 (a)): oanda's facts stamp
/// `SymbolPropertiesWire::account_currency` with the currency the listing (and so the rate) was read in, for a symbol quoted
/// in another currency AND for one quoted in the account's own, so the node, which holds the engine at the
/// currency it was started with, refuses an answer in another one. Nothing is stamped for the plain answer.
#[test]
fn oanda_stamps_the_ledger_currency_its_answer_is_in() {
    let (lister, _) = oanda_lister("EUR", oanda_rows());
    let (rate, _) = rates(&[("GBP", 0.85), ("USD", 0.92)], 1);
    let facts = oanda_facts();
    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("a priced cross");
    assert_eq!(cross.quote_currency.as_deref(), Some("GBP"));
    assert_eq!(cross.account_currency.as_deref(), Some("EUR"), "the rate is in the listing's EUR");
    let usd = facts.facts_quoted("GBPUSD", Arc::clone(&lister), &rate).expect("a priced USD pair");
    assert_eq!(usd.account_currency.as_deref(), Some("EUR"));

    // a symbol quoted in the account's own money carries no rate, and still says which ledger it is in
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let own = oanda_facts().facts_quoted("GBPUSD", lister, &rate).expect("quoted in USD");
    assert_eq!((&own.quote_currency, own.quote_rate), (&None, None));
    assert_eq!(own.account_currency.as_deref(), Some("USD"));

    // the plain answer (a node that converts nothing) is as it was
    let (lister, _) = oanda_lister("USD", oanda_rows());
    assert_eq!(oanda_facts().facts("GBPUSD", lister).expect("plain").account_currency, None);
}

/// [`oanda_stamps_the_ledger_currency_its_answer_is_in`] for cTrader: the deposit currency of the listing the rate
/// was priced against.
#[test]
fn ctrader_stamps_the_ledger_currency_its_answer_is_in() {
    let (rate, _, _) = ctrader_rates(&[("JPY", 1.0 / 165.0)], 1);
    let (lister, _) = ctrader_lister("EUR", ctrader_rows());
    let facts = ctrader_facts();
    let jpy = facts
        .facts_quoted("GBPJPY", Arc::clone(&lister), &rate)
        .expect("a priced yen pair on a EUR account");
    assert_eq!(jpy.account_currency.as_deref(), Some("EUR"));
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let own = ctrader_facts()
        .facts_quoted("EURUSD", lister, &rate)
        .expect("quoted in the deposit currency");
    assert_eq!((&own.quote_currency, own.quote_rate), (&None, None));
    assert_eq!(own.account_currency.as_deref(), Some("USD"));
}

/// **A transient failure at the start on a EUR account, through the gateway's own pieces** (review 2, HIGH-1 (d)):
/// the start's `/summary` timed out, so the engine is told USD ASSUMED; the lister reads the summary a moment
/// later and gets EUR. The facts it answers STATE EUR (the node, which holds the engine at USD, refuses them
/// whether the symbol is quoted in another currency or in the account's own), and the balance stays withheld
/// because only a start moves what the balance gate reads.
#[test]
fn a_start_that_missed_the_summary_on_a_eur_account_is_never_priced_in_eur_under_a_usd_label() {
    use super::super::assumed_currency::{AssumedCurrencySaid, lister_currency, start_currency};
    use vike_model::AccountCurrency;

    let said = Arc::new(AssumedCurrencySaid::default());
    let timed_out = AccountCurrency::Assumed { why: "timed out".into() };
    assert_eq!(start_currency(&said, "oanda", "EURACC", timed_out.clone()), timed_out);
    let (inner, _) = oanda_lister("EUR", oanda_rows());
    let lister: CurrencyLister = {
        let said = Arc::clone(&said);
        Arc::new(move || {
            let currency = lister_currency(&said, "oanda", "EURACC", true, Ok("EUR".to_string()))?;
            inner().map(|(_, rows)| (currency, rows))
        })
    };
    let (rate, _) = rates(&[("GBP", 0.85)], 1);
    let facts = oanda_facts();
    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("priced in EUR");
    assert_eq!(cross.quote_rate, Some(0.85));
    assert_eq!(
        cross.account_currency.as_deref(),
        Some("EUR"),
        "the answer says it is NOT the start's USD"
    );
    assert!(
        said.is_assumed("oanda", "EURACC"),
        "the engine is still USD assumed: balance withheld"
    );
}

/// [`a_start_that_missed_the_summary_on_a_eur_account_is_never_priced_in_eur_under_a_usd_label`] for cTrader: the
/// start's handshake failed (so the engine is told USD assumed), the broker's asset list names EUR when the listing
/// is read a moment later. The answer states EUR, the rate is asked in EUR, and the balance gate still follows the
/// start.
#[test]
fn a_ctrader_start_that_missed_the_handshake_is_never_priced_in_eur_under_a_usd_label() {
    use super::super::assumed_currency::listing_or_assumed;
    use super::super::assumed_currency::{AssumedCurrencySaid, start_currency};
    use vike_ctrader::symbol_properties::deposit_named;
    use vike_model::AccountCurrency;

    let said = Arc::new(AssumedCurrencySaid::default());
    let timed_out = AccountCurrency::Assumed { why: "handshake timed out".into() };
    assert_eq!(start_currency(&said, "ctrader", "CTEUR", timed_out.clone()), timed_out);
    let rows = ctrader_rows();
    let lister: CurrencyLister = {
        let said = Arc::clone(&said);
        Arc::new(move || {
            listing_or_assumed(&said, "ctrader", "CTEUR", || {
                Ok((deposit_named(Some("EUR".to_string())), rows.clone()))
            })
        })
    };
    let (rate, _, last) = ctrader_rates(&[("JPY", 1.0 / 165.0)], 1);
    let wire = ctrader_facts().facts_quoted("GBPJPY", lister, &rate).expect("priced in EUR");
    assert_eq!(
        wire.account_currency.as_deref(),
        Some("EUR"),
        "the answer says it is not the start's USD"
    );
    assert_eq!(*last.lock().expect("last"), ("JPY".to_string(), "EUR".to_string()));
    assert!(said.is_assumed("ctrader", "CTEUR"), "the engine is still USD assumed");
}

// ---- binance: TradFi perpetuals, dated USDS-M futures, COIN-M by name, the demo network's own list --------

/// A dated USDS-M future as the typed catalog row `BTCUSDT_261225.P` (the lane suffix is part of the key).
fn binance_dated(symbol: &str, expiry_ms: i64, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "binance",
        format!("{symbol}.P"),
        "BTC",
        "USDT",
        Contract::Future(vike_model::ContractFuture {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            expiry_ms,
            tenor: Some(vike_model::ContractFutureTenor::Quarter),
            delivery: vike_model::ContractFutureDelivery::Unknown,
            family: vike_model::ContractFutureFamily::Crypto,
        }),
    )
    .with_properties(p)
    .with_venue_word("CURRENT_QUARTER")
}

/// A COIN-M (inverse) row as `CatalogProviderBinance`'s dapi parse spells it: `BTCUSD_PERP.P`, 100 USD per contract.
fn coin_m(symbol: &str, word: &'static str) -> Instrument {
    Instrument::new("binance", format!("{symbol}.P"), "BTC", "USD", perp_inverse())
        .with_venue_word(word)
        .with_properties(SymbolProperties { contract_size: 100.0, ..grid(0.1, 1.0) })
}

#[test]
fn a_tradfi_perpetual_and_a_dated_future_are_answered_with_their_own_grid() {
    let rows = vec![
        // The typed row of a TRADIFI_PERPETUAL is a plain linear perpetual (decision 0230).
        perp("binance", "TSLAUSDT", Margining::Linear, grid(0.01, 0.01))
            .with_venue_word("TRADIFI_PERPETUAL"),
        binance_dated("BTCUSDT_371225", 2_145_340_800_000, grid(0.1, 0.001)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolPropertiesCache::new(list);
    let tsla = facts.facts("binance", "TSLAUSDT.P").expect("TradFi perpetual");
    assert_eq!((tsla.properties.tick_size, tsla.properties.step_size), (0.01, 0.01));
    assert_eq!(tsla.contract_value, None, "linear, one base unit per unit of quantity");
    let q = facts.facts("binance", "BTCUSDT_371225.P").expect("dated future");
    assert_eq!((q.properties.tick_size, q.properties.step_size), (0.1, 0.001));
    assert_eq!(q.contract_value, None);
    // The lane suffix is what makes it the USDS-M lane: the bare spelling is a spot lookup of another name.
    assert!(facts.facts("binance", "BTCUSDT_371225").is_err());
}

/// **A dated future is judged by the delivery day its NAME states** (`_YYMMDD`, decision 0290, at the venue's 08:00 UTC:
/// `crates/vike-venues/src/venue_delivery_guard.rs`'s `delivery_verdict`, asked as a reduce-only order would be): one
/// already delivered is refused by name, and one inside its last day is ADMITTED close-only (the bridge refuses each
/// opening order at send, okx's rule for every venue, 2026-11-22).
#[test]
fn a_delivered_future_is_refused_by_name_and_one_inside_its_last_day_admitted() {
    let now = vike_model::now_ms();
    // The next 08:00 UTC delivery inside the coming 24 hours, as the name's `YYMMDD`.
    let delivery = (now - 8 * MS_PER_HOUR).div_euclid(MS_PER_DAY) * MS_PER_DAY + 32 * MS_PER_HOUR;
    let date = vike_model::time::epoch_ms_to_utc_date(delivery);
    let last_day = format!("ETHUSDT_{}{}{}", &date[2..4], &date[5..7], &date[8..10]);
    let rows = vec![
        // The demo list kept a TRADING weekly four and a half months after its delivery date.
        binance_dated("BSBUSDT_260618", 1_781_769_600_000, grid(0.00001, 1.0)),
        binance_dated(&last_day, delivery, grid(0.01, 0.001)),
        binance_dated("BTCUSDT_371225", 2_145_340_800_000, grid(0.1, 0.001)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolPropertiesCache::new(list);
    let past = facts.facts("binance", "BSBUSDT_260618.P").expect_err("delivered");
    assert!(
        past.reason.contains("was delivered on 2026-06-18")
            && past.reason.contains("BSBUSDT_260618.P"),
        "{past}"
    );
    assert_eq!(past.code, SymbolPropertiesRefusalCode::Expired, "gone, not a retry: {past}");
    let in_last_day = facts.facts("binance", &format!("{last_day}.P"));
    assert!(in_last_day.is_ok(), "its last day is close-only: {in_last_day:?}");
    assert!(facts.facts("binance", "BTCUSDT_371225.P").is_ok(), "far from its delivery");
}

/// **A COIN-M symbol is answered from its own book's list, at its row's face** (decision 0258's binance
/// amendment): `Inverse(contractSize)` and the whole-contract grid, from the list of the account's network; a
/// name that is not COIN-M-shaped never reads that list, and a COIN-M ask never reads the USDS-M one.
#[test]
fn a_coin_m_symbol_is_answered_at_its_rows_face_from_its_networks_list() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let list: InstrumentList = Arc::new(move |key| {
        seen.lock().expect("asked").push(key.to_string());
        if key == vike_binance::symbol_properties::BINANCE_COIN_M_LIST {
            Ok(vec![coin_m("BTCUSD_PERP", "PERPETUAL"), coin_m("BTCUSD_371225", "CURRENT_QUARTER")])
        } else if key == vike_binance::symbol_properties::BINANCE_DEMO_COIN_M_LIST {
            let mut faceless = coin_m("ETHUSD_PERP", "PERPETUAL");
            faceless.properties.contract_size = 0.0;
            Ok(vec![coin_m("BTCUSD_PERP", "PERPETUAL"), faceless])
        } else {
            Ok(binance_rows())
        }
    });
    let facts = SymbolPropertiesCache::new(list);
    let inverse = |face| Some(ContractValue { kind: NotionalKind::Inverse, per_unit: face });
    let perp = facts.facts("binance", "BTCUSD_PERP.P").expect("COIN-M perpetual");
    assert_eq!(perp.contract_value, inverse(100.0), "100 US dollars a contract");
    assert_eq!((perp.properties.tick_size, perp.properties.step_size), (0.1, 1.0));
    assert_eq!(
        perp.properties.contract_size, 0.0,
        "the face is the value's, never a grid multiplier"
    );
    let dated = facts.facts("binance", "BTCUSD_371225.P").expect("COIN-M quarterly");
    assert_eq!(dated.contract_value, inverse(100.0));
    // A demo account is answered from the DEMO COIN-M host's list, which lacks the quarterly here.
    assert!(facts.facts_on("binance", "testnet", "BTCUSD_PERP.P").is_ok());
    let demo = facts.facts_on("binance", "testnet", "BTCUSD_371225.P").expect_err("not on demo");
    assert_eq!(demo.code, SymbolPropertiesRefusalCode::NoInstrument, "{demo}");
    assert!(
        demo.reason.contains("no instrument BTCUSD_371225.P")
            && demo.reason.contains("DEMO COIN-M host"),
        "{demo}"
    );
    let faceless = facts.facts_on("binance", "testnet", "ETHUSD_PERP.P").expect_err("no face");
    assert_eq!(faceless.code, SymbolPropertiesRefusalCode::Verdict, "{faceless}");
    assert!(faceless.reason.contains("value is unknown"), "{faceless}");
    let absent = facts.facts("binance", "FOOUSD_PERP.P").expect_err("shaped but absent");
    assert!(absent.reason.contains("no instrument FOOUSD_PERP.P"), "{absent}");
    // No COIN-M ask read the USDS-M list; a USDT-shaped miss reads no COIN-M list.
    let coin_m_reads = |keys: &[String]| keys.iter().filter(|k| k.contains("coin-m")).count();
    let keys = asked.lock().expect("asked").clone();
    assert!(
        !keys
            .iter()
            .any(|k| k == "binance" || k == vike_binance::symbol_properties::BINANCE_DEMO_LIST),
        "{keys:?}"
    );
    let before = coin_m_reads(&keys);
    let typo = facts.facts("binance", "NOPEUSDT.P").expect_err("unknown");
    assert!(typo.reason.contains("no instrument NOPEUSDT.P"), "{typo}");
    assert_eq!(
        coin_m_reads(&asked.lock().expect("asked")),
        before,
        "no COIN-M read for a USDT-shaped miss"
    );
}

/// An unreadable COIN-M list is a transient failure, never a verdict and never an answer.
#[test]
fn an_unreadable_coin_m_list_is_a_transient_failure() {
    let list: InstrumentList = Arc::new(move |key| {
        if key == vike_binance::symbol_properties::BINANCE_COIN_M_LIST {
            Err("dapi down".to_string())
        } else {
            Ok(binance_rows())
        }
    });
    let err =
        SymbolPropertiesCache::new(list).facts("binance", "BTCUSD_PERP.P").expect_err("unreadable");
    assert!(err.reason.contains("dapi down"), "{err}");
    assert_eq!(
        err.code,
        SymbolPropertiesRefusalCode::ListUnavailable,
        "a retry, not a verdict: {err}"
    );
}

#[test]
fn a_demo_account_is_answered_from_the_demo_networks_list() {
    // mainnet lists a TradFi perpetual the demo host does not.
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let list: InstrumentList = Arc::new(move |key| {
        seen.lock().expect("asked").push(key.to_string());
        if key == vike_binance::symbol_properties::BINANCE_DEMO_LIST {
            Ok(vec![perp("binance", "XAUUSDT", Margining::Linear, grid(0.01, 0.001))])
        } else {
            Ok(vec![
                perp("binance", "XAUUSDT", Margining::Linear, grid(0.01, 0.001)),
                perp("binance", "NVDAUSDT", Margining::Linear, grid(0.01, 0.01)),
            ])
        }
    });
    let facts = SymbolPropertiesCache::new(list);
    assert!(facts.facts_on("binance", "mainnet", "NVDAUSDT.P").is_ok());
    let err = facts.facts_on("binance", "testnet", "NVDAUSDT.P").expect_err("not on the demo host");
    assert!(
        err.reason.contains("no instrument NVDAUSDT.P") && err.reason.contains("DEMO futures host"),
        "{err}"
    );
    assert!(facts.facts_on("binance", "testnet", "XAUUSDT.P").is_ok());
    let mut keys = asked.lock().expect("asked").clone();
    keys.sort();
    keys.dedup();
    assert_eq!(keys, ["binance", vike_binance::symbol_properties::BINANCE_DEMO_LIST]);
}

/// **A bridge's refusal reaches the node in the words it always had, under its kind's code**: a verdict passes as its
/// own sentence (the node remembers it), and the two "not now" kinds are `<venue>: <marker>: <detail>` with the wire's
/// own markers (the node asks again). The dukascopy and oanda details below are the moved lanes' own words.
#[test]
fn a_bridges_refusal_is_worded_with_the_wires_markers() {
    use super::worded;
    use vike_bridge_core::symbol_properties::SymbolPropertiesError;
    use vike_gatewayhub_client::proto::{PROPERTIES_LIST_UNAVAILABLE, PROPERTIES_RATE_UNAVAILABLE};
    let said = worded(
        "dukascopy",
        SymbolPropertiesError::Refused(
            "dukascopy: `GBPNOK` is not on the account's instrument list".into(),
        ),
    );
    assert_eq!(said, verdict("dukascopy: `GBPNOK` is not on the account's instrument list"));
    let terms = worded(
        "dukascopy",
        SymbolPropertiesError::ListUnavailable("the terms of `XAU/USD`: no line".into()),
    );
    assert_eq!(
        terms,
        super::coded(
            SymbolPropertiesRefusalCode::ListUnavailable,
            format!("dukascopy: {PROPERTIES_LIST_UNAVAILABLE}: the terms of `XAU/USD`: no line")
        )
    );
    let rate = worded(
        "oanda",
        SymbolPropertiesError::RateUnavailable("no rate for JPY in USD (a 503)".into()),
    );
    assert_eq!(
        rate,
        super::coded(
            SymbolPropertiesRefusalCode::RateUnavailable,
            format!("oanda: {PROPERTIES_RATE_UNAVAILABLE}: no rate for JPY in USD (a 503)")
        )
    );
}

/// **A bridge's refusal id rides beside its words on the wire** (`SymbolPropertiesRefusal::refusal_id`): a verdict
/// keeps its sentence BARE under the id, a "not now" its `<venue>: <marker>: <detail>` (the detail bare, the id the
/// bridge's), and free text carries none.
#[test]
fn a_bridges_refusal_id_rides_beside_its_bare_words() {
    use super::worded;
    use vike_bridge_core::symbol_properties::SymbolPropertiesError;
    use vike_gatewayhub_client::proto::PROPERTIES_RATE_UNAVAILABLE;
    use vike_utility::errors::CarriedRefusal;
    let carried = |id: &str, text: &str| CarriedRefusal::new(Some(id.into()), text.into());
    let said = worded(
        "ibkr",
        SymbolPropertiesError::Refused(carried("ibkr.symbol.class.no_details", "XYZ: no details")),
    );
    assert_eq!(
        (said.code, said.reason.as_str(), said.refusal_id.as_deref()),
        (
            SymbolPropertiesRefusalCode::Verdict,
            "XYZ: no details",
            Some("ibkr.symbol.class.no_details")
        )
    );
    let rate = worded(
        "ibkr",
        SymbolPropertiesError::RateUnavailable(carried("ibkr.symbol.rate.x", "no rate for JPY")),
    );
    assert_eq!(
        (rate.reason, rate.refusal_id),
        (
            format!("ibkr: {PROPERTIES_RATE_UNAVAILABLE}: no rate for JPY"),
            Some("ibkr.symbol.rate.x".to_string())
        )
    );
    let free = worded("ibkr", SymbolPropertiesError::Refused("ibkr: no".into()));
    assert_eq!(free.refusal_id, None);
}

/// **Each kind a bridge refuses with is its own code, decided by the kind and never by the words, and a "not now" is
/// never coded gone** (the boot gives a flat symbol back on a gone code alone: `crates/vike-tradehub/src/readmit.rs`'s
/// `give_back`): a list failure that quotes a venue's "no instrument" stays a list failure, and a verdict that quotes
/// a marker of the wire stays a verdict (decision 0301).
#[test]
fn every_kind_a_bridge_refuses_with_is_coded_and_a_transient_one_is_never_gone() {
    use super::worded;
    use vike_bridge_core::symbol_properties::SymbolPropertiesError;
    use vike_gatewayhub_client::proto::{
        PROPERTIES_BUSY, PROPERTIES_EXPIRED, PROPERTIES_LIST_UNAVAILABLE, PROPERTIES_NO_INSTRUMENT,
    };
    for (refusal, code) in [
        (
            SymbolPropertiesError::ListUnavailable("bybit: no instrument X in the answer".into()),
            SymbolPropertiesRefusalCode::ListUnavailable,
        ),
        (
            SymbolPropertiesError::RateUnavailable("no rate for JPY in USD".into()),
            SymbolPropertiesRefusalCode::RateUnavailable,
        ),
        (
            SymbolPropertiesError::NoInstrument(format!(
                "bybit{PROPERTIES_NO_INSTRUMENT}X in the venue's instrument list (linear)"
            ).into()),
            SymbolPropertiesRefusalCode::NoInstrument,
        ),
        (
            SymbolPropertiesError::Expired(format!(
                "binance: X was delivered on 2026-01-01; the venue's list still shows it as TRADING, but \
                 {PROPERTIES_EXPIRED}"
            ).into()),
            SymbolPropertiesRefusalCode::Expired,
        ),
        (
            SymbolPropertiesError::Refused(
                "bybit: X is a quanto contract, whose notional is not valued".into(),
            ),
            SymbolPropertiesRefusalCode::Verdict,
        ),
        (
            SymbolPropertiesError::Refused(format!(
                "ibkr: the venue said `{PROPERTIES_LIST_UNAVAILABLE}` and `{PROPERTIES_BUSY}`"
            ).into()),
            SymbolPropertiesRefusalCode::Verdict,
        ),
    ] {
        let said = worded("bybit", refusal);
        assert_eq!(said.code, code, "{said}");
        assert!(!(code.is_transient() && code.is_gone()), "{said}");
    }
}
