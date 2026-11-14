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
        symbol: String::new(),
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
