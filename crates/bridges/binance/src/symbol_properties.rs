//! **What the gatewayhub asks of binance to admit a symbol the engine was not started on** (decision 0173, decision
//! 0230, decision 0258's binance amendment): the venue's half of the list-answered symbol facts
//! ([`SymbolPropertiesListSpecBinance`]): which list answers which spelling on which network, the COIN-M book's own
//! list, and the dated future refused near or past its delivery. The listing, its bounds, the checks every venue
//! shares and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`); this
//! module names no wire (the wire's words arrive as `SymbolPropertiesWords`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesRowLookup,
    SymbolPropertiesWords,
};
use vike_model::{Contract, ContractValue, Instrument, NotionalKind, SymbolProperties};

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

/// How long before its delivery a dated USDS-M future stops being admitted: a symbol that is delivered inside
/// this window is refused by name, because a position opened there is settled by the venue within hours and the
/// engine learns of that only as a position that vanished (the reconcile's divergence).
pub const DELIVERY_GUARD_MS: i64 = 24 * 60 * 60 * 1000;

/// **binance's symbol facts** (decision 0173): spot read one symbol at a time (`exchangeInfo?symbol=`), the USDS-M
/// perpetuals AND dated futures (`BTCUSDT_261225.P`, all linear) out of one list per network, a dated one refused by
/// name when its delivery has passed or is under [`DELIVERY_GUARD_MS`] away (`futures_expiry_refusal`), and a COIN-M
/// symbol (`BTCUSD_PERP.P`) answered from the COIN-M list of the account's network ([`BINANCE_COIN_M_LIST`],
/// [`BINANCE_DEMO_COIN_M_LIST`]) with `Inverse(contractSize)`, the row's face, never a guessed one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecBinance;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecBinance {
    fn venue(&self) -> &'static str {
        "binance"
    }

    fn list_key(&self, network: &str) -> &'static str {
        if network == "testnet" { BINANCE_DEMO_LIST } else { "binance" }
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

    /// A COIN-M symbol is not in the USDS-M list; its book's own list (of the account's network) answers.
    fn answered_elsewhere(
        &self,
        key: &str,
        list_key: &str,
        now_ms: i64,
        words: &SymbolPropertiesWords,
        row_in: &SymbolPropertiesRowLookup<'_>,
    ) -> Option<Result<SymbolPropertiesListAnswer, String>> {
        binance_coin_m_shaped(key)
            .then(|| binance_coin_m_facts(key, list_key, now_ms, words, row_in))
    }

    /// SPOT (`exchangeInfo?symbol=`, a few hundred bytes against ~17.6 MB). The perpetual lane stays one bulk read
    /// (fapi ignores `symbol=`), kept per network and shared by every `.P` ask.
    fn one_at_a_time(&self, key: &str) -> bool {
        !key.ends_with(".P")
    }

    /// A dated future whose delivery has passed (the demo list keeps one as TRADING) or is hours away.
    fn row_answer(
        &self,
        _symbol: &str,
        key: &str,
        row: &Instrument,
        now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, String> {
        if key.ends_with(".P")
            && let Some(why) = futures_expiry_refusal(row, now_ms, words)
        {
            return Err(why);
        }
        Ok(SymbolPropertiesListAnswer::default())
    }
}

/// Whether `key` (the engine's spelling, `.P` suffixed) has the shape of a COIN-M symbol: the quote is USD and the
/// part after the underscore is `PERP` or a delivery date (`BTCUSD_PERP`, `BTCUSD_261225`). A shape decides only
/// WHETHER the COIN-M list is read; the list decides the verdict.
fn binance_coin_m_shaped(key: &str) -> bool {
    let Some((head, tail)) = key.strip_suffix(".P").and_then(|k| k.split_once('_')) else {
        return false;
    };
    head.ends_with("USD")
        && !head.ends_with("USDT")
        && (tail == "PERP" || (tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit())))
}

/// **Why a dated USDS-M future cannot be admitted at `now_ms`**, or `None` (a perpetual has no expiry, and a
/// contract with more than [`DELIVERY_GUARD_MS`] to run is fine). MEASURED on the demo host (2026-10-30): its list
/// carries `BSBUSDT_260618`, a `CURRENT_WEEK` row whose status is `TRADING` and whose `deliveryDate` is four and a
/// half months in the past (the demo venue still ACCEPTS an order on it once it holds a leverage: it is stale test
/// data, not a contract that can deliver), so a row's status is not proof that it is a live contract.
fn futures_expiry_refusal(
    row: &Instrument,
    now_ms: i64,
    words: &SymbolPropertiesWords,
) -> Option<String> {
    let Contract::Future(spec) = &row.contract else { return None };
    let date = vike_model::time::epoch_ms_to_utc_date(spec.expiry_ms);
    let left = spec.expiry_ms - now_ms;
    if left <= 0 {
        return Some(format!(
            "{}: {} was delivered on {date}; the venue's list still shows it as TRADING, but {}, \
             so the symbol is not admitted",
            row.venue, row.raw_symbol, words.expired
        ));
    }
    (left < DELIVERY_GUARD_MS).then(|| {
        format!(
            "{}: {} is delivered on {date}, in under 24 hours; a position opened now would be settled by the \
             venue almost at once, so the symbol is not admitted",
            row.venue, row.raw_symbol
        )
    })
}

/// **The facts of a COIN-M symbol** (`key`, a `.P` spelling of COIN-M shape), from the COIN-M listing of the
/// account's network ([`BINANCE_DEMO_COIN_M_LIST`] for a `demo` account, [`BINANCE_COIN_M_LIST`] else): the
/// row's grid in whole contracts and its value `Inverse(contractSize)` (decisions 0170, 0258), the face the
/// adapter's own mount reads from the same row (`crates/bridges/binance/src/execution/rest/coin_m.rs`'s `mount_of`). A dated
/// contract inside [`DELIVERY_GUARD_MS`] of its delivery, a row with no positive `contractSize` and a list that
/// cannot be read are refused, never answered with a guessed face.
fn binance_coin_m_facts(
    key: &str,
    list_key: &str,
    now_ms: i64,
    words: &SymbolPropertiesWords,
    row_in: &SymbolPropertiesRowLookup<'_>,
) -> Result<SymbolPropertiesListAnswer, String> {
    let (coin_m, network) = if list_key == BINANCE_DEMO_LIST {
        (BINANCE_DEMO_COIN_M_LIST, "the DEMO COIN-M host, the network this account trades on")
    } else {
        (BINANCE_COIN_M_LIST, "mainnet's COIN-M host")
    };
    let row = row_in(coin_m, key)?.ok_or_else(|| {
        format!(
            "binance{}{key} in the venue's instrument list (the COIN-M contracts that are TRADING on {network})",
            words.no_instrument
        )
    })?;
    if let Some(why) = futures_expiry_refusal(&row, now_ms, words) {
        return Err(why);
    }
    let face = row.properties.contract_size;
    let value = ContractValue::of(&row.contract, face)
        .filter(|v| v.kind == NotionalKind::Inverse && face.is_finite() && face > 0.0)
        .ok_or_else(|| {
            format!(
                "binance: {key} is a COIN-M row whose value is unknown (contractSize {face}, {:?}): its face \
                 is the row's alone, so it is not admitted",
                row.contract
            )
        })?;
    // The face is the contract value's, never a multiplier on the grid (decision 0170).
    Ok(SymbolPropertiesListAnswer {
        properties: Some(SymbolProperties { contract_size: 0.0, ..row.properties }),
        contract_value: Some(value),
        whole: true,
        ..SymbolPropertiesListAnswer::default()
    })
}
