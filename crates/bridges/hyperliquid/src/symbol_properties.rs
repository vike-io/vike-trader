//! **What the gatewayhub asks of hyperliquid to admit a symbol the engine was not started on** (decision 0173,
//! decision 0218): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecHyperliquid`]): the
//! network's own universe, a USER name's catalog row found through its kept row (decision 0290), a builder-dex coin
//! read alone, the wire name `@N` refused before any read, and what a row is (`hyperliquid_class`, by the venue words this crate's catalog writes:
//! `crate::catalog::VENUE_WORD_DELISTED` and `VENUE_WORD_ISOLATED_ONLY`, named here, never restated). The
//! listing, its bounds, the grid checks every venue shares and the wire are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{Contract, Instrument};
use vike_utility::errors::CarriedRefusal;

use crate::catalog::{VENUE_WORD_DELISTED, VENUE_WORD_ISOLATED_ONLY};
use crate::refusals::HyperliquidPropertiesRefusal;

/// **The listing key of Hyperliquid's TESTNET universe**, the venue whose universe differs by the asking account's
/// network (a `demo` account trades the testnet): `SymbolFacts::facts_on` keys the listing, asks the instrument list
/// and words its failures with it, so the two networks keep separate slots (their coins, asset ids and `maxLeverage`
/// differ: 19 mainnet coins are not on the testnet, BTC is asset 0 on mainnet and 3 on the testnet). The mainnet
/// universe stays under the bare venue slug.
pub const HYPERLIQUID_TESTNET_LIST: &str = "hyperliquid@testnet";

/// **hyperliquid's symbol facts** (decisions 0218 and 0290): the engine's symbol is a USER name (`BTCUSDC.P`,
/// `KPEPEUSDC.P`, `HYPEUSDC`, `XYZ:TSLAUSDC.P`), and the catalog row it names is keyed by the listing name its KEPT row
/// carries (`BTC`, `kPEPE`, `HYPE/USDC`, `xyz:TSLA`): a perpetual (an isolated-only one is answered with
/// `margin_mode: Isolated`, the catalog row's venue word), a builder-dex (HIP-3) coin (read ONE at a time with its own
/// dex's `meta`) and a spot pair quoted in USDC (answered `Cash` with a margin rate of 1); a delisted coin, a pair
/// quoted in another token and the wire name `@N` are refused by name. A testnet account is answered from
/// [`HYPERLIQUID_TESTNET_LIST`] (a coin's listing name is one on both networks, its asset id is not). An answer that
/// states a margin mode reaches only a node that said it books one (the gatewayhub's `session.rs`'s `mode_refusal`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecHyperliquid;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecHyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }

    fn list_key(&self, network: &str) -> &'static str {
        if network == vike_model::VenueNetwork::Testnet.word() {
            HYPERLIQUID_TESTNET_LIST
        } else {
            "hyperliquid"
        }
    }

    fn covers(&self, list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some(if list_key == HYPERLIQUID_TESTNET_LIST {
            "perpetuals, builder-dex coins and USDC spot pairs of Hyperliquid's testnet, the network this \
             account trades on; the testnet list is not mainnet's"
        } else {
            "perpetuals, builder-dex coins and USDC spot pairs"
        })
    }

    /// The wire name `@N` by its shape, then a name no kept row answers (an old spelling, `BTC` or `HYPE/USDC`, whose
    /// listing row would otherwise answer it) in the translator's words. Asked of nothing but the kept rows.
    fn symbol_refusal(
        &self,
        symbol: &str,
        words: &SymbolPropertiesWords,
    ) -> Option<CarriedRefusal> {
        name_refusal(symbol, words).map(|refusal| refusal.carried())
    }

    /// Upper case: a user name is (decision 0290), and its kept row is found ignoring case anyway.
    fn case_kept(&self, _symbol: &str) -> bool {
        false
    }

    /// The catalog's key of the row: the user name's kept row's listing name (`BTCUSDC.P` is `BTC`, `KPEPEUSDC.P` the
    /// case-kept `kPEPE`, `HYPEUSDC` `HYPE/USDC`), the key the lister's rows are listed under (`crate::catalog`).
    /// A name with no kept row was refused already ([`Self::symbol_refusal`]).
    fn key(&self, symbol: &str) -> String {
        listing_name(symbol).unwrap_or_else(|_| symbol.to_string())
    }

    /// A builder-dex coin (its key `xyz:TSLA`, decision 0218), read with its own dex's `meta` alone. The testnet lists
    /// hundreds of dexes, and the whole HIP-3 universe is an opt-in flag a lookup must not depend on.
    fn one_at_a_time(&self, key: &str) -> bool {
        key.contains(vike_catalog::DEX_SEPARATOR)
    }

    /// The lister is told which NETWORK is meant, by the listing key.
    fn lister_key<'k>(&self, list_key: &'k str) -> &'k str {
        list_key
    }

    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        _now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        hyperliquid_class(row, symbol, words)
            .map_err(|refusal| SymbolPropertiesError::Refused(refusal.carried()))
    }
}

/// `SymbolPropertiesListSpecHyperliquid::symbol_refusal`, typed: the wire name `@N` by its shape
/// ([`is_wire_name`]), then a name no kept row answers, in the translator's words.
fn name_refusal(
    symbol: &str,
    words: &SymbolPropertiesWords,
) -> Option<HyperliquidPropertiesRefusal> {
    let not_answered = words.not_answered;
    if is_wire_name(symbol) {
        let symbol = symbol.to_string();
        return Some(HyperliquidPropertiesRefusal::WireName { not_answered, symbol });
    }
    listing_name(symbol)
        .err()
        .map(|why| HyperliquidPropertiesRefusal::NoKeptRow { not_answered, why })
}

/// **The listing name of the USER name `symbol`** (`BTCUSDC.P` -> `BTC`, `HYPEUSDC` -> `HYPE/USDC`, `XYZ:TSLAUSDC.P` ->
/// `xyz:TSLA`): the `raw_symbol` of its kept row (`vike_catalog::kept_list`, the datahub's
/// `<project>/instruments/hyperliquid.json`). A listing name is one on both networks, so the testnet's list is
/// keyed by it too.
///
/// # Errors
/// No list is held, none of its rows answers `symbol`, or two do: the translator's words.
fn listing_name(symbol: &str) -> Result<String, String> {
    let rows = vike_catalog::kept_list("hyperliquid");
    let rows = rows.as_deref().map_or(&[][..], Vec::as_slice);
    vike_catalog::kept_row_of_user_name(rows, "hyperliquid", symbol)
        .map(|row| row.raw_symbol.clone())
}

/// Whether a Hyperliquid symbol's TEXT is a spelling this lane does not take: the venue's own wire coin of a spot
/// pair (`@N`; the pair is written base and quote, `HYPEUSDC`). Asked of nothing. A perpetual (`BTCUSDC.P`), a spot
/// pair (`HYPEUSDC`) and a builder-dex coin (`XYZ:TSLAUSDC.P`) are all user names this lane answers (decisions 0218
/// and 0290); what a ROW is decides the rest ([`hyperliquid_class`]).
fn is_wire_name(symbol: &str) -> bool {
    symbol.starts_with('@')
}

/// Classify a hyperliquid catalog row, or say why it cannot be admitted:
///
/// * a PERPETUAL (core, or a builder-dex coin: its row carries the dex as its wire group) trades with the account
///   as the venue holds it. A delisted one is refused; one the venue lets an account hold in isolated margin ONLY
///   (`onlyIsolated`, `strictIsolated`, `noCross`: the catalog's venue word) is answered with `Isolated`, which
///   the node's engine books its positions in; every other is cross, as it always was.
/// * a SPOT pair quoted in USDC is answered `Cash` with a margin rate of 1 (fully funded, never liquidated); a
///   pair quoted in any other token is refused by name.
/// * anything else the catalog may one day list is not answered.
fn hyperliquid_class(
    row: &Instrument,
    symbol: &str,
    words: &SymbolPropertiesWords,
) -> Result<SymbolPropertiesListAnswer, HyperliquidPropertiesRefusal> {
    let symbol_owned = || symbol.to_string();
    match &row.contract {
        Contract::Perp(_) => match row.contract_type.as_deref() {
            Some(VENUE_WORD_DELISTED) => {
                Err(HyperliquidPropertiesRefusal::Delisted { symbol: symbol_owned() })
            }
            Some(VENUE_WORD_ISOLATED_ONLY) => Ok(SymbolPropertiesListAnswer {
                margin_mode: Some(vike_model::MarginMode::Isolated),
                ..SymbolPropertiesListAnswer::default()
            }),
            _ => Ok(SymbolPropertiesListAnswer::default()),
        },
        // The only quote token a spot pair is admitted in: HL settles its perpetuals in USDC, and the engine's ledger
        // is one currency (a pair quoted in another token, USDH or USDT0, would be valued at par against the wrong
        // money).
        Contract::CryptoSpot if row.quote.eq_ignore_ascii_case(vike_model::USDC) => {
            Ok(SymbolPropertiesListAnswer {
                margin_mode: Some(vike_model::MarginMode::Cash),
                margin_rate: Some(1.0),
                ..SymbolPropertiesListAnswer::default()
            })
        }
        Contract::CryptoSpot => Err(HyperliquidPropertiesRefusal::SpotNotUsdcQuoted {
            symbol: symbol_owned(),
            quote: row.quote.clone(),
        }),
        _ => Err(HyperliquidPropertiesRefusal::NeitherPerpNorSpot {
            not_answered: words.not_answered,
            symbol: symbol_owned(),
        }),
    }
}

#[cfg(test)]
#[path = "symbol_properties_tests.rs"]
mod symbol_properties_tests;
