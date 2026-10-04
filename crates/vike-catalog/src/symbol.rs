//! The CORE-symbol ⇄ EXCHANGE-symbol conversion, in one place.
//!
//! A core symbol names an instrument unambiguously across the whole workspace; an exchange symbol is
//! what goes on the wire. For most instruments they are identical. They diverge for **perpetuals on
//! venues whose raw symbol does not distinguish a perp from its spot twin**: Binance lists
//! `BTCUSDT` on spot AND `BTCUSDT` as a USDⓈ-M perp, so the core vocabulary appends
//! [`PERP_SUFFIX`] and calls the perp `BTCUSDT.P` (the TradingView convention — see
//! [`crate::instrument`]'s `id()` doc). OKX needs no suffix: `BTC-USDT` and `BTC-USDT-SWAP` are
//! already distinct at the venue.
//!
//! ## Why this is a module and not an idiom
//!
//! It WAS an idiom — the same
//! `symbol.strip_suffix(".P").map(|s| (s.to_string(), true)).unwrap_or(…)` was written out at
//! **eight** call sites across three bridges, twice behind a local helper with a different name
//! (`split_symbol`, `perp_split`). Every one of them was correct.
//!
//! The bug that motivated this module is the site that did **none** of it: Binance's depth feed
//! passed the raw core symbol straight into its stream-name builder, producing
//! `btcusdt.p@depth@100ms` — a stream no venue resolves. It connected, streamed nothing, and the
//! recorder wrote placeholder rows for hours without a single error. An idiom repeated eight times
//! cannot be checked; a named function with one home can be grepped for, and its absence at a
//! wire-facing call site is visible.
//!
//! Nothing here allocates: [`split_perp`] borrows out of its input, so a caller that needs an owned
//! `String` opts into that cost explicitly.
//!
//! ## ⚠ Two halves, and only one of them has consumers
//!
//! The SUFFIX half ([`PERP_SUFFIX`], [`split_perp`], [`split_perp_at`], [`uses_perp_suffix`],
//! [`fee_lane`]) is wired: every wire-facing site on binance, bybit and aster calls it, and
//! `crates/vike-datahub/src/server/seed_series.rs`'s `refuse_unhonourable_class` calls it at its own door.
//!
//! The RENDERER half ([`to_core_symbol`], [`to_exchange_symbol`], [`PAIR_SEPARATOR`],
//! [`DEX_MARKER`], [`writes_a_pair_with_a_slash`]) has **none, deliberately, and it cannot get one
//! byte-identically** — which is a statement about what wiring it COSTS, not about it being
//! forgotten. Today `crates/bridges/hyperliquid/src/symbology.rs` mints `HYPE/USDC` as the
//! workspace's symbol for that instrument, and `vike_model::paths::store_path::refuse_a_path_hostile_symbol`
//! REFUSES it at the store door: the slash would silently add a partition level. Calling
//! [`to_core_symbol`] at that venue's catalog would spell the same instrument `HYPE-USDC`, which is
//! a different series key, a different store partition and a different thing for a mount to name.
//! That is `docs/decisions/0061`'s STEP 2 — one venue, one row, its own argument and its own
//! migration question — and the playbook's rule is that STEP 1 declares and STEP 2 changes. This
//! module is the declaration; nothing here is licensed to rewrite a symbol a venue already answers
//! to.

/// The market suffix marking a core symbol as a PERPETUAL contract.
///
/// Siblings exist in the same convention (`.F` futures, `.O` options) but no venue adapter maps
/// them today, so only this one is named here.
pub const PERP_SUFFIX: &str = ".P";

/// Split a CORE symbol into its EXCHANGE symbol and whether it names a perpetual.
///
/// `"BTCUSDT.P"` → `("BTCUSDT", true)`; `"BTCUSDT"` → `("BTCUSDT", false)`.
///
/// The returned symbol borrows from `symbol` — the WIRE form, for URLs, stream names and REST
/// params. The caller's original (suffixed) string stays the SERIES/sink label, so a perp's series
/// key never collides with its spot twin's.
pub fn split_perp(symbol: &str) -> (&str, bool) {
    match symbol.strip_suffix(PERP_SUFFIX) {
        Some(base) => (base, true),
        None => (symbol, false),
    }
}

/// Whether this venue's CORE symbols carry [`PERP_SUFFIX`] to tell a perp from its spot twin.
///
/// `false` does NOT mean "no perps" — it means the venue's own symbols are already unambiguous, so
/// a suffix would be noise the adapter would immediately strip again. Two worked examples, both
/// read from their adapters rather than assumed: OKX (`BTC-USDT` spot vs `BTC-USDT-SWAP`) and
/// **Hyperliquid**, whose catalog says so outright — *"No `.P` suffix — the bare coin is already a
/// distinct id namespace from any spot pair"* (`hyperliquid/src/catalog.rs`), its perps being bare
/// coins (`BTC`) against `HYPE/USDC`-style spot pairs.
///
/// # ⚠ DERIVED from the addressing table, and it used to be a second list of the same three venues
///
/// This read `matches!(venue, "binance" | "bybit" | "aster")` — a hand-written roster sitting one
/// module away from [`crate::addressing_for`], whose [`crate::Naming::PerpSuffix`] rows name
/// EXACTLY those three venues and answer the SAME question. Nothing held the two equal. That is
/// `docs/decisions/0061`'s own objection to this axis — *"two vocabularies for one question is the
/// defect this tree gates elsewhere"*, and the record lists four partial encodings of it already —
/// so rather than add a fifth and a gate to hold it in step, this function now READS the table.
/// The addressing row is the authority because it is the one that carries the adapter citation per
/// venue and the `vike:new-venue:row` marker, so a new bridge answers this question once, there.
///
/// BYTE-IDENTICAL, and the pin is `derivation_matches_the_roster_it_replaced` below: the same three
/// roster venues answer `true`, every other roster venue answers `false`, and an OFF-roster string
/// still answers `false` (the addressing fallback is [`crate::Naming::Unaddressable`], not
/// `PerpSuffix`), which is the direction that would have been easy to get wrong.
pub fn uses_perp_suffix(venue: &str) -> bool {
    matches!(crate::addressing_for(venue).naming, crate::Naming::PerpSuffix)
}

/// [`split_perp`] asked of a VENUE rather than of a bare string — the wire seam a bridge should
/// call.
///
/// At a venue that does not [`uses_perp_suffix`] this is the IDENTITY: `(symbol, false)`, nothing
/// stripped. At a suffix venue it is exactly [`split_perp`].
///
/// # Why this exists, in `docs/decisions/0061`'s own words
///
/// That record's Phase 1 pins the contradiction this closes: *"`uses_perp_suffix` has no production
/// consumer. Every reference outside its own tests is a doc comment; every bridge that uses the
/// suffix calls [`split_perp`] unconditionally without ever asking whether its venue is a suffix
/// venue."* A bridge that strips `.P` on a venue with no suffix convention is answering a question
/// its venue never asked — and it is not hypothetical that a lane can be venue-generic: this crate's
/// binance FAMILY driver
/// (`crates/bridges/binance/src/family/market_feed.rs`'s `depth_main`) already has the venue in
/// hand as `ctx.spec.venue` and split unconditionally anyway.
///
/// # Why BOTH functions stay, rather than one replacing the other
///
/// They answer different questions and neither is a spelling of the other. [`split_perp`] is the
/// STRING primitive — *does this text carry the marker* — and it is what [`fee_lane`] and the
/// datahub's `refuse_unhonourable_class` need, because each has already established which venue it
/// is talking about by another route. `split_perp_at` is the VENUE question, and it is the one a
/// wire-facing adapter is actually asking. A bridge that knows its venue should call this one; a
/// caller holding only a string calls the other and says why.
///
/// # Byte-identity today
///
/// Every production caller of [`split_perp`] in a bridge is on binance, bybit or aster — the three
/// [`uses_perp_suffix`] venues — so swapping a call site to this function changes nothing that goes
/// on a wire. `split_perp_at_is_split_perp_at_every_suffix_venue` is the pin, and its twin
/// `split_perp_at_is_inert_at_a_non_suffix_venue` is what the guard buys.
pub fn split_perp_at<'a>(venue: &str, symbol: &'a str) -> (&'a str, bool) {
    if !uses_perp_suffix(venue) {
        return (symbol, false);
    }
    split_perp(symbol)
}

/// The FEE-table lane key for a `(venue, symbol)` pair — the [`vike_model::fee_schedule_for`]
/// argument a caller must use once the venue's exec routes SPOT vs PERP on [`PERP_SUFFIX`].
///
/// Returns `venue` unchanged for every single-lane venue, and the venue's PERP LANE SUB-KEY
/// (`"binance-perp"` / `"aster-perp"`) for a `.P` symbol on a DUAL-LANE venue. Borrowed either way
/// — nothing allocates.
///
/// ## Why a lane key rather than a second registry
///
/// This is the SAME convention `vike_model::venues::venue_tif::venue_tif` already established for the one
/// axis that had already hit this problem: a lane sub-key is a plain string that gets its own arm
/// in the ONE authority table, the BARE venue id keeps meaning the lane it always meant, and the
/// caller that knows which lane it is on passes the sub-key (binance's perp order builder passes
/// `vike_binance::perp::TIF_LANE`; here the mount passes what this function returns). A lane
/// sub-key is NOT a roster venue — `vike_model::VENUES` completeness tests classify roster ids
/// only, exactly as `venue_tif`'s do.
///
/// ## Why this lives in vike-catalog and not in vike-model
///
/// The fee VALUES belong to `vike_model::money::fees` (the bottom layer every crate reaches down to), but
/// resolving a lane needs [`split_perp`] — and `vike-model` sits BELOW this crate, so it cannot see
/// it. Re-deriving `.P` inside `vike-model` is precisely the repeated idiom this module's doc exists
/// to prevent, so the split-owning crate owns the lane resolution and hands `vike-model` a key.
///
/// ## Which venues are dual-lane, and why bybit is not
///
/// A venue is dual-lane here iff its `exec.rs` branches on the suffix — binance's and aster's
/// `run()` both do `let (api_symbol, is_perp) = split_symbol(&symbol); if is_perp { run_perp } else
/// { run_spot }`. bybit also [`uses_perp_suffix`], but its exec is V5 LINEAR-PERP ONLY (no spot
/// arm exists to route to), so its single fee row already IS its perp row and a lane key would be
/// noise. `fee_lane_is_declared_for_every_perp_suffix_venue` below is what forces that question to
/// be answered for a NEW `.P` venue instead of letting it inherit one lane's fees silently.
///
/// ## A venue may have MORE than two lanes — aster has three
///
/// "Spot vs perp" is not the only split a venue can price on. Aster runs ONE perp order API and
/// charges three different taker rates on it, by CONTRACT CLASS, so a `.P` aster symbol resolves to
/// either `"aster-perp"` or `"aster-perp-usd1"` depending on what the contract settles in. See
/// [`USD1_SETTLED_SUFFIX`] for the rule and for the third class this function deliberately does
/// NOT resolve.
pub fn fee_lane<'a>(venue: &'a str, symbol: &str) -> &'a str {
    let (exchange_symbol, perp) = split_perp(symbol);
    if !perp {
        return venue;
    }
    match venue {
        "binance" => "binance-perp",
        // Aster's perp lane is priced by CONTRACT CLASS, not by one flat rate.
        "aster" if exchange_symbol.ends_with(USD1_SETTLED_SUFFIX) => "aster-perp-usd1",
        "aster" => "aster-perp",
        // Single-fee-lane venues (bybit and every non-suffix venue): the bare row is the only row.
        _ => venue,
    }
}

/// **Whether an engine mounted on `engine_symbol` at `venue` can hold a TP/SL bracket's
/// stop-loss** — the ONE rule the node and the Trade window both read (final review B, I-1): the
/// node refuses a bracket by it (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_engine_refusal`),
/// and the desktop's dispatcher and glue refuse and explain the same bracket before the click, so
/// the two sides cannot disagree about a lane. It lived in the node until the window needed it.
///
/// `false` exactly for an engine on the SPOT lane of a dual-lane venue (binance, aster): their
/// `vike_model::caps_for` rows list `stop` as the spot+perp UNION, while the spot order builder
/// sends `type=STOP` with no stop price, so the core preflight passes the stop-loss leg and the
/// venue rejects it only after the entry filled — an unprotected position the trader believes is
/// protected. Every other venue has one lane, which holds a stop whatever the engine trades.
///
/// ⚠ It is the ENGINE's symbol, never an order's: an adapter picks its lane ONCE, from the symbol
/// its engine was mounted on, and signs every order on it, so a `.P` order to a spot-mounted engine
/// is a spot order. The lane is read with [`split_perp_at`], exactly as the adapter reads it, so a
/// lower-case `.p` is the SPOT lane.
pub fn engine_lane_holds_stop(venue: &str, engine_symbol: &str) -> bool {
    !spot_lane_holds_no_stop(venue) || split_perp_at(venue, engine_symbol).1
}

/// The venues whose SPOT lane cannot hold a bracket's stop-loss: the DUAL-LANE ones, whose exec
/// routes spot vs perp on the `.P` suffix and whose one caps row is the union of both lanes.
///
/// ⚠ A second spelling of a set [`fee_lane`] already encodes (its doc says which venues branch on
/// the suffix, and why bybit does not), so it is PINNED to that function over the whole roster
/// (`the_spot_lane_venues_are_exactly_fee_lanes_dual_lane_venues`): a new dual-lane venue reddens
/// that test rather than silently accepting spot brackets.
fn spot_lane_holds_no_stop(venue: &str) -> bool {
    matches!(venue, "binance" | "aster")
}

/// The settlement asset that puts an aster perp on its own fee lane.
///
/// Aster charges three different taker rates on ONE perp order API, split by contract class — a
/// live sweep of the venue's own `GET /fapi/v3/commissionRate` across 17 symbols on 2026-08-05
/// found crypto at 4 bps, equity/ETF/commodity at 0.9 bps and USD1-settled at 0.5 bps (see
/// `vike_model::money::fees`' `fee_schedule_for` for the full table and its sourcing).
///
/// Of those three classes, **only the USD1 one is decidable from the symbol**, because the
/// settlement asset is part of the name: `BTCUSD1` settles in USD1, `BTCUSDT` in USDT. The equity
/// class is deliberately NOT resolved here — `AAPLUSDT` and `ASTERUSDT` are the same string shape,
/// and only the venue's `exchangeInfo` `underlyingSubType` separates them. A pure string function
/// has no honest way to know that, and baking in a dated ~105-symbol membership list is exactly the
/// drift this module's doc exists to argue against; `fee_schedule_for`'s `"aster-perp"` arm carries
/// the measured numbers and the reason it stays a documented gap.
///
/// ⚠ Matched as a SUFFIX of the EXCHANGE symbol (post-`.P`-strip), never as a substring:
/// `USD1USDT` is USDT-settled with USD1 as the BASE asset and must keep the crypto lane.
/// `usd1_lane_matches_settlement_not_substring` is the pin.
pub const USD1_SETTLED_SUFFIX: &str = "USD1";

/// The separator a CORE symbol writes where a venue writes `/`.
///
/// `/` cannot appear in a core symbol at all: the symbol becomes the directory name
/// `symbol=<symbol>` in the hist store, so a slash silently adds a path level —
/// `vike_model::paths::store_path::refuse_a_path_hostile_symbol` carries that measurement and refuses it
/// at the store's door. A hyphen is already the workspace's pair separator in every venue that
/// needs one (`BTC-1JAN27-100000-C`, `EUR_USD`'s sibling spellings), so this is the spelling that
/// was already here rather than a new convention.
pub const PAIR_SEPARATOR: char = '-';

/// The marker a CORE symbol writes for an instrument listed on a venue's THIRD-PARTY sub-venue.
///
/// Hyperliquid's builder-deployed perp DEXes name their instruments `<dex>:<coin>` (`xyz:TSLA`),
/// and a colon is a Win32-reserved character — a directory holding one cannot be created on
/// Windows at all, measured natively. So the core spelling moves the dex to the END behind this
/// marker: `TSLA.d-xyz`.
///
/// ⚠ **The `d-` half is load-bearing and is not decoration.** The suffix namespace already carries
/// PRODUCT CLASSES ([`PERP_SUFFIX`] `.P`, with `.F`/`.O` reserved beside it) — a closed set this
/// workspace owns. Dex names are chosen by third parties and the set is open: eleven exist today
/// (`xyz`, `flx`, `vntl`, `hyna`, `km`, `abcd`, `cash`, `para`, `mkts`, `io`), and nothing stops a
/// twelfth being called `s` or `p`. Without `d-`, `TSLA.S` would be unreadable — a spot TSLA, or a
/// TSLA perp on a dex named `s`? With it, the two namespaces cannot collide by construction.
pub const DEX_MARKER: &str = ".d-";

/// Whether this venue writes a PAIR with a slash, so its core symbols carry [`PAIR_SEPARATOR`]
/// instead.
///
/// Declared per venue rather than inferred, the [`uses_perp_suffix`] shape and for its reason: a
/// new venue has to answer the question rather than inherit whichever answer it was written next
/// to. MEASURED 2026-09-19 across every wired venue — these two are the whole set.
///
/// * **hyperliquid** — spot pairs only. Its unified spelling is `BASE/QUOTE` (`HYPE/USDC`), built
///   by this workspace from `spotMeta.universe[].tokens`; the venue itself says `@<pairIndex>` for
///   327 of its 328 pairs (`PURR/USDC` is the one literal). Its PERPS are bare coins and carry no
///   separator at all.
/// * **alpaca** — crypto pairs only (`BTC/USD`). ⚠ There the slash is LOAD-BEARING on the wire:
///   `crates/bridges/alpaca/src/data.rs`'s `class_of` is `if symbol.contains('/') { Crypto } else
///   { Equity }`, which is exactly why [`to_exchange_symbol`] needs a class rather than a string.
///
/// Not here, and each checked rather than assumed: oanda writes `EUR_USD` (its `/` lives only in
/// `displayName`), deribit writes `BTC-1JAN27-100000-C`, binance/bybit/okx/aster concatenate, and
/// polymarket keys a series on a GROUP rather than a symbol.
pub fn writes_a_pair_with_a_slash(venue: &str) -> bool {
    matches!(venue, "hyperliquid" | "alpaca")
}

/// A venue's own spelling -> the CORE symbol, which is always safe to be a directory name.
///
/// Never needs a class: the venue's spelling is what disambiguates it. A `/` means a pair and a
/// `:` means a dex listing, and neither can appear in a core symbol, so the mapping is total in
/// this direction.
///
/// ```text
/// hyperliquid  HYPE/USDC -> HYPE-USDC      xyz:TSLA -> TSLA.d-xyz      BTC -> BTC
/// alpaca       BTC/USD   -> BTC-USD        AAPL     -> AAPL
/// binance      BTCUSDT   -> BTCUSDT   (untouched — no venue but the two above writes a pair)
/// ```
pub fn to_core_symbol(venue: &str, exchange: &str) -> String {
    if let Some((dex, coin)) = exchange.split_once(':') {
        return format!("{coin}{DEX_MARKER}{dex}");
    }
    if writes_a_pair_with_a_slash(venue) {
        return exchange.replace('/', &PAIR_SEPARATOR.to_string());
    }
    exchange.to_string()
}

/// The CORE symbol -> a venue's own spelling, the inverse of [`to_core_symbol`].
///
/// # Why this one takes a class and its inverse does not
///
/// The forward direction is where the information is missing. On **alpaca** a core `BRK-B` is an
/// equity share class and a core `BTC-USD` is a crypto pair, and **no string function can tell them
/// apart** — the venue's own classifier is the slash this conversion exists to restore. So the
/// caller must say which, and a caller that does not say is REFUSED rather than guessed at, which
/// is `docs/decisions/0061-an-instrument-names-its-kind.md`'s ruling 3 in this workspace's own
/// words: *"a missing claim on an AMBIGUOUS venue symbol is a REFUSAL"*. `vike_catalog::Instrument`
/// carries `asset_class` as a field, so the claim is available wherever an instrument is.
///
/// **hyperliquid needs no claim** and is not asked for one: MEASURED 2026-09-19 over its live
/// metadata, not one of its 523 perp names nor one of its spot token names contains a hyphen, so a
/// core symbol's shape decides it — [`DEX_MARKER`] is a dex listing, a [`PAIR_SEPARATOR`] is a spot
/// pair, and neither is a bare coin.
///
/// # Errors
///
/// An ambiguous core symbol with no class — today only alpaca's hyphenated shape.
pub fn to_exchange_symbol(
    venue: &str,
    core: &str,
    class: Option<vike_model::AssetClass>,
) -> Result<String, String> {
    if let Some((coin, dex)) = core.split_once(DEX_MARKER) {
        return Ok(format!("{dex}:{coin}"));
    }
    if !writes_a_pair_with_a_slash(venue) || !core.contains(PAIR_SEPARATOR) {
        return Ok(core.to_string());
    }
    // Hyperliquid's shape decides: a hyphen there is a spot pair and can be nothing else.
    if venue == "hyperliquid" {
        return Ok(core.replacen(PAIR_SEPARATOR, "/", 1));
    }
    match class {
        Some(vike_model::AssetClass::CryptoSpot | vike_model::AssetClass::CryptoPerp) => {
            Ok(core.replacen(PAIR_SEPARATOR, "/", 1))
        }
        Some(_) => Ok(core.to_string()),
        None => Err(format!(
            "{venue} symbol {core:?} carries a {PAIR_SEPARATOR:?} and no asset class was claimed, \
             so this conversion cannot tell a crypto PAIR (`BTC-USD` -> `BTC/USD`) from an equity \
             share CLASS (`BRK-B`, which must not gain a slash). The venue's own classifier IS the \
             slash — `crates/bridges/alpaca/src/data.rs`'s `class_of` reads it — so restoring it is \
             the one thing a string cannot decide. Pass the class; \
             `vike_catalog::Instrument::asset_class` carries it."
        )),
    }
}

#[path = "symbol_tests.rs"]
#[cfg(test)]
mod symbol_tests;
