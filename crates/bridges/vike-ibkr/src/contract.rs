//! IBKR contract model: ONE secType-tagged contract (STK/OPT/FUT/CASH/IND/CRYPTO + Other) covering
//! every asset class, plus symbology and the contract-details→`SymbolProperties` bridge.
//!
//! Symbology (Nautilus IB_SIMPLIFIED): a canonical vike symbol `SYMBOL.EXCHANGE.CURRENCY`
//! (e.g. `AAPL.SMART.USD`, `EUR.USD.IDEALPRO`). SecType is inferred from exchange conventions:
//! IDEALPRO → CASH (forex), else STK by default. OPT/FUT/CRYPTO need explicit fields, resolved via
//! `contractDetails` (later); Phase 1 parses the common STK/CASH shapes and carries the rest as
//! fields. A `conId ↔ symbol` bidirectional map lets inbound events (keyed by numeric conId) map
//! back to the canonical symbol.

use std::collections::HashMap;
use vike_model::SymbolProperties;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SecType {
    Stk,
    Opt,
    Fut,
    Cash,
    Ind,
    Crypto,
    Other(String),
}

impl SecType {
    pub fn as_ib_code(&self) -> &str {
        match self {
            SecType::Stk => "STK",
            SecType::Opt => "OPT",
            SecType::Fut => "FUT",
            SecType::Cash => "CASH",
            SecType::Ind => "IND",
            SecType::Crypto => "CRYPTO",
            SecType::Other(s) => s.as_str(),
        }
    }

    pub fn from_ib_code(code: &str) -> SecType {
        match code {
            "STK" => SecType::Stk,
            "OPT" => SecType::Opt,
            "FUT" => SecType::Fut,
            "CASH" => SecType::Cash,
            "IND" => SecType::Ind,
            "CRYPTO" => SecType::Crypto,
            other => SecType::Other(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IbkrContract {
    pub sec_type: SecType,
    pub symbol: String,
    pub exchange: String,
    pub currency: String,
    pub expiry: Option<String>,     // YYYYMMDD (OPT/FUT)
    pub strike: Option<f64>,        // OPT
    pub right: Option<char>,        // 'C' | 'P' (OPT)
    pub multiplier: Option<String>, // OPT/FUT
    pub con_id: Option<i64>,
}

impl IbkrContract {
    fn stk(symbol: &str, exchange: &str, currency: &str) -> Self {
        IbkrContract {
            sec_type: SecType::Stk,
            symbol: symbol.into(),
            exchange: exchange.into(),
            currency: currency.into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    }
}

/// Parse the canonical `SYMBOL.EXCHANGE.CURRENCY` form. IDEALPRO → CASH (forex, `SYMBOL`=base,
/// `CURRENCY`=quote); everything else defaults to STK. OPT/FUT/CRYPTO are carried by explicit
/// fields once `contractDetails` resolves them (Phase 1 only auto-parses STK/CASH).
pub fn parse_simplified(canonical: &str) -> Option<IbkrContract> {
    let parts: Vec<&str> = canonical.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let (a, b, c) = (parts[0], parts[1], parts[2]);
    // Forex: `EUR.USD.IDEALPRO` → base=EUR quote=USD on IDEALPRO.
    if c.eq_ignore_ascii_case("IDEALPRO") {
        return Some(IbkrContract {
            sec_type: SecType::Cash,
            symbol: a.into(),
            exchange: c.into(),
            currency: b.into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        });
    }
    // Default `SYMBOL.EXCHANGE.CURRENCY` → equity.
    Some(IbkrContract::stk(a, b, c))
}

/// Bidirectional `conId ↔ canonical symbol` map (inbound events are keyed by numeric conId).
#[derive(Default)]
pub struct ConIdMap {
    by_con_id: HashMap<i64, String>,
    by_symbol: HashMap<String, i64>,
}

impl ConIdMap {
    pub fn insert(&mut self, con_id: i64, symbol: &str) {
        self.by_con_id.insert(con_id, symbol.to_string());
        self.by_symbol.insert(symbol.to_string(), con_id);
    }
    pub fn symbol_of(&self, con_id: i64) -> Option<&str> {
        self.by_con_id.get(&con_id).map(|s| s.as_str())
    }
    pub fn con_id_of(&self, symbol: &str) -> Option<i64> {
        self.by_symbol.get(symbol).copied()
    }
}

/// Map IB `contractDetails` fields to vike `SymbolProperties` for the live-`RiskGate` pre-fetch
/// (`vike_exec::RiskLimits::from_properties`, called from `vike_mount::make_engine`'s ibkr arm via
/// [`crate::fetch_ibkr_properties`]).
///
/// - `min_tick` → `tick_size`: already in display units; IB's `priceMagnifier` (bonds) does NOT
///   scale it, so the tick stays `min_tick` verbatim.
/// - `size_increment` → `step_size`, `min_size` → `min_qty`: the order-size grid IB publishes on
///   the same `contractDetails` reply. Older TWS builds may report these as `0.0` ("not sent"),
///   which `RiskLimits::from_properties` folds to unconstrained via `nz_step` — the inert case,
///   byte-identical to a venue that reports no grid.
///
/// `contract_size` stays `0.0` (= the inert `1.0` multiplier): IBKR DOES publish a per-contract
/// multiplier for OPT/FUT, but folding it into the `Account` multiplier grid is the deribit-path
/// follow-up (out of scope for the RiskGate tick/size grid this builds).
pub fn contract_details_to_properties(
    min_tick: f64,
    size_increment: f64,
    min_size: f64,
) -> SymbolProperties {
    SymbolProperties {
        tick_size: min_tick,
        step_size: size_increment,
        min_qty: min_size,
        // Everything else absent: no per-contract max/min-notional, no contract multiplier, a
        // flat grid (`contractDetails` reports ONE `minTick`, no tiers), and no venue taker hold.
        // FRU rather than an exhaustive literal so a new `SymbolProperties` field costs this
        // parser nothing.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_us_equity_simplified() {
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        assert_eq!(c.sec_type, SecType::Stk);
        assert_eq!(c.symbol, "AAPL");
        assert_eq!(c.exchange, "SMART");
        assert_eq!(c.currency, "USD");
    }

    #[test]
    fn forex_pair_maps_to_cash_on_idealpro() {
        let c = parse_simplified("EUR.USD.IDEALPRO").unwrap();
        assert_eq!(c.sec_type, SecType::Cash);
        assert_eq!(c.symbol, "EUR");
        assert_eq!(c.currency, "USD");
        assert_eq!(c.exchange, "IDEALPRO");
    }

    #[test]
    fn sec_type_code_roundtrip() {
        for (st, code) in [
            (SecType::Stk, "STK"),
            (SecType::Opt, "OPT"),
            (SecType::Fut, "FUT"),
            (SecType::Cash, "CASH"),
            (SecType::Ind, "IND"),
            (SecType::Crypto, "CRYPTO"),
        ] {
            assert_eq!(st.as_ib_code(), code);
            assert_eq!(SecType::from_ib_code(code), st);
        }
        assert_eq!(SecType::from_ib_code("BAG"), SecType::Other("BAG".into()));
    }

    #[test]
    fn conid_map_is_bidirectional() {
        let mut m = ConIdMap::default();
        m.insert(265598, "AAPL.SMART.USD");
        assert_eq!(m.symbol_of(265598), Some("AAPL.SMART.USD"));
        assert_eq!(m.con_id_of("AAPL.SMART.USD"), Some(265598));
    }

    #[test]
    fn contract_details_map_to_properties() {
        // A US equity: penny tick, whole-share size grid.
        let f = contract_details_to_properties(0.01, 1.0, 1.0);
        assert_eq!(f.tick_size, 0.01);
        assert_eq!(f.step_size, 1.0, "size_increment → step_size");
        assert_eq!(f.min_qty, 1.0, "min_size → min_qty");
        assert_eq!(f.contract_size, 0.0, "multiplier deferred (deribit-path follow-up)");
        // A futures-style tick with the size grid unreported (older TWS): 0.0 = unconstrained, which
        // `RiskLimits::from_properties` folds to inert — byte-identical to no grid.
        let f2 = contract_details_to_properties(0.25, 0.0, 0.0);
        assert_eq!(f2.tick_size, 0.25);
        assert_eq!(f2.step_size, 0.0);
        assert_eq!(f2.min_qty, 0.0);
    }
}
