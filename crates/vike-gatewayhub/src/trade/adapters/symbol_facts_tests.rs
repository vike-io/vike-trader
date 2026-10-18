//! `SymbolFacts` over an injected instrument list: the three venues' spellings, the refusals, and the
//! listing cache. No network: the list is a counting closure over fixture rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_model::{Contract, Instrument, Margining, PerpSpec, SymbolProperties};

use super::{AccountFacts, AccountLister, InstrumentList, NOT_ANSWERED, SymbolFacts};

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
    let err = facts.facts("bybit", "BTCUSD").expect_err("inverse refused");
    assert!(err.contains("InversePerpetual") && err.contains("linear perpetuals only"), "{err}");
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
    assert_eq!(
        err,
        "binance: no instrument NOPEUSDT.P in the venue's instrument list (spot and USDS-M \
         perpetuals)"
    );
    assert!(!facts_refusal_is_transient(&err), "an unknown symbol is a verdict, not a retry");
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
    let err = SymbolFacts::new(list).facts("oanda", "EUR_USD").expect_err("not served");
    assert_eq!(err, NOT_ANSWERED);
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
        "expTime": if inst_type == "FUTURES" { "1798156800000" } else { "" },
        "alias": "", "lever": "50", "tickSz": tick, "lotSz": lot, "minSz": min, "maxMktSz": max,
        "state": "live",
    })
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
        serde_json::json!({
            "instType": "SPOT", "instId": "BTC-USDT", "baseCcy": "BTC", "quoteCcy": "USDT",
            "tickSz": "0.1", "lotSz": "0.00000001", "minSz": "0.00001", "maxMktSz": "1000000",
            "state": "live",
        }),
    ]})
}

fn okx_facts() -> SymbolFacts {
    let rows = vike_okx::catalog::parse_insts(&okx_payload());
    assert_eq!(rows.len(), 9, "every fixture row parses into the catalog");
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
    for inst in ["BTC-USDT-SWAP", "ETH-USDT-SWAP", "DOGE-USDT-SWAP"] {
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

/// Only a linear USDT-settled SWAP is admitted, and each refusal is in the venue's own word.
#[test]
fn okx_refuses_what_is_not_a_linear_usdt_swap_naming_why() {
    let facts = okx_facts();
    let why = |symbol: &str| facts.facts("okx", symbol).expect_err(symbol);
    assert!(why("BTC-USD-SWAP").contains("inverse"), "{}", why("BTC-USD-SWAP"));
    assert!(why("BTC-USD-SWAP").contains("not a linear perpetual"));
    let spot = why("BTC-USDT");
    assert!(spot.contains("CryptoSpot") && spot.contains("not a SWAP perpetual"), "{spot}");
    let dated = why("BTC-USDT-261225");
    assert!(dated.contains("CryptoFuture") && dated.contains("not a SWAP perpetual"), "{dated}");
    let usdc = why("BTC-USDC-SWAP");
    assert!(usdc.contains("USDC") && usdc.contains("USDT-settled"), "{usdc}");
    let quote = why("QUOTE-USDT-SWAP");
    assert!(quote.contains("USDT") && quote.contains("not in the base asset"), "{quote}");
    let no_ct = why("NOCT-USDT-SWAP");
    assert!(no_ct.contains("no contract size"), "{no_ct}");
    let unknown = why("NOPE-USDT-SWAP");
    assert_eq!(
        unknown,
        "okx: no instrument NOPE-USDT-SWAP in the venue's instrument list (linear USDT-settled \
         SWAP perpetuals)"
    );
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
        spot("hyperliquid", "HYPE/USDC", grid(0.001, 0.01)),
    ]
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

#[test]
fn hyperliquid_spot_and_builder_dex_coins_are_not_answered_and_ask_nothing() {
    for symbol in ["HYPE/USDC", "@107", "test:ABC", "PURR/USDC"] {
        let (list, calls) = counting(hl_rows());
        let err = SymbolFacts::new(list).facts("hyperliquid", symbol).expect_err(symbol);
        assert!(err.starts_with(NOT_ANSWERED), "{symbol}: {err}");
        assert!(err.contains(symbol), "{symbol} is named: {err}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{symbol}: no listing was fetched");
    }
    // The row-side belt: a builder-dex or spot row that reached the listing under a plain name is
    // still refused (the HIP-3 flag puts `dex:COIN` rows in the same listing).
    let mut sneaky = hl_perp("SNEAK", grid(0.1, 1.0));
    sneaky.wire.group = Some("dex".into());
    let (list, _) = counting(vec![sneaky, spot("hyperliquid", "SPOTCOIN", grid(0.1, 1.0))]);
    let facts = SymbolFacts::new(list);
    for symbol in ["SNEAK", "SPOTCOIN"] {
        let err = facts.facts("hyperliquid", symbol).expect_err(symbol);
        assert!(err.starts_with(NOT_ANSWERED), "{symbol}: {err}");
    }
}

#[test]
fn hyperliquid_refuses_an_isolated_only_and_a_delisted_perp_naming_which() {
    let (list, _) = counting(hl_rows());
    let facts = SymbolFacts::new(list);
    let iso = facts.facts("hyperliquid", "RLB").expect_err("isolated-only");
    assert!(iso.contains("isolated") && iso.contains("RLB"), "{iso}");
    let old = facts.facts("hyperliquid", "OLD").expect_err("delisted");
    assert!(old.contains("delisted") && old.contains("OLD"), "{old}");
    // A plain row is untouched by either rule, and a missing tick is still named.
    assert!(facts.facts("hyperliquid", "BTC").is_ok());
    let tick = facts.facts("hyperliquid", "NOTICK").expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
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
    assert!(facts.facts("hyperliquid", "RLB").expect_err("iso").contains("isolated"));
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

#[test]
fn deribit_refuses_a_dated_future_a_spot_pair_a_quanto_row_and_a_missing_tick_by_name() {
    let (list, _) = counting(deribit_rows());
    let facts = SymbolFacts::new(list);
    let future = facts.facts("deribit", "BTC-27MAR27").expect_err("dated future");
    assert!(future.contains("not a perpetual") && future.contains("BTC-27MAR27"), "{future}");
    let spot = facts.facts("deribit", "BTC_USDC").expect_err("spot pair");
    assert!(spot.contains("not a perpetual"), "{spot}");
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

/// A lister that serves `rows` and counts how often the venue was asked.
fn account_lister(rows: Vec<Instrument>) -> (AccountLister, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let lister: AccountLister = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(rows.clone())
    });
    (lister, calls)
}

/// alpaca answers an equity and a slashed crypto pair from its OWN rows, linear (`contract_value` None)
/// and on each instrument's own grid, and one listing request serves every symbol asked meanwhile.
#[test]
fn alpaca_answers_equities_and_crypto_pairs_from_one_credentialed_listing() {
    let (lister, calls) = account_lister(alpaca_rows());
    let facts = AccountFacts::new();
    let aapl = facts.facts("alpaca", "aapl", Arc::clone(&lister)).expect("equity");
    assert_eq!((aapl.properties.tick_size, aapl.properties.step_size), (0.01, 1.0));
    assert_eq!(aapl.contract_value, None, "a share is one unit: linear, multiplier 1");
    let btc = facts.facts("alpaca", "BTC/USD", Arc::clone(&lister)).expect("crypto pair");
    assert_eq!((btc.properties.tick_size, btc.properties.step_size), (1.0, 0.000000001));
    assert_eq!(btc.properties.min_qty, 0.000016, "the row's own minimum travels");
    let unlisted = facts.facts("alpaca", "NOPE", lister).expect_err("unlisted");
    assert_eq!(
        unlisted,
        "alpaca: no instrument NOPE in the venue's instrument list (US equities and crypto pairs)"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one listing request for three asks");
}

/// A row with no tick is refused naming it, and an ask with no account to list through (or a venue that
/// cannot be listed) is the lister's own words, never a guess.
#[test]
fn alpaca_refuses_a_missing_tick_and_a_failed_listing_in_words() {
    let (lister, _) = account_lister(alpaca_rows());
    let tick = AccountFacts::new().facts("alpaca", "NOTICK", lister).expect_err("no tick");
    assert!(tick.contains("no tick size"), "{tick}");
    let failing: AccountLister = Arc::new(|| Err("HTTP 401".to_string()));
    let err = AccountFacts::new().facts("alpaca", "AAPL", failing).expect_err("listing failed");
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
    let shib = facts.facts("alpaca", "SHIB/USD", Arc::clone(&lister)).expect_err("sub-cent grid");
    assert!(shib.contains("SHIB/USD") && shib.contains("to the cent only"), "{shib}");
    assert!(!facts_refusal_is_transient(&shib), "a verdict, not a retry: {shib}");
    for ok in ["ETH/USD", "BTC/USD", "AAPL"] {
        facts.facts("alpaca", ok, Arc::clone(&lister)).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

/// `AccountFacts` serves the venues `SymbolFacts` names and no others: a credentialed venue it was not
/// taught (oanda) is the default answer, and its lister is never called.
#[test]
fn another_credentialed_venue_is_still_not_answered() {
    let (lister, calls) = account_lister(alpaca_rows());
    let err = AccountFacts::new().facts("oanda", "EUR_USD", lister).expect_err("not served");
    assert_eq!(err, NOT_ANSWERED);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
