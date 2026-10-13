//! Public instrument catalog — the Deribit twin of okx's `catalog` module, exposed as the venue's
//! [`CatalogProvider`] contribution to the chart's cross-venue symbol search. Reads the KEYLESS
//! public `/api/v2/public/get_instruments` (UNSIGNED GETs — no credentials, no signer), mapped to
//! universal [`Instrument`]s, each carrying the typed [`Contract`] its row DEFINES and the order
//! grid ([`SymbolProperties`]) the same row publishes
//! (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`).
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the blocking `fetch_json` so
//! the mapping stays testable without network — mirrors binance/bybit/okx's catalog modules
//! exactly.
//!
//! # What it lists: five requests, each naming its `kind`
//!
//! MEASURED against the live keyless endpoint, 2026-10-04, active rows:
//!
//! | request | rows | becomes |
//! |---|---|---|
//! | `currency=any&kind=future` | 209 = 134 perpetual + 75 dated (26 `day`, 20 `week`, 29 `month`) | [`Contract::Perp`] / [`Contract::Future`] |
//! | `currency=any&kind=spot` | 57 | [`Contract::CryptoSpot`] |
//! | `currency=BTC&kind=option` | 996 | [`Contract::Option`] |
//! | `currency=ETH&kind=option` | 836 | [`Contract::Option`] |
//! | `currency=USDC&kind=option` | 3,816 (MEASURED 2026-10-11: AVAX 446, BTC 636, ETH 552, HYPE 512, SOL 850, TRX 330, XRP 490) | [`Contract::Option`] |
//!
//! ⚠ **This module used to say Deribit has no all-currencies endpoint, and that was wrong.**
//! `currency=any` is accepted: with a `kind=` it narrows, and without one it answers the whole venue
//! in a single body (209 futures, 57 spot, 5,276 options, 232 combos; the options alone are 4.6 MB).
//! Until 2026-10-04 the catalog asked `currency=BTC` and `currency=ETH` with no `kind`, so it held
//! the two coin-settled perpetuals out of the venue's 134 and no spot pair at all — the 132
//! USDC-settled perpetuals live under `currency=USDC`, which the old list never named.
//!
//! **Every request names a `kind`, and that is what keeps the five disjoint.** A request with no
//! `kind` returns every kind, so an unfiltered `currency=BTC` also answers the BTC-quoted spot
//! rows (four of them) and both combo kinds, and asked beside `kind=future&currency=any` it would
//! return `BTC-PERPETUAL` twice. Requests that share a `kind` differ in currency (the three option
//! books: the coin-settled BTC and ETH books, and the USDC-settled book every underlying's linear
//! options sit in), so no instrument can arrive on two of them.
//!
//! # What each row becomes — every field from the PAYLOAD, never from the instrument name
//!
//! [`contract_of`] reads the row's own fields; the instrument name is never parsed for identity:
//!
//! * **margining** is `instrument_type`: `"reversed"` → [`Margining::Inverse`] (`BTC-PERPETUAL`,
//!   the coin-settled futures and the BTC/ETH option books), `"linear"` → [`Margining::Linear`]
//!   (the USDC-settled books). Any other word, or none, mints NOTHING for a derivative row — there
//!   is no `Unknown` margining to put there, and a guess is the defect 0126 exists to remove.
//! * **settle** is `settlement_currency`, uppercased (the venue sends `"usdc"` on some rows).
//! * **value_ccy**, the currency `contract_size` is counted in, is the venue's documented contract
//!   unit and the one `crates/bridges/deribit/CLAUDE.md`'s DOM-lane bullet MEASURED: an INVERSE
//!   future or perpetual is sized in its `counter_currency` (`BTC-PERPETUAL`: 10 USD a contract);
//!   a LINEAR one and every option in its `base_currency` (`BTC_USDC-PERPETUAL`: 0.0001 BTC; an
//!   option: 1 BTC of underlying). ⚠ Not the `quote_currency` alone: on a linear USDC perpetual
//!   that would value a 0.0001-BTC contract at 0.0001 USDC.
//! * a **perpetual** (`kind == "future"`, `settlement_period == "perpetual"`) carries NO expiry.
//!   The venue publishes `expiration_timestamp: 32503708800000` (year 3000) on every perpetual and
//!   spot row; that sentinel is never read, so it cannot leak out as a date.
//! * a **dated future** carries `expiration_timestamp` verbatim as `expiry_ms` and its
//!   `settlement_period` (`day`/`week`/`month`) as the [`Tenor`]. Delivery is [`Delivery::Unknown`]:
//!   the row does not say (Deribit futures are cash-settled by the venue's rules, which no field
//!   states).
//! * an **option** takes `option_type` (`"call"`/`"put"`) as its right, `strike` as its strike,
//!   `expiration_timestamp` as its expiry — the exact millisecond, 08:00 UTC on the expiry day —
//!   `base_currency` as its underlying, and [`ExerciseStyle::European`]: every Deribit option is
//!   European by the venue's contract specification (no field carries the style).
//!
//! The venue's own word (`instrument_type`) still rides along VERBATIM as
//! [`Instrument::contract_type`]: a spot row publishes `"linear"` too, which no [`Contract`] field
//! could hold.
//!
//! The order grid is [`properties_of`]: `tick_size` (+ the tiered `tick_size_steps` as a
//! [`TickScheme`]), `min_trade_amount` as step and minimum, `contract_size`. It is the SAME function
//! `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` runs on the singular
//! `public/get_instrument` row, so the catalog and the order path cannot read two grids for one
//! instrument.
//!
//! ## Combos: skipped, and why
//!
//! `future_combo` and `option_combo` rows carry no legs — only `public/get_combo_details` (or
//! `public/get_combos`) names them — so a [`Contract::Combo`] built from this payload would hold an
//! empty leg list, which is a lie rather than an absence. They stay out until the catalog fetches
//! the legs.
//!
//! ## Perpetuals: not all of them are crypto
//!
//! The venue's own `underlying_type` says 99 of the 134 are `crypto` and 35 are something else
//! (`equity` 23, `equity_etf` 5, `commodity` 4, `preipo` 2, `crypto_index` 1 —
//! `AAPL_USDC-PERPETUAL`, `GOLD_USDC-PERPETUAL`). [`Contract::Perp`] has no field that says what a
//! perpetual is written ON, so the picker files an AAPL perpetual under Perps with the rest.
//!
//! ## Options: the two coin-settled books AND the USDC-settled book
//!
//! The venue lists its options in two settlement families: the coin-settled (inverse) BTC and ETH
//! books, under `currency=BTC` / `currency=ETH`, and ONE USDC-settled (linear) book under
//! `currency=USDC` that holds every underlying's linear options — AVAX, BTC, ETH, HYPE, SOL, TRX
//! and XRP. SOL and the other altcoins have NO coin-settled options at all, so without the USDC
//! book they are absent from this catalog entirely. Until 2026-10-11 it was left out on purpose
//! (the Options tool fetched its own universe); it is in since the desktop's Greeks tool builds
//! its `OptionIndex` (`crates/vike-options/src/chain/index.rs`) from THIS listing through the
//! datahub's `VenueCatalog` verb, and a SOL position missing from the index reads as unpriced.
//!
//! What it costs, MEASURED 2026-10-11 against the live keyless endpoint: the USDC book is 3,816
//! active rows in a 3.30 MB body, on top of the 1.83 MB the other four requests answered
//! (2026-10-04), roughly 1.1 MB more cache JSON at an estimated 300 bytes a row, which
//! `vike_catalog::save_cache` rewrites WHOLE on every refresh and `vike_catalog::load_cache` parses
//! at every start. Each USDC row maps through the same [`contract_of`] as the coin books: margining
//! [`Margining::Linear`] from `instrument_type: "linear"`, `settle` `USDC`, the underlying and
//! `value_ccy` from `base_currency`, `raw_symbol` the verbatim name (`SOL_USDC-12OCT26-96-P`).
//!
//! ## Spot: classified here, not yet claimed by the data path
//!
//! `kind == "spot"` is [`Contract::CryptoSpot`] (`BTC_USDC`, `ETH_USDC`; 48 of the 57 are
//! USDC-quoted), so the picker can offer the pair and a spot row's
//! `SymbolProperties::asset_class` is set. ⚠ **`crates/vike-catalog/src/addressing.rs`'s
//! `addressing_for` row for this venue deliberately does NOT list the class** — a spot instrument
//! is only half addressable, MEASURED 2026-10-04 against the public mainnet endpoints:
//!
//! * REST history addresses it: `get_tradingview_chart_data` answers `BTC_USDC` at resolutions 1, 5,
//!   720 and `1D`, the symbol passing through [`crate::data`] verbatim.
//! * the `book.{instrument}.100ms` and `quote.{instrument}` channels subscribe and stream.
//! * the `trades.{instrument}.100ms` and `chart.trades.{instrument}.{resolution}` channels do NOT:
//!   `public/subscribe` answers an EMPTY result for a spot instrument (`agg2`,
//!   `trades.spot.{currency}.100ms` and every other spelling tried matched nothing either; `raw`
//!   needs authentication). `crates/bridges/deribit/src/market_data.rs`'s `classify_rpc_reply`
//!   reads an empty result as an error, which `crates/bridges/deribit/src/market_feed.rs`'s
//!   `bars_main` and `trades_main` turn into a fatal frame and the shared driver into a reconnect
//!   loop — so a spot chart would seed from REST and then never go live.
//!
//! A linear USDC perpetual has no such gap (REST history and all four live channels answer for
//! `BTC_USDC-PERPETUAL`), which is why the `CryptoPerp` already in that row covers the 132 new
//! linear perpetuals without a table change.

use serde_json::Value;
use vike_bridge_core::json::{json_int, json_num};
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_marketdata::OptionKind;
use vike_model::{
    AssetClass, Contract, Delivery, ExerciseStyle, FutureFamily, FutureSpec, Instrument, Margining,
    OptionSpec, PerpSpec, SymbolProperties, Tenor, TickScheme, TickTier,
};

const BASE: &str = "https://www.deribit.com/api/v2/public/get_instruments";

/// Deribit's all-currencies selector for `currency=`. Accepted with a `kind=` filter (measured
/// 2026-10-04) and, unfiltered, answers every kind at once — see the module doc.
const ANY_CURRENCY: &str = "any";

/// The `kind=` words fetched across EVERY currency: dated futures and perpetuals, and spot pairs.
const ALL_CURRENCY_KINDS: [&str; 2] = ["future", "spot"];

/// The currencies whose `kind=option` book is fetched: the coin-settled BTC and ETH books and the
/// USDC-settled book (every underlying's linear options; SOL and the other altcoins have no other).
/// See the module doc's options section.
const OPTION_CURRENCIES: [&str; 3] = ["BTC", "ETH", "USDC"];

/// One `public/get_instruments` URL. `kind` is mandatory by construction: an unfiltered request
/// returns every kind and would overlap the others (module doc).
fn instruments_url(currency: &str, kind: &str) -> String {
    format!("{BASE}?currency={currency}&kind={kind}")
}

/// The whole request set, in fetch order: the all-currency kinds, then one option book per
/// [`OPTION_CURRENCIES`] entry.
fn request_urls() -> Vec<String> {
    let mut urls: Vec<String> =
        ALL_CURRENCY_KINDS.iter().map(|kind| instruments_url(ANY_CURRENCY, kind)).collect();
    urls.extend(OPTION_CURRENCIES.iter().map(|currency| instruments_url(currency, "option")));
    urls
}

/// Classify ONE Deribit instrument row from the VENUE'S OWN words: `kind`, refined for futures by
/// `settlement_period`. `kind == "option"` → [`AssetClass::Option`]; `kind == "spot"` →
/// [`AssetClass::CryptoSpot`]; `kind == "future"` → [`AssetClass::CryptoPerp`] when
/// `settlement_period == "perpetual"`, else [`AssetClass::CryptoFuture`].
///
/// `None` for every other `kind` (`future_combo`, `option_combo`) and for a row carrying no `kind`
/// at all — an honestly ABSENT class, never a guess. The catalog SKIPS such a row (it advertises
/// exactly the four classes above); the grid parser leaves `SymbolProperties::asset_class` unset.
///
/// **This is the venue's ONE kind decision.** [`contract_of`] dispatches on it, and
/// [`properties_of`] — which both of Deribit's instrument payloads reach: [`parse_instruments`]
/// below (the plural `public/get_instruments` list) and `crates/bridges/deribit/src/exec.rs`'s
/// `parse_get_instrument` (the singular `public/get_instrument` grid, the row the PIT properties
/// store actually records) — stores it, so neither can answer differently about the same
/// instrument. It survives beside [`contract_of`] because a GRID needs a class even when the row
/// lacks the fields a full [`Contract`] needs.
///
/// ⚠ **Never derived from `instrument_name`.** `BTC-PERPETUAL` looks decisive and is exactly the
/// implicit encoding `docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`
/// exists to remove; the venue publishes both words this reads. Only `kind` decides between a spot
/// pair, an option and a future, and `settlement_period` is consulted for a `future` ALONE — a spot
/// or option row that happens to carry `"perpetual"` stays what its `kind` says.
pub fn asset_class_of(row: &Value) -> Option<AssetClass> {
    match row.get("kind").and_then(|v| v.as_str())? {
        "option" => Some(AssetClass::Option),
        "spot" => Some(AssetClass::CryptoSpot),
        "future" => {
            Some(if row.get("settlement_period").and_then(|v| v.as_str()) == Some("perpetual") {
                AssetClass::CryptoPerp
            } else {
                AssetClass::CryptoFuture
            })
        }
        _ => None,
    }
}

/// A non-empty string field, uppercased (the venue lowercases a currency on some rows).
fn upper(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_uppercase)
}

/// The venue's `instrument_type` as a [`Margining`]: `"reversed"` is coin-margined (inverse),
/// `"linear"` is linear. `None` for any other word or none — the caller mints no derivative then.
fn margining_of(row: &Value) -> Option<Margining> {
    match row.get("instrument_type").and_then(|v| v.as_str())? {
        "linear" => Some(Margining::Linear),
        "reversed" => Some(Margining::Inverse),
        _ => None,
    }
}

/// The currency a derivative's `contract_size` is counted in (module doc): an inverse FUTURE or
/// PERPETUAL is sized in its `counter_currency` (USD), a linear one and every option in its
/// `base_currency`. `None` when the row lacks that field.
fn contract_unit(row: &Value, margining: Margining, is_option: bool) -> Option<String> {
    if !is_option && matches!(margining, Margining::Inverse) {
        upper(row, "counter_currency")
    } else {
        upper(row, "base_currency")
    }
}

/// A dated future's `settlement_period` as its [`Tenor`]; a word the venue has not used so far is
/// [`Tenor::Other`], an absent one `None`.
fn tenor_of(row: &Value) -> Option<Tenor> {
    Some(match row.get("settlement_period").and_then(|v| v.as_str())? {
        "day" => Tenor::Day,
        "week" => Tenor::Week,
        "month" => Tenor::Month,
        _ => Tenor::Other,
    })
}

/// The venue's `option_type` word as an [`OptionKind`].
fn option_right_of(row: &Value) -> Option<OptionKind> {
    match row.get("option_type").and_then(|v| v.as_str())? {
        "call" => Some(OptionKind::Call),
        "put" => Some(OptionKind::Put),
        _ => None,
    }
}

/// The typed [`Contract`] ONE `get_instruments`/`get_instrument` row defines — the module doc's
/// "What each row becomes" is the field map. Dispatches on [`asset_class_of`], so the kind is
/// decided in one place.
///
/// `None` when the row is a kind this tree mints nothing for (the combos, a `kind`-less row), or
/// when a derivative row lacks a field its variant REQUIRES (margining, settlement currency, an
/// expiry for a dated future or an option, a strike or right for an option): there is no honest
/// default for any of them. Every real row the fixture holds carries all of them.
pub fn contract_of(row: &Value) -> Option<Contract> {
    let class = asset_class_of(row)?;
    if class == AssetClass::CryptoSpot {
        return Some(Contract::CryptoSpot);
    }
    let margining = margining_of(row)?;
    let settle = upper(row, "settlement_currency")?;
    let expiry = || row.get("expiration_timestamp").and_then(json_int);
    match class {
        // ⚠ `expiration_timestamp` is NOT read: a perpetual's is the 32503708800000 sentinel.
        AssetClass::CryptoPerp => Some(Contract::Perp(PerpSpec {
            margining,
            settle,
            value_ccy: contract_unit(row, margining, false),
            funding_interval_ms: None,
            settling_ms: None,
        })),
        AssetClass::CryptoFuture => Some(Contract::Future(FutureSpec {
            margining,
            settle,
            value_ccy: contract_unit(row, margining, false),
            expiry_ms: expiry()?,
            tenor: tenor_of(row),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        })),
        AssetClass::Option => Some(Contract::Option(OptionSpec {
            right: option_right_of(row)?,
            strike: row.get("strike").and_then(json_num)?,
            expiry_ms: expiry()?,
            style: ExerciseStyle::European,
            margining,
            settle,
            value_ccy: contract_unit(row, margining, true),
            underlying: upper(row, "base_currency"),
        })),
        _ => None,
    }
}

/// The order grid ONE instrument row publishes — shared by [`parse_instruments`] (each row of the
/// plural list) and `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` (the singular
/// `public/get_instrument` result, the venue's PRODUCTION properties writer for every kind).
///
/// `tick_size` gates (`None` without it); `min_trade_amount` is both step and minimum quantity (no
/// per-instrument max or min-notional — `..Default::default()` covers those and `taker_hold_ms`,
/// Deribit declaring no venue hold). `contract_size` absent → `0.0`, which
/// `SymbolProperties::multiplier` folds to the inert 1.0; it is what makes a Deribit option's
/// notional `qty * price * contract_size`, the value the order-entry notional cap needs. The class
/// is [`asset_class_of`]'s — ABSENT for a combo or `kind`-less row, whose grid is still returned and
/// still usable. The tiered `tick_size_steps` grid is attached by [`parse_tick_scheme`].
pub fn properties_of(row: &Value) -> Option<SymbolProperties> {
    let tick = row.get("tick_size").and_then(json_num)?;
    let step = row.get("min_trade_amount").and_then(json_num).unwrap_or(0.0);
    let properties = SymbolProperties {
        tick_size: tick,
        step_size: step,
        min_qty: step,
        contract_size: row.get("contract_size").and_then(json_num).unwrap_or(0.0),
        asset_class: asset_class_of(row),
        ..Default::default()
    };
    // Attach the tiered grid when the venue reports one; absent/empty/malformed steps leave it
    // scheme-less. The scheme's base tick IS `tick` above, so the
    // `SymbolProperties::with_tick_scheme` base-tick invariant holds by construction.
    Some(match parse_tick_scheme(row, tick) {
        Some(scheme) => properties.with_tick_scheme(scheme),
        None => properties,
    })
}

/// Parse Deribit's optional `tick_size_steps` array —
/// `[{"above_price": 0.005, "tick_size": 0.0005}, …]` (options carry it; futures/perps do not) —
/// into a tiered [`TickScheme`] whose base tick IS the instrument's scalar `tick_size`. The equal
/// base tick is what satisfies the [`SymbolProperties::with_tick_scheme`] caller invariant.
///
/// Returns `None` when the array is ABSENT or EMPTY (a flat grid — the scalar `tick_size` IS the
/// whole grid), OR when a tier is MALFORMED (a missing/non-finite field, an unsorted or over-long
/// array — all rejected by [`TickScheme::new`]): a bad venue array degrades to the scalar grid
/// (logged) rather than failing the whole grid parse or shipping a zero-tick grid. A present,
/// valid, non-empty array builds the scheme with the tiers in venue order.
fn parse_tick_scheme(row: &Value, tick_size: f64) -> Option<TickScheme> {
    let steps = row.get("tick_size_steps")?.as_array()?;
    if steps.is_empty() {
        return None; // an explicit empty array is a flat grid → no scheme (see above)
    }
    // A missing/unparseable tier field becomes NaN, which `TickScheme::new` rejects (BadBoundary /
    // BadTierTick) — so a malformed tier lands in the `Err` arm below, never a silent zero-tick
    // tier. `json_num` decodes both number- and string-encoded values, like the rest of this parse.
    let tiers: Vec<TickTier> = steps
        .iter()
        .map(|s| TickTier {
            above_price: s.get("above_price").and_then(json_num).unwrap_or(f64::NAN),
            tick_size: s.get("tick_size").and_then(json_num).unwrap_or(f64::NAN),
        })
        .collect();
    match TickScheme::new(tick_size, &tiers) {
        Ok(scheme) => Some(scheme),
        Err(e) => {
            tracing::warn!(
                target: "vike_deribit::catalog",
                error = %e,
                "malformed deribit tick_size_steps; falling back to the scalar tick grid"
            );
            None
        }
    }
}

/// Parse a Deribit `{result:[…]}` payload (one request's instruments) into universal
/// [`Instrument`]s, each with its [`contract_of`] contract and [`properties_of`] grid.
/// `is_active == false` rows are skipped, so is every row [`asset_class_of`] cannot classify
/// (`future_combo`/`option_combo`, or a `kind`-less row), and so is a derivative row lacking a
/// field its contract requires — those are counted and warned once per payload, never guessed.
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut incomplete = 0usize;
    for e in list {
        if e.get("is_active").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        if asset_class_of(e).is_none() {
            continue;
        }
        let name = e.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if name.is_empty() {
            continue;
        }
        let Some(contract) = contract_of(e) else {
            incomplete += 1;
            continue;
        };
        let base = e.get("base_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let quote = e.get("quote_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let mut instrument = Instrument::new("deribit", name, base, quote, contract)
            .with_properties(properties_of(e).unwrap_or_default());
        // The venue's own word, VERBATIM (`"reversed"`/`"linear"`) — a spot row publishes
        // `"linear"` too, which no `Contract` field could hold.
        if let Some(word) = e.get("instrument_type").and_then(|v| v.as_str()) {
            instrument = instrument.with_venue_word(word);
        }
        out.push(instrument);
    }
    if incomplete > 0 {
        tracing::warn!(
            target: "vike_deribit::catalog",
            incomplete,
            "deribit instrument rows lacking a field their contract requires were skipped"
        );
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — `public/get_instruments` is a keyless public read); called once per
/// request. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of `list_instruments` over an injectable fetch — per-REQUEST TOLERANT: one request
/// failing must NOT drop the others (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for url in request_urls() {
        match fetch(&url) {
            Ok(v) => out.extend(parse_instruments(&v)),
            Err(e) => {
                let query = url.strip_prefix(BASE).unwrap_or(&url);
                tracing::warn!(target: "vike_deribit::catalog", "{query} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// ONE `public/get_instruments?currency={currency}&kind={kind}` request, mapped by
/// [`parse_instruments`] — the same rows, the same [`contract_of`] and the same skips the catalog
/// applies, for a consumer whose universe the catalog's own `request_urls` does not cover.
/// `currency` is a venue currency or `"any"` (accepted with a `kind`, module doc). Keyless,
/// blocking, NOT per-request tolerant: the one request is the whole answer, so its failure is the
/// caller's error.
///
/// The consumer it exists for: `crates/vike-options/src/chain/index.rs`'s `OptionIndex` as the
/// datahub's options lane builds it (`currency=any` with `kind=option` and `kind=future`, one
/// request each), which needs every option and every future to look a streamed instrument name up
/// instead of parsing it.
pub fn fetch_instruments(currency: &str, kind: &str) -> Result<Vec<Instrument>, CatalogError> {
    Ok(parse_instruments(&fetch_json(&instruments_url(currency, kind))?))
}

/// The deribit venue's `CatalogProvider` contribution: perpetuals + dated futures + spot pairs of
/// EVERY currency, and the BTC, ETH and USDC option books — five requests, tagged via
/// [`parse_instruments`]. Keyless. Per-request tolerant (see [`list_tolerant`]) — mirrors okx's
/// per-instType accumulate.
pub struct DeribitCatalog;

impl CatalogProvider for DeribitCatalog {
    fn venue(&self) -> &str {
        "deribit"
    }
    /// The classes [`Self::list_instruments`] can return — exactly the projections of
    /// [`contract_of`]'s four variants, and `a_full_pass_mints_exactly_the_advertised_classes`
    /// holds the two lists equal in both directions (okx advertised `Option` for a long while
    /// without ever minting one).
    fn asset_classes(&self) -> &[AssetClass] {
        &[
            AssetClass::Option,
            AssetClass::CryptoFuture,
            AssetClass::CryptoPerp,
            AssetClass::CryptoSpot,
        ]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(list_tolerant(fetch_json))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
