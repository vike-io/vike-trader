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
//! `crates/vike-datahub/src/server.rs`'s `refuse_unhonourable_class` calls it at its own door.
//!
//! The RENDERER half ([`to_core_symbol`], [`to_exchange_symbol`], [`PAIR_SEPARATOR`],
//! [`DEX_MARKER`], [`writes_a_pair_with_a_slash`]) has **none, deliberately, and it cannot get one
//! byte-identically** — which is a statement about what wiring it COSTS, not about it being
//! forgotten. Today `crates/bridges/hyperliquid/src/symbology.rs` mints `HYPE/USDC` as the
//! workspace's symbol for that instrument, and `vike_model::store_path::refuse_a_path_hostile_symbol`
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
/// This is the SAME convention `vike_bridge_core::tif::venue_tif` already established for the one
/// axis that had already hit this problem: a lane sub-key is a plain string that gets its own arm
/// in the ONE authority table, the BARE venue id keeps meaning the lane it always meant, and the
/// caller that knows which lane it is on passes the sub-key (binance's perp order builder passes
/// `vike_binance::perp::TIF_LANE`; here the mount passes what this function returns). A lane
/// sub-key is NOT a roster venue — `vike_model::VENUES` completeness tests classify roster ids
/// only, exactly as `venue_tif`'s do.
///
/// ## Why this lives in vike-catalog and not in vike-model
///
/// The fee VALUES belong to `vike_model::fees` (the bottom layer every crate reaches down to), but
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

/// The settlement asset that puts an aster perp on its own fee lane.
///
/// Aster charges three different taker rates on ONE perp order API, split by contract class — a
/// live sweep of the venue's own `GET /fapi/v3/commissionRate` across 17 symbols on 2026-08-05
/// found crypto at 4 bps, equity/ETF/commodity at 0.9 bps and USD1-settled at 0.5 bps (see
/// `vike_model::fees`' `fee_schedule_for` for the full table and its sourcing).
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
/// `vike_model::store_path::refuse_a_path_hostile_symbol` carries that measurement and refuses it
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suffixed_symbol_splits_into_wire_form_and_flag() {
        assert_eq!(split_perp("BTCUSDT.P"), ("BTCUSDT", true));
        assert_eq!(split_perp("BTCUSDT"), ("BTCUSDT", false));
        assert_eq!(split_perp("ETHUSDT.P").0, "ETHUSDT");
        assert!(split_perp("SOLUSDT.P").1);
        assert!(!split_perp("SOLUSDT").1);
    }

    /// The suffix is a TRAILING marker, not a substring: a symbol that merely contains `.P`
    /// somewhere, or ends with a lone `P`, is not a perp.
    #[test]
    fn only_a_trailing_suffix_counts() {
        assert_eq!(split_perp("BTC.PERP"), ("BTC.PERP", false));
        assert_eq!(split_perp("XPUSDT"), ("XPUSDT", false));
        assert_eq!(split_perp("BTC-USDT-SWAP"), ("BTC-USDT-SWAP", false));
    }

    /// Degenerate inputs must not panic or produce an empty wire symbol by accident.
    #[test]
    fn degenerate_inputs_are_inert() {
        assert_eq!(split_perp(""), ("", false));
        // `.P` alone strips to empty — nonsense as a symbol, but the function's job is the split,
        // and returning `("", true)` is the honest answer rather than a silent fallback.
        assert_eq!(split_perp(".P"), ("", true));
    }

    /// Splitting is idempotent on an already-split symbol, so a double call cannot eat a second
    /// suffix off a legitimate name.
    #[test]
    fn splitting_twice_is_the_same_as_splitting_once() {
        let (once, _) = split_perp("BTCUSDT.P");
        assert_eq!(split_perp(once), ("BTCUSDT", false));
    }

    /// Roster completeness (the per-venue-map playbook): every venue in the canonical roster is
    /// CLASSIFIED, and the ones that use the suffix are named explicitly — a new venue trips this
    /// until someone decides which side it is on.
    #[test]
    fn every_roster_venue_is_classified() {
        let suffixed: Vec<&str> =
            vike_model::VENUES.iter().copied().filter(|v| uses_perp_suffix(v)).collect();
        assert_eq!(
            suffixed,
            vec!["binance", "bybit", "aster"],
            "the `.P`-suffix venues changed — update this pin AND every adapter that splits"
        );
        // The two `false` venues the doc names are real classifications read from their adapters,
        // not an untested default: both are perp venues that need no suffix.
        for v in ["okx", "hyperliquid"] {
            assert!(vike_model::VENUES.contains(&v));
            assert!(!uses_perp_suffix(v), "{v} does not suffix its perps");
        }
    }

    /// **The byte-identity pin for the derivation.** [`uses_perp_suffix`] was a hand-written
    /// `matches!(venue, "binance" | "bybit" | "aster")` and now reads
    /// [`crate::addressing_for`]'s `Naming` column. This asserts the two answer the SAME for every
    /// roster venue AND for the off-roster strings, where the fallback is what could have diverged:
    /// the old literal answered `false` for anything unknown, and the new one must too — which it
    /// does because [`crate::VenueAddressing::UNCLASSIFIED`] is `Unaddressable`, not `PerpSuffix`.
    ///
    /// The literal below is deliberately a COPY of the deleted arm rather than a reference to the
    /// table: a pin that derives from the thing it pins cannot fail.
    #[test]
    fn derivation_matches_the_roster_it_replaced() {
        let was = |venue: &str| matches!(venue, "binance" | "bybit" | "aster");
        for &v in vike_model::VENUES {
            assert_eq!(uses_perp_suffix(v), was(v), "{v}: the derivation moved a roster venue");
        }
        // Off-roster: a lane sub-key, an unknown venue, and the empty string. The old literal said
        // `false` to all three and so must the table's fallback.
        for v in ["aster-perp", "ibkr_cpapi", "no-such-venue", "", "binance-perp"] {
            assert_eq!(uses_perp_suffix(v), was(v), "{v:?}: the fallback answers permissive");
            assert!(!uses_perp_suffix(v), "{v:?} is not a suffix venue");
        }
    }

    /// At a suffix venue `split_perp_at` IS [`split_perp`] — the property that makes wiring it into
    /// a bridge byte-identical, since every production caller today is on one of the three.
    #[test]
    fn split_perp_at_is_split_perp_at_every_suffix_venue() {
        for &v in vike_model::VENUES.iter().filter(|v| uses_perp_suffix(v)) {
            for sym in ["BTCUSDT.P", "BTCUSDT", "BTCUSD_PERP.P", "", ".P", "BTC.PERP", "XPUSDT"] {
                assert_eq!(split_perp_at(v, sym), split_perp(sym), "{v}/{sym:?}");
            }
        }
    }

    /// ...and the half the guard exists for: at a venue with no suffix convention nothing is
    /// stripped, whatever the string looks like. `BTC-PERPETUAL` is deribit's real spelling and
    /// `BTC-USDT-SWAP` okx's; neither ends in `.P`, so the interesting case is the CONTRIVED one —
    /// a venue-native id that happens to, which today's unconditional split would eat.
    #[test]
    fn split_perp_at_is_inert_at_a_non_suffix_venue() {
        for &v in vike_model::VENUES.iter().filter(|v| !uses_perp_suffix(v)) {
            for sym in ["BTC-PERPETUAL", "BTC-USDT-SWAP", "HYPE/USDC", "SOMETHING.P", ".P"] {
                assert_eq!(
                    split_perp_at(v, sym),
                    (sym, false),
                    "{v} has no suffix convention, so {sym:?} must pass through whole"
                );
            }
        }
        // An unknown venue string takes the same inert path — the addressing fallback refuses to
        // claim a naming convention, so nothing is stripped on a venue nobody has classified.
        assert_eq!(split_perp_at("no-such-venue", "BTCUSDT.P"), ("BTCUSDT.P", false));
    }

    // -----------------------------------------------------------------------------------------
    // FEE-LANE GATE.
    //
    // The defect this exists to catch: `vike_model::fee_schedule_for` carried ONE row per venue id
    // while binance's `exec.rs` mounted TWO order APIs behind that id, chosen by the `.P` suffix —
    // so a `BTCUSDT.P` paper/backtest mount was charged Binance SPOT fees (10/10 bps) on a lane that
    // really costs 2/5. Nothing checked the declared value against the routed lane.
    //
    // The gate is keyed off [`uses_perp_suffix`] — the roster-gated declaration that a `.P` symbol
    // on this venue names a DIFFERENT instrument than the bare symbol — rather than off a
    // hand-copied venue list, so a NEW suffix venue trips it until its fee lane is classified. Then
    // each classification is pinned THROUGH THE REAL RESOLVER ([`fee_lane`] → `fee_schedule_for`),
    // not against a copied matrix, so collapsing two lanes back onto one row is loud.
    // -----------------------------------------------------------------------------------------

    /// Venues whose exec routes on the suffix AND prices the two lanes differently. `fee_lane` must
    /// hand back a DISTINCT key whose row is a DISTINCT schedule.
    ///
    /// **aster graduated here on 2026-08-05.** It sat in `LANE_NAMED_SAME_PRICE` below because both
    /// of its rows carried ONE unsourced Binance-perp-shaped 0.02%/0.05% assumption and its spot
    /// schedule was called unverifiable. It is not unverifiable — Aster publishes both
    /// (<https://docs.asterdex.com/trading/spot/spot-fee-structure> = 0.005%/0.04% and
    /// <https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees> = 0%/0.04%, read
    /// 2026-08-05) — and they DIFFER on the maker leg, so the venue belongs in this set. That is
    /// exactly the move `LANE_NAMED_SAME_PRICE`'s doc prescribed: a deliberate edit here plus real
    /// numbers in `fees.rs`.
    const LANE_PRICED: &[&str] = &["binance", "aster"];
    /// Venues whose exec routes on the suffix but whose two lanes carry the SAME schedule today, by
    /// a documented decision rather than by omission.
    ///
    /// EMPTY today — aster was its only member and has graduated to `LANE_PRICED` above. The set is
    /// kept (rather than deleted with its two tests) because it is the honest landing place for the
    /// next dual-lane venue whose second lane is genuinely unpriced: it lets
    /// `fee_lane_is_declared_for_every_perp_suffix_venue` accept "NAMED but same number" as a
    /// classification instead of forcing a guess. The one test below that iterates it is therefore
    /// VACUOUS while it is empty — deliberately so;
    /// `fee_lane_is_declared_for_every_perp_suffix_venue` is the gate that still forces every
    /// suffix venue into exactly one of the three sets, and it is not vacuous.
    const LANE_NAMED_SAME_PRICE: &[&str] = &[];
    /// Venues that use the suffix in their SYMBOLS but whose `exec.rs` has only one lane to route
    /// to, so a fee lane key would be noise. **bybit**: `BybitExecutionClient` is V5 LINEAR-PERP
    /// only (its module doc says so, and there is no `run_spot` arm anywhere in the crate) — its
    /// single `"bybit"` row already IS the perp row.
    const SINGLE_EXEC_LANE: &[&str] = &["bybit"];

    /// Every `.P`-suffix roster venue is classified into exactly ONE of the three sets above. A new
    /// suffix venue fails here until someone decides whether its fee table needs a lane — the
    /// question that went unasked when binance grew its perp lane.
    #[test]
    fn fee_lane_is_declared_for_every_perp_suffix_venue() {
        let suffixed: Vec<&str> =
            vike_model::VENUES.iter().copied().filter(|v| uses_perp_suffix(v)).collect();
        assert!(
            !suffixed.is_empty(),
            "the derivation is vacuous — uses_perp_suffix matched nothing"
        );
        for v in suffixed {
            let n = [LANE_PRICED, LANE_NAMED_SAME_PRICE, SINGLE_EXEC_LANE]
                .iter()
                .filter(|set| set.contains(&v))
                .count();
            assert_eq!(
                n, 1,
                "roster venue {v} uses the `.P` suffix but is not classified exactly once — decide \
                 whether its fee table needs a lane key (LANE_PRICED / LANE_NAMED_SAME_PRICE) or \
                 whether its exec has a single lane (SINGLE_EXEC_LANE), and say why"
            );
        }
    }

    /// THE regression gate. For a `LANE_PRICED` venue, resolving fees through the REAL path —
    /// `fee_lane(venue, symbol)` then `vike_model::fee_schedule_for` — must give a DIFFERENT answer
    /// for a `.P` symbol than for its bare twin. This is red on the tree that had one binance row,
    /// and red again the moment someone points both keys at the same schedule.
    #[test]
    fn a_dual_lane_venue_prices_its_two_lanes_apart() {
        for &v in LANE_PRICED {
            let spot_key = fee_lane(v, "BTCUSDT");
            let perp_key = fee_lane(v, "BTCUSDT.P");
            assert_eq!(spot_key, v, "{v}: a bare symbol must keep the bare id");
            assert_ne!(perp_key, v, "{v}: a `.P` symbol must resolve to a LANE key");
            assert_ne!(
                vike_model::fee_schedule_for(spot_key),
                vike_model::fee_schedule_for(perp_key),
                "{v}: both lanes resolve to the SAME schedule — a perp mount is being charged its \
                 spot fees (or vice versa). Give {perp_key} its own row in vike_model::fees."
            );
        }
    }

    /// The other half of the same law: a `LANE_NAMED_SAME_PRICE` venue must still emit a distinct,
    /// NAMED lane key, and its two keys must resolve EQUAL. Fixing one side without the other fails
    /// here and forces the classification to move — which is precisely what happened to aster on
    /// 2026-08-05: sourcing its spot rate (0.5 bps maker) against a perp lane that charges 0 broke
    /// this equality, and the venue moved to `LANE_PRICED` rather than the gate being weakened.
    ///
    /// ⚠ VACUOUS while `LANE_NAMED_SAME_PRICE` is empty — see that set's doc.
    #[test]
    fn a_declared_same_price_venue_names_its_lane_and_keeps_one_number() {
        for &v in LANE_NAMED_SAME_PRICE {
            let perp_key = fee_lane(v, "BTCUSDT.P");
            assert_ne!(perp_key, v, "{v}: the lane must be NAMED even when it is priced the same");
            assert_eq!(
                vike_model::fee_schedule_for(v),
                vike_model::fee_schedule_for(perp_key),
                "{v}: the lanes diverged — promote it to LANE_PRICED and say where the number came \
                 from"
            );
        }
    }

    /// No lane key may ride `fee_schedule_for`'s fail-safe `_ => Free` fallback: a key that resolves
    /// to `Free` when its bare venue does not is a typo'd key silently zeroing every fee.
    #[test]
    fn every_emitted_lane_key_has_a_real_row() {
        for &v in LANE_PRICED.iter().chain(LANE_NAMED_SAME_PRICE) {
            let perp_key = fee_lane(v, "BTCUSDT.P");
            assert_ne!(
                vike_model::fee_schedule_for(perp_key),
                vike_model::FeeSchedule::Free,
                "{perp_key} has no arm in vike_model::fees — it fell through to the Free fallback"
            );
        }
    }

    /// `fee_lane` is the IDENTITY for every venue that is not lane-keyed, on BOTH symbol forms — so
    /// wiring it into a call site that used to pass a bare venue string is byte-identical
    /// everywhere except the two lanes it exists to split.
    #[test]
    fn fee_lane_is_the_identity_for_every_other_venue() {
        let lane_keyed: Vec<&str> =
            LANE_PRICED.iter().chain(LANE_NAMED_SAME_PRICE).copied().collect();
        for &v in vike_model::VENUES.iter().chain(["ibkr_cpapi", "no-such-venue"].iter()) {
            for sym in ["BTCUSDT", "BTCUSDT.P", "", "BTC-USDT-SWAP"] {
                if lane_keyed.contains(&v) && split_perp(sym).1 {
                    continue;
                }
                assert_eq!(fee_lane(v, sym), v, "{v}/{sym} must resolve to the bare venue id");
            }
        }
        // SINGLE_EXEC_LANE venues are the interesting case: they DO carry `.P` symbols, and must
        // still resolve to the bare id (their one row is their only row).
        for &v in SINGLE_EXEC_LANE {
            assert_eq!(fee_lane(v, "BTCUSDT.P"), v);
        }
    }

    // -----------------------------------------------------------------------------------------
    // ASTER'S THIRD LANE: contract-class pricing on ONE perp order API.
    //
    // Aster charges three taker rates on `/fapi` — crypto 4 bps, equity/ETF/commodity 0.9 bps,
    // USD1-settled 0.5 bps (live sweep, 2026-08-05; `vike_model::fees` carries the table). Only the
    // USD1 class is decidable from a symbol, so only it is routed here. These tests pin BOTH
    // halves: that the new lane resolves, and that adding it moved NOTHING else.
    // -----------------------------------------------------------------------------------------

    /// **The byte-identity gate.** Adding the USD1 lane must not have moved a single fee any caller
    /// already resolved. Pinned through the REAL path (`fee_lane` → `fee_schedule_for`) against the
    /// literal schedules that were in the table before the lane existed, so a future edit to either
    /// crate that shifts one of them is loud here rather than silent in a backtest.
    ///
    /// `BTCUSDT` is the specific symbol both aster rows were originally measured against, which is
    /// why it is the anchor.
    #[test]
    fn the_usd1_lane_leaves_every_previously_resolved_aster_fee_unchanged() {
        use vike_model::FeeSchedule::PercentMakerTaker;
        // The two lanes that existed before, on the symbol they were measured on.
        assert_eq!(fee_lane("aster", "BTCUSDT"), "aster");
        assert_eq!(
            vike_model::fee_schedule_for(fee_lane("aster", "BTCUSDT")),
            PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
            "aster SPOT BTCUSDT moved — measured live at 0.5/4 bps on 2026-08-05"
        );
        assert_eq!(fee_lane("aster", "BTCUSDT.P"), "aster-perp");
        assert_eq!(
            vike_model::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
            PercentMakerTaker { maker_bps: 0.0, taker_bps: 4.0 },
            "aster PERP BTCUSDT.P moved — measured live at 0/4 bps on 2026-08-05"
        );
        // Every other crypto perp the sweep covered stays on the crypto lane too — including the
        // two shapes most likely to be caught by a sloppy suffix match.
        for sym in ["ETHUSDT.P", "ASTERUSDT.P", "1000PEPEUSDT.P", "BTCDOMUSDT.P", "BTCU.P"] {
            assert_eq!(fee_lane("aster", sym), "aster-perp", "{sym} left the crypto perp lane");
        }
        // And the equity class — measured at 0.9 bps but deliberately NOT encoded — must still
        // resolve to the crypto row. This pins the documented GAP: if someone later routes these,
        // this test is where they must come and say so.
        for sym in ["AAPLUSDT.P", "TSLAUSDT.P", "NVDAUSDT.P", "SPYUSDT.P", "XAUUSDT.P"] {
            assert_eq!(
                fee_lane("aster", sym),
                "aster-perp",
                "{sym} is an equity-class perp (true rate 0/0.9 bps, measured 2026-08-05) that this \
                 table deliberately charges the conservative crypto rate — see fee_schedule_for's \
                 `aster-perp` arm before changing this"
            );
        }
    }

    /// The other half: a USD1-settled perp resolves to a DIFFERENT lane with a DIFFERENT, cheaper
    /// schedule. Without this the lane could be wired and silently never taken.
    #[test]
    fn a_usd1_settled_aster_perp_resolves_to_its_own_cheaper_lane() {
        use vike_model::FeeSchedule::PercentMakerTaker;
        for sym in ["BTCUSD1.P", "ETHUSD1.P", "SOLUSD1.P"] {
            assert_eq!(fee_lane("aster", sym), "aster-perp-usd1", "{sym}");
            assert_eq!(
                vike_model::fee_schedule_for(fee_lane("aster", sym)),
                PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
                "{sym}: USD1-Perp is 0/0.005% (published) and measured 0/0.5 bps live 2026-08-05"
            );
            assert_ne!(
                vike_model::fee_schedule_for(fee_lane("aster", sym)),
                vike_model::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
                "{sym} resolves to the SAME schedule as a USDT-settled perp — the 8x taker \
                 difference this lane exists for has been collapsed"
            );
        }
    }

    /// ⚠ The dangerous direction. This lane makes fees CHEAPER, so an over-broad match FLATTERS a
    /// backtest — the failure mode the rest of this module works to avoid. The match is therefore a
    /// suffix of the EXCHANGE symbol, and these are the cases that a `contains`, a base-asset match
    /// or a pre-`.P`-strip match would each get wrong.
    #[test]
    fn usd1_lane_matches_settlement_not_substring() {
        // USD1 as the BASE asset, settled in USDT: a `contains("USD1")` would wrongly route it.
        assert_eq!(fee_lane("aster", "USD1USDT.P"), "aster-perp");
        // The SPOT USD1 pairs stay on the spot row entirely — the sweep measured all of them at the
        // flat spot rate (`BUSD1`, `ANUSD1` -> 0.5/4 bps), so a perp lane must not leak onto them.
        for sym in ["BTCUSD1", "BUSD1", "ANUSD1", "USD1USDT"] {
            assert_eq!(fee_lane("aster", sym), "aster", "{sym} is a SPOT symbol");
        }
        // The suffix is tested AFTER the `.P` strip: a raw `ends_with` on the core symbol would
        // never match, silently disabling the lane.
        assert_ne!(fee_lane("aster", "BTCUSD1.P"), "aster-perp");
        // No OTHER venue grew the lane by accident — the arm is aster-gated.
        for v in ["binance", "bybit", "okx", "hyperliquid"] {
            assert_ne!(fee_lane(v, "BTCUSD1.P"), "aster-perp-usd1", "{v}");
        }
        // And the key is real, not a typo riding `fee_schedule_for`'s `_ => Free` fallback.
        assert_ne!(
            vike_model::fee_schedule_for("aster-perp-usd1"),
            vike_model::FeeSchedule::Free,
            "the USD1 lane key has no arm in vike_model::fees — every fee on it is silently zero"
        );
    }

    /// The three shapes this conversion exists for, each round-tripped.
    ///
    /// A round trip is the assertion that matters: either direction alone can look right while the
    /// pair loses information, and the pair is what a store key and a wire call actually use.
    #[test]
    fn the_three_venue_spellings_round_trip_through_the_core_form() {
        for (venue, exchange, core, class) in [
            // hyperliquid spot — OUR unified BASE/QUOTE spelling; 328 pairs, 12 bases carry more
            // than one quote, which is why the quote stays IN the name.
            ("hyperliquid", "HYPE/USDC", "HYPE-USDC", None),
            ("hyperliquid", "HYPE/USDT0", "HYPE-USDT0", None),
            ("hyperliquid", "PURR/USDC", "PURR-USDC", None),
            // hyperliquid builder-dex perps — the venue's OWN colon spelling; 289 across 11 dexes.
            ("hyperliquid", "xyz:TSLA", "TSLA.d-xyz", None),
            ("hyperliquid", "para:GOLD", "GOLD.d-para", None),
            // alpaca crypto — the venue's own slash spelling, where the slash IS the classifier.
            ("alpaca", "BTC/USD", "BTC-USD", Some(vike_model::AssetClass::CryptoSpot)),
            ("alpaca", "ETH/USDT", "ETH-USDT", Some(vike_model::AssetClass::CryptoSpot)),
        ] {
            assert_eq!(to_core_symbol(venue, exchange), core, "{venue} {exchange} -> core");
            assert_eq!(
                to_exchange_symbol(venue, core, class).as_deref(),
                Ok(exchange),
                "{venue} {core} -> exchange"
            );
        }
    }

    /// Everything that already works must pass through untouched — a conversion that also rewrites
    /// a working symbol is worse than none.
    #[test]
    fn every_symbol_that_needs_no_conversion_is_untouched() {
        for (venue, symbol, class) in [
            ("hyperliquid", "BTC", None),   // core perp — bare coin, 234 of them
            ("hyperliquid", "kPEPE", None), // ...including the k-prefixed
            ("binance", "BTCUSDT", None),
            ("binance", "BTCUSDT.P", None), // the perp suffix that already exists
            ("oanda", "EUR_USD", None),     // FX — its slash lives only in `displayName`
            ("deribit", "BTC-1JAN27-100000-C", None), // an option: hyphens that must NOT become slashes
            ("alpaca", "AAPL", Some(vike_model::AssetClass::Equity)),
            ("alpaca", "BRK-B", Some(vike_model::AssetClass::Equity)), // a share class, NOT a pair
        ] {
            assert_eq!(to_core_symbol(venue, symbol), symbol, "{venue} {symbol} -> core");
            assert_eq!(
                to_exchange_symbol(venue, symbol, class).as_deref(),
                Ok(symbol),
                "{venue} {symbol} -> exchange"
            );
        }
    }

    /// ⚠ The one case a string cannot decide, and the refusal that says so rather than guessing.
    ///
    /// `BRK-B` and `BTC-USD` are the same shape on the same venue and mean different things. The
    /// venue's classifier is the slash this conversion restores, so restoring it is precisely what
    /// needs the claim — `docs/decisions/0061`'s ruling 3.
    #[test]
    fn an_ambiguous_alpaca_symbol_without_a_class_is_refused_rather_than_guessed() {
        let err = to_exchange_symbol("alpaca", "BTC-USD", None)
            .expect_err("a hyphenated alpaca symbol with no class must be refused");
        assert!(err.contains("BRK-B"), "the refusal must show the shape it collides with: {err}");
        assert!(err.contains("asset_class"), "...and name where the claim comes from: {err}");
        // With a claim either way it decides, and the two answers differ — which is the proof that
        // the claim is doing work rather than being ceremony.
        assert_eq!(
            to_exchange_symbol("alpaca", "BTC-USD", Some(vike_model::AssetClass::CryptoSpot))
                .as_deref(),
            Ok("BTC/USD")
        );
        assert_eq!(
            to_exchange_symbol("alpaca", "BTC-USD", Some(vike_model::AssetClass::Equity))
                .as_deref(),
            Ok("BTC-USD")
        );
        // Hyperliquid is NOT asked, because its shape decides — no HL name carries a hyphen.
        assert_eq!(
            to_exchange_symbol("hyperliquid", "HYPE-USDC", None).as_deref(),
            Ok("HYPE/USDC")
        );
    }

    /// Only the FIRST separator is a pair boundary, so a quote that itself carries one cannot be
    /// silently re-split — and the dex marker is matched before the pair rule, so a dex listing is
    /// never mistaken for a pair.
    #[test]
    fn the_first_separator_wins_and_the_dex_marker_is_matched_first() {
        assert_eq!(
            to_exchange_symbol("hyperliquid", "A-B-C", None).as_deref(),
            Ok("A/B-C"),
            "only the first hyphen is the pair boundary"
        );
        assert_eq!(
            to_core_symbol("hyperliquid", "xyz:A-B"),
            "A-B.d-xyz",
            "a dex listing is read as a dex listing even when its coin carries a hyphen"
        );
        assert_eq!(
            to_exchange_symbol("hyperliquid", "A-B.d-xyz", None).as_deref(),
            Ok("xyz:A-B"),
            "...and back, because the marker is matched before the pair rule"
        );
    }

    /// The roster half: every venue that writes a pair with a slash is NAMED, so adding one forces
    /// the question rather than letting it inherit an answer.
    #[test]
    fn the_slash_venues_are_exactly_the_two_measured_ones() {
        assert!(writes_a_pair_with_a_slash("hyperliquid"));
        assert!(writes_a_pair_with_a_slash("alpaca"));
        for quiet in ["binance", "bybit", "okx", "aster", "deribit", "oanda", "ig", "polymarket"] {
            assert!(
                !writes_a_pair_with_a_slash(quiet),
                "{quiet} was measured as writing no slash pair; if that changed, change the roster \
                 and say what was measured"
            );
        }
    }
}
