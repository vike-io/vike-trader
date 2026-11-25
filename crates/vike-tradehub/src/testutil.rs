//! The fixtures several of this lib's `#[cfg(test)]` suites share, written once (`lib.rs` declares this
//! module under `#[cfg(test)]`, so it compiles into the lib's own unit-test binary and nowhere else; the
//! integration groups under `tests/` keep their helpers beside them).

use vike_execution::SymbolAdmission;
use vike_model::SymbolGrid;
use vike_secrets::account_directory::AccountDirectory;

/// A plain admission of `symbol` under `route_key`: a 0.01 tick and a 0.001 lot, no contract value, no
/// mark, no foreign quote and no release.
pub(crate) fn admission(route_key: &str, symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: route_key.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// **The account rows of a box whose every roster venue's DEFAULT account is an active `demo` row stating its fixture
/// symbol** (`vike_secrets::fixture_account_symbol`: the symbol each venue was mounted on before the 2026-11-13 ruling;
/// polymarket's states none, its engine being account-wide).
static FIXTURE_ACCOUNTS: std::sync::LazyLock<AccountDirectory> = std::sync::LazyLock::new(|| {
    let rows = vike_model::VENUES
        .iter()
        .zip(1..)
        .map(|(venue, id)| {
            vike_secrets::account_row(id, venue, vike_model::AccountTier::Demo, None, true)
        })
        .collect();
    AccountDirectory::from_rows(vike_secrets::AccountRows::Known(rows), None)
});

/// A kept-list reader whose venues each list exactly their fixture symbol.
fn fixture_kept(venue: &str, symbol: &str) -> Result<(), String> {
    if vike_secrets::fixture_account_symbol(venue) == Some(symbol) {
        Ok(())
    } else {
        Err(format!("it is not on {venue}'s kept instrument list"))
    }
}

/// **The live box the profile and feed suites judge a profile against**: [`FIXTURE_ACCOUNTS`] and [`fixture_kept`].
pub(crate) fn live_box() -> crate::config::LiveBox<'static> {
    crate::config::LiveBox { accounts: &FIXTURE_ACCOUNTS, kept: &fixture_kept }
}

/// The rows of a node whose binance default account (id 3, demo) states `leverage` or not, built the way a
/// root builds them (`AccountDirectory::read`): while it states one the table DERIVES the twin
/// `binance#PERP` and MARKS it, on the symbol the parent states for it (`lane_twin_symbol`, `BTCUSDT.P`).
pub(crate) fn account_three(leverage: Option<f64>) -> AccountDirectory {
    let parent = vike_secrets::AccountRow {
        id: 3,
        venue: "binance".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: leverage.and_then(vike_secrets::AccountLeverage::new),
        symbol: Some("BTCUSDT".to_string()),
        facts: [("lane_twin_symbol".to_string(), "BTCUSDT.P".to_string())].into(),
    };
    AccountDirectory::read(
        Ok::<_, String>(vike_secrets::AccountRows::Known(vec![parent])),
        Ok::<_, String>(None),
        Ok::<_, String>(vike_secrets::account_credentials::CredentialRegistry::new()),
    )
}

/// **Teach the price tap binance's derived lane twin** (`BTCUSDT.P`, [`account_three`]'s), as the boot's account read
/// does (`crate::feeds::price_tap::note_lane_twins`). The tap's set only grows, so a test that leans on it calls this
/// first and the order the tests run in does not matter.
pub(crate) fn note_the_binance_lane_twin() {
    crate::feeds::price_tap::note_lane_twins(&account_three(Some(5.0)));
}

/// **The kept rows every door suite of this lib reads** (decision 0290: a typed symbol's user name is resolved through
/// its venue's KEPT row), spelled as each venue's list keys them, each row's user name in the comment beside it. One
/// set for every test: [`hold_door_lists`] holds it whole, so two tests never hold different rows for one venue.
pub(crate) fn door_rows() -> Vec<vike_model::Instrument> {
    use vike_model::{
        Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
        ContractEquity, ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractFx,
        ContractOption, ContractOptionExerciseStyle, ContractPerp, Instrument, InstrumentVenueIds,
        Margining,
    };
    let at = |(y, m, d): (i64, u32, u32)| {
        vike_utility::calendar::days_from_civil(y, m, d) * 86_400_000 + 8 * 3_600_000
    };
    let perp = |margining: Margining, settle: &str| {
        Contract::Perp(ContractPerp {
            margining,
            settle: settle.into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    };
    let future = |margining: Margining, settle: &str, day, family| {
        Contract::Future(ContractFuture {
            margining,
            settle: settle.into(),
            value_currency: None,
            expiry_ms: at(day),
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family,
        })
    };
    let cfd = |underlying_kind| {
        Contract::Cfd(ContractCfd {
            underlying_kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Unknown,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        })
    };
    let option = |strike: f64, day| {
        Contract::Option(ContractOption {
            right: vike_marketdata::OptionKind::Call,
            strike,
            expiry_ms: at(day),
            style: ContractOptionExerciseStyle::Unknown,
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_currency: None,
            underlying: None,
        })
    };
    let fx = || Contract::Fx(ContractFx::default());
    let equity = || Contract::Equity(ContractEquity::default());
    let (lin, inv) = (Margining::Linear, Margining::Inverse);
    let crypto = ContractFutureFamily::Crypto;
    let row = |venue: &str, raw: &str, base: &str, quote: &str, contract: Contract| {
        Instrument::new(venue, raw, base, quote, contract)
    };
    let coin = |raw: &str, base: &str, coin: &str, contract: Contract| {
        row("hyperliquid", raw, base, "USDC", contract)
            .with_wire(InstrumentVenueIds { coin: Some(coin.into()), ..Default::default() })
    };
    let mut rows = Vec::new();
    for venue in ["binance", "aster", "bybit"] {
        for base in ["BTC", "ETH", "SOL", "XRP", "DOGE"] {
            rows.push(row(venue, &format!("{base}USDT"), base, "USDT", Contract::CryptoSpot));
            rows.push(row(venue, &format!("{base}USDT.P"), base, "USDT", perp(lin, "USDT")));
        }
    }
    for (base, venue) in [("XAU", "binance"), ("TSLA", "binance")] {
        rows.push(row(venue, &format!("{base}USDT.P"), base, "USDT", perp(lin, "USDT")));
    }
    rows.extend([
        // binance COIN-M `BTCUSD.P` and a dated USDS-M future `BTCUSDT_261225`.
        row("binance", "BTCUSD_PERP.P", "BTC", "USD", perp(inv, "BTC")),
        row(
            "binance",
            "BTCUSDT_261225.P",
            "BTC",
            "USDT",
            future(lin, "USDT", (2026, 12, 25), crypto),
        ),
        // binance's USDC-settled linear perpetual `BTCUSDC.P` (another wallet than the USDT book's).
        row("binance", "BTCUSDC.P", "BTC", "USDC", perp(lin, "USDC")),
        // bybit: the inverse `BTCUSD.P`, `ETHUSD.P`; the dated `ETHUSDT_261127`, `BTCUSD_261225`.
        row("bybit", "BTCUSD.P", "BTC", "USD", perp(inv, "BTC")),
        row("bybit", "ETHUSD.P", "ETH", "USD", perp(inv, "ETH")),
        row("bybit", "ETHUSDT-27NOV26", "ETH", "USDT", future(lin, "USDT", (2026, 11, 27), crypto)),
        row("bybit", "BTCUSDZ26", "BTC", "USD", future(inv, "BTC", (2026, 12, 25), crypto)),
        // okx: `BTCUSDT`, `ETHUSDT`, `BTCUSDT.P`, `ETHUSDT.P`, `ETHUSDT_261225`.
        row("okx", "BTC-USDT", "BTC", "USDT", Contract::CryptoSpot),
        row("okx", "ETH-USDT", "ETH", "USDT", Contract::CryptoSpot),
        row("okx", "BTC-USDT-SWAP", "BTC", "USDT", perp(lin, "USDT")),
        row("okx", "ETH-USDT-SWAP", "ETH", "USDT", perp(lin, "USDT")),
        row("okx", "ETH-USDT-261225", "ETH", "USDT", future(lin, "USDT", (2026, 12, 25), crypto)),
        // deribit: `BTCUSD.P`, `ETHUSD.P`, `SOLUSD.P`, `BTCUSDC.P`, `ETHUSDC.P`, `BTCUSD_260327`, `BTCUSD_261225`,
        // `ETHUSD_260327`, `BTCUSDC_260327`, the spot `BTCUSDC`, two USDC options by their own names.
        row("deribit", "BTC-PERPETUAL", "BTC", "USD", perp(inv, "BTC")),
        row("deribit", "ETH-PERPETUAL", "ETH", "USD", perp(inv, "ETH")),
        row("deribit", "SOL-PERPETUAL", "SOL", "USD", perp(inv, "SOL")),
        row("deribit", "BTC_USDC-PERPETUAL", "BTC", "USDC", perp(lin, "USDC")),
        row("deribit", "ETH_USDC-PERPETUAL", "ETH", "USDC", perp(lin, "USDC")),
        row("deribit", "BTC-27MAR26", "BTC", "USD", future(inv, "BTC", (2026, 3, 27), crypto)),
        row("deribit", "BTC-25DEC26", "BTC", "USD", future(inv, "BTC", (2026, 12, 25), crypto)),
        row("deribit", "ETH-27MAR26", "ETH", "USD", future(inv, "ETH", (2026, 3, 27), crypto)),
        row(
            "deribit",
            "BTC_USDC-27MAR26",
            "BTC",
            "USDC",
            future(lin, "USDC", (2026, 3, 27), crypto),
        ),
        row("deribit", "BTC_USDC", "BTC", "USDC", Contract::CryptoSpot),
        row("deribit", "ETH_USDC-27MAR26-2500-C", "ETH", "USDC", option(2500.0, (2026, 3, 27))),
        row("deribit", "XRP_USDC-27MAR26-2D5-C", "XRP", "USDC", option(2.5, (2026, 3, 27))),
        // hyperliquid: `BTCUSDC.P`, `ETHUSDC.P`, `KPEPEUSDC.P`, the spot `HYPEUSDC`, the dex coin `XYZ:TSLAUSDC.P`.
        coin("BTC", "BTC", "BTC", perp(lin, "USDC")),
        coin("ETH", "ETH", "ETH", perp(lin, "USDC")),
        coin("kPEPE", "kPEPE", "kPEPE", perp(lin, "USDC")),
        coin("HYPE/USDC", "HYPE", "@107", Contract::CryptoSpot),
        coin("xyz:TSLA", "TSLA", "xyz:TSLA", perp(lin, "USDC")),
        // alpaca: `AAPL`, `MSFT`, `TSLA`, the class share `BRK.B`, the coin `BTCUSD`.
        row("alpaca", "AAPL", "AAPL", "USD", equity()),
        row("alpaca", "MSFT", "MSFT", "USD", equity()),
        row("alpaca", "TSLA", "TSLA", "USD", equity()),
        row("alpaca", "BRK.B", "BRK.B", "USD", equity()),
        row("alpaca", "BTC/USD", "BTC", "USD", Contract::CryptoSpot),
        // ibkr (the operator's rows, O-10): `AAPL`, `MSFT`, `TSLA`, `NVDA`, `SAP` (EUR), `VOD` (GBP), `7203` (JPY), the
        // minor-unit listings `LLOY` (GBX), `TEVA` (ILS), `NPN` (ZAR) and `PRX` (ZAc, South African cents), the pairs
        // `EURUSD` and `USDJPY`, the future `ES_261218`.
        row("ibkr", "AAPL.SMART.USD", "AAPL", "USD", equity()),
        row("ibkr", "MSFT.SMART.USD", "MSFT", "USD", equity()),
        row("ibkr", "TSLA.SMART.USD", "TSLA", "USD", equity()),
        row("ibkr", "NVDA.SMART.USD", "NVDA", "USD", equity()),
        row("ibkr", "SAP.SMART.EUR", "SAP", "EUR", equity()),
        row("ibkr", "VOD.SMART.GBP", "VOD", "GBP", equity()),
        row("ibkr", "7203.SMART.JPY", "7203", "JPY", equity()),
        row("ibkr", "LLOY.SMART.GBX", "LLOY", "GBX", equity()),
        row("ibkr", "TEVA.SMART.ILS", "TEVA", "ILS", equity()),
        row("ibkr", "NPN.SMART.ZAR", "NPN", "ZAR", equity()),
        row("ibkr", "PRX.SMART.ZAc", "PRX", "ZAc", equity()),
        row("ibkr", "EUR.USD.IDEALPRO", "EUR", "USD", fx()),
        row("ibkr", "USD.JPY.IDEALPRO", "USD", "JPY", fx()),
        row(
            "ibkr",
            "ES.CME.USD.FUT.20261218",
            "ES",
            "USD",
            future(lin, "USD", (2026, 12, 18), ContractFutureFamily::Traditional),
        ),
    ]);
    let pairs = [
        "EURUSD", "GBPUSD", "USDJPY", "EURGBP", "EURJPY", "GBPJPY", "AUDUSD", "EURCHF", "EURAUD",
        "EURCAD",
    ];
    for (base, quote) in pairs.map(|p| (&p[..3], &p[3..])) {
        rows.push(row("ctrader", &format!("{base}{quote}"), base, quote, fx()));
        rows.push(row("oanda", &format!("{base}_{quote}"), base, quote, fx()));
        rows.push(row("ig", &format!("CS.D.{base}{quote}.MINI.IP"), base, quote, fx()));
        rows.push(row("dukascopy", &format!("{base}/{quote}"), base, quote, fx()));
        rows.push(row("fxcm", &format!("{base}/{quote}"), base, quote, fx()));
    }
    rows.extend([
        // A metal and an index CFD: `XAUUSD`, `US500.CFD`; dukascopy's share CFD `AAPL.CFD`.
        row("ctrader", "XAUUSD", "XAU", "USD", cfd(ContractCfdUnderlying::Metal)),
        row("ctrader", "US500", "US500", "USD", cfd(ContractCfdUnderlying::Index)),
        row("oanda", "XAU_USD", "XAU", "USD", cfd(ContractCfdUnderlying::Metal)),
        row("oanda", "SPX500_USD", "US500", "USD", cfd(ContractCfdUnderlying::Index)),
        row("dukascopy", "XAU/USD", "XAU", "USD", cfd(ContractCfdUnderlying::Metal)),
        row("dukascopy", "AAPL.US/USD", "", "USD", cfd(ContractCfdUnderlying::Share)),
        row("fxcm", "XAU/USD", "XAU", "USD", cfd(ContractCfdUnderlying::Metal)),
    ]);
    // named as the datahub's write stores them
    vike_catalog::spelled(rows)
}

/// **Hold [`door_rows`] as every venue's kept list in this test process** (`vike_catalog::hold_kept_list`), once: the
/// door's `door_kept_rows` asks it first in a test build, so every door suite reads the same lists.
pub(crate) fn hold_door_lists() {
    static HELD: std::sync::Once = std::sync::Once::new();
    HELD.call_once(|| {
        let rows = door_rows();
        for venue in vike_model::VENUES {
            let own = rows.iter().filter(|r| r.venue == *venue).cloned().collect::<Vec<_>>();
            if !own.is_empty() {
                vike_catalog::hold_kept_list(venue, own);
            }
        }
    });
}

/// A two-sided quote at `bid`/`ask` (no size: the price tap and the door read the two sides alone), the
/// price-tap and price-door suites' tick.
pub(crate) fn quote_of(bid: f64, ask: f64) -> vike_marketdata::TickQuote {
    vike_marketdata::TickQuote {
        timestamp: 1,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    }
}

/// The strategy-lane dial of a SCRIPTED `StrategyLaneFactories`, which builds `client()` feeds only: refused, naming the
/// venue. `StrategyLaneFactories::strat` is the seam's one REQUIRED method (`crate::feeds::StrategyLaneFactories`' doc), so every
/// scripted double spells it; this is that spelling.
macro_rules! scripted_strategy {
    () => {
        fn strat(
            &self,
            venue: &str,
            _network: Option<&str>,
            _sink: std::sync::Arc<dyn vike_data::DataClientSink>,
        ) -> Result<vike_datahub_client::strat::DataClientStrategyLane, String> {
            Err(format!("{venue}: this scripted constructor builds client() feeds only"))
        }
    };
}
pub(crate) use scripted_strategy;

/// The `DataClient` subscriptions a test double answers ALIKE: one method per named lane (`bars`,
/// `quotes`, `trades`, `book`), each returning `$answer` with `$this` as its `self`. A lane a double answers
/// on its own (a recording `subscribe_quotes`) stays spelled out beside the invocation.
macro_rules! lanes_answer {
    ($this:ident => $answer:expr; $($lane:ident),+ $(,)?) => {
        $(crate::testutil::lanes_answer!(@ $lane $this $answer);)+
    };
    (@ bars $this:ident $answer:expr) => {
        fn subscribe_bars(
            &mut $this,
            _symbol: &str,
            _interval: &str,
        ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
            $answer
        }
    };
    (@ quotes $this:ident $answer:expr) => {
        fn subscribe_quotes(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
            $answer
        }
    };
    (@ trades $this:ident $answer:expr) => {
        fn subscribe_trades(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
            $answer
        }
    };
    (@ book $this:ident $answer:expr) => {
        fn subscribe_book(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
            $answer
        }
    };
}
pub(crate) use lanes_answer;
