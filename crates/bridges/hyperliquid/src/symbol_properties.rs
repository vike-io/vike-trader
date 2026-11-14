//! **What the gatewayhub asks of hyperliquid to admit a symbol the engine was not started on** (decision 0173,
//! decision 0218): the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecHyperliquid`]): the
//! network's own universe, the coin's case kept, a builder-dex coin read alone, the wire name `@N` refused before any
//! read, and what a row is (`hyperliquid_class`, by the venue words this crate's catalog writes:
//! `crate::catalog::VENUE_WORD_DELISTED` and `VENUE_WORD_ISOLATED_ONLY`, named here, never restated). The
//! listing, its bounds, the grid checks every venue shares and the wire are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesWords,
};
use vike_model::{Contract, Instrument};

use crate::catalog::{VENUE_WORD_DELISTED, VENUE_WORD_ISOLATED_ONLY};

/// **The listing key of Hyperliquid's TESTNET universe**, the venue whose universe differs by the asking account's
/// network (a `demo` account trades the testnet): `SymbolFacts::facts_on` keys the listing, asks the instrument list
/// and words its failures with it, so the two networks keep separate slots (their coins, asset ids and `maxLeverage`
/// differ: 19 mainnet coins are not on the testnet, BTC is asset 0 on mainnet and 3 on the testnet). The mainnet
/// universe stays under the bare venue slug.
pub const HYPERLIQUID_TESTNET_LIST: &str = "hyperliquid@testnet";

/// **hyperliquid's symbol facts** (decision 0218): the coin name EXACTLY as listed, case kept (`kPEPE`): a perpetual
/// (an isolated-only one is answered with `margin_mode: Isolated`, the catalog row's venue word), a builder-dex
/// (HIP-3) coin (`dex:COIN`, read ONE at a time with its own dex's `meta`) and a spot pair quoted in USDC
/// (`HYPE/USDC`, answered `Cash` with a margin rate of 1); a delisted coin, a pair quoted in another token and the wire
/// name `@N` are refused by name. A testnet account is answered from [`HYPERLIQUID_TESTNET_LIST`]. An answer that
/// states a margin mode reaches only a node that said it books one (the gatewayhub's `session.rs`'s `mode_refusal`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecHyperliquid;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecHyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }

    fn list_key(&self, network: &str) -> &'static str {
        if network == "testnet" { HYPERLIQUID_TESTNET_LIST } else { "hyperliquid" }
    }

    fn covers(&self, list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some(if list_key == HYPERLIQUID_TESTNET_LIST {
            "perpetuals, builder-dex coins and USDC spot pairs of Hyperliquid's testnet, the network this \
             account trades on; the testnet list is not mainnet's"
        } else {
            "perpetuals, builder-dex coins and USDC spot pairs"
        })
    }

    fn symbol_refusal(&self, symbol: &str, words: &SymbolPropertiesWords) -> Option<String> {
        hyperliquid_not_core_perp(symbol)
            .map(|what| format!("{}: hyperliquid `{symbol}` is {what}", words.not_answered))
    }

    /// The key is the coin name EXACTLY as the venue lists it (`kPEPE` is not `KPEPE`).
    fn case_kept(&self, _symbol: &str) -> bool {
        true
    }

    /// A builder-dex coin (`dex:COIN`, decision 0218), read with its own dex's `meta` alone. The testnet lists
    /// hundreds of dexes, and the whole HIP-3 universe is an opt-in flag a lookup must not depend on.
    fn one_at_a_time(&self, key: &str) -> bool {
        key.contains(':')
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
    ) -> Result<SymbolPropertiesListAnswer, String> {
        hyperliquid_class(row, symbol, words)
    }
}

/// What a Hyperliquid symbol's TEXT says it is when this lane does not take that spelling: the venue's own
/// wire coin of a spot pair (`@N`; the pair is written `BASE/QUOTE`). Asked of nothing. A core perpetual
/// (`BTC`), a spot pair (`HYPE/USDC`) and a builder-dex coin (`dex:COIN`) are all spellings this lane answers
/// (decision 0218); what a ROW is decides the rest ([`hyperliquid_class`]).
fn hyperliquid_not_core_perp(symbol: &str) -> Option<&'static str> {
    symbol.starts_with('@').then_some(
        "the venue's wire name of a spot pair; write the pair as BASE/QUOTE (for example HYPE/USDC)",
    )
}

/// The only quote token a spot pair is admitted in: HL settles its perpetuals in USDC, and the engine's ledger is
/// one currency (a pair quoted in another token, USDH or USDT0, would be valued at par against the wrong money).
const HL_SPOT_QUOTE: &str = "USDC";

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
) -> Result<SymbolPropertiesListAnswer, String> {
    match &row.contract {
        Contract::Perp(_) => match row.contract_type.as_deref() {
            Some(VENUE_WORD_DELISTED) => {
                Err(format!("hyperliquid: `{symbol}` is delisted; no order can open on it"))
            }
            Some(VENUE_WORD_ISOLATED_ONLY) => Ok(SymbolPropertiesListAnswer {
                margin_mode: Some(vike_model::MarginMode::Isolated),
                ..SymbolPropertiesListAnswer::default()
            }),
            _ => Ok(SymbolPropertiesListAnswer::default()),
        },
        Contract::CryptoSpot if row.quote.eq_ignore_ascii_case(HL_SPOT_QUOTE) => {
            Ok(SymbolPropertiesListAnswer {
                margin_mode: Some(vike_model::MarginMode::Cash),
                margin_rate: Some(1.0),
                ..SymbolPropertiesListAnswer::default()
            })
        }
        Contract::CryptoSpot => Err(format!(
            "hyperliquid: `{symbol}` is quoted in {}, and only a pair quoted in {HL_SPOT_QUOTE} is admitted (the \
             engine's ledger is one currency)",
            row.quote
        )),
        _ => Err(format!(
            "{}: hyperliquid `{symbol}` is neither a perpetual nor a spot pair",
            words.not_answered
        )),
    }
}

#[cfg(test)]
#[path = "symbol_properties_tests.rs"]
mod symbol_properties_tests;
