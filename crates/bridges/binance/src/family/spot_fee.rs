//! A SPOT fill's fee, stated in the QUOTE currency, and its quantity NET of a base-coin fee: the one
//! place the family turns a venue's `commission` + `commissionAsset` into what the core books.
//!
//! The core folds `FillEvent::commission` into `balance` and `fees_paid` AS STATED on every linear
//! contract, whatever `commission_asset` says (`vike_exec::Account`'s `commission_in_quote` converts an
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
//!   position is the coin that is left.
//! - Anything else (BNB on a pair that is not BNB's own): `fee x` the asset's price in the pair's quote,
//!   when a rate source is registered for the venue ([`register_rates`]); the fee comes out of the BNB
//!   balance, not out of the delivered coin, so the quantity is NOT netted. With no source, or none
//!   that answers, the fee is unchanged and WARNED (`commission_asset` stays `BNB`, which keeps
//!   `fees_by_asset` true in coin): a guessed rate is worse than a named gap.
//! - No fee asset stated (the frozen `fixtures/r6/mapper.json` frames): unchanged.
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

use vike_bridge_core::json::json_num;
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
    /// The base coin a BUY's fee took out of what the fill delivered, in base units (0 otherwise).
    withheld_qty: f64,
}

impl SpotFee {
    fn stated(fee: f64, asset: &str) -> Self {
        SpotFee { commission: fee, commission_asset: asset.to_string(), withheld_qty: 0.0 }
    }

    /// The quantity the wallet actually received for a fill of `qty`: `qty` less a base-coin fee a BUY
    /// paid out of it. A fee that is not smaller than the fill (a malformed row) leaves `qty` as stated.
    #[must_use]
    pub fn net_qty(&self, qty: f64) -> f64 {
        if self.withheld_qty > 0.0 && self.withheld_qty < qty {
            qty - self.withheld_qty
        } else {
            qty
        }
    }
}

/// The venue's price of one `asset` in `quote` (a blocking public read, cold), or `None` when it has none.
pub trait QuoteRates: Send + Sync {
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

/// Register the public spot ticker of `base_url` as `venue`'s rate source, unless one is there already
/// (a start is repeated per account; the cache should outlive it). `base_url` is a PUBLIC host: no key
/// signs the read and no order rides it.
pub fn register_public_ticker_rates(venue: &'static str, base_url: &'static str) {
    if RATES.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(v, _)| v == venue) {
        return;
    }
    let fetch = move |symbol: &str| {
        // A short-timeout agent: the read runs on the user-data pump's thread.
        let transport = UreqTransport::with_agent(
            venue,
            vike_bridge_core::http::blocking_agent_with_timeout(Duration::from_secs(5)),
        );
        ticker_price(&transport, base_url, symbol)
    };
    register_rates(venue, Arc::new(CachedRates::new(Box::new(fetch), RATE_TTL, MISS_TTL)));
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
    body.get("price").and_then(json_num).filter(|p| p.is_finite() && *p > 0.0)
}

/// One blocking price read by exchange symbol (`BNBUSDT`), `None` for any failure.
pub type PriceFetch = Box<dyn Fn(&str) -> Option<f64> + Send + Sync>;

/// A [`QuoteRates`] over a one-symbol price fetch, with a per-symbol answer cache.
pub struct CachedRates {
    fetch: PriceFetch,
    hit_ttl: Duration,
    miss_ttl: Duration,
    cache: Mutex<HashMap<String, (Option<f64>, Instant)>>,
}

impl CachedRates {
    pub fn new(fetch: PriceFetch, hit_ttl: Duration, miss_ttl: Duration) -> Self {
        CachedRates { fetch, hit_ttl, miss_ttl, cache: Mutex::new(HashMap::new()) }
    }
}

impl QuoteRates for CachedRates {
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

/// The quote assets a symbol is split by when its fee asset is neither of its halves. A longer name is
/// listed before a shorter one it ends with.
// shortcut: a suffix list, not the instrument's own quote asset; a quote missing here prices nothing
// (the fee stays as stated and warned), so extend it when a pair with a BNB cross is added. Read
// against the spot exchange's TRADING symbols on 2026-10-31: the quotes with a BNB cross that this list
// lacked were USD1, USD, THB, IDR and U; RLUSD has none but must precede USD (a suffix of it).
const QUOTES: [&str; 28] = [
    "FDUSD", "RLUSD", "USDT", "USDC", "USD1", "TUSD", "BUSD", "USDP", "USD", "BTC", "ETH", "BNB",
    "EUR", "TRY", "BRL", "GBP", "AUD", "JPY", "ZAR", "MXN", "PLN", "RON", "ARS", "RUB", "UAH",
    "THB", "IDR", "U",
];

fn quote_of(symbol: &str) -> Option<&'static str> {
    QUOTES.into_iter().find(|q| {
        symbol.len() > q.len()
            && symbol.get(symbol.len() - q.len()..).is_some_and(|tail| tail.eq_ignore_ascii_case(q))
    })
}

/// `venue`'s registered price of `asset` in `quote`, asked outside the registry's lock.
fn registered_rate(venue: &str, asset: &str, quote: &str) -> Option<f64> {
    let source = RATES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(v, _)| v == venue)
        .map(|(_, source)| Arc::clone(source))?;
    source.price(asset, quote)
}

/// One spot fill's fee as the ledger must book it; see the module doc. `symbol` is the exchange symbol
/// (`BTCUSDT`), `price` the fill's own price, `buy` its side.
pub fn spot_fee_in_quote(
    symbol: &str,
    fee: f64,
    asset: &str,
    price: f64,
    buy: bool,
    venue: &str,
) -> SpotFee {
    let (sym, a) = (symbol.as_bytes(), asset.as_bytes());
    if a.is_empty() || sym.len() <= a.len() {
        return SpotFee::stated(fee, asset);
    }
    if sym[sym.len() - a.len()..].eq_ignore_ascii_case(a) {
        return SpotFee::stated(fee, asset);
    }
    if sym[..a.len()].eq_ignore_ascii_case(a) {
        let quote = symbol.get(a.len()..).unwrap_or_default().to_ascii_uppercase();
        return SpotFee {
            commission: fee * price,
            commission_asset: quote,
            withheld_qty: if buy { fee } else { 0.0 },
        };
    }
    if let Some(quote) = quote_of(symbol)
        && let Some(rate) = registered_rate(venue, asset, quote)
    {
        return SpotFee {
            commission: fee * rate,
            commission_asset: quote.to_string(),
            withheld_qty: 0.0,
        };
    }
    // A logarithmic schedule (1st, 2nd, 4th, 8th ...): a market maker pays this on every fill.
    let seen = THIRD_ASSET_FEES.fetch_add(1, Ordering::Relaxed) + 1;
    if seen.is_power_of_two() {
        tracing::warn!(
            venue,
            symbol,
            fee,
            fee_asset = asset,
            seen,
            "spot fee charged in an asset that is neither the symbol's base nor its quote (BNB \
             discount) and no rate priced it: booked as that many quote units, so `fees_paid` is \
             understated by the asset's price"
        );
    }
    SpotFee::stated(fee, asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicUsize;
    use vike_bridge_core::signer::Signer;
    use vike_bridge_core::transport::VenueApiError;

    fn fee(symbol: &str, n: f64, asset: &str, px: f64, buy: bool) -> SpotFee {
        spot_fee_in_quote(symbol, n, asset, px, buy, "binance")
    }

    /// BTC fee on a BTCUSDT BUY: booked = fee x price, in USDT.
    #[test]
    fn a_base_coin_fee_is_booked_at_the_fill_price_in_the_quote() {
        let f = fee("BTCUSDT", 0.000_000_1, "BTC", 65_000.0, true);
        assert!((f.commission - 0.0065).abs() < 1e-12, "{}", f.commission);
        assert_eq!(f.commission_asset, "USDT");
    }

    /// The same BUY delivers the quantity LESS that fee; a base fee on a SELL (no venue does this) nets nothing.
    #[test]
    fn a_buy_nets_the_base_coin_fee_out_of_its_quantity() {
        let f = fee("BTCUSDT", 0.000_000_1, "BTC", 65_000.0, true);
        assert!((f.net_qty(0.0001) - 0.000_099_9).abs() < 1e-15, "{}", f.net_qty(0.0001));
        assert_eq!(fee("BTCUSDT", 0.000_000_1, "BTC", 65_000.0, false).net_qty(0.0001), 0.0001);
        // a fee that is not smaller than the fill is malformed: the quantity stands as stated
        assert_eq!(fee("BTCUSDT", 0.0002, "BTC", 65_000.0, true).net_qty(0.0001), 0.0001);
        assert_eq!(fee("BTCUSDT", f64::NAN, "BTC", 65_000.0, true).net_qty(0.0001), 0.0001);
    }

    /// A quote fee (every SELL, and a BUY on a venue that charges the quote) is untouched, quantity included.
    #[test]
    fn a_quote_fee_is_unchanged() {
        let f = fee("BTCUSDT", 0.0065, "USDT", 65_000.0, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0065, "USDT"));
        assert_eq!(f.net_qty(0.5), 0.5);
        let f = fee("ETHBTC", 0.000_01, "BTC", 0.05, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.000_01, "BTC"));
        assert_eq!(f.net_qty(0.5), 0.5);
    }

    /// A base fee on a coin-quoted pair converts into THAT quote; a BNB pair's own coin is its base.
    #[test]
    fn the_quote_is_whatever_the_symbol_says() {
        let f = fee("ETHBTC", 0.001, "ETH", 0.05, true);
        assert!((f.commission - 0.000_05).abs() < 1e-15 && f.commission_asset == "BTC");
        let f = fee("BNBUSDT", 0.001, "BNB", 600.0, true);
        assert!((f.commission - 0.6).abs() < 1e-12 && f.commission_asset == "USDT");
        assert_eq!(f.net_qty(2.0), 1.999);
    }

    /// BNB on BTCUSDT with NO source registered for the venue: unchanged, still named BNB, quantity whole.
    #[test]
    fn a_third_asset_fee_with_no_rate_stays_as_stated() {
        let f = fee("BTCUSDT", 0.0003, "BNB", 65_000.0, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
        assert_eq!(f.net_qty(0.5), 0.5);
    }

    /// No stated asset, an empty symbol and a symbol that is only the asset change nothing.
    #[test]
    fn an_unstated_asset_or_symbol_is_unchanged() {
        let f = fee("BTCUSDT", 0.0004, "", 62_584.56, true);
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0004, ""));
        assert_eq!(f.net_qty(1.0), 1.0);
        assert_eq!(fee("", 0.5, "BTC", 1.0, true).commission_asset, "BTC");
        assert_eq!(fee("BTC", 0.5, "BTC", 1.0, true).commission_asset, "BTC");
    }

    struct Fixed(Option<f64>);
    impl QuoteRates for Fixed {
        fn price(&self, asset: &str, quote: &str) -> Option<f64> {
            (asset == "BNB" && quote == "USDT").then_some(self.0).flatten()
        }
    }

    /// With a source registered, BNB is valued in the pair's quote and the quantity is NOT netted (the
    /// fee came out of the BNB balance). Another quote the source cannot price stays as stated.
    #[test]
    fn a_registered_rate_values_a_third_asset_fee_in_the_pairs_quote() {
        register_rates("test-priced", Arc::new(Fixed(Some(600.0))));
        let f = spot_fee_in_quote("BTCUSDT", 0.0003, "BNB", 65_000.0, true, "test-priced");
        assert!((f.commission - 0.18).abs() < 1e-12, "{}", f.commission);
        assert_eq!(f.commission_asset, "USDT");
        assert_eq!(f.net_qty(0.5), 0.5);
        let f = spot_fee_in_quote("ETHBTC", 0.0003, "BNB", 0.05, true, "test-priced");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
        // a venue with no source of its own is not priced by another venue's
        let f = spot_fee_in_quote("BTCUSDT", 0.0003, "BNB", 65_000.0, true, "test-unregistered");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
    }

    /// A source that answers nothing leaves the fee as stated (the venue is down, or lists no such pair).
    #[test]
    fn a_source_with_no_answer_changes_nothing() {
        register_rates("test-silent", Arc::new(Fixed(None)));
        let f = spot_fee_in_quote("BTCUSDT", 0.0003, "BNB", 65_000.0, true, "test-silent");
        assert_eq!((f.commission, f.commission_asset.as_str()), (0.0003, "BNB"));
    }

    #[test]
    fn the_quote_is_found_by_its_suffix_longest_name_first() {
        assert_eq!(quote_of("BTCUSDT"), Some("USDT"));
        assert_eq!(quote_of("BTCFDUSD"), Some("FDUSD"));
        assert_eq!(quote_of("ETHBTC"), Some("BTC"));
        assert_eq!(quote_of("btcusdc"), Some("USDC"));
        // the quotes with a BNB cross the list lacked (read against the exchange's TRADING symbols)
        assert_eq!(quote_of("BTCUSD1"), Some("USD1"));
        assert_eq!(quote_of("BTCUSD"), Some("USD"));
        assert_eq!(
            quote_of("BTCRLUSD"),
            Some("RLUSD"),
            "a longer name first: USD is a suffix of it"
        );
        assert_eq!(quote_of("BNBTHB"), Some("THB"));
        assert_eq!(quote_of("BNBIDR"), Some("IDR"));
        assert_eq!(quote_of("BTCU"), Some("U"));
        assert_eq!(quote_of("USDT"), None, "a symbol that is only a quote has no base");
        assert_eq!(quote_of("BTCXYZ"), None);
    }

    /// The cache asks the venue once per symbol inside the hit window, and asks again after a miss expires.
    #[test]
    fn the_cache_reuses_an_answer_and_retries_a_miss() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let answers = [None, Some(601.5)];
        let rates = CachedRates::new(
            Box::new(move |symbol: &str| {
                assert_eq!(symbol, "BNBUSDT");
                answers[seen.fetch_add(1, Ordering::SeqCst).min(1)]
            }),
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
}
