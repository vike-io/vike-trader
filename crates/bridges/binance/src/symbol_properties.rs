//! **What the gatewayhub asks of binance to admit a symbol the engine was not started on** (decision 0173, decision
//! 0230, decision 0258's binance amendment): the venue's half of the list-answered symbol facts
//! ([`SymbolPropertiesListSpecBinance`]): which list answers which spelling on which network, the COIN-M book's own
//! list, and the dated future refused once it is delivered (inside its last day it is admitted close-only, and the
//! per-order guard is `crate::execution::rest::perp`'s, okx's rule for every venue). The listing, its bounds, the
//! checks every venue shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`);
//! this module names no wire (the wire's words arrive as `SymbolPropertiesWords`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{ContractValue, Instrument, NotionalKind, SymbolProperties};
use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_guard_for};

use crate::refusals::BinancePropertiesRefusal;

/// **The listing key of Binance's DEMO futures list**: a `demo` account signs on `demo-fapi`, whose list is not
/// mainnet's (2026-10-30: 30 TradFi perpetuals TRADING against 235, one weekly future whose delivery date had
/// passed, a quarterly pair, no `NEXT_QUARTER` trading), so facts read from mainnet's would admit a symbol the
/// adapter then cannot find a grid for. Keyed apart from mainnet's; SPOT symbols are read one at a time from the
/// public spot host for either network.
pub const BINANCE_DEMO_LIST: &str = "binance@demo";

/// **The listing key of Binance's COIN-M (`dapi`) book on mainnet**: a COIN-M symbol (`BTCUSD_PERP.P`) is
/// answered from it (`Inverse(contractSize)`, decision 0258's binance amendment), a `demo` account's from
/// [`BINANCE_DEMO_COIN_M_LIST`]. Kept and failure-remembered under its own slot like any listing, and asked only for
/// a symbol shaped like a COIN-M one (`binance_coin_m_shaped`), so a typo costs no dapi read.
pub const BINANCE_COIN_M_LIST: &str = "binance@coin-m";

/// **The listing key of Binance's DEMO COIN-M book** (`demo-dapi`): a `demo` account signs COIN-M orders there,
/// and its list is not mainnet's (2026-11-08: 51 rows, stale quarterlies among them), so a demo account's COIN-M
/// facts come from it, as its USDS-M ones come from [`BINANCE_DEMO_LIST`].
pub const BINANCE_DEMO_COIN_M_LIST: &str = "binance@demo-coin-m";

/// **binance's symbol facts** (decision 0173): spot read one symbol at a time (`exchangeInfo?symbol=`), the USDS-M
/// perpetuals AND dated futures (`BTCUSDT_261225.P`, all linear) out of one list per network, a dated one refused by
/// name once its delivery has passed (`futures_expiry_refusal`), and a COIN-M
/// symbol (`BTCUSD_PERP.P`) answered from the COIN-M list of the account's network ([`BINANCE_COIN_M_LIST`],
/// [`BINANCE_DEMO_COIN_M_LIST`]) with `Inverse(contractSize)`, the row's face, never a guessed one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecBinance;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecBinance {
    fn venue(&self) -> &'static str {
        "binance"
    }

    fn list_key(&self, network: &str) -> &'static str {
        if network == vike_model::VenueNetwork::Testnet.word() {
            BINANCE_DEMO_LIST
        } else {
            "binance"
        }
    }

    fn covers(&self, list_key: &str, _account_listed: bool) -> Option<&'static str> {
        // binance's USDS-M list is the TRADING perpetuals (TradFi equity and commodity ones included) AND the
        // TRADING dated futures (`BTCUSDT_261225.P`).
        Some(if list_key == BINANCE_DEMO_LIST {
            "spot, and the USDS-M perpetuals (TradFi included) and dated futures of the DEMO futures host, \
             the network this account trades on; that list is shorter than mainnet's"
        } else {
            "spot, USDS-M perpetuals (TradFi included) and dated USDS-M futures that are TRADING; a \
             contract that is pending, settling or being delivered is not in the list"
        })
    }

    /// The USER name's KEPT row's lane name (decision 0290: `BTCUSD.P` is listed as `BTCUSD_PERP.P`, `BTCUSDT_261225` as
    /// `BTCUSDT_261225.P`); a name with no row keys as written, which no listing holds (the not-found refusal).
    fn key(&self, symbol: &str) -> String {
        crate::instrument::names::lane_name_of(symbol).unwrap_or_else(|_| symbol.to_string())
    }

    /// A COIN-M symbol is not in the USDS-M list; its book's own list (of the account's network) answers.
    fn listed_elsewhere(&self, key: &str, list_key: &str) -> Option<&'static str> {
        binance_coin_m_shaped(key).then(|| coin_m_list_of(list_key).0)
    }

    fn answered_elsewhere(
        &self,
        key: &str,
        list_key: &str,
        row: Option<&Instrument>,
        now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        binance_coin_m_facts(key, list_key, row, now_ms, words)
    }

    /// SPOT (`exchangeInfo?symbol=`, a few hundred bytes against ~17.6 MB). The perpetual lane stays one bulk read
    /// (fapi ignores `symbol=`), kept per network and shared by every `.P` ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        !key.ends_with(".P")
    }

    /// A dated future whose delivery has passed (the demo list keeps one as TRADING).
    fn row_answer(
        &self,
        _symbol: &str,
        key: &str,
        row: &Instrument,
        now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        if key.ends_with(".P")
            && let Some(why) = futures_expiry_refusal(key, row, now_ms, words)
        {
            return Err(why);
        }
        Ok(SymbolPropertiesListAnswer::default())
    }
}

/// Whether `key` (the lane name, `.P` suffixed) has the shape of a COIN-M symbol (`BTCUSD_PERP.P`,
/// `BTCUSD_261225.P`): the bridge's ONE COIN-M reading (`crate::instrument::names::is_coin_m`, BIN-5) on the futures
/// lane. A shape decides only WHETHER the COIN-M list is read; the list decides the verdict.
fn binance_coin_m_shaped(key: &str) -> bool {
    key.ends_with(vike_venues::PERP_SUFFIX) && crate::instrument::names::is_coin_m(key)
}

/// **Why a dated future cannot be admitted at `now_ms`**, or `None`: the one delivery verdict, asked as a
/// reduce-only order would be (`vike_venues::venue_delivery_guard`), so only a contract already delivered is refused;
/// inside its last day it is admitted close-only and the per-order guard refuses an OPENING order. The expiry is the
/// delivery day the lane name `key` carries (`BTCUSDT_261225.P`, decision 0290), never the row's. A perpetual names
/// no day. MEASURED on the demo host (2026-10-30): its list carries `BSBUSDT_260618`, a `CURRENT_WEEK` row whose
/// status is `TRADING` and whose `deliveryDate` is four and a half months in the past (the demo venue still ACCEPTS an
/// order on it once it holds a leverage: it is stale test data, not a contract that can deliver), so a row's status
/// is not proof that it is a live contract.
fn futures_expiry_refusal(
    key: &str,
    row: &Instrument,
    now_ms: i64,
    words: &SymbolPropertiesWords,
) -> Option<SymbolPropertiesError> {
    let guard = delivery_guard_for(crate::VENUE);
    let expiry = guard.expiry_of(key)?;
    (guard.verdict(expiry, now_ms, true) == DeliveryGuardVerdict::Delivered).then(|| {
        let refusal = BinancePropertiesRefusal::Delivered {
            venue: row.venue.clone(),
            symbol: row.raw_symbol.clone(),
            date: vike_model::time::epoch_ms_to_utc_date(expiry),
            expired: words.expired,
        };
        SymbolPropertiesError::Expired(refusal.carried())
    })
}

/// The COIN-M listing of the network `list_key` answers, and how the not-found words name its host.
fn coin_m_list_of(list_key: &str) -> (&'static str, &'static str) {
    if list_key == BINANCE_DEMO_LIST {
        (BINANCE_DEMO_COIN_M_LIST, "the DEMO COIN-M host, the network this account trades on")
    } else {
        (BINANCE_COIN_M_LIST, "mainnet's COIN-M host")
    }
}

/// **The facts of a COIN-M symbol** (`key`, a `.P` spelling of COIN-M shape), from the COIN-M listing of the
/// account's network ([`BINANCE_DEMO_COIN_M_LIST`] for a `demo` account, [`BINANCE_COIN_M_LIST`] else): the
/// row's grid in whole contracts and its value `Inverse(contractSize)` (decisions 0170, 0258), the face the
/// adapter's own mount reads from the same row (`crates/bridges/binance/src/execution/rest/coin_m.rs`'s `mount_of`). A dated
/// contract already delivered and a row with no positive `contractSize` are refused, never answered with a guessed
/// face. `row` is the key's row in that listing (a listing that cannot be read is the gatewayhub's refusal, before this).
fn binance_coin_m_facts(
    key: &str,
    list_key: &str,
    row: Option<&Instrument>,
    now_ms: i64,
    words: &SymbolPropertiesWords,
) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
    let row = row.ok_or_else(|| {
        let refusal = BinancePropertiesRefusal::CoinMNotListed {
            no_instrument: words.no_instrument,
            key: key.to_string(),
            host: coin_m_list_of(list_key).1,
        };
        SymbolPropertiesError::NoInstrument(refusal.carried())
    })?;
    if let Some(why) = futures_expiry_refusal(key, row, now_ms, words) {
        return Err(why);
    }
    let face = row.properties.contract_size;
    let value = ContractValue::of(&row.contract, face)
        .filter(|v| v.kind == NotionalKind::Inverse && face.is_finite() && face > 0.0)
        .ok_or_else(|| {
            let refusal = BinancePropertiesRefusal::CoinMValueUnknown {
                key: key.to_string(),
                face,
                contract: row.contract.clone(),
            };
            SymbolPropertiesError::Refused(refusal.carried())
        })?;
    // The face is the contract value's, never a multiplier on the grid (decision 0170).
    Ok(SymbolPropertiesListAnswer {
        properties: Some(SymbolProperties { contract_size: 0.0, ..row.properties }),
        contract_value: Some(value),
        whole: true,
        ..SymbolPropertiesListAnswer::default()
    })
}
