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
//! forgotten. Today `crates/bridges/hyperliquid/src/base/symbology.rs` mints `HYPE/USDC` as the
//! workspace's symbol for that instrument, and `vike_paths::store_path::refuse_a_path_hostile_symbol`
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

/// The FEE-table lane key for a `(venue, symbol)` pair — the [`vike_venues::fee_schedule_for`]
/// argument a caller must use once the venue's exec routes SPOT vs PERP on [`PERP_SUFFIX`].
///
/// Returns `venue` unchanged for every single-lane venue, and the venue's PERP LANE SUB-KEY
/// (`"binance-perp"` / `"aster-perp"`) for a `.P` symbol on a DUAL-LANE venue. Borrowed either way
/// — nothing allocates.
///
/// ## Why a lane key rather than a second registry
///
/// This is the SAME convention `vike_venues::venue_tif::venue_tif` already established for the one
/// axis that had already hit this problem: a lane sub-key is a plain string that gets its own arm
/// in the ONE authority table, the BARE venue id keeps meaning the lane it always meant, and the
/// caller that knows which lane it is on passes the sub-key (binance's perp order builder passes
/// `vike_binance::perp::TIF_LANE`; here the mount passes what this function returns). A lane
/// sub-key is NOT a roster venue — `vike_model::VENUES` completeness tests classify roster ids
/// only, exactly as `venue_tif`'s do.
///
/// ## Why this lives in vike-catalog and not in vike-model
///
/// The fee VALUES belong to `vike_venues::fees` (the bottom layer every crate reaches down to), but
/// resolving a lane needs [`split_perp`] — and `vike-model` sits BELOW this crate, so it cannot see
/// it. Re-deriving `.P` inside `vike-model` is precisely the repeated idiom this module's doc exists
/// to prevent, so the split-owning crate owns the lane resolution and hands `vike-model` a key.
///
/// ## Which venues are dual-lane, and why bybit is not
///
/// A venue is dual-lane here iff its `exec.rs` branches on the suffix — binance's and aster's
/// `run()` both do `let (api_symbol, is_perp) = split_symbol(&symbol); if is_perp { run_perp } else
/// { run_spot }`. bybit also [`uses_perp_suffix`], but its exec does not branch on `.P` (the bare
/// name is the perpetual, whatever suffix is typed), so it is not dual-lane in this sense: its bare row IS its
/// perp row. Since decision 0231 its exec also signs a SPOT pair, spelled `PAIR.S`, and that is priced by the
/// shape of the symbol (below) like hyperliquid's. `fee_lane_is_declared_for_every_perp_suffix_venue` below is
/// what forces that question to be answered for a NEW `.P` venue instead of letting it inherit one lane's fees
/// silently.
///
/// ## A venue may have MORE than two lanes — aster has three
///
/// "Spot vs perp" is not the only split a venue can price on. Aster runs ONE perp order API and
/// charges three different taker rates on it, by CONTRACT CLASS, so a `.P` aster symbol resolves to
/// either `"aster-perp"` or `"aster-perp-usd1"` depending on what the contract settles in. See
/// [`USD1_SETTLED_SUFFIX`] for the rule and for the third class this function deliberately does
/// NOT resolve.
///
/// ## Two lanes are decided by the symbol's SHAPE, not by a `.P` suffix
///
/// **deribit** prices an options book and a perpetual book under one id: a `...-PERPETUAL` instrument
/// resolves to `"deribit-perp"` (a flat taker rate), every other instrument keeps the bare
/// options row. **hyperliquid** prices perps and spot pairs apart: a spot pair (`HYPE/USDC`,
/// `HYPE-USDC`) resolves to `"hyperliquid-spot"`, every perp keeps the bare row. Both are
/// decidable from the string alone (`is_deribit_perpetual`, `is_hyperliquid_spot`), so they clear
/// the bar the paper/backtest mount sets (it fetches no venue metadata). The HIP-3 builder-dex perps
/// (`xyz:TSLA`, `TSLA.d-xyz`) are NOT a lane: their schedule depends on the deployer's growth mode,
/// which only the venue's metadata states.
pub fn fee_lane<'a>(venue: &'a str, symbol: &str) -> &'a str {
    // The two lanes decided by the symbol's SHAPE rather than a `.P` suffix (see the section below).
    match venue {
        "deribit" if is_deribit_perpetual(symbol) => return DERIBIT_PERP_LANE,
        "hyperliquid" if is_hyperliquid_spot(symbol) => return HYPERLIQUID_SPOT_LANE,
        // bybit's exec plane spells a SPOT pair `PAIR.S` (decision 0231; the suffix is the bybit bridge's
        // `classes::SPOT_SUFFIX`, which this crate cannot name): its fee is the spot schedule, not the perpetual's.
        "bybit" if symbol.ends_with(".S") => return BYBIT_SPOT_LANE,
        _ => {}
    }
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

/// The lane key of a Deribit PERPETUAL (`BTC-PERPETUAL`, `BTC_USDC-PERPETUAL`): the venue's bare
/// `deribit` row is its OPTIONS schedule (`min(0.03 % of the underlying, 12.5 % of the premium)`), and a
/// perpetual is charged a flat taker rate instead (measured 5.0 bps on ETH-PERPETUAL, testnet).
const DERIBIT_PERP_LANE: &str = "deribit-perp";

/// The lane key of a Bybit SPOT pair (`DOGEUSDT.S`): the venue's bare row is its linear-perpetual schedule
/// (2 / 5.5 bps), and a spot pair is charged 10 / 10 bps at VIP0.
const BYBIT_SPOT_LANE: &str = "bybit-spot";

/// The lane key of a Hyperliquid SPOT pair: the venue's bare row is its PERP schedule, and a spot pair
/// is charged more (4 bps maker, 7 bps taker at the base tier, against 1.5 and 4.5).
const HYPERLIQUID_SPOT_LANE: &str = "hyperliquid-spot";

/// Whether `symbol` names a Deribit perpetual: an instrument name ending in `-PERPETUAL`.
///
/// Decidable from the symbol alone, which is the bar a lane must clear (see `vike_venues::fees`'
/// `fee_schedule_for`): Deribit's instrument names are `<CCY>-PERPETUAL` and `<CCY>_<CCY>-PERPETUAL`,
/// dated futures are `<CCY>-<DDMMMYY>` and options `<CCY>-<DDMMMYY>-<strike>-<C|P>`. Deribit's names
/// are upper-case, but the match ignores case: a lower-cased mount must not silently take the
/// OPTIONS row. Dated futures are NOT this lane (their fee is not the perpetual's).
fn is_deribit_perpetual(symbol: &str) -> bool {
    const SUFFIX: &str = "-PERPETUAL";
    symbol.len() > SUFFIX.len()
        && symbol.get(symbol.len() - SUFFIX.len()..).is_some_and(|s| s.eq_ignore_ascii_case(SUFFIX))
}

/// Whether `symbol` names a Hyperliquid SPOT pair, in either of its two spellings: the venue-side
/// `HYPE/USDC` the bridge mints today, and the core `HYPE-USDC` of [`to_core_symbol`]. Both are ONE
/// separator between two non-empty names. A PERP is a bare coin (`BTC`, `kPEPE`), a builder-dex perp is
/// `xyz:TSLA` / `TSLA.d-xyz` ([`DEX_MARKER`]), and none of the 523 perp or the spot token names
/// measured on 2026-09-19 contains a hyphen (see [`to_exchange_symbol`]), so the shape decides it. A
/// symbol with two separators (`BTC-USDT-SWAP`, another venue's spelling) is not a Hyperliquid pair.
fn is_hyperliquid_spot(symbol: &str) -> bool {
    if symbol.contains(':') || symbol.contains(DEX_MARKER) {
        return false;
    }
    let mut parts = symbol.split(['/', PAIR_SEPARATOR]);
    matches!((parts.next(), parts.next(), parts.next()), (Some(base), Some(quote), None) if !base.is_empty() && !quote.is_empty())
}

/// **Whether an engine mounted on `engine_symbol` at `venue` can hold a TP/SL bracket's
/// stop-loss** — the ONE rule the node and the Trade window both read (final review B, I-1): the
/// node refuses a bracket by it (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_engine_refusal`),
/// and the desktop's dispatcher and glue refuse and explain the same bracket before the click, so
/// the two sides cannot disagree about a lane. It lived in the node until the window needed it.
///
/// `false` exactly for an engine on the SPOT lane of a dual-lane venue (binance, aster): their
/// `vike_venues::caps_for` rows list `stop` as the spot+perp UNION, while the spot order builder
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
/// `vike_venues::fees`' `fee_schedule_for` for the full table and its sourcing).
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
pub(crate) const USD1_SETTLED_SUFFIX: &str = "USD1";

/// The separator a CORE symbol writes where a venue writes `/`.
///
/// `/` cannot appear in a core symbol at all: the symbol becomes the directory name
/// `symbol=<symbol>` in the hist store, so a slash silently adds a path level —
/// `vike_paths::store_path::refuse_a_path_hostile_symbol` carries that measurement and refuses it
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
/// words: *"a missing claim on an AMBIGUOUS venue symbol is a REFUSAL"*. `vike_model::Instrument`
/// projects its class from its contract (`asset_class()`), so the claim is available wherever an
/// instrument is.
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
             `vike_model::Instrument::asset_class` carries it."
        )),
    }
}

/// Canonical symbol → OANDA instrument form (`"eurusd"` -> `"EUR_USD"`).
///
/// It lives here, not in the OANDA bridge, because a process that links no bridge (the data
/// daemon) still has to know the spelling OANDA's own catalog lists before it stores a series.
pub fn to_oanda_instrument(symbol: &str) -> String {
    let up = symbol.to_uppercase();
    if up.contains('_') {
        return up;
    }
    if up.len() == 6 && up.chars().all(|c| c.is_ascii_alphabetic()) {
        format!("{}_{}", &up[..3], &up[3..])
    } else {
        up
    }
}

/// The fiat currencies a roster FX or CFD venue quotes an instrument in: the closed set
/// [`fiat_quote_of`] recognises. Anything outside it (a crypto asset, a stablecoin, a metal) is NOT
/// a fiat quote, and a name ending in one is unrecognised rather than guessed at.
const FIAT_CODES: &[&str] = &[
    "AUD", "CAD", "CHF", "CNH", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "ILS", "INR",
    "JPY", "KRW", "MXN", "NOK", "NZD", "PLN", "RUB", "SEK", "SGD", "THB", "TRY", "USD", "ZAR",
];

/// `token` (trimmed, any case) as a [`FIAT_CODES`] entry, upper-cased, or `None`: the one door a
/// ledger currency a person TYPED (`engine.account_ccy = "jpy"`) goes through.
#[must_use]
pub fn fiat_code(token: &str) -> Option<&'static str> {
    let token = token.trim();
    FIAT_CODES.iter().copied().find(|code| code.eq_ignore_ascii_case(token))
}

/// **The fiat currency a symbol's NAME says it is quoted in**, or `None` when the name does not say.
///
/// Two kinds of spelling are read, each positively. Split on `_ / . -`, a token that states a quote
/// by itself decides, else a name of two or more tokens whose LAST token is a fiat code is quoted in
/// it (`EUR_GBP`, `SPX500_USD`, `DE30_EUR`, `AAPL.SMART.EUR`, `BTC-EUR`). A token states one when it is
/// (ASCII letters and digits only):
///
/// 1. six letters that are two fiat codes (`EURGBP`, `eurgbp`, and the pair inside an IG epic,
///    `CS.D.GBPJPY.CFD.IP`): quoted in the last three;
/// 2. six letters, an asset ticker that is NOT a fiat code and then a fiat code (`BTCEUR`, `ETHTRY`,
///    `BTCJPY`, `XAUUSD`): quoted in the fiat code;
/// 3. longer, and ending in a fiat code OTHER than the dollar (`DOGEEUR`, `SHIBTRY`, `DE30EUR`,
///    `DEUIDXEUR`, `JPNIDXJPY`): quoted in it. The dollar is left out on purpose: `AAPLUSD` and
///    `BTCFDUSD` (a dollar stablecoin) are dollar-like already, and "does not say" is the dollar's
///    answer on the dollar ledger;
/// 4. a fiat pair followed by one to three more letters, the broker-suffix spellings (`USDJPYm`,
///    `EURUSDecn`, `EURUSDpro`): quoted in the pair's last three, EXCEPT the uppercase stablecoin
///    tails `T` and `C` after a dollar quote (`EURUSDT` is EUR priced in USDT, not a suffixed
///    EURUSD). A suffix that is itself ambiguous (`USDJPYM`) is read as the pair, which is a refusal
///    on the dollar ledger rather than a pass.
///
/// ⚠ **`None` means "does not say", never "dollars"**: `BTCUSDT`, `BTC`, `AAPL` and `7203.T` carry no
/// fiat quote, and a caller that books them at par is relying on the crypto convention (a stablecoin
/// quote is a dollar), not on this function. It exists so the code that cannot CONVERT (the paper
/// book, the simulator, the offline statistics: no rate source) can refuse the names it can PROVE are
/// foreign-quoted, and leave every other name exactly as it was.
///
/// shortcut: a name only; the venue's instrument row (`vike_model::Instrument::quote`) is the better
/// source wherever a catalog is in hand. NOT read, and so unrefused until a caller has that row: a
/// bare stock ticker listed in pounds (`VOD`), a broker suffix of four letters or more or one made of
/// symbols (`EURUSD#`, `EURUSDmicro`), and a name of seven letters or more that merely ENDS in a
/// non-dollar fiat code without being quoted in it (an English word ending `TRY` or `INR`: read as
/// fiat-quoted, which is a refusal and never a pass).
#[must_use]
pub fn fiat_quote_of(symbol: &str) -> Option<&'static str> {
    // One pass and no allocation: a paper book asks on every submit.
    let (mut count, mut last) = (0_usize, "");
    for token in symbol.split(['_', '/', '.', '-']).map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(quote) = quote_of_token(token) {
            return Some(quote);
        }
        count += 1;
        last = token;
    }
    if count >= 2 { fiat_code(last) } else { None }
}

/// The fiat quote ONE separator-free token states by itself (the four spellings of [`fiat_quote_of`]).
fn quote_of_token(token: &str) -> Option<&'static str> {
    let n = token.len();
    // ASCII letters and digits only: it keeps every byte slice below on a character boundary, and a
    // token carrying anything else (`#`, `+`, a non-Latin letter) states nothing here.
    if n < 6 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let head = fiat_code(&token[..3]);
    let tail = fiat_code(&token[n - 3..]);
    if n == 6 {
        // EURGBP: two fiat codes. BTCEUR: an asset ticker (not itself a fiat code) on a fiat code.
        return head.and(tail).or_else(|| if head.is_none() { tail } else { None });
    }
    if let Some(quote) = head.and(fiat_code(&token[3..6]))
        && n <= 9
        && token[6..].bytes().all(|b| b.is_ascii_alphabetic())
        && !(quote == LEDGER_CCY && matches!(&token[6..], "T" | "C"))
    {
        return Some(quote);
    }
    tail.filter(|quote| *quote != LEDGER_CCY)
}

/// The currency a book, run or engine books its money in when nothing has told it another one: the
/// dollar (the constructor's `USDT` label is its stablecoin spelling). A symbol quoted in anything
/// else is FOREIGN to it, and code with no rate source (a paper book, the simulator, the offline
/// statistics) refuses it instead of booking it at par.
pub const LEDGER_CCY: &str = "USD";

/// What [`off_ledger_quote`] answers for a name that states no fiat quote on a ledger that is not the
/// dollar: the symbol is neither proven to be in that currency nor covered by the crypto convention.
const NO_FIAT_QUOTE: &str = "no fiat quote";

/// Why a symbol cannot be booked in a `ledger` ledger (a [`fiat_code`] spelling), or `None` when it
/// can: the quote its name states when that is not the ledger's, or [`NO_FIAT_QUOTE`] when it states
/// none and the ledger is not the dollar.
///
/// The asymmetry is the rule: a name that "does not say" is booked at par only on the dollar ledger
/// (the crypto convention: a stablecoin quote is a dollar). On a yen ledger `BTCUSDT` is not yen, and
/// `7203.T` (a name that really is yen) is not PROVEN yen by its name, so both are refused until a
/// caller holds the instrument row.
fn off_ledger_quote(symbol: &str, ledger: &str) -> Option<&'static str> {
    match fiat_quote_of(symbol) {
        Some(quote) if quote.eq_ignore_ascii_case(ledger.trim()) => None,
        Some(quote) => Some(quote),
        None if ledger.trim().eq_ignore_ascii_case(LEDGER_CCY) => None,
        None => Some(NO_FIAT_QUOTE),
    }
}

/// The ones among `symbols` that cannot be booked in the `ledger` ledger (a [`fiat_code`] spelling;
/// the rule is `off_ledger_quote`'s, private), each with the reason and listed once in first-seen order (`"USD_JPY (JPY), BTCUSDT (no fiat quote)"`),
/// or `None` when every symbol fits: the phrase a refusal names its offenders with.
#[must_use]
pub fn off_ledger_list<'a>(
    symbols: impl IntoIterator<Item = &'a str>,
    ledger: &str,
) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut named: Vec<String> = Vec::new();
    for symbol in symbols {
        if seen.contains(&symbol) {
            continue;
        }
        seen.push(symbol);
        if let Some(why) = off_ledger_quote(symbol, ledger) {
            named.push(format!("{symbol} ({why})"));
        }
    }
    (!named.is_empty()).then(|| named.join(", "))
}

/// [`fiat_quote_of`] when the name states a fiat currency other than [`LEDGER_CCY`], else `None` (a
/// dollar-quoted name, and every name that states no fiat quote at all): `off_ledger_quote` on the
/// dollar ledger.
#[must_use]
pub fn foreign_fiat_quote_of(symbol: &str) -> Option<&'static str> {
    off_ledger_quote(symbol, LEDGER_CCY)
}

/// The foreign-quoted ones among `symbols` ([`foreign_fiat_quote_of`]), each with its quote and listed
/// once in first-seen order (`"USD_JPY (JPY), EUR_GBP (GBP)"`), or `None` when there are none:
/// [`off_ledger_list`] on the dollar ledger, the phrase the journal and paper doors refuse with.
#[must_use]
pub fn foreign_fiat_quoted_list<'a>(symbols: impl IntoIterator<Item = &'a str>) -> Option<String> {
    off_ledger_list(symbols, LEDGER_CCY)
}

#[path = "symbol_tests.rs"]
#[cfg(test)]
mod symbol_tests;
