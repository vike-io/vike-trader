//! **The venue facts of ANY symbol of an exchange, from its PUBLIC instrument list** (`TradeUp::SymbolFacts`,
//! decision 0173): what the node needs to admit a symbol the account's engine does not hold yet.
//!
//! * **Credential-free.** The rows come from the instrument catalog the daemon already serves (the same
//!   `exchangeInfo` / `instruments-info` rows the bridges' exec grid parsers read), handed in as an
//!   [`InstrumentList`] by the composition root, so this module names no bridge and no account secret is
//!   touched.
//! * **Bounded.** A full listing per order would be abusive: each venue's listing is kept for
//!   [`LISTING_TTL`], and the fetch is SINGLE-FLIGHT per venue (a second asker waits on the slot's lock and
//!   then reads the fresh listing). A failed fetch is remembered for [`FAILED_TTL`] (a failing or rate-limited venue is not asked again by a flood of orders and previews: each fetch is a full `exchangeInfo`, weight 20 on binance). The credentialed sources whose every fetch is a whole authenticated session or connect (ctrader, ig, ibkr) remember a failure for [`SESSION_FAILED_TTL`] instead.
//! * **Rate-bounded per venue.** A per-symbol lookup is one upstream request, so each venue has a token
//!   bucket ([`LOOKUP_BURST`], one token back per [`LOOKUP_REFILL`]: 20 lookups per 10 s) in front of it. A
//!   refused token answers the transient [`FACTS_BUSY`] with no network call and is NOT remembered as the
//!   symbol's failure; a symbol already kept (or in its failure memory) costs no token. The slot table is
//!   a hard cap too ([`MAX_SLOTS`]).
//! * **One symbol at a time where the venue filters** (`answered_one_at_a_time`, a [`SymbolLister`]):
//!   binance and aster SPOT (`exchangeInfo?symbol=`, a few hundred bytes against ~17.6 MB), bybit
//!   (`instruments-info?symbol=`) and alpaca (`GET /v1/assets/{symbol}`, against the several-MB active
//!   list). Such a symbol is kept and single-flighted under its OWN slot, so a slow read holds up no other
//!   symbol and no other venue; "the venue has no such symbol" is remembered for [`FAILED_TTL`] only. The
//!   USDS-M perpetual lane (fapi ignores `symbol=`) and every other venue still read one list per venue.
//! * **A failed or half read is a failure, never an unknown symbol.** A source that could not read all it
//!   needed returns an `Err` (not a partial `Ok`): it is worded [`FACTS_LIST_UNAVAILABLE`] (transient, so the
//!   node does not remember it as a verdict), kept for [`FAILED_TTL`] and never stored as the listing. The
//!   picker's catalog is tolerant of one endpoint failing on purpose; this module's sources are not.
//! * **Nine venues, by name** - bybit, binance, aster, okx, hyperliquid, deribit, alpaca, ctrader, oanda - and every
//!   other venue answers [`NOT_ANSWERED`]. (alpaca, ctrader and oanda are the three whose lists are NOT public:
//!   [`AccountFacts`] lists them with the asking account's own credentials, under the same bounds;
//!   `docs/ops/any-symbol-remaining-venues-plan.md`. alpaca's symbol is the venue's own spelling: an equity ticker
//!   `AAPL`, a crypto pair slashed `BTC/USD`, read ONE asset at a time. ctrader's is the broker's name EXACTLY as
//!   the account's symbol list spells it (`EURUSD`; names are case-sensitive), looked up in that account's WHOLE
//!   listing ([`AccountFacts::listed`]) and fenced by [`CtraderFacts`]: a forex pair quoted in the account's
//!   deposit currency, nothing else (or, for a node that converts a quote currency, a forex pair quoted in
//!   another one, with its rate: [`CtraderFacts::facts_quoted`]). oanda's arrives as the engine spells it (`GBPUSD`, `SPX500_USD`) and is
//!   looked up as OANDA's `GBP_USD` in that account's WHOLE listing too, fenced by [`OandaFacts`]: the listing
//!   is the account's own and so is its currency. ig is not here: its facts are per epic.) The symbol arrives in the node's ENGINE spelling: binance and aster upper-case, `.P` =
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

use vike_gatewayhub_client::proto::{
    FACTS_BUSY, FACTS_LIST_EMPTY, FACTS_LIST_UNAVAILABLE, WireSymbolFacts,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, NotionalKind, SymbolProperties};

/// The whole instrument list of one venue, or the reason it could not be had. Called with the venue slug.
pub type InstrumentList = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// How long a venue's listing is kept.
pub const LISTING_TTL: Duration = Duration::from_secs(600);

/// How long a venue's FAILED listing fetch is remembered (the answer is the failure, served again).
pub const FAILED_TTL: Duration = Duration::from_secs(30);

/// How long a FAILED fetch is remembered by a CREDENTIALED source whose every fetch costs a whole
/// authenticated session or connect (ctrader: application auth + account auth; ig: a login; ibkr: a data
/// connect from a budget of six an hour). At [`FAILED_TTL`] an outage would dial a new session every 30 s
/// (about twenty in ten minutes); at this one, about two. The public and per-symbol REST sources keep
/// [`FAILED_TTL`].
pub const SESSION_FAILED_TTL: Duration = Duration::from_secs(300);

/// The answer of a venue this module does not serve (also the trait default's words).
pub const NOT_ANSWERED: &str = "this venue's adapter does not answer symbol facts";

/// One venue's rows for ONE symbol (the venue's own spelling, see `answered_one_at_a_time`): the venue
/// slug and the symbol it is asked for. An empty `Ok` is the venue's "no such symbol"; a failed or partial
/// read MUST be an `Err` (it is remembered briefly and worded as a failure, never as an unknown symbol).
pub type SymbolLister = Arc<dyn Fn(&str, &str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// Slots kept (one slot per venue listing or per looked-up symbol): a HARD cap, so a flood of distinct
/// symbols cannot grow the map without bound. A full map drops the expired slots, then the OLDEST finished
/// one; a slot whose read is in flight is never dropped (and if every slot is, the ask is answered "busy").
const MAX_SLOTS: usize = 512;

/// How many per-symbol upstream lookups one venue may make in a burst, from a full bucket.
pub const LOOKUP_BURST: u32 = 10;

/// One lookup token comes back this often: with [`LOOKUP_BURST`], 20 lookups per 10 s per venue. Every
/// per-symbol lookup is one upstream request (binance `exchangeInfo?symbol=`, bybit `instruments-info`,
/// alpaca `/v1/assets/{symbol}`) against a per-IP weight the gatewayhub's data REST shares, and only the
/// number IN FLIGHT was bounded before (`session.rs`'s `MAX_SYMBOL_FACTS_INFLIGHT`), not how many in a minute.
pub const LOOKUP_REFILL: Duration = Duration::from_millis(500);

/// A venue's lookup tokens: `tokens` as of `at`, topped up at one per refill, never above the burst.
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    /// Take one token as of `now`; `false` = none left (nothing is spent). `now` is a parameter so a test
    /// drives the clock.
    fn take(&mut self, now: Instant, burst: u32, refill: Duration) -> bool {
        if !refill.is_zero() {
            let gained =
                now.saturating_duration_since(self.at).as_secs_f64() / refill.as_secs_f64();
            self.tokens = (self.tokens + gained).min(f64::from(burst));
        } else {
            self.tokens = f64::from(burst);
        }
        self.at = now;
        let granted = self.tokens >= 1.0;
        if granted {
            self.tokens -= 1.0;
        }
        granted
    }
}

type Listing = Arc<HashMap<String, Instrument>>;
/// One slot's state: the kept rows (taken at, kept for, rows), and the last failure.
#[derive(Default)]
struct SlotState {
    listing: Option<(Instant, Duration, Listing)>,
    failed: Option<(Instant, String)>,
}

impl SlotState {
    /// When this slot last finished a read (rows or failure); `None` = never.
    fn taken_at(&self) -> Option<Instant> {
        self.listing.as_ref().map(|(at, ..)| *at).max(self.failed.as_ref().map(|(at, _)| *at))
    }
}

type Slot = Arc<Mutex<SlotState>>;

/// See the module doc.
pub struct SymbolFacts {
    list: InstrumentList,
    /// How a venue that can answer for ONE symbol is asked (`None` = every venue reads its whole list).
    one: Option<SymbolLister>,
    ttl: Duration,
    failed_ttl: Duration,
    slots: Mutex<HashMap<String, Slot>>,
    /// Per venue, the tokens a PER-SYMBOL upstream lookup spends (`burst`, one back per `refill`).
    buckets: Mutex<HashMap<String, Bucket>>,
    burst: u32,
    refill: Duration,
    /// Whether this source is an [`AccountFacts`]' inner one. ctrader and oanda are answered only then:
    /// their fences (the account's currency, the instrument class) are [`CtraderFacts`]' and
    /// [`OandaFacts`], so a PUBLIC list of their rows handed to a plain `SymbolFacts` would admit what
    /// they refuse.
    account_scoped: bool,
}

/// Whether `venue` is asked for the ONE symbol `key` (the catalog spelling: `.P` = the perpetual lane)
/// when a [`SymbolLister`] is set, instead of for its whole list.
///
/// * binance, aster: a SPOT symbol (`exchangeInfo?symbol=`, a few hundred bytes against ~17.6 MB). The
///   perpetual lane stays one bulk read (fapi ignores `symbol=`), kept per venue and shared by every `.P` ask.
/// * bybit: every symbol (`instruments-info?symbol=`).
/// * alpaca: every symbol (`GET /v1/assets/{symbol}`).
fn answered_one_at_a_time(venue: &str, key: &str) -> bool {
    match venue {
        "binance" | "aster" => !key.ends_with(".P"),
        "bybit" | "alpaca" => true,
        _ => false,
    }
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
        Self {
            list,
            one: None,
            ttl,
            failed_ttl,
            slots: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            burst: LOOKUP_BURST,
            refill: LOOKUP_REFILL,
            account_scoped: false,
        }
    }

    /// These facts with an explicit per-venue lookup rate (the tests'): `burst` tokens, one back per `refill`.
    #[must_use]
    pub fn with_lookup_rate(mut self, burst: u32, refill: Duration) -> Self {
        self.burst = burst;
        self.refill = refill;
        self
    }

    /// Take one of `venue`'s lookup tokens as of `now`.
    fn take_lookup(&self, venue: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let bucket = buckets
            .entry(venue.to_string())
            .or_insert_with(|| Bucket { tokens: f64::from(self.burst), at: now });
        bucket.take(now, self.burst, self.refill)
    }

    /// These facts, with `one` answering the asks `answered_one_at_a_time` names (a symbol is then
    /// kept and single-flighted per `(venue, symbol)`, so an unrelated symbol never waits on it).
    #[must_use]
    pub fn with_symbol_lister(mut self, one: SymbolLister) -> Self {
        self.one = Some(one);
        self
    }

    /// This source as an [`AccountFacts`]' inner one (the only kind that answers ctrader and oanda).
    fn account_scoped(mut self) -> Self {
        self.account_scoped = true;
        self
    }

    /// The facts of `symbol` on `venue`, or the operator's sentence for why there are none.
    pub fn facts(&self, venue: &str, symbol: &str) -> Result<WireSymbolFacts, String> {
        self.facts_via(venue, symbol, self.one.as_ref(), None, &|_| Ok(()))
    }

    /// [`Self::facts`] with the per-symbol source (`one`) or the whole-list source (`whole`, instead of
    /// this source's own) supplied by the caller (the credentialed venue's, which holds the asking
    /// account's credentials and so cannot be kept here), and `check` run on the
    /// instrument's row once it is found, before any grid check: the hook the account-scoped venues' own
    /// fences (a currency, an instrument class) use, so a refusal names the fundamental reason instead
    /// of a grid detail.
    fn facts_via(
        &self,
        venue: &str,
        symbol: &str,
        one: Option<&SymbolLister>,
        whole: Option<&InstrumentList>,
        check: &dyn Fn(&Instrument) -> Result<(), String>,
    ) -> Result<WireSymbolFacts, String> {
        let covers = match venue {
            "binance" | "aster" => "spot and USDS-M perpetuals",
            "bybit" => "linear perpetuals",
            "okx" => "linear USDT-settled SWAP perpetuals",
            "hyperliquid" => "core perpetuals",
            "deribit" => "perpetuals of the testnet book",
            "alpaca" => "US equities and crypto pairs",
            "ctrader" if self.account_scoped => "forex pairs of the account's own symbol list",
            "oanda" if self.account_scoped => "FX and CFD instruments of the practice account",
            _ => return Err(NOT_ANSWERED.to_string()),
        };
        // Hyperliquid's key is the coin name EXACTLY as the venue lists it (`kPEPE` is not `KPEPE`), and
        // so is ctrader's (the broker's names are case-sensitive: `Wheat_H6`); the others spell upper case.
        let hyperliquid = venue == "hyperliquid";
        if hyperliquid && let Some(what) = hyperliquid_not_core_perp(symbol) {
            return Err(format!("{NOT_ANSWERED}: hyperliquid `{symbol}` is {what}"));
        }
        let case_kept = hyperliquid || venue == "ctrader";
        let symbol = if case_kept { symbol.to_string() } else { symbol.to_ascii_uppercase() };
        let key = if venue == "bybit" {
            format!("{}.P", symbol.strip_suffix(".P").unwrap_or(&symbol))
        } else if venue == "oanda" {
            // The engine spells a pair as the mount does (`GBPUSD`); OANDA's list names it `GBP_USD`.
            vike_catalog::to_oanda_instrument(&symbol)
        } else {
            symbol.clone()
        };
        let single = one.filter(|_| answered_one_at_a_time(venue, &key));
        let listing = match single {
            Some(lister) => {
                // bybit's catalog row is `<SYMBOL>.P`, its venue symbol is the bare `<SYMBOL>`.
                let wanted = if venue == "bybit" {
                    key.strip_suffix(".P").unwrap_or(&key)
                } else {
                    key.as_str()
                };
                self.symbol_listing(venue, wanted, lister)?
            }
            None => self.listing(venue, whole.unwrap_or(&self.list))?,
        };
        let Some(row) = listing.get(&key) else {
            return Err(if single.is_some() && venue == "alpaca" {
                format!(
                    "alpaca: unknown asset {symbol}: the venue has no tradable US equity or crypto \
                     pair of that name"
                )
            } else {
                format!("{venue}: no instrument {symbol} in the venue's instrument list ({covers})")
            });
        };
        check(row)?;
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
        // (oanda's two fences, the account's currency and what the adapter can write, are `OandaFacts`'.)
        Ok(WireSymbolFacts { properties, contract_value, ..WireSymbolFacts::default() })
    }

    /// `venue`'s whole listing: the kept one while fresh, else fetched with the venue's slot held, so a
    /// concurrent asker waits for this fetch instead of making its own.
    fn listing(&self, venue: &str, list: &InstrumentList) -> Result<Listing, String> {
        self.kept(venue, venue, false, || list(venue))
    }

    /// `venue`'s rows for the ONE symbol `wanted`, kept and single-flighted under their own slot: the
    /// slot's lock is held across this symbol's fetch only, so an ask for any other symbol (or venue) never
    /// waits on it. A venue that answers with no row is remembered for [`FAILED_TTL`] only (the symbol may
    /// be listed a minute later), not for the listing's keep time.
    fn symbol_listing(
        &self,
        venue: &str,
        wanted: &str,
        lister: &SymbolLister,
    ) -> Result<Listing, String> {
        self.kept(&format!("{venue}\u{1f}{wanted}"), venue, true, || lister(venue, wanted))
    }

    /// The rows kept under `slot_key` while fresh, else `fetch`ed with that slot held. `one_symbol`: an
    /// empty answer is "no such symbol" (kept briefly), not an empty LIST (a failure).
    fn kept(
        &self,
        slot_key: &str,
        venue: &str,
        one_symbol: bool,
        fetch: impl FnOnce() -> Result<Vec<Instrument>, String>,
    ) -> Result<Listing, String> {
        let slot = self.slot(slot_key, venue)?;
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, keep, rows)) = held.listing.as_ref()
            && at.elapsed() < *keep
        {
            return Ok(Arc::clone(rows));
        }
        if let Some((at, why)) = held.failed.as_ref()
            && at.elapsed() < self.failed_ttl
        {
            return Err(why.clone());
        }
        // A per-symbol lookup is one upstream request: spend the venue's token first. A refusal makes no
        // network call and is NOT remembered (`held.failed` stays as it was): it is "not now", not a verdict.
        if one_symbol && !self.take_lookup(venue, Instant::now()) {
            return Err(format!(
                "{venue}: {FACTS_BUSY} (per-symbol lookups are rate-limited to protect the venue's \
                 request budget; retry shortly)"
            ));
        }
        let fetched = fetch()
            .map_err(|why| format!("{venue}: {FACTS_LIST_UNAVAILABLE}: {why}"))
            .and_then(|rows| {
                if rows.is_empty() && !one_symbol {
                    Err(format!("{venue}: {FACTS_LIST_EMPTY}"))
                } else {
                    Ok(rows)
                }
            });
        match fetched {
            Ok(rows) => {
                let keep = if rows.is_empty() { self.failed_ttl } else { self.ttl };
                let rows: Listing =
                    Arc::new(rows.into_iter().map(|row| (row.raw_symbol.clone(), row)).collect());
                held.listing = Some((Instant::now(), keep, Arc::clone(&rows)));
                held.failed = None;
                Ok(rows)
            }
            Err(why) => {
                held.failed = Some((Instant::now(), why.clone()));
                Err(why)
            }
        }
    }

    /// The slot named `key`, made if absent. Only the map is locked here, and only briefly (never across
    /// a fetch). At [`MAX_SLOTS`] the slots whose rows and failure have both expired are dropped first,
    /// then (all still fresh) the OLDEST finished one; a slot whose lock is taken is mid-fetch and stays,
    /// and if every slot is, the ask is answered "busy" (not now, not remembered).
    fn slot(&self, key: &str, venue: &str) -> Result<Slot, String> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.len() >= MAX_SLOTS && !slots.contains_key(key) {
            slots.retain(|_, slot| match slot.try_lock() {
                Ok(state) => {
                    state.listing.as_ref().is_some_and(|(at, keep, _)| at.elapsed() < *keep)
                        || state
                            .failed
                            .as_ref()
                            .is_some_and(|(at, _)| at.elapsed() < self.failed_ttl)
                }
                Err(_) => true,
            });
            if slots.len() >= MAX_SLOTS {
                let oldest = slots
                    .iter()
                    .filter_map(|(k, slot)| match slot.try_lock() {
                        Ok(state) => Some((k, state.taken_at())),
                        Err(std::sync::TryLockError::Poisoned(p)) => {
                            Some((k, p.into_inner().taken_at()))
                        }
                        Err(std::sync::TryLockError::WouldBlock) => None,
                    })
                    .min_by_key(|(_, at)| *at)
                    .map(|(k, _)| k.clone());
                let Some(oldest) = oldest else {
                    return Err(format!(
                        "{venue}: {FACTS_BUSY} (every slot of the symbol table is mid-lookup; retry \
                         shortly)"
                    ));
                };
                slots.remove(&oldest);
            }
        }
        Ok(Arc::clone(slots.entry(key.to_string()).or_default()))
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

/// One account's way to read ONE symbol's rows from its venue (the argument is the venue's symbol): it
/// owns what the call needs (alpaca: a catalog holding the account's sandbox config), so it can be kept
/// past the borrow of the credentials.
pub type AccountLister = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// **The facts of a venue whose instruments sit behind the asking account's OWN credentials** (alpaca:
/// `GET /v1/assets/{symbol}` takes the account's Bearer; decision 0174 verdict 1 says "public list", which
/// no alpaca endpoint is). It is [`SymbolFacts`] with the symbol source supplied per ask, and everything
/// that bounds a public lookup bounds this one: an asset is kept for [`LISTING_TTL`] and fetched
/// single-flight per `(account, symbol)`, a failed fetch is remembered for [`FAILED_TTL`], and "no such
/// asset" for [`FAILED_TTL`] too. ⚠ The WHOLE asset list (several MB) is never read here: one lookup is
/// one small request. Everything is kept PER ACCOUNT (the caller's `account` key): a failure (an account
/// with no credentials reads nothing, and that refusal is its own) is never handed to another account, and
/// a fetch always runs with the credentials of the account it is for. The credentials never leave the
/// lister.
///
/// **Two ways to read, one set of bounds.** [`Self::facts`] reads ONE symbol per request (alpaca);
/// [`Self::listed`] reads the account's WHOLE list per request, with the account's currency (ctrader, oanda:
/// no filter exists, and the currency is the account's too). Both are kept per account.
pub struct AccountFacts {
    accounts: Mutex<HashMap<String, Arc<AccountSlot>>>,
    /// How long a failed fetch is remembered (by account slot): [`FAILED_TTL`], or [`SESSION_FAILED_TTL`]
    /// for a source whose fetch is a whole authenticated session (ctrader).
    failed_ttl: Duration,
}

impl Default for AccountFacts {
    fn default() -> Self {
        Self::with_failed_ttl(FAILED_TTL)
    }
}

/// One account's list of ALL its instruments and its currency, read with its own credentials (ctrader's
/// one protobuf session, oanda's `GET /summary` + `GET /instruments`).
pub type CurrencyLister = Arc<dyn Fn() -> Result<(String, Vec<Instrument>), String> + Send + Sync>;

/// One account's state in [`AccountFacts`]: the bounded source and the currency of its last whole listing.
/// ⚠ It holds NO reader of the account's list: that holds the account's grant, so [`AccountFacts::listed`]
/// takes it per ask and drops it when the ask returns.
struct AccountSlot {
    facts: SymbolFacts,
    /// The currency of the last successful whole listing (written by the per-ask reader `listed` builds).
    currency: Arc<Mutex<Option<String>>>,
}

impl AccountSlot {
    fn open(failed_ttl: Duration) -> Self {
        let facts = SymbolFacts::with_ttls(
            // Reached only by a venue asked through neither reader: `facts` answers per symbol, `listed`
            // brings its own list.
            Arc::new(|venue| {
                Err(format!("{venue}: this venue's assets are read one symbol at a time"))
            }),
            LISTING_TTL,
            failed_ttl,
        )
        .account_scoped();
        Self { facts, currency: Arc::default() }
    }
}

impl AccountFacts {
    /// A source with no account yet: [`Self::facts`] opens one per account key it is asked for.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// [`Self::new`] remembering a failed fetch for `failed_ttl` (a source whose fetch is a session).
    #[must_use]
    pub fn with_failed_ttl(failed_ttl: Duration) -> Self {
        Self { accounts: Mutex::new(HashMap::new()), failed_ttl }
    }

    /// The slot of `account`, opened on first use.
    fn slot(&self, account: &str) -> Arc<AccountSlot> {
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            accounts
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(AccountSlot::open(self.failed_ttl))),
        )
    }

    /// The facts of `symbol` on `venue` for `account` (any stable text that names ONE account: label and
    /// tier), reading the symbol through `lister` when that account's kept rows are stale.
    pub fn facts(
        &self,
        account: &str,
        venue: &str,
        symbol: &str,
        lister: AccountLister,
    ) -> Result<WireSymbolFacts, String> {
        let one: SymbolLister = Arc::new(move |_venue, symbol| lister(symbol));
        self.slot(account).facts.facts_via(venue, symbol, Some(&one), None, &|_| Ok(()))
    }

    /// The facts of `symbol` on `venue` for `account`, from the account's WHOLE list read through
    /// `lister` when the kept one is stale (one read per [`LISTING_TTL`], single-flight, a failure
    /// remembered). `check` runs on the instrument's row with the account's currency once the row is
    /// found, before any grid check (the venue's own fence: `SymbolFacts::facts_via`); with no currency
    /// known, no row is admitted. ⚠ `lister` holds the account's grant: it is used for THIS ask and
    /// dropped when it returns (nothing in a slot keeps it).
    pub fn listed(
        &self,
        account: &str,
        venue: &str,
        symbol: &str,
        lister: CurrencyLister,
        check: &dyn Fn(&Instrument, &str) -> Result<(), String>,
    ) -> Result<WireSymbolFacts, String> {
        let slot = self.slot(account);
        let currency = Arc::clone(&slot.currency);
        let whole: InstrumentList = Arc::new(move |_venue| {
            let (deposit, rows) = lister()?;
            *currency.lock().unwrap_or_else(PoisonError::into_inner) = Some(deposit);
            Ok(rows)
        });
        slot.facts.facts_via(venue, symbol, None, Some(&whole), &|row| {
            let held = slot.currency.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let Some(currency) = held else {
                // Unreachable after a listing, and fail-closed if it ever is: no currency, no admission.
                return Err(format!(
                    "{venue}: the account's currency is not known, so a quote currency cannot be \
                     checked against it"
                ));
            };
            check(row, &currency)
        })
    }
}

/// **The per-account facts (`T`: [`CtraderFacts`], [`OandaFacts`]) of every account of one venue, each kept for the
/// CREDENTIALS it was read with.** The map is keyed by the wire account text (`""` = the default account),
/// and each entry carries a non-reversible fingerprint of the account id behind it (`None` = the venue has
/// none, or the account holds no credentials). An operator who swaps the credentials under one label
/// (another account, with its own instrument list and its own currency) changes the fingerprint, and the
/// next ask gets a FRESH `T`: otherwise the old account's listing and currency would answer for up to
/// [`LISTING_TTL`], and the currency fence would judge a pair against a currency the account no longer
/// holds. Only the fingerprint is kept: no token, no account id.
pub struct FactsByAccount<T> {
    held: Mutex<HashMap<String, Held<T>>>,
}

/// One label's facts and the fingerprint of the credentials they were read with.
type Held<T> = (Option<u64>, Arc<T>);

impl<T> Default for FactsByAccount<T> {
    fn default() -> Self {
        Self { held: Mutex::new(HashMap::new()) }
    }
}

impl<T: Default> FactsByAccount<T> {
    /// The facts of `account` read with the credentials whose fingerprint is `fingerprint`: the held ones
    /// while the fingerprint is unchanged, a fresh one (replacing the held one, so the map never outgrows
    /// the accounts) when it changed.
    pub fn of(&self, account: &str, fingerprint: Option<u64>) -> Arc<T> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = held.entry(account.to_string()).or_insert_with(|| (fingerprint, Arc::default()));
        if slot.0 != fingerprint {
            *slot = (fingerprint, Arc::default());
        }
        Arc::clone(&slot.1)
    }
}

/// A venue's admission fence over one instrument row and the account's currency: `Some(why)` refuses.
pub type RowFence = fn(&Instrument, &str) -> Option<String>;

/// **ctrader's facts, for ONE account**: [`AccountFacts::listed`] (the account's own listing, one session per
/// ten minutes, single-flight, a failure remembered) plus the account's deposit currency and the venue's
/// row fence. A ctrader symbol list is a protobuf session's answer for ONE account (ids are per server, the
/// deposit currency per account), so unlike alpaca's shared listing there is one of these per ctrader account
/// ([`FactsByAccount`], which `BridgeTrade` keeps).
///
/// **The fence is `fence(row, deposit)`**, handed in by the composition (the ctrader bridge's
/// `any_symbol_refusal`, so this module names no bridge): the engine's ledger is one currency and nothing
/// converts a quote currency (`docs/ops/any-symbol-remaining-venues-plan.md`, blocker 1), so a row is
/// admitted only when its `qty x price` IS the real exposure, and it is run BEFORE the grid checks so
/// a refusal names the fundamental reason (a JPY pair would otherwise be refused for its three-decimal
/// price, which is true and not the point). The currency is that of the last successful listing; if it
/// is somehow absent the instrument is refused, never admitted unchecked.
pub struct CtraderFacts {
    facts: AccountFacts,
}

impl Default for CtraderFacts {
    /// A failed listing is a failed authenticated session: it is remembered for [`SESSION_FAILED_TTL`].
    fn default() -> Self {
        Self { facts: AccountFacts::with_failed_ttl(SESSION_FAILED_TTL) }
    }
}

impl CtraderFacts {
    /// [`CtraderFacts::default`] remembering a failure for `failed_ttl` (the tests').
    #[must_use]
    pub fn with_failed_ttl(failed_ttl: Duration) -> Self {
        Self { facts: AccountFacts::with_failed_ttl(failed_ttl) }
    }
}

impl CtraderFacts {
    /// The facts of `symbol` (the broker's name, case kept), listing through `lister` when the kept
    /// listing is stale and fencing the row with `fence`.
    pub fn facts(
        &self,
        symbol: &str,
        lister: CurrencyLister,
        fence: RowFence,
    ) -> Result<WireSymbolFacts, String> {
        self.facts.listed("ctrader", "ctrader", symbol, lister, &|row, deposit| {
            fence(row, deposit).map_or(Ok(()), Err)
        })
    }

    /// [`Self::facts`] for a node that CONVERTS a quote currency (`TradeUp::SymbolFacts::quoted`, decision 0183,
    /// phase 1): a forex pair quoted in the deposit currency is answered exactly as before; one quoted in
    /// ANOTHER is answered with its quote currency and the rate `rate` reads for it (asked NOW: only the
    /// listing is kept, never a rate), instead of being refused for that alone. `fence` is the converting
    /// fence (`vike_ctrader::catalog::quoted_symbol_refusal`: every refusal but the foreign quote), and
    /// `rate` is called as `(the row's QUOTE CURRENCY, the deposit currency)` (the [`RateSource`] contract,
    /// whose first argument is an instrument for OANDA and a currency here: a broker's symbol name does not
    /// say its quote). A row that names no quote, or one `rate` cannot price, is refused: no rate, no admission.
    pub fn facts_quoted(
        &self,
        symbol: &str,
        lister: CurrencyLister,
        fence: RowFence,
        rate: &RateSource,
    ) -> Result<WireSymbolFacts, String> {
        // The row's quote currency and the account's deposit currency, kept from the row check.
        let seen: Mutex<Option<(String, String)>> = Mutex::new(None);
        let mut wire =
            self.facts.listed("ctrader", "ctrader", symbol, lister, &|row, deposit| {
                if let Some(why) = fence(row, deposit) {
                    return Err(why);
                }
                *seen.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((row.quote.trim().to_ascii_uppercase(), deposit.to_string()));
                Ok(())
            })?;
        let (quote, deposit) =
            seen.into_inner().unwrap_or_else(PoisonError::into_inner).ok_or_else(|| {
                "ctrader: the account's currency is not known, so a quote currency cannot be \
                 converted"
                    .to_string()
            })?;
        if let Some(quoted) = rate(&quote, &deposit)? {
            wire.quote_ccy = Some(quoted.quote_ccy);
            wire.quote_rate = Some(quoted.account_per_quote);
            wire.rate_as_of_ms = Some(quoted.as_of_ms);
        }
        Ok(wire)
    }
}

/// **oanda's facts, for ONE account**: [`AccountFacts::listed`] (the account's own listing, one request pair
/// per ten minutes, single-flight, a failure remembered) plus the account's HOME CURRENCY (`GET /summary`,
/// read with the listing, `GET /instruments`) and the venue's two fences. The instrument list and the home
/// currency are both the ACCOUNT's, so unlike alpaca's shared listing there is one of these per oanda
/// account ([`FactsByAccount`], which `BridgeTrade` keeps).
///
/// **The fence of `OandaFacts::facts` (the plain answer): an instrument is admitted only when its QUOTE
/// currency is the account's currency.** The engine's ledger is one currency, and only a node that asks to
/// convert (`OandaFacts::facts_quoted`, decision 0183) gets a conversion rate into it
/// (`docs/ops/any-symbol-remaining-venues-plan.md`, blocker 1): `USDJPY` on a USD account would book 100k
/// units at 150 as about 15 million "USD" of notional and a 0.10 yen move as 10,000 "USD" of PnL against
/// about 66 real, and overstated PnL feeds the margin-call sweep and the drawdown latch. So it is refused,
/// by name, as `deribit`'s other money model is. With the quote equal to the home currency, `units x price`
/// IS the real exposure whatever the instrument (FX or CFD), which is what the engine computes. It is run
/// BEFORE the grid checks (a JPY pair would otherwise be refused for its three-decimal price grid, which is
/// true and not the point); the adapter's own grid fence follows the row's grid: a currency pair on its OWN
/// price precision (a JPY pair is three decimals, admitted on an account whose currency is the yen), whole
/// units only, and any other class on the five decimals the adapter was proven on.
#[derive(Default)]
pub struct OandaFacts {
    /// A failed listing is two plain REST reads, not a session: it is remembered for [`FAILED_TTL`].
    facts: AccountFacts,
}

impl OandaFacts {
    /// The facts of `symbol` (the engine's spelling, `GBPUSD` or `SPX500_USD`), listing through `lister`
    /// when the kept listing is stale: the account's home currency and its instrument rows.
    pub fn facts(&self, symbol: &str, lister: CurrencyLister) -> Result<WireSymbolFacts, String> {
        let instrument = vike_catalog::to_oanda_instrument(&symbol.to_ascii_uppercase());
        // The row's OANDA `type` (`CURRENCY`, `CFD`, `METAL`), kept from the row check for the grid fence
        // below: the wire facts carry the grid, not the class.
        let word = std::cell::RefCell::new(None::<String>);
        let wire = self.facts.listed("oanda", "oanda", symbol, lister, &|row, home| {
            *word.borrow_mut() = row.contract_type.as_deref().map(str::to_owned);
            quote_currency_refusal(&instrument, home).map_or(Ok(()), Err)
        })?;
        oanda_adapter_fence(&instrument, word.borrow().as_deref(), &wire.properties)?;
        Ok(wire)
    }

    /// [`Self::facts`] for a node that CONVERTS a quote currency (`TradeUp::SymbolFacts::quoted`, decision
    /// 0183, phase 1): an instrument quoted in the account's own currency is answered exactly as before; one
    /// quoted in ANOTHER is answered with its quote currency and the rate `rate` reads for it (asked NOW:
    /// nothing here keeps a rate, only the listing is kept), instead of being refused for that alone. The
    /// adapter's own fence (five-decimal prices, whole units) still runs after the listing, and an instrument
    /// that names no quote currency, or one `rate` cannot price, is refused: no rate, no admission.
    pub fn facts_quoted(
        &self,
        symbol: &str,
        lister: CurrencyLister,
        rate: &RateSource,
    ) -> Result<WireSymbolFacts, String> {
        let instrument = vike_catalog::to_oanda_instrument(&symbol.to_ascii_uppercase());
        let home: Mutex<Option<String>> = Mutex::new(None);
        // The row's OANDA `type`, kept for the grid fence below, as in [`Self::facts`].
        let word = std::cell::RefCell::new(None::<String>);
        let mut wire = self.facts.listed("oanda", "oanda", symbol, lister, &|row, h| {
            *word.borrow_mut() = row.contract_type.as_deref().map(str::to_owned);
            *home.lock().unwrap_or_else(PoisonError::into_inner) = Some(h.to_string());
            Ok(())
        })?;
        oanda_adapter_fence(&instrument, word.borrow().as_deref(), &wire.properties)?;
        let home = home.into_inner().unwrap_or_else(PoisonError::into_inner).ok_or_else(|| {
            "oanda: the account's currency is not known, so a quote currency cannot be converted"
                .to_string()
        })?;
        if let Some(quoted) = rate(&instrument, &home)? {
            wire.quote_ccy = Some(quoted.quote_ccy);
            wire.quote_rate = Some(quoted.account_per_quote);
            wire.rate_as_of_ms = Some(quoted.as_of_ms);
        }
        Ok(wire)
    }
}

/// What OANDA states one unit of an instrument's QUOTE currency is worth in the account's own currency, at
/// one moment (decision 0183, phase 1): `account_per_quote` is account-currency units per ONE quote-currency
/// unit (EUR_GBP on a USD account: about `1.35`; USD_JPY: about `1/150`, never `150`), `as_of_ms` the
/// moment the venue was asked, epoch ms on this process's clock.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotedRate {
    /// The instrument's quote currency (upper case).
    pub quote_ccy: String,
    /// Account-currency units per one `quote_ccy` unit. Finite and positive, or this does not exist.
    pub account_per_quote: f64,
    /// When the rate was asked of the venue.
    pub as_of_ms: i64,
}

/// How [`OandaFacts::facts_quoted`] (and [`CtraderFacts::facts_quoted`], whose first argument is the row's
/// QUOTE CURRENCY, not an instrument) asks the venue for a conversion, handed in by the composition (this
/// module names no bridge): `(instrument, home currency)`. `Ok(None)` = the instrument is quoted in the home currency,
/// so nothing converts; `Ok(Some(rate))` = it is quoted in another and this is the rate; `Err` = no rate could
/// be had, or the instrument names no quote currency, in the operator's sentence. The symbol is then NOT
/// admitted: an absent rate is never read as par.
pub type RateSource = Arc<dyn Fn(&str, &str) -> Result<Option<QuotedRate>, String> + Send + Sync>;

/// Why an OANDA `instrument` (`GBP_USD`) is not quoted in `home`, the account's currency, or `None`.
fn quote_currency_refusal(instrument: &str, home: &str) -> Option<String> {
    let quote = instrument.split_once('_').map(|(_, quote)| quote).filter(|q| !q.is_empty());
    match quote {
        Some(quote) if quote.eq_ignore_ascii_case(home) => None,
        Some(quote) => Some(format!(
            "oanda: {instrument} is quoted in {quote} but this account's currency is {home}, and the \
             engine books money in ONE currency (nothing converts a quote currency), so a position \
             in it would be mis-valued; only instruments quoted in {home} are admitted"
        )),
        None => Some(format!(
            "oanda: {instrument} names no quote currency, so it cannot be checked against this \
             account's currency ({home})"
        )),
    }
}

/// **What the oanda adapter can say on the wire**, for an instrument other than the mounted one (the
/// mounted symbol's start is not touched). The adapter writes a CURRENCY pair's price with its OWN
/// `displayPrecision` decimals (five for `EUR_USD`, three for a JPY pair) and rounds the quantity to WHOLE
/// units (`fmt_price` and `signed_units` in the oanda bridge's exec client read the instrument's row); any
/// other class (a metal's or a CFD's amount unit is unmeasured) keeps the five decimals and whole units the
/// adapter was proven on. The rule is the bridge's own, `vike_oanda::grid_refusal`, so the grid the engine
/// judges an order on is the grid the exec client writes it on. `venue_word` is the row's OANDA `type`.
///
/// ⚠ The row's class reaches this fence through [`OandaFacts::facts`]'s row check, not through the wire
/// facts (they carry the grid only): a second caller of this fence must hand the class in the same way.
fn oanda_adapter_fence(
    symbol: &str,
    venue_word: Option<&str>,
    p: &SymbolProperties,
) -> Result<(), String> {
    vike_oanda::grid_refusal(symbol, venue_word, p).map_or(Ok(()), Err)
}

impl std::fmt::Debug for SymbolFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolFacts").field("ttl", &self.ttl).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "symbol_facts_tests.rs"]
mod symbol_facts_tests;

#[cfg(test)]
#[path = "symbol_facts_one_tests.rs"]
mod symbol_facts_one_tests;

#[cfg(test)]
#[path = "symbol_facts_oanda_grid_tests.rs"]
mod symbol_facts_oanda_grid_tests;
