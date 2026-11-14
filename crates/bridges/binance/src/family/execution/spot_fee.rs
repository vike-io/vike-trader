//! A SPOT fill's fee, stated in the QUOTE currency, and its quantity NET of a base-coin fee: the one
//! place the family turns a venue's `commission` + `commissionAsset` into what the core books.
//!
//! The core folds `FillEvent::commission` into `balance` and `fees_paid` AS STATED on every linear
//! contract, whatever `commission_asset` says (`vike_execution::Account`'s `commission_in_quote` converts an
//! INVERSE contract's coin fee and nothing else). A spot venue that charges the fee in the coin it
//! DELIVERS therefore has to be converted here, where the fill price is known. Binance spot takes the fee
//! from what the fill receives: a BUY pays in the BASE coin (`n` = 0.0000001 `N` = BTC on a BTCUSDT
//! BUY), a SELL in the quote (USDT), and with the BNB discount either in BNB. Booked raw, every BUY of
//! the demo spot account counted its fee as that many USDT, i.e. at 1/80,000 of its value (the BTC
//! price; measured on the CI box, v0.1.91).
//!
//! The rule, by the fee asset `N` against the exchange symbol (`BTCUSDT`):
//!
//! - `N` is the symbol's SUFFIX (the quote): the fee is already quote units, unchanged.
//! - `N` is the symbol's PREFIX (the base): `fee x fill price`, stated in the quote asset (the rest of
//!   the symbol), so `fees_by_asset` keeps a true quote tally too. **A BUY also loses that fee out of
//!   the quantity it delivers** (the wallet holds `qty - fee`), so [`SpotFee::net_qty`] is what the
//!   fill's `last_qty` must carry or the engine's long is larger than the wallet's coin by the sum of the
//!   fees and a sell of the whole position asks for coin the wallet lacks. Quantity net AND the fee in
//!   quote do not double count: the quote figure is the cost of the coin the fee took, the smaller
//!   position is the coin that is left. The arithmetic and its refusals (a fee that is no number or is not
//!   smaller than the fill leaves the whole fill as stated) are
//!   `crates/vike-bridge-core/src/event_map.rs`'s `spot_base_fee_net`; this module says which asset is the
//!   base and how the quote is spelled.
//! - Anything else (BNB on a pair that is not BNB's own): `fee x` the asset's price in the pair's quote,
//!   when a rate source is registered for the venue ([`register_rates`]); the fee comes out of the BNB
//!   balance, not out of the delivered coin, so the quantity is NOT netted. The pair's quote is its
//!   instrument row's `quoteAsset`, which the same source reads ([`QuoteRates::quote_of`]), never a
//!   guess from the name. With no source, a symbol it lists no row for, or no price, the fee is
//!   unchanged and WARNED (`commission_asset` stays `BNB`, which keeps `fees_by_asset` true in coin): a
//!   guessed rate is worse than a named gap.
//! - No fee asset stated (the frozen `fixtures/r6/mapper.json` frames): unchanged.
//!
//! A PERPETUAL's fee takes the third-asset arm alone ([`perp_fee_in_quote`], on its stream, history and reconcile legs):
//! the quote (the venue's USDT figure) is unchanged, and so is a coin-margined contract's fee in its own settle coin;
//! any other asset (BNB, under the USDⓈ-M BNB fee discount) is priced by the registered rate or left as stated and
//! warned, and its quantity is never netted.
//!
//! ⚠ The base test is a prefix test, so a symbol whose base merely STARTS with the fee asset
//! (`BNBUPUSDT` against a BNB-discounted fee) reads as a base fee. Those leveraged-token pairs are
//! delisted; a venue that lists one again needs the instrument's base asset passed in instead.
//!
//! Shared by the live executionReport mapper, the history replay and the reconcile fill parser, so the
//! three legs book one fill identically (the engine keeps the FIRST delivery of a trade id for good).
//! Aster's spot lane runs the same code: it converts only a fee its own venue states in the base asset
//! (and nets a BUY by it), and registers no rate source, so a third-asset fee there stays as stated.
//!
//! # Why a registry and not a parameter
//!
//! The rate is wanted by three legs whose signatures are `(frame, venue, symbol)`-shaped and are called
//! from about forty sites across two crates. The family's discriminator is already the caller-supplied
//! `venue` string, so a venue registers its OWN source under that string once, and the family looks it up
//! only on the third-asset path, which is cold. Nothing is registered by a test that maps a frame, so a
//! mapper test never opens a socket.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::event_map::{SpotFeeArm, SpotFeeArms, spot_base_fee_net};
use vike_bridge_core::json::{get_upper, json_number};
use vike_bridge_core::transport::{RestTransport, UreqTransport};

/// Fills met whose fee is in a third asset (BNB) that no rate source priced, for the warning schedule only.
static THIRD_ASSET_FEES: AtomicU64 = AtomicU64::new(0);

/// One spot fill's fee as the ledger must book it; see the module doc.
#[derive(Debug, Clone, PartialEq)]
pub struct SpotFee {
    /// `FillEvent::commission`: quote units wherever the family could state them.
    pub commission: f64,
    /// `FillEvent::commission_asset`.
    pub commission_asset: String,
    /// `FillEvent::last_qty`: the quantity the wallet actually received, the fill's less a base-coin fee a BUY paid
    /// out of it.
    pub net_qty: f64,
}

impl SpotFee {
    fn stated(qty: f64, fee: f64, asset: &str) -> Self {
        SpotFee { commission: fee, commission_asset: asset.to_string(), net_qty: qty }
    }
}

/// The family's cells beyond the BUY: a SELL pays in the quote (the module doc) and no negative `n` is in the tree, so
/// neither arm has been reached. Were one stated in the base the wallet would move by it and this quantity would not
/// (decision 0215's "What would reopen this"); the commission is restated all the same, zero fee included. Kept until a
/// ruling or a measured row. Aster runs these too, and what its spot charges is unmeasured.
const SPOT_FEE_ARMS: SpotFeeArms =
    SpotFeeArms { sell: SpotFeeArm::CommissionOnly, rebate: SpotFeeArm::CommissionOnly };

/// What a venue prices a third-asset fee with: the quote a symbol trades in and the price of one asset in a quote. Both
/// are blocking public reads (cold), `None` when the venue has no answer.
pub trait QuoteRates: Send + Sync {
    /// The quote asset of the exchange symbol `symbol` (`BTCUSDT`): its instrument row's `quoteAsset` on its book
    /// (`perp`: the USDⓈ-M list, else spot), or `None` when the venue lists no such row.
    fn quote_of(&self, symbol: &str, perp: bool) -> Option<String>;
    /// The venue's price of one `asset` in `quote`.
    fn price(&self, asset: &str, quote: &str) -> Option<f64>;
}

/// Registered sources, by the `venue` string the mappers are called with.
static RATES: Mutex<Vec<(String, Arc<dyn QuoteRates>)>> = Mutex::new(Vec::new());

/// Register (or replace) the source the family prices a third-asset fee with for `venue`.
pub fn register_rates(venue: &str, rates: Arc<dyn QuoteRates>) {
    let mut all = RATES.lock().unwrap_or_else(PoisonError::into_inner);
    all.retain(|(v, _)| v != venue);
    all.push((venue.to_string(), rates));
}

/// Register `venue`'s PUBLIC books as its rate source, unless one is there already (a start is repeated per
/// account; the cache should outlive it): the spot ticker of `spot_url` prices, and the fill's own instrument row
/// names the quote (`spot_url`'s spot `exchangeInfo`, `usdm_url`'s USDⓈ-M one). Both are PUBLIC hosts: no key signs
/// a read and no order rides one.
pub fn register_public_ticker_rates(
    venue: &'static str,
    spot_url: &'static str,
    usdm_url: &'static str,
) {
    if RATES.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(v, _)| v == venue) {
        return;
    }
    // A short-timeout agent: the reads run on the user-data pump's thread.
    let transport = move || {
        UreqTransport::with_agent(
            venue,
            vike_bridge_core::http::blocking_agent_with_timeout(Duration::from_secs(5)),
        )
    };
    let fetch = move |symbol: &str| ticker_price(&transport(), spot_url, symbol);
    let quote = move |symbol: &str, perp: bool| {
        let (base_url, path) = if perp {
            (usdm_url, "/fapi/v1/exchangeInfo")
        } else {
            (spot_url, "/api/v3/exchangeInfo")
        };
        exchange_info_quote(&transport(), base_url, path, symbol)
    };
    register_rates(
        venue,
        Arc::new(QuoteRatesCached::new(Box::new(fetch), Box::new(quote), RATE_TTL, MISS_TTL)),
    );
}

/// How long a priced answer is reused. A BNB rate that is a minute old values a fee of a few cents
/// within a few hundredths of a percent of it.
const RATE_TTL: Duration = Duration::from_secs(60);
/// How long a missing answer is reused, so a venue that does not list the pair (or is down) is asked
/// about once in a while and not once per fill.
const MISS_TTL: Duration = Duration::from_secs(5);

/// `GET /api/v3/ticker/price?symbol=` → the last price, or `None` for any failure or a non-positive one.
pub fn ticker_price<T: RestTransport>(transport: &T, base_url: &str, symbol: &str) -> Option<f64> {
    let body = transport
        .public(base_url, "/api/v3/ticker/price", &[("symbol", symbol.to_string())])
        .ok()?;
    body.get("price").and_then(json_number).filter(|p| p.is_finite() && *p > 0.0)
}

/// `GET <path>?symbol=` (an `exchangeInfo`) → `symbol`'s row's `quoteAsset`, or `None` for any failure or a symbol the
/// answer lists no row for. The row is found by its NAME: the USDⓈ-M list answers every symbol whatever was asked.
pub fn exchange_info_quote<T: RestTransport>(
    transport: &T,
    base_url: &str,
    path: &str,
    symbol: &str,
) -> Option<String> {
    let body = transport.public(base_url, path, &[("symbol", symbol.to_string())]).ok()?;
    let rows = body.get("symbols")?.as_array()?;
    let row = rows.iter().find(|row| {
        row.get("symbol").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
    })?;
    get_upper(row, "quoteAsset")
}

/// One blocking price read by exchange symbol (`BNBUSDT`), `None` for any failure.
pub type PriceFetch = Box<dyn Fn(&str) -> Option<f64> + Send + Sync>;

/// One blocking read of an exchange symbol's quote asset on its book (`true`: USDⓈ-M), `None` for any failure.
pub type QuoteFetch = Box<dyn Fn(&str, bool) -> Option<String> + Send + Sync>;

/// A [`QuoteRates`] over a one-symbol price fetch and a one-symbol quote fetch, each with an answer cache.
pub struct QuoteRatesCached {
    fetch: PriceFetch,
    quote_fetch: QuoteFetch,
    hit_ttl: Duration,
    miss_ttl: Duration,
    cache: Mutex<HashMap<String, (Option<f64>, Instant)>>,
    /// By (exchange symbol, book). A listed symbol's quote never changes, so an answer is kept for good and a miss
    /// for `miss_ttl`.
    #[expect(
        clippy::type_complexity,
        reason = "`cache`'s shape keyed by the book too, named once here"
    )]
    quotes: Mutex<HashMap<(String, bool), (Option<String>, Instant)>>,
}

impl QuoteRatesCached {
    pub fn new(
        fetch: PriceFetch,
        quote_fetch: QuoteFetch,
        hit_ttl: Duration,
        miss_ttl: Duration,
    ) -> Self {
        QuoteRatesCached {
            fetch,
            quote_fetch,
            hit_ttl,
            miss_ttl,
            cache: Mutex::new(HashMap::new()),
            quotes: Mutex::new(HashMap::new()),
        }
    }
}

impl QuoteRates for QuoteRatesCached {
    fn quote_of(&self, symbol: &str, perp: bool) -> Option<String> {
        let key = (symbol.to_ascii_uppercase(), perp);
        if let Some((quote, at)) =
            self.quotes.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned()
            && (quote.is_some() || at.elapsed() < self.miss_ttl)
        {
            return quote;
        }
        // Not under the lock, as the price read below.
        let quote = (self.quote_fetch)(&key.0, perp);
        self.quotes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, (quote.clone(), Instant::now()));
        quote
    }

    fn price(&self, asset: &str, quote: &str) -> Option<f64> {
        let symbol = format!("{}{}", asset.to_ascii_uppercase(), quote.to_ascii_uppercase());
        if let Some((px, at)) =
            self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(&symbol).copied()
        {
            let ttl = if px.is_some() { self.hit_ttl } else { self.miss_ttl };
            if at.elapsed() < ttl {
                return px;
            }
        }
        // Not under the lock: a slow venue must not stall a second symbol's lookup.
        let px = (self.fetch)(&symbol);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(symbol, (px, Instant::now()));
        px
    }
}

/// `venue`'s registered source, taken out of the registry so it is asked outside the registry's lock.
fn registered(venue: &str) -> Option<Arc<dyn QuoteRates>> {
    RATES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(v, _)| v == venue)
        .map(|(_, source)| Arc::clone(source))
}

/// One spot fill's fee as the ledger must book it; see the module doc. `symbol` is the exchange symbol
/// (`BTCUSDT`), `qty` the fill's quantity, `price` its own price, `buy` its side.
pub fn spot_fee_in_quote(
    symbol: &str,
    qty: f64,
    fee: f64,
    asset: &str,
    price: f64,
    buy: bool,
    venue: &str,
) -> SpotFee {
    let (symbol_name, a) = (symbol.as_bytes(), asset.as_bytes());
    if a.is_empty() || symbol_name.len() <= a.len() {
        return SpotFee::stated(qty, fee, asset);
    }
    if symbol_name[symbol_name.len() - a.len()..].eq_ignore_ascii_case(a) {
        return SpotFee::stated(qty, fee, asset);
    }
    if symbol_name[..a.len()].eq_ignore_ascii_case(a) {
        let side = if buy { 1 } else { -1 };
        return match spot_base_fee_net(side, qty, price, fee, SPOT_FEE_ARMS) {
            Some((net_qty, commission)) => SpotFee {
                commission,
                commission_asset: symbol.get(a.len()..).unwrap_or_default().to_ascii_uppercase(),
                net_qty,
            },
            None => SpotFee::stated(qty, fee, asset),
        };
    }
    match third_asset_fee(symbol, false, fee, asset, venue) {
        Some((commission, quote)) => SpotFee { commission, commission_asset: quote, net_qty: qty },
        None => SpotFee::stated(qty, fee, asset),
    }
}

/// A PERPETUAL fill's fee in the pair's quote, or `None` to keep it as stated. `series` is the perp's series label
/// (`BTCUSDT.P`) or its exchange symbol. A fee in the quote (the venue's USDT or USDC figure) is as stated; a fee in any
/// other asset (BNB, under the USDⓈ-M BNB fee discount) is priced like a spot fee in a third asset ([`spot_fee_in_quote`]),
/// through the source the venue registered, and with none it is left as stated and warned. A perpetual's quantity is
/// a position delta and is never netted, so only the commission moves.
///
/// A COIN-MARGINED contract's fee in the coin it settles in (`N` = `BTC` on `BTCUSD_PERP`) is as stated and is no
/// third asset: it is the ledger's own money (decision 0258), so it is neither priced nor warned. The settle coin is
/// read off the name by the catalog's rule (`vike_catalog::inverse_settle_coin`), on the linear-quote miss only.
pub fn perp_fee_in_quote(
    series: &str,
    fee: f64,
    asset: &str,
    venue: &str,
) -> Option<(f64, String)> {
    let pair = series.strip_suffix(vike_venues::PERP_SUFFIX).unwrap_or(series);
    let (symbol, a) = (pair.as_bytes(), asset.as_bytes());
    if a.is_empty()
        || symbol.len() <= a.len()
        || symbol[symbol.len() - a.len()..].eq_ignore_ascii_case(a)
    {
        return None;
    }
    if vike_catalog::inverse_settle_coin(venue, pair).is_some_and(|c| c.eq_ignore_ascii_case(asset))
    {
        return None;
    }
    third_asset_fee(pair, true, fee, asset, venue)
}

/// A fee in an asset that is neither half of `symbol` (an exchange symbol, `BTCUSDT`, on the USDⓈ-M book when `perp`):
/// `fee x` the asset's price in the pair's quote and that quote, when the venue's registered source names the quote
/// and prices it; else `None`, WARNED on a logarithmic schedule (1st, 2nd, 4th, 8th ...: a market maker pays this on
/// every fill).
fn third_asset_fee(
    symbol: &str,
    perp: bool,
    fee: f64,
    asset: &str,
    venue: &str,
) -> Option<(f64, String)> {
    if let Some(source) = registered(venue)
        && let Some(quote) = source.quote_of(symbol, perp)
        && let Some(rate) = source.price(asset, &quote)
    {
        return Some((fee * rate, quote));
    }
    let seen = THIRD_ASSET_FEES.fetch_add(1, Ordering::Relaxed) + 1;
    if seen.is_power_of_two() {
        tracing::warn!(
            venue,
            symbol,
            fee,
            fee_asset = asset,
            seen,
            "fee charged in an asset that is neither the symbol's base nor its quote (BNB \
             discount) and no rate priced it: booked as that many quote units, so `fees_paid` is \
             understated by the asset's price"
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicUsize;
    use vike_bridge_core::signer::Signer;
    use vike_bridge_core::transport::VenueApiError;

    fn fee(symbol: &str, qty: f64, n: f64, asset: &str, px: f64, buy: bool) -> SpotFee {
        spot_fee_in_quote(symbol, qty, n, asset, px, buy, "binance")
    }

    /// BTC fee on a BTCUSDT BUY: booked = fee x price, in USDT.
    #[test]
    fn a_base_coin_fee_is_booked_at_the_fill_price_in_the_quote() {
        let f = fee("BTCUSDT", 0.0001, 0.000_000_1, "BTC", 65_000.0, true);
        assert!((f.commission - 0.0065).abs() < 1e-12, "{}", f.commission);
        assert_eq!(f.commission_asset, "USDT");
    }

    /// The same BUY delivers the quantity LESS that fee; a base fee on a SELL (no venue does this) nets nothing.
    #[test]
    fn a_buy_nets_the_base_coin_fee_out_of_its_quantity() {
        let f = fee("BTCUSDT", 0.0001, 0.000_000_1, "BTC", 65_000.0, true);
        assert!((f.net_qty - 0.000_099_9).abs() < 1e-15, "{}", f.net_qty);
        assert_eq!(fee("BTCUSDT", 0.0001, 0.000_000_1, "BTC", 65_000.0, false).net_qty, 0.0001);
        // a fee that is not smaller than the fill is malformed: the fill stands as stated, the commission too
        let f = fee("BTCUSDT", 0.0001, 0.0002, "BTC", 65_000.0, true);
        assert_eq!((f.net_qty, f.commission, f.commission_asset.as_str()), (0.0001, 0.0002, "BTC"));
        let f = fee("BTCUSDT", 0.0001, f64::NAN, "BTC", 65_000.0, true);
        assert_eq!((f.net_qty, f.commission_asset.as_str()), (0.0001, "BTC"));
        assert!(f.commission.is_nan());
    }

    /// A quote fee (every SELL, and a BUY on a venue that charges the quote) is untouched, quantity included.
    #[test]
    fn a_quote_fee_is_unchanged() {
        let f = fee("BTCUSDT", 0.5, 0.0065, "USDT", 65_000.0, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0065, "USDT"));
        assert_eq!(f.net_qty, 0.5);
        let f = fee("ETHBTC", 0.5, 0.000_01, "BTC", 0.05, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.000_01, "BTC"));
        assert_eq!(f.net_qty, 0.5);
    }

    /// A base fee on a coin-quoted pair converts into THAT quote; a BNB pair's own coin is its base.
    #[test]
    fn the_quote_is_whatever_the_symbol_says() {
        let f = fee("ETHBTC", 2.0, 0.001, "ETH", 0.05, true);
        assert!((f.commission - 0.000_05).abs() < 1e-15 && f.commission_asset == "BTC");
        let f = fee("BNBUSDT", 2.0, 0.001, "BNB", 600.0, true);
        assert!((f.commission - 0.6).abs() < 1e-12 && f.commission_asset == "USDT");
        assert_eq!(f.net_qty, 1.999);
    }

    /// BNB on BTCUSDT with NO source registered for the venue: unchanged, still named BNB, quantity whole.
    #[test]
    fn a_third_asset_fee_with_no_rate_stays_as_stated() {
        let f = fee("BTCUSDT", 0.5, 0.0003, "BNB", 65_000.0, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
        assert_eq!(f.net_qty, 0.5);
    }

    /// No stated asset, an empty symbol and a symbol that is only the asset change nothing.
    #[test]
    fn an_unstated_asset_or_symbol_is_unchanged() {
        let f = fee("BTCUSDT", 1.0, 0.0004, "", 62_584.56, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0004, ""));
        assert_eq!(f.net_qty, 1.0);
        assert_eq!(fee("", 1.0, 0.5, "BTC", 1.0, true).commission_asset, "BTC");
        assert_eq!(fee("BTC", 1.0, 0.5, "BTC", 1.0, true).commission_asset, "BTC");
    }

    /// The grid every spot venue walks (`vike_bridge_core::spot_fee_pins`), by the family's rule.
    #[test]
    fn spot_fee_grid_is_the_pinned_one() {
        vike_bridge_core::spot_fee_pins::assert_spot_fee_grid(
            include_str!("../../../tests/fixtures/spot_fee_grid.txt"),
            |side, qty, px, n, asset| {
                let f = spot_fee_in_quote("HYPEUSDC", qty, n, asset, px, side > 0, "grid");
                (f.net_qty, f.commission, f.commission_asset)
            },
        );
    }

    /// The rows a test source lists, `(symbol, quoteAsset)`, on either book.
    fn listed(symbol: &str) -> Option<String> {
        [("BTCUSDT", "USDT"), ("ETHBTC", "BTC")]
            .into_iter()
            .find(|(s, _)| *s == symbol)
            .map(|(_, quote)| quote.to_string())
    }

    struct Fixed(Option<f64>);
    impl QuoteRates for Fixed {
        fn quote_of(&self, symbol: &str, _perp: bool) -> Option<String> {
            listed(symbol)
        }
        fn price(&self, asset: &str, quote: &str) -> Option<f64> {
            (asset == "BNB" && quote == "USDT").then_some(self.0).flatten()
        }
    }

    /// With a source registered, BNB is valued in the pair's quote and the quantity is NOT netted (the
    /// fee came out of the BNB balance). Another quote the source cannot price stays as stated.
    #[test]
    fn a_registered_rate_values_a_third_asset_fee_in_the_pairs_quote() {
        register_rates("test-priced", Arc::new(Fixed(Some(600.0))));
        let f = spot_fee_in_quote("BTCUSDT", 0.5, 0.0003, "BNB", 65_000.0, true, "test-priced");
        assert!((f.commission - 0.18).abs() < 1e-12, "{}", f.commission);
        assert_eq!(f.commission_asset, "USDT");
        assert_eq!(f.net_qty, 0.5);
        let f = spot_fee_in_quote("ETHBTC", 0.5, 0.0003, "BNB", 0.05, true, "test-priced");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
        // a venue with no source of its own is not priced by another venue's
        let f =
            spot_fee_in_quote("BTCUSDT", 0.5, 0.0003, "BNB", 65_000.0, true, "test-unregistered");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
    }

    /// A source that answers nothing leaves the fee as stated (the venue is down, or lists no such pair).
    #[test]
    fn a_source_with_no_answer_changes_nothing() {
        register_rates("test-silent", Arc::new(Fixed(None)));
        let f = spot_fee_in_quote("BTCUSDT", 0.5, 0.0003, "BNB", 65_000.0, true, "test-silent");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
    }

    /// **The row decides, never the name.** Read against mainnet's lists on 2026-11-13, the retired suffix list
    /// disagreed with the row on two `TRADING` names and on every dated USDⓈ-M future: `USDTUSD` quotes `USD` (the list
    /// read `TUSD`), and the quarterly `BTCUSDT_261225` quotes `USDT` (the list read nothing, so its fee stayed as stated).
    #[test]
    fn the_quote_is_the_rows_and_never_read_off_the_name() {
        struct Listed;
        impl QuoteRates for Listed {
            fn quote_of(&self, symbol: &str, perp: bool) -> Option<String> {
                match (symbol, perp) {
                    ("USDTUSD", false) => Some("USD".to_string()),
                    ("BTCUSDT_261225", true) => Some("USDT".to_string()),
                    _ => None,
                }
            }
            fn price(&self, asset: &str, quote: &str) -> Option<f64> {
                (asset == "BNB").then_some(if quote == "USD" { 601.0 } else { 600.0 })
            }
        }
        register_rates("test-listed", Arc::new(Listed));
        let f = spot_fee_in_quote("USDTUSD", 10.0, 0.0001, "BNB", 1.0, true, "test-listed");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0001 * 601.0, "USD"));
        assert_eq!(
            perp_fee_in_quote("BTCUSDT_261225.P", 0.0001, "BNB", "test-listed"),
            Some((0.0001 * 600.0, "USDT".to_string()))
        );
        // a spot name is not looked up in the USDⓈ-M list, nor the other way round
        let f = spot_fee_in_quote("BTCUSDT_261225", 10.0, 0.0001, "BNB", 1.0, true, "test-listed");
        assert_eq!(f.commission_asset, "BNB");
        assert_eq!(perp_fee_in_quote("USDTUSD.P", 0.0001, "BNB", "test-listed"), None);
    }

    /// A transport that answers every public read with one body (`Err` for an error) and checks what was asked.
    struct Rows(Result<Value, VenueApiError>, &'static str);
    impl RestTransport for Rows {
        fn signed(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[(&str, String)],
            _: &dyn Signer,
        ) -> Result<Value, VenueApiError> {
            panic!("a quote read is never signed")
        }
        fn public(
            &self,
            base_url: &str,
            path: &str,
            params: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            assert_eq!((base_url, path), ("https://host.test", "/x/exchangeInfo"));
            assert_eq!(params, [("symbol", self.1.to_string())]);
            self.0.clone()
        }
    }

    /// The quote is the row's `quoteAsset`, upper-cased, on the row named like the symbol: the one row the spot list
    /// answers, or one row of the whole USDⓈ-M list. No row, an empty quote, an error and another shape are `None`.
    #[test]
    fn the_exchange_info_row_names_the_quote() {
        let read = |body: Result<Value, VenueApiError>, symbol: &'static str| {
            exchange_info_quote(&Rows(body, symbol), "https://host.test", "/x/exchangeInfo", symbol)
        };
        let spot =
            json!({"symbols": [{"symbol": "USDTUSD", "baseAsset": "USDT", "quoteAsset": "USD"}]});
        assert_eq!(read(Ok(spot), "USDTUSD"), Some("USD".to_string()));
        let usdm = json!({"symbols": [
            {"symbol": "BTCUSDT", "quoteAsset": "USDT"},
            {"symbol": "ETHUSDC", "quoteAsset": "usdc"},
        ]});
        assert_eq!(read(Ok(usdm.clone()), "ethusdc"), Some("USDC".to_string()));
        assert_eq!(read(Ok(usdm), "SOLUSDT"), None, "a row the list does not hold");
        assert_eq!(
            read(Ok(json!({"symbols": [{"symbol": "BTCU", "quoteAsset": ""}]})), "BTCU"),
            None
        );
        assert_eq!(read(Ok(json!({"code": 0})), "BTCU"), None);
        let refused = VenueApiError { code: -1121, msg: "Invalid symbol.".into() };
        assert_eq!(read(Err(refused), "BTCXYZ"), None);
    }

    /// A quote the venue named is kept for good (a listing's quote never changes); a miss is asked again once its
    /// window is over, on the same book only.
    #[test]
    fn a_quote_is_kept_for_good_and_a_miss_is_asked_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let rates = QuoteRatesCached::new(
            Box::new(|_: &str| -> Option<f64> { panic!("no price is asked") }),
            Box::new(move |symbol: &str, perp: bool| {
                assert_eq!((symbol, perp), ("BTCUSDT", true));
                (seen.fetch_add(1, Ordering::SeqCst) > 0).then(|| "USDT".to_string())
            }),
            Duration::from_millis(1),
            Duration::from_millis(250),
        );
        assert_eq!(rates.quote_of("BTCUSDT", true), None);
        assert_eq!(rates.quote_of("btcusdt", true), None, "a miss is reused inside its window");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(rates.quote_of("BTCUSDT", true), Some("USDT".to_string()));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            rates.quote_of("BTCUSDT", true),
            Some("USDT".to_string()),
            "past the hit window"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "an answer is never asked again");
    }

    /// The cache asks the venue once per symbol inside the hit window, and asks again after a miss expires.
    #[test]
    fn the_cache_reuses_an_answer_and_retries_a_miss() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let answers = [None, Some(601.5)];
        let rates = QuoteRatesCached::new(
            Box::new(move |symbol: &str| {
                assert_eq!(symbol, "BNBUSDT");
                answers[seen.fetch_add(1, Ordering::SeqCst).min(1)]
            }),
            Box::new(|_: &str, _: bool| -> Option<String> { panic!("no quote is asked") }),
            Duration::from_secs(60),
            Duration::from_millis(250),
        );
        assert_eq!(rates.price("bnb", "USDT"), None);
        assert_eq!(rates.price("BNB", "USDT"), None, "a miss is reused inside its window");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(rates.price("BNB", "USDT"), Some(601.5), "an expired miss is asked again");
        assert_eq!(rates.price("BNB", "USDT"), Some(601.5));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a hit is reused");
    }

    struct Wire(Result<Value, VenueApiError>);
    impl RestTransport for Wire {
        fn signed(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[(&str, String)],
            _: &dyn Signer,
        ) -> Result<Value, VenueApiError> {
            panic!("a rate read is never signed")
        }
        fn public(
            &self,
            base_url: &str,
            path: &str,
            params: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            assert_eq!((base_url, path), ("https://host.test", "/api/v3/ticker/price"));
            assert_eq!(params, [("symbol", "BNBUSDT".to_string())]);
            self.0.clone()
        }
    }

    #[test]
    fn the_ticker_body_is_read_for_a_positive_price_and_nothing_else() {
        let read = |body: Result<Value, VenueApiError>| {
            ticker_price(&Wire(body), "https://host.test", "BNBUSDT")
        };
        assert_eq!(read(Ok(json!({"symbol": "BNBUSDT", "price": "612.30000000"}))), Some(612.3));
        assert_eq!(read(Ok(json!({"price": "0.00000000"}))), None);
        assert_eq!(read(Ok(json!({"symbol": "BNBUSDT"}))), None);
        assert_eq!(read(Err(VenueApiError { code: -1121, msg: "Invalid symbol.".into() })), None);
    }

    /// One perpetual `ORDER_TRADE_UPDATE` fill of `s`, its fee `n` in `asset`, through the family's mapper:
    /// the bare fill and every event logged while mapping it.
    fn perp_fill(
        s: &str,
        n: &str,
        asset: &str,
        venue: &str,
    ) -> (vike_model::events::FillEvent, Vec<vike_log::capture::CapturedEvent>) {
        let frame = json!({"e": "ORDER_TRADE_UPDATE", "T": 5, "o": {
            "s": s, "c": "c1", "x": "TRADE", "X": "FILLED", "S": "BUY", "l": "1", "L": "75000.0",
            "n": n, "N": asset, "t": 7, "m": false, "ps": "BOTH"}});
        let (events, logs) = vike_log::capture::captured(|| {
            crate::family::execution::event_mapper::perp::map_perp(&frame, venue, s, None)
        });
        let fill = events.into_iter().find_map(|e| match e {
            vike_model::events::Event::Fill(f) => Some(f),
            _ => None,
        });
        (fill.expect("a TRADE row maps to a bare fill"), logs)
    }

    /// **A COIN-M fee in the contract's own settle coin is no third asset** (decision 0258): `N` = `BTC` on
    /// `BTCUSD_PERP` is the coin ledger's money as stated, so nothing prices it and nothing is said. Base: it took
    /// the third-asset arm and WARNED that the fee was "booked as that many quote units", on every COIN-M fill's
    /// schedule. A linear perpetual's fee in a third asset that no rate prices is still warned.
    #[test]
    fn a_coin_m_fee_in_its_settle_coin_is_not_warned_and_a_linear_third_asset_fee_still_is() {
        // The schedule warns on the 1st, 2nd, 4th ... unpriced fee of the PROCESS: this test starts at the 1st.
        THIRD_ASSET_FEES.store(0, Ordering::Relaxed);
        let warned = |logs: &[vike_log::capture::CapturedEvent]| {
            logs.iter().any(|e| e.level == tracing::Level::WARN && e.field("fee_asset").is_some())
        };

        let (fill, logs) = perp_fill("BTCUSD_PERP", "0.00000053", "BTC", "binance");
        assert_eq!((fill.commission, fill.commission_asset.as_str()), (0.000_000_53, "BTC"));
        assert!(!warned(&logs), "a COIN-M fee in its own coin is said nothing about: {logs:?}");

        // Other tests of this binary meet unpriced fees on their own threads under `cargo test` and move the
        // process-wide count between the reset and this fee, so the control resets and asks again until its fee
        // is the 1st: a race costs a retry, never a false red.
        let warned_once = (0..100).any(|_| {
            THIRD_ASSET_FEES.store(0, Ordering::Relaxed);
            let (fill, logs) = perp_fill("BTCUSDT", "0.0003", "BNB", "test-perp-unpriced");
            assert_eq!((fill.commission, fill.commission_asset.as_str()), (0.0003, "BNB"));
            warned(&logs)
        });
        assert!(warned_once, "an unpriced third-asset fee is still warned");
    }
}
