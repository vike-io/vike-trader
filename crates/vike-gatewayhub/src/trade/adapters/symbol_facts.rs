//! **The venue facts of ANY symbol of an exchange, from its PUBLIC instrument list** (`TradeUp::SymbolFacts`,
//! decision 0173): what the node needs to admit a symbol the account's engine does not hold yet.
//!
//! * **Credential-free.** The rows come from the instrument catalog the daemon already serves (the same
//!   `exchangeInfo` / `instruments-info` rows the bridges' exec grid parsers read), handed in as an
//!   [`InstrumentList`] by the composition root, so this module names no bridge and no account secret is
//!   touched.
//! * **Bounded.** A full listing per order would be abusive: each venue's listing is kept for
//!   [`LISTING_TTL`], and the fetch is SINGLE-FLIGHT per venue (a second asker waits on the slot's lock and
//!   then reads the fresh listing). A failed fetch is remembered for [`FAILED_TTL`] (a failing or rate-limited venue is not asked again by a flood of orders and previews: each fetch is a full `exchangeInfo`, weight 20 on binance).
//! * **Seven venues, by name** - bybit, binance, aster, okx, hyperliquid, deribit, alpaca - and every other venue
//!   answers [`NOT_ANSWERED`]. (alpaca is the one whose list is NOT public: [`AccountFacts`] lists it with the
//!   asking account's own credentials, under the same bounds; `docs/ops/any-symbol-remaining-venues-plan.md`.
//!   Its symbol is the venue's own spelling: an equity ticker `AAPL`, a crypto pair slashed `BTC/USD`.) The symbol arrives in the node's ENGINE spelling: binance and aster upper-case, `.P` =
//!   the USDS-M perpetual lane, bare = spot; bybit a bare upper-case symbol = the LINEAR perpetual (bybit exec
//!   is linear-only), whose catalog row is `<SYMBOL>.P`; okx the venue's own instId (`ETH-USDT-SWAP`), of a
//!   linear USDT-settled SWAP only, its grid converted from CONTRACTS to BASE units (`okx_swap_in_base`);
//!   hyperliquid the coin name EXACTLY as listed, case kept (`kPEPE`), a CORE perpetual only: a spot pair, a
//!   builder-dex (HIP-3) coin, a delisted coin and an isolated-only coin (the catalog row's venue word) are
//!   each refused by name.
//! * **Linear only, except deribit.** The answer's `contract_value` is `None` (`Linear`, 1.0), so a
//!   perpetual whose row is not linear is refused with the venue's own word rather than priced as linear.
//!   **deribit** (a sixth venue, perpetuals only, rows from the TESTNET book the adapter signs on, not
//!   the public catalog's mainnet) answers `Some(Inverse, 1.0)` for a coin-margined perpetual and `None`
//!   for a linear one: the node refuses the model that is not the engine's, and the adapter again
//!   (`docs/ops/deribit-any-symbol-plan.md`). A dated future, an option and a spot pair are refused by name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{FACTS_LIST_EMPTY, FACTS_LIST_UNAVAILABLE, WireSymbolFacts};
use vike_model::{Contract, ContractValue, Instrument, Margining, NotionalKind, SymbolProperties};

/// The whole instrument list of one venue, or the reason it could not be had. Called with the venue slug.
pub type InstrumentList = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// How long a venue's listing is kept.
pub const LISTING_TTL: Duration = Duration::from_secs(600);

/// How long a venue's FAILED listing fetch is remembered (the answer is the failure, served again).
pub const FAILED_TTL: Duration = Duration::from_secs(30);

/// The answer of a venue this module does not serve (also the trait default's words).
pub const NOT_ANSWERED: &str = "this venue's adapter does not answer symbol facts";

type Listing = Arc<HashMap<String, Instrument>>;
/// One venue's state: the kept listing, and the last failure.
#[derive(Default)]
struct SlotState {
    listing: Option<(Instant, Listing)>,
    failed: Option<(Instant, String)>,
}

type Slot = Arc<Mutex<SlotState>>;

/// See the module doc.
pub struct SymbolFacts {
    list: InstrumentList,
    ttl: Duration,
    failed_ttl: Duration,
    slots: Mutex<HashMap<String, Slot>>,
}

impl SymbolFacts {
    /// Facts over `list`, each venue's listing kept for [`LISTING_TTL`].
    #[must_use]
    pub fn new(list: InstrumentList) -> Self {
        Self::with_ttl(list, LISTING_TTL)
    }

    /// [`SymbolFacts::new`] with an explicit keep time (the tests').
    #[must_use]
    pub fn with_ttl(list: InstrumentList, ttl: Duration) -> Self {
        Self::with_ttls(list, ttl, FAILED_TTL)
    }

    /// [`SymbolFacts::with_ttl`] with an explicit time to remember a failed fetch too.
    #[must_use]
    pub fn with_ttls(list: InstrumentList, ttl: Duration, failed_ttl: Duration) -> Self {
        Self { list, ttl, failed_ttl, slots: Mutex::new(HashMap::new()) }
    }

    /// The facts of `symbol` on `venue`, or the operator's sentence for why there are none.
    pub fn facts(&self, venue: &str, symbol: &str) -> Result<WireSymbolFacts, String> {
        let covers = match venue {
            "binance" | "aster" => "spot and USDS-M perpetuals",
            "bybit" => "linear perpetuals",
            "okx" => "linear USDT-settled SWAP perpetuals",
            "hyperliquid" => "core perpetuals",
            "deribit" => "perpetuals of the testnet book",
            "alpaca" => "US equities and crypto pairs",
            _ => return Err(NOT_ANSWERED.to_string()),
        };
        // Hyperliquid's key is the coin name EXACTLY as the venue lists it (`kPEPE` is not `KPEPE`);
        // the other three venues spell upper case.
        let hyperliquid = venue == "hyperliquid";
        if hyperliquid && let Some(what) = hyperliquid_not_core_perp(symbol) {
            return Err(format!("{NOT_ANSWERED}: hyperliquid `{symbol}` is {what}"));
        }
        let symbol = if hyperliquid { symbol.to_string() } else { symbol.to_ascii_uppercase() };
        let key = if venue == "bybit" {
            format!("{}.P", symbol.strip_suffix(".P").unwrap_or(&symbol))
        } else {
            symbol.clone()
        };
        let listing = self.listing(venue)?;
        let Some(row) = listing.get(&key) else {
            return Err(format!(
                "{venue}: no instrument {symbol} in the venue's instrument list ({covers})"
            ));
        };
        // okx's catalog also lists spot, dated futures and options under their own instIds: only a SWAP
        // is a perpetual lane.
        if venue == "okx" && !matches!(row.contract, Contract::Perp(_)) {
            return Err(format!(
                "okx: {symbol} is a {} instrument, not a SWAP perpetual; the gatewayhub trades \
                 linear SWAP perpetuals only",
                row.asset_class().sql_word()
            ));
        }
        if hyperliquid {
            hyperliquid_refusal(row, &symbol)?;
        }
        if (key.ends_with(".P") || venue == "okx")
            && row.contract.margining() != Some(Margining::Linear)
        {
            return Err(format!(
                "{venue}: {symbol} is {}, not a linear perpetual; the gatewayhub trades linear \
                 perpetuals only",
                row.contract_type.as_deref().unwrap_or("a non-linear contract")
            ));
        }
        // deribit: a PERPETUAL only (an inverse one counts its amount in dollars, a linear one in the
        // base coin; the answer says which), and `contract_size` is the LOT, never a multiplier
        // (decision 0170). The node refuses a perpetual of the other money model than the engine's.
        let mut contract_value = None;
        if venue == "deribit" {
            if !matches!(row.contract, Contract::Perp(_)) {
                return Err(format!(
                    "deribit: {symbol} is {}, not a perpetual; the gatewayhub trades perpetuals only",
                    row.asset_class().sql_word()
                ));
            }
            let Some(value) = ContractValue::of(&row.contract, 1.0) else {
                return Err(format!(
                    "deribit: {symbol} is a quanto contract, whose notional is not valued"
                ));
            };
            contract_value = (value.kind != NotionalKind::Linear).then_some(value);
        }
        let properties =
            if venue == "okx" { okx_swap_in_base(&symbol, row)? } else { row.properties };
        for (what, value) in
            [("tick size", properties.tick_size), ("step size", properties.step_size)]
        {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!(
                    "{venue}: instrument {symbol}: the venue's instrument list carries no {what} \
                     (missing or zero)"
                ));
            }
        }
        // alpaca: the adapter writes every limit and stop price to the CENT (`build_order_body` formats on a
        // fixed 0.01 step), so a price grid finer than a cent would be rounded after the engine judged the
        // price at its own tick (a coin quoted at 0.0002 would rest at 0.00). Only a grid of whole cents is
        // admitted; the mounted symbol's start is not touched.
        if venue == "alpaca" {
            let cents = properties.tick_size / 0.01;
            if cents.round() < 1.0 || (cents - cents.round()).abs() > 1e-6 {
                return Err(format!(
                    "alpaca: {symbol} has a price increment of {}, and this adapter writes a price to \
                     the cent only, so an instrument whose price grid is not whole cents is not admitted",
                    properties.tick_size
                ));
            }
        }
        Ok(WireSymbolFacts { properties, contract_value })
    }

    /// `venue`'s listing: the kept one while fresh, else fetched with the venue's slot held, so a
    /// concurrent asker waits for this fetch instead of making its own.
    fn listing(&self, venue: &str) -> Result<Listing, String> {
        let slot = Arc::clone(
            self.slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(venue.to_string())
                .or_default(),
        );
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, rows)) = held.listing.as_ref()
            && at.elapsed() < self.ttl
        {
            return Ok(Arc::clone(rows));
        }
        if let Some((at, why)) = held.failed.as_ref()
            && at.elapsed() < self.failed_ttl
        {
            return Err(why.clone());
        }
        let fetched = (self.list)(venue)
            .map_err(|why| format!("{venue}: {FACTS_LIST_UNAVAILABLE}: {why}"))
            .and_then(|rows| {
                if rows.is_empty() { Err(format!("{venue}: {FACTS_LIST_EMPTY}")) } else { Ok(rows) }
            });
        match fetched {
            Ok(rows) => {
                let rows: Listing =
                    Arc::new(rows.into_iter().map(|row| (row.raw_symbol.clone(), row)).collect());
                held.listing = Some((Instant::now(), Arc::clone(&rows)));
                held.failed = None;
                Ok(rows)
            }
            Err(why) => {
                held.failed = Some((Instant::now(), why.clone()));
                Err(why)
            }
        }
    }
}

/// **An okx SWAP's grid in BASE units**, from its catalog row. OKX counts size in CONTRACTS (`lotSz`,
/// `minSz` and `maxMktSz` are contract counts, `ctVal` base units per contract) and the catalog row
/// carries `ctVal x ctMult` as `contract_size`; the engine's orders and fills are in BASE, so the three
/// quantity fields scale by it and nothing else does (a tick is quote per base, `min_notional` is
/// quote). The result must equal `vike_okx::perp::properties_in_base` of the mounted path's exec row
/// (`symbol_facts_tests`' `okx_facts_equal_the_mounted_paths_base_unit_grid` holds it), and its
/// `contract_size` is 0 (absent): the scaling is DONE, so setting it too would multiply a notional
/// twice.
///
/// Refused, in words: a contract with no usable size, a contract counted in something other than the
/// base asset (the conversion would be meaningless), and a swap settled in anything but USDT (first
/// version: how the engine's cash treats a second settle currency is not checked).
///
/// ⚠ The catalog row has the PRODUCT `ctVal x ctMult`, so a `ctMult` other than 1 cannot be seen here:
/// the adapter refuses such an instrument itself when it builds the client (`crates/bridges/okx/src/exec.rs`'s
/// `foreign_rest`), so the order is rejected, never mis-sized.
fn okx_swap_in_base(symbol: &str, row: &Instrument) -> Result<SymbolProperties, String> {
    let Contract::Perp(spec) = &row.contract else {
        return Err(format!("okx: {symbol} is not a SWAP perpetual"));
    };
    if !spec.settle.eq_ignore_ascii_case("USDT") {
        return Err(format!(
            "okx: {symbol} settles in {}; the gatewayhub admits USDT-settled swaps only for now",
            spec.settle
        ));
    }
    if let Some(ccy) = spec.value_ccy.as_deref()
        && !ccy.eq_ignore_ascii_case(&row.base)
    {
        return Err(format!(
            "okx: {symbol}: a contract is counted in {ccy}, not in the base asset {}",
            row.base
        ));
    }
    let ct = row.properties.contract_size;
    if !ct.is_finite() || ct <= 0.0 {
        return Err(format!(
            "okx: instrument {symbol}: the venue's instrument list carries no contract size \
             (missing or zero)"
        ));
    }
    let p = row.properties;
    Ok(SymbolProperties {
        step_size: p.step_size * ct,
        min_qty: p.min_qty * ct,
        max_qty: p.max_qty * ct,
        contract_size: 0.0,
        ..p
    })
}

/// What a Hyperliquid symbol's TEXT says it is when it is not a core perpetual: a spot pair
/// (`BASE/QUOTE`, `@N`) or a builder-dex coin (`dex:COIN`). Those are asked of nothing.
fn hyperliquid_not_core_perp(symbol: &str) -> Option<&'static str> {
    if symbol.contains('/') || symbol.starts_with('@') {
        Some("a spot pair; this lane admits core perpetuals only")
    } else if symbol.contains(':') {
        Some("a builder-dex (HIP-3) coin; this lane admits core perpetuals only")
    } else {
        None
    }
}

/// The venue word the hyperliquid catalog writes on a delisted perp's row
/// (`vike_hyperliquid::feeds::catalog::VENUE_WORD_DELISTED`; a test with the `data` feature holds them equal).
const HL_DELISTED: &str = "isDelisted";
/// The venue word on a perp whose cross margin is unavailable (`VENUE_WORD_ISOLATED_ONLY`).
const HL_ISOLATED_ONLY: &str = "onlyIsolated";

/// Why a hyperliquid catalog row cannot be admitted: not a core perpetual (the belt behind the text
/// check), delisted, or isolated-only. The engine trades on cross margin and the adapter sends no
/// leverage or margin-mode action, so an isolated-only coin would be refused by the venue.
fn hyperliquid_refusal(row: &Instrument, symbol: &str) -> Result<(), String> {
    if !matches!(row.contract, Contract::Perp(_)) || row.wire.group.is_some() {
        return Err(format!(
            "{NOT_ANSWERED}: hyperliquid `{symbol}` is not a core perpetual; this lane admits core \
             perpetuals only"
        ));
    }
    match row.contract_type.as_deref() {
        Some(HL_DELISTED) => {
            Err(format!("hyperliquid: `{symbol}` is delisted; no order can open on it"))
        }
        Some(HL_ISOLATED_ONLY) => Err(format!(
            "hyperliquid: `{symbol}` is isolated-margin only; this account trades cross margin and \
             cannot open it"
        )),
        _ => Ok(()),
    }
}

/// One account's way to list its venue's instruments: it owns what the call needs (alpaca: a catalog
/// holding the account's sandbox config), so it can be kept past the borrow of the credentials.
pub type AccountLister = Arc<dyn Fn() -> Result<Vec<Instrument>, String> + Send + Sync>;

/// **The facts of a venue whose instrument list sits behind the asking account's OWN credentials**
/// (alpaca: `GET /v1/assets` takes the account's Bearer; decision 0174 verdict 1 says "public list",
/// which no alpaca endpoint is). It is [`SymbolFacts`] with the listing source supplied per ask, and
/// everything that bounds a public lookup bounds this one: the listing is kept for [`LISTING_TTL`]
/// and fetched single-flight, a failed fetch is remembered for [`FAILED_TTL`], so a flood of lookups
/// costs the account ONE listing request per ten minutes, not one per symbol. The listing is the
/// venue's universe, the same for every account of one tier, so it is shared: the latest asker's
/// lister is the one a refresh uses. The credentials never leave the lister.
pub struct AccountFacts {
    facts: SymbolFacts,
    lister: Arc<Mutex<Option<AccountLister>>>,
}

impl AccountFacts {
    /// A source with no lister yet: [`Self::facts`] gives it one per ask.
    #[must_use]
    pub fn new() -> Self {
        let lister: Arc<Mutex<Option<AccountLister>>> = Arc::default();
        let read = Arc::clone(&lister);
        let facts = SymbolFacts::new(Arc::new(move |venue| {
            let held = read.lock().unwrap_or_else(PoisonError::into_inner).clone();
            match held {
                Some(list) => list(),
                None => Err(format!("{venue}: no account was offered to list its instruments")),
            }
        }));
        Self { facts, lister }
    }

    /// The facts of `symbol` on `venue`, listing through `lister` when the kept listing is stale.
    pub fn facts(
        &self,
        venue: &str,
        symbol: &str,
        lister: AccountLister,
    ) -> Result<WireSymbolFacts, String> {
        *self.lister.lock().unwrap_or_else(PoisonError::into_inner) = Some(lister);
        self.facts.facts(venue, symbol)
    }
}

impl Default for AccountFacts {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SymbolFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolFacts").field("ttl", &self.ttl).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "symbol_facts_tests.rs"]
mod symbol_facts_tests;
