//! The fixtures several of this lib's `#[cfg(test)]` suites share, written once (`lib.rs` declares this
//! module under `#[cfg(test)]`, so it compiles into the lib's own unit-test binary and nowhere else; the
//! integration groups under `tests/` keep their helpers beside them).

use vike_exec::SymbolAdmission;
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
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// The rows of a node whose binance default account (id 3, demo) states `leverage` or not, built the way a
/// root builds them (`AccountDirectory::read`): while it states one the table DERIVES the twin
/// `binance#PERP` and MARKS it.
pub(crate) fn account_three(leverage: Option<f64>) -> AccountDirectory {
    let parent = vike_secrets::Account {
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
    };
    AccountDirectory::read(
        Ok::<_, String>(vike_secrets::Accounts::Known(vec![parent])),
        Ok::<_, String>(None),
        Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
    )
}

/// A two-sided quote at `bid`/`ask` (no size: the price tap and the door read the two sides alone), the
/// price-tap and price-door suites' tick.
pub(crate) fn quote_of(bid: f64, ask: f64) -> vike_marketdata::QuoteTick {
    vike_marketdata::QuoteTick {
        ts: 1,
        local_ts: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    }
}

/// The strategy-lane dial of a SCRIPTED `FeedCtors`, which builds `client()` feeds only: refused, naming the
/// venue. `FeedCtors::strat` is the seam's one REQUIRED method (`crate::feeds::FeedCtors`' doc), so every
/// scripted double spells it; this is that spelling.
macro_rules! scripted_strat {
    () => {
        fn strat(
            &self,
            venue: &str,
            _network: Option<&str>,
            _sink: std::sync::Arc<dyn vike_data::LiveDataSink>,
        ) -> Result<vike_datahub_client::strat::StratFeed, String> {
            Err(format!("{venue}: this scripted constructor builds client() feeds only"))
        }
    };
}
pub(crate) use scripted_strat;

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
        ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
            $answer
        }
    };
    (@ quotes $this:ident $answer:expr) => {
        fn subscribe_quotes(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
            $answer
        }
    };
    (@ trades $this:ident $answer:expr) => {
        fn subscribe_trades(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
            $answer
        }
    };
    (@ book $this:ident $answer:expr) => {
        fn subscribe_book(
            &mut $this,
            _symbol: &str,
        ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
            $answer
        }
    };
}
pub(crate) use lanes_answer;
