//! `SymbolFacts` over an injected instrument list: the three venues' spellings, the refusals, and the
//! listing cache. No network: the list is a counting closure over fixture rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_model::{
    Contract, Delivery, FutureFamily, FutureSpec, Instrument, Margining, NotionalKind, PerpSpec,
    SymbolProperties,
};

use super::{
    AccountFacts, AccountLister, CtraderFacts, CurrencyLister, FactsByAccount, InstrumentList,
    NOT_ANSWERED, OandaFacts, QuotedRate, RateSource, SymbolFacts,
};

fn grid(tick: f64, step: f64) -> SymbolProperties {
    SymbolProperties {
        tick_size: tick,
        step_size: step,
        min_qty: step,
        min_notional: 5.0,
        ..Default::default()
    }
}

fn perp(venue: &str, symbol: &str, margining: Margining, p: SymbolProperties) -> Instrument {
    Instrument::new(
        venue,
        format!("{symbol}.P"),
        "BASE",
        "USDT",
        Contract::Perp(PerpSpec {
            margining,
            settle: "USDT".into(),
            value_ccy: None,
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
        let facts = SymbolFacts::new(list);
        let spot = facts.facts(venue, "SOLUSDT").expect("spot");
        assert_eq!((spot.properties.tick_size, spot.properties.step_size), (0.01, 0.001));
        let perp = facts.facts(venue, "SOLUSDT.P").expect("perp");
        assert_eq!((perp.properties.tick_size, perp.properties.step_size), (0.001, 1.0));
        assert_eq!(perp.properties.min_notional, 5.0, "the row's own minimums travel");
        assert_eq!(perp.contract_value, None);
        assert_eq!(facts.facts(venue, "solusdt.p").expect("case-insensitive"), perp);
    }
}

#[test]
fn bybit_reads_a_bare_symbol_as_the_linear_perpetual() {
    let rows = vec![
        perp("bybit", "SOLUSDT", Margining::Linear, grid(0.001, 0.1)),
        Instrument::new("bybit", "BTCUSD.P", "BTC", "USD", perp_inverse())
            .with_venue_word("InversePerpetual")
            .with_properties(grid(0.5, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolFacts::new(list);
    let sol = facts.facts("bybit", "SOLUSDT").expect("linear perp");
    assert_eq!((sol.properties.tick_size, sol.properties.step_size), (0.001, 0.1));
    assert_eq!(sol.contract_value, None, "a linear perpetual is the default contract value");
    // Decision 0231: a coin-margined perpetual is admitted, as an INVERSE contract (one unit of quantity is one
    // USD of notional), where it used to be refused by name.
    let inverse = facts.facts("bybit", "BTCUSD").expect("a coin-margined perpetual is admitted");
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
        Contract::Future(FutureSpec {
            margining,
            settle: "USDT".into(),
            value_ccy: None,
            expiry_ms,
            tenor: None,
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        }),
    )
    .with_venue_word(if margining == Margining::Linear {
        "LinearFutures"
    } else {
        "InverseFutures"
    })
    .with_properties(p)
}

/// **A dated future is admitted under the venue's own name** (decision 0231): the row is keyed by that name (no
/// `.P`), the contract is linear or inverse by its margining, and a contract within a day of its delivery is
/// refused in words (the venue closes every position at delivery and this adapter does not fold that).
#[test]
fn bybit_admits_a_dated_future_by_its_own_name_and_refuses_one_about_to_deliver() {
    let day = 24 * 3_600_000_i64;
    let now = vike_model::now_ms();
    let rows = vec![
        dated("ETHUSDT-27NOV26", Margining::Linear, now + 20 * day, grid(0.01, 0.01)),
        dated("ETHUSDT-01NOV26", Margining::Linear, now + 3_600_000, grid(0.01, 0.01)),
        dated("ETHUSDT-01JAN20", Margining::Linear, now - day, grid(0.01, 0.01)),
        dated("BTCUSDZ26", Margining::Inverse, now + 60 * day, grid(0.5, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolFacts::new(list);
    let far = facts.facts("bybit", "ETHUSDT-27NOV26").expect("a far dated linear future");
    assert_eq!((far.properties.tick_size, far.properties.step_size), (0.01, 0.01));
    assert_eq!(far.contract_value, None);
    assert_eq!(facts.facts("bybit", "ethusdt-27nov26").expect("case-insensitive"), far);

    let inverse = facts.facts("bybit", "BTCUSDZ26").expect("a far dated coin-margined future");
    assert_eq!(inverse.contract_value.map(|v| v.kind), Some(NotionalKind::Inverse));

    for symbol in ["ETHUSDT-01NOV26", "ETHUSDT-01JAN20"] {
        let err = facts.facts("bybit", symbol).expect_err("about to deliver, or delivered");
        assert!(err.contains("delivers in") && err.contains("next one"), "{symbol}: {err}");
    }
}

/// **A spot pair is `PAIR.S`**, answered `Cash` at a margin rate of 1 (fully funded, never liquidated) on the SPOT
/// row's grid; the bare name stays the perpetual of the same name. A spot row under a bare name (a lister that
/// answered the wrong book) is refused, never admitted as the perpetual.
#[test]
fn bybit_reads_a_spot_pair_by_its_suffix_as_cash_and_keeps_the_perpetual_apart() {
    let rows = vec![
        spot("bybit", "DOGEUSDT", grid(0.00001, 0.1)),
        perp("bybit", "DOGEUSDT", Margining::Linear, grid(0.00001, 1.0)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolFacts::new(list);
    let pair = facts.facts("bybit", "DOGEUSDT.S").expect("the spot pair");
    assert_eq!((pair.properties.tick_size, pair.properties.step_size), (0.00001, 0.1));
    assert_eq!(pair.margin_mode, Some(vike_model::MarginMode::Cash));
    assert_eq!(pair.margin_rate, Some(1.0));
    assert_eq!(pair.contract_value, None);
    let perpetual = facts.facts("bybit", "DOGEUSDT").expect("the perpetual of the same name");
    assert_eq!(perpetual.properties.step_size, 1.0, "the perpetual's own grid, not the pair's");
    assert_eq!(perpetual.margin_mode, None);

    let err = facts.facts("bybit", "NOPEUSDT.S").expect_err("an unlisted pair");
    assert!(
        err.starts_with("bybit: no instrument NOPEUSDT.S in the venue's instrument list"),
        "{err}"
    );
    // A derivative row under the spot spelling: the lister answered the wrong book.
    let (list, _) = counting(vec![perp("bybit", "DOGEUSDT", Margining::Linear, grid(0.1, 1.0))]);
    assert!(SymbolFacts::new(list).facts("bybit", "DOGEUSDT.S").is_err());
}

fn perp_inverse() -> Contract {
    Contract::Perp(PerpSpec {
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_ccy: None,
        funding_interval_ms: None,
        settling_ms: None,
    })
}

#[test]
fn an_unknown_symbol_names_the_venue_the_symbol_and_the_lane() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolFacts::new(list);
    let err = facts.facts("binance", "NOPEUSDT.P").expect_err("unknown");
    assert!(
        err.starts_with(
            "binance: no instrument NOPEUSDT.P in the venue's instrument list (spot, USDS-M \
             perpetuals (TradFi included) and dated USDS-M futures that are TRADING"
        ),
        "{err}"
    );
    assert!(!facts_refusal_is_transient(&err), "an unknown symbol is a verdict, not a retry");
    // Aster's list is its perpetuals: the old words.
    let err = facts.facts("aster", "NOPEUSDT.P").expect_err("unknown");
    assert_eq!(
        err,
        "aster: no instrument NOPEUSDT.P in the venue's instrument list (spot and USDS-M \
         perpetuals)"
    );
    // The perp lane is not the spot lane: a bare symbol that only exists as a perp is unknown.
    let rows = vec![perp("bybit", "SOLUSDT", Margining::Linear, grid(0.1, 1.0))];
    let (list, _) = counting(rows);
    let err = SymbolFacts::new(list).facts("bybit", "ETHUSDT").expect_err("unknown");
    assert!(
        err.starts_with("bybit: no instrument ETHUSDT in the venue's instrument list"),
        "{err}"
    );
}

#[test]
fn a_perp_that_is_not_linear_is_refused_with_the_venues_word() {
    let (list, _) = counting(binance_rows());
    let err = SymbolFacts::new(list).facts("binance", "COINUSD.P").expect_err("inverse");
    assert!(err.contains("not a linear perpetual"), "{err}");
}

#[test]
fn a_missing_tick_or_step_is_refused_naming_which() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolFacts::new(list);
    let tick = facts.facts("binance", "NOTICKUSDT.P").expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
    let step = facts.facts("binance", "NOSTEPUSDT.P").expect_err("no step");
    assert!(step.contains("no step size"), "{step}");
}

#[test]
fn another_venue_keeps_the_default_answer_and_asks_nothing() {
    let (list, calls) = counting(binance_rows());
    let facts = SymbolFacts::new(list);
    // oanda included: its fences (the account's currency, what the adapter writes) are `OandaFacts`',
    // so a PUBLIC source of oanda rows answers nothing.
    for venue in ["ctrader", "oanda"] {
        let err = facts.facts(venue, "EUR_USD").expect_err("not served");
        assert_eq!(err, NOT_ANSWERED, "{venue}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_fresh_listing_is_not_fetched_again_and_a_stale_one_is() {
    let (list, calls) = counting(binance_rows());
    let facts = SymbolFacts::with_ttl(list, Duration::from_secs(3600));
    for _ in 0..3 {
        facts.facts("binance", "SOLUSDT").expect("spot");
        facts.facts("binance", "SOLUSDT.P").expect("perp");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing served six asks");
    facts.facts("aster", "SOLUSDT").expect("another venue lists for itself");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let (list, calls) = counting(binance_rows());
    let stale = SymbolFacts::with_ttl(list, Duration::ZERO);
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
    let facts = SymbolFacts::with_ttls(list, Duration::from_secs(3600), Duration::ZERO);
    let failed = facts.facts("binance", "SOLUSDT").expect_err("failed");
    assert_eq!(failed, "binance: the venue's instrument list could not be fetched: HTTP 503");
    assert!(facts_refusal_is_transient(&failed), "a failed fetch is not a verdict on the symbol");
    let empty = facts.facts("binance", "SOLUSDT").expect_err("empty");
    assert!(empty.contains("came back empty"), "{empty}");
    assert!(facts_refusal_is_transient(&empty), "{empty}");
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
    let facts = SymbolFacts::new(list);
    for _ in 0..50 {
        assert_eq!(
            facts.facts("binance", "SOLUSDT").expect_err("rate limited"),
            "binance: the venue's instrument list could not be fetched: HTTP 429"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "fifty asks, one fetch");
    facts.facts("aster", "SOLUSDT").expect_err("another venue is asked for itself");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let patient = SymbolFacts::with_ttls(
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
    let facts = Arc::new(SymbolFacts::new(slow));
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
    inst: &str,
    inst_type: &str,
    settle: &str,
    ct: (&str, &str, &str, &str),
    grid: (&str, &str, &str, &str),
) -> serde_json::Value {
    let (ct_val, ct_mult, ct_val_ccy, ct_type) = ct;
    let (tick, lot, min, max) = grid;
    let uly = inst.split('-').take(2).collect::<Vec<_>>().join("-");
    serde_json::json!({
        "instType": inst_type, "instId": inst, "uly": uly, "instFamily": uly,
        "baseCcy": "", "quoteCcy": "", "settleCcy": settle,
        "ctVal": ct_val, "ctMult": ct_mult, "ctValCcy": ct_val_ccy, "ctType": ct_type,
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

/// A linear USDT future delivered in three hours: inside the last day, where an admission is refused.
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
    let swap =
        |inst, ct_val, ccy, grid| okx_row(inst, "SWAP", "USDT", (ct_val, "1", ccy, "linear"), grid);
    serde_json::json!({ "code": "0", "msg": "", "data": [
        swap("BTC-USDT-SWAP", "0.01", "BTC", ("0.1", "0.01", "0.01", "35000")),
        swap("ETH-USDT-SWAP", "0.1", "ETH", ("0.01", "0.01", "0.01", "15000")),
        swap("DOGE-USDT-SWAP", "1000", "DOGE", ("0.00001", "1", "1", "100000")),
        swap("QUOTE-USDT-SWAP", "1", "USDT", ("0.1", "1", "1", "1000")),
        swap("NOCT-USDT-SWAP", "", "NOCT", ("0.1", "1", "1", "1000")),
        okx_row("BTC-USD-SWAP", "SWAP", "BTC", ("100", "1", "USD", "inverse"), ("0.1", "0.1", "0.1", "49000")),
        okx_row("BTC-USDC-SWAP", "SWAP", "USDC", ("0.001", "1", "BTC", "linear"), ("0.1", "0.1", "0.1", "5000")),
        okx_row("BTC-USDT-261225", "FUTURES", "USDT", ("0.01", "1", "BTC", "linear"), ("0.1", "0.01", "0.01", "5000")),
        // delivered in three hours: inside the last day, where an admission is refused
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

fn okx_facts() -> SymbolFacts {
    let rows = vike_okx::catalog::parse_insts(&okx_payload());
    assert_eq!(rows.len(), 14, "every fixture row parses into the catalog");
    let (list, _) = counting(rows);
    SymbolFacts::new(list)
}

/// **The equality that matters.** The gatewayhub reads the CATALOG row (the grid in CONTRACTS, the
/// contract size in `contract_size`); the engine must hold what the MOUNTED path holds for the same
/// instrument - `vike_okx::perp::properties_in_base` of the exec row - or an admitted instrument
/// would snap and cap on a grid of another unit. Compared bit for bit, per instrument.
#[test]
fn okx_facts_equal_the_mounted_paths_base_unit_grid() {
    let exec_rows = vike_okx::perp::parse_okx_perp_instruments(&okx_payload());
    let facts = okx_facts();
    for inst in ["BTC-USDT-SWAP", "ETH-USDT-SWAP", "DOGE-USDT-SWAP", "BTC-USDT-261225"] {
        let exec = &exec_rows[inst];
        let want = vike_okx::perp::properties_in_base(&exec.properties, exec.ct_val);
        let got = facts.facts("okx", inst).unwrap_or_else(|e| panic!("{inst}: {e}"));
        assert_eq!(got.properties, want, "{inst}");
        assert_eq!(got.contract_value, None, "{inst}: a contract value would scale notional twice");
    }
    let eth = facts.facts("okx", "ETH-USDT-SWAP").expect("eth").properties;
    assert_eq!(eth.tick_size, 0.01, "a price grid is not scaled");
    assert!((eth.step_size - 0.001).abs() < 1e-15, "0.01 contracts x ctVal 0.1: {eth:?}");
    assert!((eth.min_qty - 0.001).abs() < 1e-15 && (eth.max_qty - 1500.0).abs() < 1e-9, "{eth:?}");
    assert_eq!(eth.contract_size, 0.0, "already in base units: no second multiplier");
    let doge = facts.facts("okx", "doge-usdt-swap").expect("case-insensitive").properties;
    assert_eq!(doge.step_size, 1000.0, "1 contract of 1000 DOGE");
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
    for inst in ["BTC-USDT-SWAP", "BTC-USDT-261225"] {
        let got = facts.facts("okx", inst).expect(inst);
        assert_eq!((got.margin_mode, got.margin_rate), (None, None), "{inst}");
    }
}

/// A dated USDT-linear future is admitted on its base-unit grid, but only until its last day: the engine
/// books no delivery settlement. The fence restates the bridge's own figure.
#[test]
fn okx_refuses_a_future_inside_its_last_day_and_the_two_fences_agree() {
    let facts = okx_facts();
    let why = facts.facts("okx", "ETH-USDT-261226").expect_err("delivered in three hours");
    assert!(why.contains("delivered in 2 h") || why.contains("delivered in 3 h"), "{why}");
    assert!(why.contains("next expiry"), "{why}");
    assert_eq!(super::OKX_EXPIRY_GUARD_MS, vike_okx::inst_class::EXPIRY_GUARD_MS);
}

/// Only a linear USDT-settled SWAP or future and a USDT-quoted spot pair are admitted, and each refusal is
/// in the venue's own word.
#[test]
fn okx_refuses_what_is_not_a_linear_usdt_contract_or_a_usdt_pair_naming_why() {
    let facts = okx_facts();
    let why = |symbol: &str| facts.facts("okx", symbol).expect_err(symbol);
    for inverse in ["BTC-USD-SWAP", "BTC-USD-261225"] {
        let w = why(inverse);
        assert!(w.contains("inverse") && w.contains("not a linear perpetual or future"), "{w}");
        assert!(w.contains("USDT balance did not move"), "the measurement is in the refusal: {w}");
    }
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
    let no_ct = why("NOCT-USDT-SWAP");
    assert!(no_ct.contains("no contract size"), "{no_ct}");
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
    let rows = vike_okx::catalog::parse_insts(&okx_payload());
    let (list, list_calls) = counting(rows.clone());
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let lister: super::SymbolLister = Arc::new(move |venue, symbol| {
        seen.lock().unwrap().push(format!("{venue}/{symbol}"));
        Ok(rows.iter().filter(|r| r.raw_symbol == symbol).cloned().collect())
    });
    let facts = SymbolFacts::new(list).with_symbol_lister(lister);
    facts.facts("okx", "btc-usdt").expect("spot");
    facts.facts("okx", "BTC-USDT-261225").expect("future");
    assert_eq!(*asked.lock().unwrap(), ["okx/BTC-USDT", "okx/BTC-USDT-261225"]);
    assert_eq!(list_calls.load(Ordering::SeqCst), 0, "no bulk list for them");
    facts.facts("okx", "ETH-USDT-SWAP").expect("swap");
    assert_eq!(list_calls.load(Ordering::SeqCst), 1, "a SWAP is the bulk list's");
    assert_eq!(asked.lock().unwrap().len(), 2, "and no one-symbol read");
    // an id the venue does not list answers an empty list: the usual named refusal
    let why = facts.facts("okx", "NOPE-USDT").expect_err("unknown spot pair");
    assert!(why.contains("no instrument NOPE-USDT"), "{why}");
}

// ---------------------------------------------------------------------------------------------
// hyperliquid: the coin name is the key, exactly as the venue lists it
// ---------------------------------------------------------------------------------------------

/// A core perp row as the hyperliquid catalog builds it: the bare coin, linear, USDC-settled.
fn hl_perp(coin: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "hyperliquid",
        coin,
        coin,
        "USDC",
        Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_ccy: None,
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
        hl_perp("test:ABC", grid(0.01, 0.01)).with_wire(vike_model::WireIds {
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
fn hyperliquid_answers_a_core_perp_by_its_exact_coin_name() {
    let (list, _) = counting(hl_rows());
    let facts = SymbolFacts::new(list);
    let btc = facts.facts("hyperliquid", "BTC").expect("BTC");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (0.1, 0.00001));
    assert_eq!(btc.contract_value, None, "a linear perpetual sized in the coin: Linear, 1.0");
    // `kPEPE` is a different coin from `KPEPE`: the key is the catalog's `raw_symbol`, NOT upper-cased.
    let pepe = facts.facts("hyperliquid", "kPEPE").expect("kPEPE");
    assert_eq!((pepe.properties.tick_size, pepe.properties.step_size), (0.000001, 1.0));
    for wrong_case in ["KPEPE", "btc", "Btc"] {
        let err = facts.facts("hyperliquid", wrong_case).expect_err(wrong_case);
        assert!(
            err.starts_with(&format!(
                "hyperliquid: no instrument {wrong_case} in the venue's instrument list"
            )),
            "{err}"
        );
    }
}

#[test]
fn the_other_venues_still_upper_case_and_hyperliquid_does_not_leak_into_them() {
    let (list, _) = counting(binance_rows());
    let facts = SymbolFacts::new(list);
    assert!(facts.facts("binance", "solusdt").is_ok(), "binance keeps upper-casing");
    assert!(facts.facts("binance", "solusdt.p").is_ok());
    let (list, _) = counting(vec![hl_perp("kPEPE", grid(0.1, 1.0))]);
    let err = SymbolFacts::new(list).facts("bybit", "kPEPE").expect_err("bybit looks up KPEPE.P");
    assert!(err.starts_with("bybit: no instrument KPEPE in the venue's"), "{err}");
}

/// **A USDC spot pair is answered `Cash` at a margin rate of 1** (decision 0218): fully funded, never liquidated,
/// sized on the pair's own grid; a pair quoted in another token is refused by name, and the venue's wire name `@N`
/// is refused before any listing is read, naming the spelling to use.
#[test]
fn hyperliquid_answers_a_usdc_spot_pair_cash_and_refuses_the_other_quotes() {
    use vike_model::MarginMode;
    let (list, calls) = counting(hl_rows());
    let facts = SymbolFacts::new(list);
    let hype = facts.facts("hyperliquid", "HYPE/USDC").expect("HYPE/USDC");
    assert_eq!(hype.margin_mode, Some(MarginMode::Cash));
    assert_eq!(hype.margin_rate, Some(1.0), "a spot position margins at its whole notional");
    assert_eq!((hype.properties.tick_size, hype.properties.step_size), (0.001, 0.01));
    assert_eq!(hype.contract_value, None, "a spot size is the base token: Linear, 1.0");
    let usdh = facts.facts("hyperliquid", "ABC/USDH").expect_err("quoted in USDH");
    assert!(usdh.contains("USDH") && usdh.contains("USDC") && usdh.contains("ABC/USDH"), "{usdh}");
    let case = facts.facts("hyperliquid", "hype/usdc").expect_err("the pair keeps its case");
    assert!(case.starts_with("hyperliquid: no instrument hype/usdc in the venue's"), "{case}");

    let before = calls.load(Ordering::SeqCst);
    let wire = facts.facts("hyperliquid", "@107").expect_err("the wire name");
    assert!(wire.starts_with(NOT_ANSWERED) && wire.contains("@107"), "{wire}");
    assert!(wire.contains("BASE/QUOTE"), "it names the spelling to use: {wire}");
    assert_eq!(calls.load(Ordering::SeqCst), before, "and asks the venue nothing");
}

/// **An isolated-only perpetual is answered `Isolated`** (decision 0218) where it used to be refused; a
/// delisted one is still refused, a plain one states no mode, and a missing tick is still named.
#[test]
fn hyperliquid_answers_an_isolated_only_perp_isolated_and_still_refuses_a_delisted_one() {
    use vike_model::MarginMode;
    let (list, _) = counting(hl_rows());
    let facts = SymbolFacts::new(list);
    let iso = facts.facts("hyperliquid", "RLB").expect("isolated-only is answered");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    assert_eq!(iso.margin_rate, None, "the engine's own margin rate stands");
    assert_eq!(iso.properties.tick_size, 0.0001);
    let old = facts.facts("hyperliquid", "OLD").expect_err("delisted");
    assert!(old.contains("delisted") && old.contains("OLD"), "{old}");
    let btc = facts.facts("hyperliquid", "BTC").expect("a plain perp");
    assert_eq!((btc.margin_mode, btc.margin_rate), (None, None), "cross states nothing");
    let tick = facts.facts("hyperliquid", "NOTICK").expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
    // The belt: a row of a kind the catalog may one day list is not answered.
    let (list, _) = counting(vec![
        Instrument::new("hyperliquid", "WEIRD", "WEIRD", "USDC", Contract::Index)
            .with_properties(grid(0.1, 1.0)),
    ]);
    let err = SymbolFacts::new(list).facts("hyperliquid", "WEIRD").expect_err("not a perp or spot");
    assert!(err.starts_with(NOT_ANSWERED), "{err}");
}

/// **A builder-dex coin is answered from its listing, or ONE dex at a time** (decision 0218): a plain list that
/// carries the `dex:COIN` row (the HIP-3 flag's) answers it with its own mode; with a per-symbol source set it is
/// read with that dex alone, on the asking account's network, and the whole list is never fetched.
#[test]
fn hyperliquid_answers_a_builder_dex_coin_alone_on_the_accounts_network() {
    use std::sync::Mutex;
    use vike_model::MarginMode;
    let dex_coin = |name: &str, word: Option<&str>| {
        let row = hl_perp(name, grid(0.01, 0.01)).with_wire(vike_model::WireIds {
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
    let abc = SymbolFacts::new(list).facts("hyperliquid", "test:ABC").expect("test:ABC");
    assert_eq!(abc.margin_mode, None, "a cross-capable dex coin states no mode");

    // From one dex alone: the lister is told the network by the listing key.
    let (list, whole_calls) = counting(hl_rows());
    let asked = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let seen = Arc::clone(&asked);
    let facts = SymbolFacts::new(list).with_symbol_lister(Arc::new(move |venue, symbol| {
        seen.lock().unwrap().push((venue.to_string(), symbol.to_string()));
        Ok(vec![dex_coin("test:ABC", None), dex_coin("test:ISO", Some("onlyIsolated"))])
    }));
    let abc = facts.facts_on("hyperliquid", "testnet", "test:ABC").expect("test:ABC");
    assert_eq!(abc.margin_mode, None);
    let iso = facts.facts_on("hyperliquid", "mainnet", "test:ISO").expect("test:ISO");
    assert_eq!(iso.margin_mode, Some(MarginMode::Isolated));
    let gone = facts.facts_on("hyperliquid", "mainnet", "test:GONE").expect_err("not on the dex");
    assert!(gone.starts_with("hyperliquid: no instrument test:GONE in the venue's"), "{gone}");
    assert_eq!(
        *asked.lock().unwrap(),
        [
            (super::HYPERLIQUID_TESTNET_LIST.to_string(), "test:ABC".to_string()),
            ("hyperliquid".to_string(), "test:ISO".to_string()),
            ("hyperliquid".to_string(), "test:GONE".to_string()),
        ],
        "one read per coin, on the account's own network"
    );
    assert_eq!(whole_calls.load(Ordering::SeqCst), 0, "the universe list was never fetched");
    // A core coin still comes from the list, never the per-symbol source.
    assert!(facts.facts("hyperliquid", "BTC").is_ok());
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
            k if k == super::HYPERLIQUID_TESTNET_LIST => Ok(testnet.clone()),
            other => Err(format!("{other}: not scripted")),
        }
    });
    let facts = SymbolFacts::new(list);

    // A testnet account: XRP is not on its network, BTC is, on the testnet's grid.
    let why =
        facts.facts_on("hyperliquid", "testnet", "XRP").expect_err("XRP is not on the testnet");
    assert!(
        why.starts_with("hyperliquid: no instrument XRP in the venue's instrument list")
            && why.contains("testnet"),
        "{why}"
    );
    let btc = facts.facts_on("hyperliquid", "testnet", "BTC").expect("BTC is on both");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (1.0, 0.001));

    // A mainnet account (and the plain `facts`) keep the mainnet universe.
    for answer in [
        facts.facts_on("hyperliquid", "mainnet", "XRP"),
        facts.facts("hyperliquid", "XRP"),
        facts.facts_on("hyperliquid", "mainnet", "LINK"),
    ] {
        assert!(answer.is_ok(), "{answer:?}");
    }
    let btc = facts.facts_on("hyperliquid", "mainnet", "BTC").expect("BTC");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (0.1, 0.00001));

    // One fetch per network, however many asks (the listings are kept apart).
    let mut keys = asked.lock().expect("asked").clone();
    keys.sort();
    assert_eq!(keys, ["hyperliquid", super::HYPERLIQUID_TESTNET_LIST]);

    // The network word changes nothing for any other venue (binance's demo list is its own: below).
    let (list, calls) = counting(binance_rows());
    let other = SymbolFacts::new(list);
    assert!(other.facts_on("aster", "testnet", "SOLUSDT.P").is_ok());
    assert!(other.facts_on("aster", "mainnet", "SOLUSDT.P").is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one aster listing for both words");
}

/// A failed testnet read is the testnet's own failure: it is worded with the testnet key and does not poison
/// the mainnet slot.
#[test]
fn a_failed_testnet_listing_does_not_poison_the_mainnet_one() {
    let mainnet = vec![hl_perp("BTC", grid(0.1, 0.00001))];
    let list: InstrumentList = Arc::new(move |key| {
        if key == super::HYPERLIQUID_TESTNET_LIST {
            Err("timed out".to_string())
        } else {
            Ok(mainnet.clone())
        }
    });
    let facts = SymbolFacts::new(list);
    let why = facts.facts_on("hyperliquid", "testnet", "BTC").expect_err("the testnet read failed");
    assert!(why.starts_with(super::HYPERLIQUID_TESTNET_LIST), "{why}");
    assert!(facts_refusal_is_transient(&why), "a failed read is not a verdict on the coin: {why}");
    assert!(facts.facts_on("hyperliquid", "mainnet", "BTC").is_ok());
}

/// The two venue words are written by the hyperliquid catalog and read here, and a word one side
/// changes would turn the refusal off silently: held equal by name.
#[cfg(feature = "data")]
#[test]
fn the_venue_words_are_the_ones_the_hyperliquid_catalog_writes() {
    use vike_hyperliquid::feeds::catalog::{VENUE_WORD_DELISTED, VENUE_WORD_ISOLATED_ONLY};
    let (list, _) = counting(vec![
        hl_perp("RLB", grid(0.1, 1.0)).with_venue_word(VENUE_WORD_ISOLATED_ONLY),
        hl_perp("OLD", grid(0.1, 1.0)).with_venue_word(VENUE_WORD_DELISTED),
    ]);
    let facts = SymbolFacts::new(list);
    // An isolated-only coin is ANSWERED with its margin mode (decision 0218), a delisted one is still refused.
    let iso = facts.facts("hyperliquid", "RLB").expect("an isolated-only coin is admitted");
    assert_eq!(iso.margin_mode, Some(vike_model::MarginMode::Isolated));
    assert!(facts.facts("hyperliquid", "OLD").expect_err("old").contains("delisted"));
}

// ---- deribit: perpetuals of the TESTNET book, inverse and linear, nothing else -----------------

fn deribit_perp(name: &str, margining: Margining, settle: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "deribit",
        name,
        "BTC",
        "USD",
        Contract::Perp(PerpSpec {
            margining,
            settle: settle.into(),
            value_ccy: None,
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
        Contract::Future(vike_model::FutureSpec {
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: None,
            expiry_ms: 1_800_000_000_000,
            tenor: None,
            delivery: vike_model::Delivery::Unknown,
            family: vike_model::FutureFamily::Crypto,
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
    let facts = SymbolFacts::new(list);
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
        Contract::Option(vike_model::OptionSpec {
            right: vike_marketdata::OptionKind::Call,
            strike: 77_000.0,
            expiry_ms: 1_800_000_000_000,
            style: vike_model::ExerciseStyle::European,
            margining,
            settle: settle.into(),
            value_ccy: None,
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
    let facts = SymbolFacts::new(list);
    let future = facts.facts("deribit", "BTC-27MAR27").expect("dated inverse future");
    assert_eq!((future.properties.tick_size, future.properties.step_size), (0.5, 10.0));
    assert_eq!(future.contract_value, Some(vike_model::ContractValue::inverse(1.0)));
    assert_eq!(future.margin_rate, None, "a future keeps the engine's margin rate");
}

/// **A USDC option is read ONE at a time and answered at a margin rate of 1** (an added option is held long
/// only, so its margin is its premium); a coin-settled option and a spot pair are refused by name. An option
/// name keeps the venue's lower-case decimal `d`, and the whole futures list is never fetched for it.
#[test]
fn deribit_reads_a_usdc_option_alone_and_refuses_a_coin_option_and_a_spot_pair() {
    let (list, whole) = counting(deribit_rows());
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let facts = SymbolFacts::new(list).with_symbol_lister(Arc::new(move |venue, symbol| {
        seen.lock().unwrap().push(format!("{venue}/{symbol}"));
        Ok(match symbol {
            "BTC_USDC-6NOV26-77000-C" => {
                vec![deribit_option(symbol, Margining::Linear, "USDC", grid(5.0, 0.01))]
            }
            "XRP_USDC-6NOV26-2d5-C" => {
                vec![deribit_option(symbol, Margining::Linear, "USDC", grid(0.0005, 1000.0))]
            }
            "BTC-6NOV26-77000-C" => {
                vec![deribit_option(symbol, Margining::Inverse, "BTC", grid(0.0001, 0.1))]
            }
            _ => Vec::new(),
        })
    }));
    let usdc = facts.facts("deribit", "BTC_USDC-6NOV26-77000-C").expect("a USDC option");
    assert_eq!((usdc.properties.tick_size, usdc.properties.step_size), (5.0, 0.01));
    assert_eq!(usdc.contract_value, None, "linear is the default");
    assert_eq!(usdc.margin_rate, Some(1.0), "a long option's margin is its premium");
    facts
        .facts("deribit", "XRP_USDC-6NOV26-2d5-C")
        .expect("a decimal strike keeps its lower-case d");
    let coin = facts.facts("deribit", "BTC-6NOV26-77000-C").expect_err("a coin-settled option");
    assert!(coin.contains("coin-settled option") && coin.contains("BTC"), "{coin}");
    let gone = facts.facts("deribit", "ETH_USDC-6NOV26-1-C").expect_err("not listed");
    assert!(gone.contains("no instrument ETH_USDC-6NOV26-1-C"), "{gone}");
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "deribit/BTC_USDC-6NOV26-77000-C",
            "deribit/XRP_USDC-6NOV26-2d5-C",
            "deribit/BTC-6NOV26-77000-C",
            "deribit/ETH_USDC-6NOV26-1-C",
        ],
        "one read per option"
    );
    assert_eq!(whole.load(Ordering::SeqCst), 0, "the futures list was never fetched");
}

#[test]
fn deribit_refuses_a_spot_pair_a_quanto_row_and_a_missing_tick_by_name() {
    let (list, _) = counting(deribit_rows());
    let facts = SymbolFacts::new(list);
    let spot = facts.facts("deribit", "BTC_USDC").expect_err("spot pair");
    assert!(spot.contains("no position"), "{spot}");
    let quanto = facts.facts("deribit", "QUANTO-PERPETUAL").expect_err("quanto");
    assert!(quanto.contains("quanto"), "{quanto}");
    let tick = facts.facts("deribit", "NOTICK-PERPETUAL").expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
    let unlisted = facts.facts("deribit", "XYZ-PERPETUAL").expect_err("unlisted");
    assert!(unlisted.contains("no instrument XYZ-PERPETUAL"), "{unlisted}");
}

// --- alpaca: a list behind the account's own credentials ----------------------------------------

fn equity(symbol: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        "alpaca",
        symbol,
        symbol,
        "USD",
        Contract::Equity(vike_model::EquitySpec {
            exchange: Some("NASDAQ".into()),
            currency: Some("USD".into()),
        }),
    )
    .with_properties(p)
}

/// Two account keys (label and tier, as `BridgeTrade` composes them).
const ACCT_A: &str = "DEFAULT/demo";
const ACCT_B: &str = "second/demo";

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
    let facts = AccountFacts::new();
    let aapl = facts.facts("acct", "alpaca", "aapl", Arc::clone(&lister)).expect("equity");
    assert_eq!((aapl.properties.tick_size, aapl.properties.step_size), (0.01, 1.0));
    assert_eq!(aapl.contract_value, None, "a share is one unit: linear, multiplier 1");
    let btc = facts.facts("acct", "alpaca", "BTC/USD", Arc::clone(&lister)).expect("crypto pair");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (1.0, 0.000000001));
    assert_eq!(btc.properties.min_qty, 0.000016, "the row's own minimum travels");
    let unlisted =
        facts.facts("acct", "alpaca", "NOPE", Arc::clone(&lister)).expect_err("unlisted");
    assert!(unlisted.starts_with("alpaca: unknown asset NOPE"), "{unlisted}");
    assert!(!facts_refusal_is_transient(&unlisted), "an unknown asset is a verdict: {unlisted}");
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
    let facts = AccountFacts::new();
    let err = facts.facts("a/demo", "alpaca", "AAPL", bad).expect_err("a has no credentials");
    assert!(err.contains("no complete credentials"), "{err}");
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
    let tick = AccountFacts::new().facts("acct", "alpaca", "NOTICK", lister).expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
    let failing: AccountLister = Arc::new(|_| Err("HTTP 401".to_string()));
    let err =
        AccountFacts::new().facts("acct", "alpaca", "AAPL", failing).expect_err("listing failed");
    assert!(err.starts_with("alpaca: ") && err.contains("HTTP 401"), "{err}");
    assert!(
        facts_refusal_is_transient(&err),
        "a failed fetch is transient, never a verdict: {err}"
    );
}

/// The adapter writes a price to the CENT, so a price grid that is not whole cents is refused by name
/// (a coin quoted at a millionth would otherwise rest at a different price than the engine judged), while
/// a grid of whole cents (a dime, a dollar) is admitted.
#[test]
fn alpaca_admits_only_a_price_grid_of_whole_cents() {
    let (lister, _) = account_lister(alpaca_rows());
    let facts = AccountFacts::new();
    let shib =
        facts.facts("acct", "alpaca", "SHIB/USD", Arc::clone(&lister)).expect_err("sub-cent grid");
    assert!(shib.contains("SHIB/USD") && shib.contains("to the cent only"), "{shib}");
    assert!(!facts_refusal_is_transient(&shib), "a verdict, not a retry: {shib}");
    for ok in ["ETH/USD", "BTC/USD", "AAPL"] {
        facts
            .facts("acct", "alpaca", ok, Arc::clone(&lister))
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

/// `AccountFacts` serves the venues `SymbolFacts` names and no others: a credentialed venue it was not
/// taught (ig) is the default answer, and its lister is never called.
#[test]
fn another_credentialed_venue_is_still_not_answered() {
    let (lister, calls) = account_lister(alpaca_rows());
    let err = AccountFacts::new().facts("acct", "ig", "EURUSD", lister).expect_err("not served");
    assert_eq!(err, NOT_ANSWERED);
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
        Contract::Fx(vike_model::FxSpec {
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
/// (`contract_value` None), on its own grid, whichever spelling the symbol arrives in, and one listing
/// (the instruments and the currency) serves every symbol asked meanwhile.
#[test]
fn oanda_answers_a_pair_quoted_in_the_accounts_currency_from_one_listing() {
    let (lister, calls) = oanda_lister("USD", oanda_rows());
    let facts = OandaFacts::default();
    for typed in ["GBPUSD", "gbpusd", "GBP_USD", "EURUSD"] {
        let got =
            facts.facts(typed, Arc::clone(&lister)).unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!((got.properties.tick_size, got.properties.step_size), (0.00001, 1.0), "{typed}");
        assert_eq!(got.contract_value, None, "{typed}: units x price in the quote IS the exposure");
    }
    let unlisted = facts.facts("NOPEUSD", lister).expect_err("unlisted");
    assert_eq!(
        unlisted,
        "oanda: no instrument NOPEUSD in the venue's instrument list (the instruments this \
         account's own OANDA list carries; a CFD, metal or index is listed only if OANDA offers it \
         to the account)",
        "named as the engine spelled it"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing for five asks");
}

/// **The currency fence**: USDJPY on a USD account is refused BY NAME (nothing converts a quote currency;
/// overstated PnL is the dangerous direction), a cross like EURGBP too, and the same cross is admitted on
/// a GBP account: the fence is the ACCOUNT's, one `OandaFacts` per account.
#[test]
fn oanda_refuses_an_instrument_not_quoted_in_the_accounts_currency() {
    let (usd, _) = oanda_lister("USD", oanda_rows());
    let usd_facts = OandaFacts::default();
    let jpy = usd_facts.facts("USDJPY", Arc::clone(&usd)).expect_err("quoted in JPY");
    assert!(
        jpy.contains("USD_JPY is quoted in JPY") && jpy.contains("account's currency is USD"),
        "{jpy}"
    );
    let cross = usd_facts.facts("EURGBP", Arc::clone(&usd)).expect_err("quoted in GBP");
    assert!(cross.contains("EUR_GBP is quoted in GBP"), "{cross}");
    assert!(!facts_refusal_is_transient(&cross), "a verdict, not a retry: {cross}");

    let (gbp, _) = oanda_lister("GBP", oanda_rows());
    let gbp_facts = OandaFacts::default();
    gbp_facts.facts("EURGBP", Arc::clone(&gbp)).expect("quoted in the account's own currency");
    let usd_pair = gbp_facts.facts("GBPUSD", gbp).expect_err("a USD quote on a GBP account");
    assert!(
        usd_pair.contains("quoted in USD") && usd_pair.contains("currency is GBP"),
        "{usd_pair}"
    );
}

/// A rate source over the account's home conversions as a table `(quote currency, account per quote)`: the
/// composition's closure in miniature (`BridgeTrade::oanda_facts_for`), so the contract the facts rely on is
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
            Some((c, r)) => {
                Ok(Some(QuotedRate { quote_ccy: c.clone(), account_per_quote: *r, as_of_ms }))
            }
            None => Err(format!("oanda: no rate for {quote} in {home}")),
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
    let facts = OandaFacts::default();

    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("a priced cross");
    assert_eq!(cross.quote_ccy.as_deref(), Some("GBP"));
    assert_eq!(cross.quote_rate, Some(1.35), "account units per ONE pound, never the inverse");
    assert_eq!(cross.rate_as_of_ms, Some(1_700_000_000_000));
    assert_eq!(cross.properties.tick_size, 0.00001, "the pair's own grid");
    assert_eq!(cross.contract_value, None, "the unit is 1: the engine multiplies the rate in");

    let own = facts.facts_quoted("GBPUSD", Arc::clone(&lister), &rate).expect("quoted in USD");
    assert_eq!((&own.quote_ccy, own.quote_rate, own.rate_as_of_ms), (&None, None, None));
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
    assert!(plain.contains("EUR_GBP is quoted in GBP"), "{plain}");
}

/// **No rate, no admission**: a currency the source holds no rate for, a pair that names no quote currency
/// and a source that fails each refuse the symbol, and the adapter's own fence still runs: USDJPY has a
/// three-decimal price grid the adapter cannot write, so it stays refused even WITH a rate.
#[test]
fn oanda_refuses_a_cross_it_cannot_price_and_keeps_the_adapters_own_fence() {
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let facts = OandaFacts::default();

    let (no_gbp, _) = rates(&[("JPY", 1.0 / 150.0)], 1);
    let why = facts.facts_quoted("EURGBP", Arc::clone(&lister), &no_gbp).expect_err("no GBP rate");
    assert!(why.contains("no rate for GBP in USD"), "{why}");

    let failing: RateSource =
        Arc::new(|_: &str, _: &str| Err("oanda: the venue is down".to_string()));
    let down = facts.facts_quoted("EURGBP", Arc::clone(&lister), &failing).expect_err("down");
    assert_eq!(down, "oanda: the venue is down");

    let (with_jpy, _) = rates(&[("JPY", 1.0 / 150.0)], 1);
    let jpy = facts.facts_quoted("USDJPY", Arc::clone(&lister), &with_jpy).expect_err("grid");
    assert!(jpy.contains("price grid of 0.001") && jpy.contains("five decimals"), "{jpy}");

    let unlisted = facts.facts_quoted("NOPEGBP", lister, &no_gbp).expect_err("unlisted");
    assert!(unlisted.contains("no instrument NOPEGBP"), "{unlisted}");
}

/// The adapter writes five decimals and whole units, so an instrument with another price precision or a
/// fractional unit step is refused by name (the mounted symbol is not touched: this is the foreign path).
#[test]
fn oanda_refuses_what_the_adapter_cannot_write_on_the_wire() {
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let facts = OandaFacts::default();
    for (symbol, word) in
        [("XAUUSD", "five decimals"), ("SPX500_USD", "five decimals"), ("FRACT_USD", "WHOLE units")]
    {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(why.contains(word) && why.contains("not admitted"), "{symbol}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict, not a retry: {why}");
    }
    // A row with no tick at all is the generic refusal, before either fence.
    let none = facts.facts("NOTICK_USD", lister).expect_err("no tick");
    assert!(none.contains("no tick size"), "{none}");
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
    let facts = OandaFacts::default();
    for _ in 0..3 {
        let err = facts.facts("GBPUSD", Arc::clone(&failing)).expect_err("listing failed");
        assert!(err.starts_with("oanda: ") && err.contains("401"), "{err}");
        assert!(facts_refusal_is_transient(&err), "{err}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the failure is remembered, not retried per ask");
}

/// **A credential swap under one label is a different account** (an operator pastes another OANDA account's
/// id and key under the same label): the kept listing and currency are the OLD account's, and answering from
/// them for up to ten minutes would judge a pair against a currency the account no longer holds. The cache is
/// keyed by the account-id fingerprint, so the swap gets a fresh `OandaFacts` that lists again, while the same
/// account (a rotated token, the same fingerprint) keeps its kept listing, and another label is its own entry.
#[test]
fn a_credential_swap_under_one_label_never_answers_from_the_old_accounts_listing() {
    let by_account = FactsByAccount::<OandaFacts>::default();
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
        swapped.facts("GBPUSD", Arc::clone(&gbp)).expect_err("USD-quoted on a GBP account");
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
    let facts = AccountFacts::new();
    let none = facts.facts(ACCT_A, "alpaca", "AAPL", down).expect_err("no credentials");
    assert!(none.contains("no complete credentials"), "{none}");
    // Inside FAILED_TTL: the same account is served its remembered refusal, another is not.
    let b = facts.facts(ACCT_B, "alpaca", "AAPL", Arc::clone(&funded)).expect("B has credentials");
    assert_eq!(b.properties.tick_size, 0.01);
    assert_eq!(funded_calls.load(Ordering::SeqCst), 1, "B was read with B's lister");
    let a_again = facts.facts(ACCT_A, "alpaca", "AAPL", funded).expect_err("A's refusal is kept");
    assert!(a_again.contains("no complete credentials"), "{a_again}");
    assert_eq!(funded_calls.load(Ordering::SeqCst), 1, "A was not read with B's lister");
    assert_eq!(down_calls.load(Ordering::SeqCst), 1, "A was not read again inside FAILED_TTL");
}

/// A fetch runs with the credentials of the account it is for: asks of two accounts interleave and each
/// asset is fetched by its own lister, kept for the TTL per account, whatever asked last.
#[test]
fn each_account_reads_with_its_own_lister_and_keeps_its_own_assets() {
    let (lister_a, calls_a) = account_lister(vec![equity("ONLYA", grid(0.01, 1.0))]);
    let (lister_b, calls_b) = account_lister(vec![equity("ONLYB", grid(0.01, 1.0))]);
    let facts = AccountFacts::new();
    facts.facts(ACCT_A, "alpaca", "ONLYA", Arc::clone(&lister_a)).expect("A reads A's asset");
    facts.facts(ACCT_B, "alpaca", "ONLYB", Arc::clone(&lister_b)).expect("B reads B's asset");
    // A asks again after B asked last: still A's kept asset, no second request.
    facts.facts(ACCT_A, "alpaca", "ONLYA", Arc::clone(&lister_a)).expect("A's kept asset");
    let cross = facts.facts(ACCT_A, "alpaca", "ONLYB", lister_a).expect_err("not in A's account");
    assert!(cross.contains("unknown asset ONLYB"), "{cross}");
    assert_eq!((calls_a.load(Ordering::SeqCst), calls_b.load(Ordering::SeqCst)), (2, 1));
}

// --- ctrader: the account's own symbol list, fenced by its deposit currency ---------------------

/// A ctrader row: `symbol` quoted in `quote`, a forex pair (`fx`) or an index CFD, on a volume grid whose
/// minimum is `min`.
fn ctrader_row(symbol: &str, base: &str, quote: &str, fx: bool, min: f64) -> Instrument {
    use vike_model::{CfdUnderlying, FxSpec};
    if fx {
        let fx = Contract::Fx(FxSpec {
            pip_size: Some(0.0001),
            lot_size: None,
            swap_long: None,
            swap_short: None,
        });
        return ctrader_instrument(symbol, base, quote, fx, min);
    }
    ctrader_cfd_row(symbol, base, quote, CfdUnderlying::Index, min)
}

/// A ctrader CFD row of the broker's asset class `kind` (a measured class or not).
fn ctrader_cfd_row(
    symbol: &str,
    base: &str,
    quote: &str,
    kind: vike_model::CfdUnderlying,
    min: f64,
) -> Instrument {
    use vike_model::{CfdExpiry, CfdProduct, CfdSpec};
    let contract = Contract::Cfd(CfdSpec {
        underlying_kind: kind,
        underlying: None,
        product: CfdProduct::Cfd,
        expiry: CfdExpiry::Unknown,
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

fn ctrader_rows() -> Vec<Instrument> {
    vec![
        ctrader_row("EURUSD", "EUR", "USD", true, 1000.0),
        ctrader_row("GBPJPY", "GBP", "JPY", true, 1000.0),
        ctrader_row("NOMIN", "EUR", "USD", true, 0.0),
        ctrader_row("US500", "US500", "USD", false, 1.0),
        // A dated futures wrapper (the broker's `Futures Commodities` class): a class nobody measured.
        ctrader_cfd_row("Wheat_H6", "WHEAT", "USD", vike_model::CfdUnderlying::Other, 1.0),
    ]
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

const FENCE: super::RowFence = vike_ctrader::catalog::any_symbol_refusal;

/// A forex pair quoted in the account's deposit currency is answered from the account's OWN listing, on
/// its own grid, linear (`contract_value` None); the broker's names keep their case (`eurusd` is another,
/// unlisted name); and one listing serves every ask meanwhile.
#[test]
fn ctrader_answers_a_deposit_quoted_forex_pair_and_keeps_the_brokers_case() {
    let (lister, calls) = ctrader_lister("USD", ctrader_rows());
    let facts = CtraderFacts::default();
    let eur = facts.facts("EURUSD", Arc::clone(&lister), FENCE).expect("a USD-quoted pair");
    assert_eq!((eur.properties.tick_size, eur.properties.step_size), (0.00001, 1000.0));
    assert_eq!(eur.properties.min_qty, 1000.0, "the row's own minimum travels");
    assert_eq!(eur.contract_value, None, "forex in the deposit currency: linear, multiplier 1");
    let lower =
        facts.facts("eurusd", Arc::clone(&lister), FENCE).expect_err("case is part of the name");
    assert_eq!(
        lower,
        "ctrader: no instrument eurusd in the venue's instrument list (the account's own symbol list)"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing session for two asks");
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
    let facts = CtraderFacts::default();
    facts.facts("EURUSD", holding(Arc::clone(&served)), FENCE).expect("answered");
    assert_eq!(Arc::strong_count(&grant), 1, "the lister is dropped after a listing ask");
    facts.facts("EURUSD", holding(served), FENCE).expect("kept listing");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the second ask is served from the kept listing");
    assert_eq!(Arc::strong_count(&grant), 1, "and after an ask that read nothing");

    let failing: CurrencyLister = Arc::new(|| Err("ctrader symbols: auth refused".to_string()));
    CtraderFacts::default().facts("EURUSD", holding(failing), FENCE).expect_err("failed");
    assert_eq!(Arc::strong_count(&grant), 1, "and after a failed ask");
}

/// **A CFD of a MEASURED class is answered like a forex pair** (metals, indices, oil and commodities, crypto:
/// `vike_ctrader::catalog::MEASURED_CFD_CLASSES`): the account's own listing, the row's own grid and minimum,
/// linear with a multiplier of 1 (`contract_value` None) because one unit of volume is `volume / 100` of the
/// symbol's own unit (measured on the demo account, 2026-10-26). The same row on a different quote currency
/// is refused as a pair is.
#[test]
fn ctrader_answers_a_measured_cfd_class_like_a_forex_pair() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = CtraderFacts::default();
    let us500 = facts.facts("US500", Arc::clone(&lister), FENCE).expect("an index CFD in USD");
    assert_eq!(us500.properties.min_qty, 1.0, "the row's own minimum travels");
    assert_eq!(us500.contract_value, None, "measured: linear, multiplier 1");
    assert_eq!(us500.quote_ccy, None, "quoted in the deposit currency: nothing to convert");
    let (eur, _) = ctrader_lister("EUR", ctrader_rows());
    let why =
        CtraderFacts::default().facts("US500", eur, FENCE).expect_err("USD index, EUR account");
    assert!(why.contains("quoted in USD but this account's currency is EUR"), "{why}");
}

/// Every refusal names its cause, and none is transient (a verdict, not a retry): another quote currency,
/// a CFD of a class nobody measured (a mixed-case dated future), a missing minimum volume.
#[test]
fn ctrader_refuses_another_currency_an_unmeasured_cfd_and_a_missing_minimum_by_name() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = CtraderFacts::default();
    for (symbol, needle) in [
        ("GBPJPY", "quoted in JPY but this account's currency is USD"),
        ("Wheat_H6", "Wheat_H6 is a CFD of a class this adapter does not know"),
        ("NOMIN", "no minimum volume"),
    ] {
        let why = facts.facts(symbol, Arc::clone(&lister), FENCE).expect_err(symbol);
        assert!(why.contains(needle), "{symbol}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict: {why}");
    }
}

/// The fence follows the ACCOUNT's currency: on a EUR account the USD-quoted pair is refused, and the
/// EUR-quoted one is the admitted kind.
#[test]
fn ctrader_fences_on_the_accounts_own_deposit_currency() {
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let (lister, _) = ctrader_lister("EUR", rows);
    let facts = CtraderFacts::default();
    let why =
        facts.facts("EURUSD", Arc::clone(&lister), FENCE).expect_err("USD quote, EUR account");
    assert!(why.contains("quoted in USD but this account's currency is EUR"), "{why}");
    facts.facts("GBPEUR", lister, FENCE).expect("a EUR-quoted pair on a EUR account");
}

/// A failed listing is the lister's own words, transient (never a verdict about the instrument).
#[test]
fn ctrader_listing_failure_is_transient_and_a_public_list_never_answers_ctrader() {
    let failing: CurrencyLister = Arc::new(|| Err("ctrader symbols: auth refused".to_string()));
    let err = CtraderFacts::default().facts("EURUSD", failing, FENCE).expect_err("failed");
    assert!(err.starts_with("ctrader: ") && err.contains("auth refused"), "{err}");
    assert!(facts_refusal_is_transient(&err), "{err}");

    // A PUBLIC source (no account behind it) must not answer ctrader from whatever rows it was given:
    // the currency fence is the account's.
    let (list, calls) = counting(ctrader_rows());
    let public = SymbolFacts::new(list).facts("ctrader", "EURUSD").expect_err("not served");
    assert_eq!(public, NOT_ANSWERED);
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
    let facts = CtraderFacts::default();
    facts.facts("EURUSD", Arc::clone(&failing), FENCE).expect_err("down");
    facts.facts("EURUSD", Arc::clone(&failing), FENCE).expect_err("the remembered failure");
    assert_eq!(sessions.load(Ordering::SeqCst), 1, "one session for two asks");

    let forgetful = CtraderFacts::with_failed_ttl(Duration::ZERO);
    forgetful.facts("EURUSD", Arc::clone(&failing), FENCE).expect_err("down");
    forgetful.facts("EURUSD", failing, FENCE).expect_err("down again");
    assert_eq!(sessions.load(Ordering::SeqCst), 3, "a zero keep time dials again");
}

/// **A credential swap under one label is a different account**: the per-account map hands out the SAME
/// facts while the fingerprint is unchanged and FRESH ones (a new listing, a new currency) when it moves;
/// another label, and an account with no fingerprint, are their own entries.
#[test]
fn a_changed_credential_fingerprint_gets_fresh_facts() {
    let by_account = FactsByAccount::<CtraderFacts>::default();
    let (usd, usd_calls) = ctrader_lister("USD", ctrader_rows());
    let (eur, eur_calls) = ctrader_lister("EUR", ctrader_rows());
    let facts = by_account.of("ALT", Some(1));
    facts.facts("EURUSD", Arc::clone(&usd), FENCE).expect("a USD account");
    assert!(
        Arc::ptr_eq(&facts, &by_account.of("ALT", Some(1))),
        "the same account keeps its facts"
    );
    by_account.of("ALT", Some(1)).facts("EURUSD", Arc::clone(&usd), FENCE).expect("kept listing");
    assert_eq!(usd_calls.load(Ordering::SeqCst), 1, "one listing for the account");

    let swapped = by_account.of("ALT", Some(2));
    assert!(!Arc::ptr_eq(&facts, &swapped), "a changed fingerprint is a new account");
    let why = swapped.facts("EURUSD", eur, FENCE).expect_err("USD quote on a EUR account");
    assert!(why.contains("currency is EUR"), "{why}");
    assert_eq!(eur_calls.load(Ordering::SeqCst), 1, "the new account listed once");
    assert!(!Arc::ptr_eq(&by_account.of("OTHER", Some(1)), &by_account.of("ALT", Some(1))));
    assert!(Arc::ptr_eq(&by_account.of("", None), &by_account.of("", None)));
}

// --- ctrader, for a node that converts a quote currency (decision 0183, phase 1) ------------------------------

/// The converting fence: every refusal of [`FENCE`] but the foreign quote.
const QUOTED_FENCE: super::RowFence = vike_ctrader::catalog::quoted_symbol_refusal;

/// A rate source over `(quote currency, account per quote)`, the composition's closure in miniature
/// (`BridgeTrade::ctrader_facts_for`): it is called as `(quote currency, deposit currency)`, answers `Ok(None)`
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
            Some((c, r)) => {
                Ok(Some(QuotedRate { quote_ccy: c.clone(), account_per_quote: *r, as_of_ms }))
            }
            None => Err(format!("ctrader: no rate for {quote} in {deposit}")),
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
    let facts = CtraderFacts::default();

    let jpy = facts
        .facts_quoted("GBPJPY", Arc::clone(&lister), QUOTED_FENCE, &rate)
        .expect("a priced foreign pair");
    assert_eq!(jpy.quote_ccy.as_deref(), Some("JPY"));
    assert_eq!(jpy.quote_rate, Some(1.0 / 150.0), "account units per ONE yen, never the inverse");
    assert_eq!(jpy.rate_as_of_ms, Some(1_700_000_000_000));
    assert_eq!(jpy.properties.min_qty, 1000.0, "the row's own grid travels");
    assert_eq!(jpy.contract_value, None, "the unit is 1: the engine multiplies the rate in");
    assert_eq!(*last.lock().expect("last"), ("JPY".to_string(), "USD".to_string()));

    let own = facts
        .facts_quoted("EURUSD", Arc::clone(&lister), QUOTED_FENCE, &rate)
        .expect("quoted in the deposit currency");
    assert_eq!((&own.quote_ccy, own.quote_rate, own.rate_as_of_ms), (&None, None, None));
    assert_eq!(asked.load(Ordering::SeqCst), 2, "the source is asked for every ask");
    facts.facts_quoted("GBPJPY", Arc::clone(&lister), QUOTED_FENCE, &rate).expect("again");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "a rate is never served from the listing's keep time"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing session for all three asks");

    let plain = facts.facts("GBPJPY", lister, FENCE).expect_err("the interim fence stands");
    assert!(plain.contains("quoted in JPY but this account's currency is USD"), "{plain}");
}

/// The rate is judged against the ACCOUNT's deposit currency: on a EUR account the USD-quoted EURUSD is the
/// foreign one (rate asked as `(USD, EUR)`) and a EUR-quoted pair needs none.
#[test]
fn ctrader_converts_against_the_accounts_own_deposit_currency() {
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let (lister, _) = ctrader_lister("EUR", rows);
    let (rate, asked, last) = ctrader_rates(&[("USD", 0.92)], 1);
    let facts = CtraderFacts::default();
    let usd = facts
        .facts_quoted("EURUSD", Arc::clone(&lister), QUOTED_FENCE, &rate)
        .expect("a USD quote on a EUR account");
    assert_eq!((usd.quote_ccy.as_deref(), usd.quote_rate), (Some("USD"), Some(0.92)));
    assert_eq!(*last.lock().expect("last"), ("USD".to_string(), "EUR".to_string()));
    let own = facts.facts_quoted("GBPEUR", lister, QUOTED_FENCE, &rate).expect("EUR quote");
    assert_eq!(own.quote_ccy, None);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
}

/// **No rate, no admission**, and the fence's other clauses stand: a currency the source holds no rate for and
/// a source that fails each refuse the pair in the source's own words; a CFD of an unmeasured class and a missing
/// minimum volume are refused by name before the source is asked at all; an unlisted name is refused (case is
/// part of it).
#[test]
fn ctrader_refuses_a_pair_it_cannot_price_and_keeps_the_other_clauses_of_the_fence() {
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let facts = CtraderFacts::default();

    let (no_jpy, asked, _) = ctrader_rates(&[("GBP", 1.35)], 1);
    let why = facts
        .facts_quoted("GBPJPY", Arc::clone(&lister), QUOTED_FENCE, &no_jpy)
        .expect_err("no JPY rate");
    assert!(why.contains("no rate for JPY in USD"), "{why}");
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    let failing: RateSource =
        Arc::new(|_: &str, _: &str| Err("ctrader: the venue is down".to_string()));
    let down = facts
        .facts_quoted("GBPJPY", Arc::clone(&lister), QUOTED_FENCE, &failing)
        .expect_err("down");
    assert_eq!(down, "ctrader: the venue is down");

    for (symbol, needle) in [("Wheat_H6", "Wheat_H6 is a CFD"), ("NOMIN", "no minimum volume")] {
        let why = facts
            .facts_quoted(symbol, Arc::clone(&lister), QUOTED_FENCE, &no_jpy)
            .expect_err(symbol);
        assert!(why.contains(needle), "{symbol}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict: {why}");
    }
    assert_eq!(asked.load(Ordering::SeqCst), 1, "a refused row never reaches the rate source");

    let lower = facts.facts_quoted("gbpjpy", lister, QUOTED_FENCE, &no_jpy).expect_err("case");
    assert!(lower.contains("no instrument gbpjpy"), "{lower}");
}

/// **A deposit currency the broker's asset list does not name no longer refuses the WHOLE listing** (owner ruling
/// 2026-10-19: "the account currency is what the venue API states; if the API does not state it, or the read
/// fails: USD"): the lenient listing hands `None`, the account's currency is the ruling's USD, ASSUMED and said at
/// `warn` once, and the account LISTS (a USD-quoted pair is answered, which the strict listing refused with
/// everything else) and PRICES a cross (EURGBP: quoted in sterling, rate asked as `(GBP, USD)`). A broker that
/// DOES name its deposit (here EUR) is answered exactly as before: the golden, no assumption, no line.
#[test]
fn ctrader_with_an_unnamed_deposit_lists_and_prices_a_cross_on_an_assumed_usd_ledger() {
    use super::super::assumed_ccy::AssumedSaid;
    use super::super::ctrader_rate::listing_or_assumed;

    let said = Arc::new(AssumedSaid::default());
    let mut rows = ctrader_rows();
    rows.push(ctrader_row("EURGBP", "EUR", "GBP", true, 1000.0));
    rows.push(ctrader_row("GBPEUR", "GBP", "EUR", true, 1000.0));
    let lister = |deposit: Option<&'static str>, account: &'static str| -> CurrencyLister {
        let (said, rows) = (Arc::clone(&said), rows.clone());
        Arc::new(move || {
            listing_or_assumed(&said, account, || Ok((deposit.map(str::to_string), rows.clone())))
        })
    };

    let unnamed = lister(None, "UNNAMED");
    let (rate, _, last) = ctrader_rates(&[("GBP", 1.35)], 7);
    let facts = CtraderFacts::default();
    let cross = facts
        .facts_quoted("EURGBP", Arc::clone(&unnamed), QUOTED_FENCE, &rate)
        .expect("listed and priced on the assumed USD ledger");
    assert_eq!((cross.quote_ccy.as_deref(), cross.quote_rate), (Some("GBP"), Some(1.35)));
    assert_eq!(*last.lock().expect("last"), ("GBP".to_string(), "USD".to_string()));
    facts
        .facts_quoted("EURUSD", Arc::clone(&unnamed), QUOTED_FENCE, &rate)
        .expect("a USD-quoted pair is listed too");
    // ... and said at `warn`, once for the account, with the broker's own reason (read on THIS thread: the
    // lookups above may run the lister anywhere, and the account's line is said once, so a second account
    // is the one observed)
    let (listing, events) = vike_log::capture::captured(|| {
        listing_or_assumed(&said, "UNNAMED2", || Ok((None, ctrader_rows())))
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
    let facts = CtraderFacts::default();
    let usd = facts
        .facts_quoted("EURUSD", Arc::clone(&named), QUOTED_FENCE, &rate)
        .expect("a USD quote on a named EUR account");
    assert_eq!((usd.quote_ccy.as_deref(), usd.quote_rate), (Some("USD"), Some(0.92)));
    assert_eq!(*last.lock().expect("last"), ("USD".to_string(), "EUR".to_string()));
    facts.facts_quoted("GBPEUR", named, QUOTED_FENCE, &rate).expect("EUR-quoted: no rate");

    // a listing that could not be read at all is still an error (nothing to price, in any currency)
    let failing = listing_or_assumed(&said, "FAILED", || Err("ctrader symbols: timed out".into()));
    assert_eq!(failing.expect_err("no session"), "ctrader symbols: timed out");
}

/// **A converted answer STATES the ledger its rate is in** (review 2, HIGH-1 (a)): oanda's facts stamp
/// `WireSymbolFacts::account_ccy` with the currency the listing (and so the rate) was read in, for a symbol quoted
/// in another currency AND for one quoted in the account's own, so the node, which holds the engine at the
/// currency it was started with, refuses an answer in another one. Nothing is stamped for the plain answer.
#[test]
fn oanda_stamps_the_ledger_currency_its_answer_is_in() {
    let (lister, _) = oanda_lister("EUR", oanda_rows());
    let (rate, _) = rates(&[("GBP", 0.85), ("USD", 0.92)], 1);
    let facts = OandaFacts::default();
    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("a priced cross");
    assert_eq!(cross.quote_ccy.as_deref(), Some("GBP"));
    assert_eq!(cross.account_ccy.as_deref(), Some("EUR"), "the rate is in the listing's EUR");
    let usd = facts.facts_quoted("GBPUSD", Arc::clone(&lister), &rate).expect("a priced USD pair");
    assert_eq!(usd.account_ccy.as_deref(), Some("EUR"));

    // a symbol quoted in the account's own money carries no rate, and still says which ledger it is in
    let (lister, _) = oanda_lister("USD", oanda_rows());
    let own = OandaFacts::default().facts_quoted("GBPUSD", lister, &rate).expect("quoted in USD");
    assert_eq!((&own.quote_ccy, own.quote_rate), (&None, None));
    assert_eq!(own.account_ccy.as_deref(), Some("USD"));

    // the plain answer (a node that converts nothing) is as it was
    let (lister, _) = oanda_lister("USD", oanda_rows());
    assert_eq!(OandaFacts::default().facts("GBPUSD", lister).expect("plain").account_ccy, None);
}

/// [`oanda_stamps_the_ledger_currency_its_answer_is_in`] for cTrader: the deposit currency of the listing the rate
/// was priced against.
#[test]
fn ctrader_stamps_the_ledger_currency_its_answer_is_in() {
    let (rate, _, _) = ctrader_rates(&[("JPY", 1.0 / 165.0)], 1);
    let (lister, _) = ctrader_lister("EUR", ctrader_rows());
    let facts = CtraderFacts::default();
    let jpy = facts
        .facts_quoted("GBPJPY", Arc::clone(&lister), QUOTED_FENCE, &rate)
        .expect("a priced yen pair on a EUR account");
    assert_eq!(jpy.account_ccy.as_deref(), Some("EUR"));
    let (lister, _) = ctrader_lister("USD", ctrader_rows());
    let own = CtraderFacts::default()
        .facts_quoted("EURUSD", lister, QUOTED_FENCE, &rate)
        .expect("quoted in the deposit currency");
    assert_eq!((&own.quote_ccy, own.quote_rate), (&None, None));
    assert_eq!(own.account_ccy.as_deref(), Some("USD"));
}

/// **A transient failure at the start on a EUR account, through the gateway's own pieces** (review 2, HIGH-1 (d)):
/// the start's `/summary` timed out, so the engine is told USD ASSUMED; the lister reads the summary a moment
/// later and gets EUR. The facts it answers STATE EUR (the node, which holds the engine at USD, refuses them
/// whether the symbol is quoted in another currency or in the account's own), and the balance stays withheld
/// because only a start moves what the balance gate reads.
#[test]
fn a_start_that_missed_the_summary_on_a_eur_account_is_never_priced_in_eur_under_a_usd_label() {
    use super::super::assumed_ccy::{AssumedSaid, CcyRead, lister_currency, start_currency};

    let said = Arc::new(AssumedSaid::default());
    assert_eq!(
        start_currency(&said, "oanda", "EURACC", CcyRead::Silent("timed out".into())),
        (Some("USD".to_string()), true)
    );
    let (inner, _) = oanda_lister("EUR", oanda_rows());
    let lister: CurrencyLister = {
        let said = Arc::clone(&said);
        Arc::new(move || {
            let ccy = lister_currency(&said, "oanda", "EURACC", true, Ok("EUR".to_string()))?;
            inner().map(|(_, rows)| (ccy, rows))
        })
    };
    let (rate, _) = rates(&[("GBP", 0.85)], 1);
    let facts = OandaFacts::default();
    let cross = facts.facts_quoted("EURGBP", Arc::clone(&lister), &rate).expect("priced in EUR");
    assert_eq!(cross.quote_rate, Some(0.85));
    assert_eq!(
        cross.account_ccy.as_deref(),
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
    use super::super::assumed_ccy::{AssumedSaid, CcyRead, start_currency};
    use super::super::ctrader_rate::listing_or_assumed;

    let said = Arc::new(AssumedSaid::default());
    assert_eq!(
        start_currency(&said, "ctrader", "CTEUR", CcyRead::Silent("handshake timed out".into())),
        (Some("USD".to_string()), true)
    );
    let rows = ctrader_rows();
    let lister: CurrencyLister = {
        let said = Arc::clone(&said);
        Arc::new(move || {
            listing_or_assumed(&said, "CTEUR", || Ok((Some("EUR".to_string()), rows.clone())))
        })
    };
    let (rate, _, last) = ctrader_rates(&[("JPY", 1.0 / 165.0)], 1);
    let wire = CtraderFacts::default()
        .facts_quoted("GBPJPY", lister, QUOTED_FENCE, &rate)
        .expect("priced in EUR");
    assert_eq!(
        wire.account_ccy.as_deref(),
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
        Contract::Future(vike_model::FutureSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: None,
            expiry_ms,
            tenor: Some(vike_model::Tenor::Quarter),
            delivery: vike_model::Delivery::Unknown,
            family: vike_model::FutureFamily::Crypto,
        }),
    )
    .with_properties(p)
    .with_venue_word("CURRENT_QUARTER")
}

/// A COIN-M (inverse) row as `BinanceCatalog`'s dapi parse spells it: `BTCUSD_PERP.P`, 100 USD per contract.
fn coin_m(symbol: &str, word: &'static str) -> Instrument {
    Instrument::new("binance", format!("{symbol}.P"), "BTC", "USD", perp_inverse())
        .with_venue_word(word)
        .with_properties(SymbolProperties { contract_size: 100.0, ..grid(0.1, 1.0) })
}

const DAY_MS: i64 = 86_400_000;

#[test]
fn a_tradfi_perpetual_and_a_dated_future_are_answered_with_their_own_grid() {
    let now = vike_model::now_ms();
    let rows = vec![
        // The typed row of a TRADIFI_PERPETUAL is a plain linear perpetual (decision 0230).
        perp("binance", "TSLAUSDT", Margining::Linear, grid(0.01, 0.01))
            .with_venue_word("TRADIFI_PERPETUAL"),
        binance_dated("BTCUSDT_261225", now + 60 * DAY_MS, grid(0.1, 0.001)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolFacts::new(list);
    let tsla = facts.facts("binance", "TSLAUSDT.P").expect("TradFi perpetual");
    assert_eq!((tsla.properties.tick_size, tsla.properties.step_size), (0.01, 0.01));
    assert_eq!(tsla.contract_value, None, "linear, one base unit per unit of quantity");
    let q = facts.facts("binance", "BTCUSDT_261225.P").expect("dated future");
    assert_eq!((q.properties.tick_size, q.properties.step_size), (0.1, 0.001));
    assert_eq!(q.contract_value, None);
    // The lane suffix is what makes it the USDS-M lane: the bare spelling is a spot lookup of another name.
    assert!(facts.facts("binance", "BTCUSDT_261225").is_err());
}

#[test]
fn a_dated_future_that_was_delivered_or_delivers_within_a_day_is_refused_by_name() {
    let now = vike_model::now_ms();
    let rows = vec![
        // The demo list kept a TRADING weekly four and a half months after its delivery date.
        binance_dated("BSBUSDT_260618", now - 135 * DAY_MS, grid(0.00001, 1.0)),
        binance_dated("ETHUSDT_261031", now + 6 * 3_600_000, grid(0.01, 0.001)),
        binance_dated("BTCUSDT_261225", now + 2 * DAY_MS, grid(0.1, 0.001)),
    ];
    let (list, _) = counting(rows);
    let facts = SymbolFacts::new(list);
    let past = facts.facts("binance", "BSBUSDT_260618.P").expect_err("delivered");
    assert!(past.contains("was delivered on") && past.contains("BSBUSDT_260618.P"), "{past}");
    assert!(!facts_refusal_is_transient(&past), "a verdict, not a retry");
    let soon = facts.facts("binance", "ETHUSDT_261031.P").expect_err("hours away");
    assert!(soon.contains("in under 24 hours") && soon.contains("not admitted"), "{soon}");
    assert!(facts.facts("binance", "BTCUSDT_261225.P").is_ok(), "two days left is admitted");
}

#[test]
fn a_coin_m_symbol_is_refused_by_name_and_a_typo_reads_no_second_list() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let list: InstrumentList = Arc::new(move |key| {
        seen.lock().expect("asked").push(key.to_string());
        if key == super::BINANCE_COIN_M_LIST {
            Ok(vec![coin_m("BTCUSD_PERP", "PERPETUAL"), coin_m("BTCUSD_261225", "CURRENT_QUARTER")])
        } else {
            Ok(binance_rows())
        }
    });
    let facts = SymbolFacts::new(list);
    let perp = facts.facts("binance", "BTCUSD_PERP.P").expect_err("COIN-M perpetual");
    assert!(
        perp.contains("COIN-M contract (PERPETUAL)") && perp.contains("100 US dollars"),
        "{perp}"
    );
    assert!(perp.contains("coin-margined") && perp.contains("-2019"), "{perp}");
    assert!(perp.contains("counted in BTC"), "{perp}");
    let dated = facts.facts("binance", "BTCUSD_261225.P").expect_err("COIN-M quarterly");
    assert!(dated.contains("COIN-M contract (CURRENT_QUARTER)"), "{dated}");
    // A name that is not COIN-M-shaped never reads the COIN-M list; one that is shaped but absent says "no instrument".
    let before = asked.lock().expect("asked").len();
    let typo = facts.facts("binance", "NOPEUSDT.P").expect_err("unknown");
    assert!(typo.contains("no instrument NOPEUSDT.P"), "{typo}");
    assert_eq!(asked.lock().expect("asked").len(), before, "no read for a USDT-shaped miss");
    let absent = facts.facts("binance", "FOOUSD_PERP.P").expect_err("shaped but absent");
    assert!(absent.contains("no instrument FOOUSD_PERP.P"), "{absent}");
    // The COIN-M list is a refusal's source only: it is never an answer, even for a row it holds.
    let keys = asked.lock().expect("asked").clone();
    assert!(keys.iter().all(|k| k == "binance" || k == super::BINANCE_COIN_M_LIST), "{keys:?}");
}

#[test]
fn an_unreadable_coin_m_list_falls_back_to_no_such_instrument() {
    let list: InstrumentList = Arc::new(move |key| {
        if key == super::BINANCE_COIN_M_LIST {
            Err("dapi down".to_string())
        } else {
            Ok(binance_rows())
        }
    });
    let err = SymbolFacts::new(list).facts("binance", "BTCUSD_PERP.P").expect_err("unknown");
    assert!(err.contains("no instrument BTCUSD_PERP.P"), "{err}");
}

#[test]
fn a_demo_account_is_answered_from_the_demo_networks_list() {
    // mainnet lists a TradFi perpetual the demo host does not.
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&asked);
    let list: InstrumentList = Arc::new(move |key| {
        seen.lock().expect("asked").push(key.to_string());
        if key == super::BINANCE_DEMO_LIST {
            Ok(vec![perp("binance", "XAUUSDT", Margining::Linear, grid(0.01, 0.001))])
        } else {
            Ok(vec![
                perp("binance", "XAUUSDT", Margining::Linear, grid(0.01, 0.001)),
                perp("binance", "NVDAUSDT", Margining::Linear, grid(0.01, 0.01)),
            ])
        }
    });
    let facts = SymbolFacts::new(list);
    assert!(facts.facts_on("binance", "mainnet", "NVDAUSDT.P").is_ok());
    let err = facts.facts_on("binance", "testnet", "NVDAUSDT.P").expect_err("not on the demo host");
    assert!(err.contains("no instrument NVDAUSDT.P") && err.contains("DEMO futures host"), "{err}");
    assert!(facts.facts_on("binance", "testnet", "XAUUSDT.P").is_ok());
    let mut keys = asked.lock().expect("asked").clone();
    keys.sort();
    keys.dedup();
    assert_eq!(keys, ["binance", super::BINANCE_DEMO_LIST]);
}
