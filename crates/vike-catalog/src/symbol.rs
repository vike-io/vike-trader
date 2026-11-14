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
//! forgotten. Today `crates/bridges/hyperliquid/src/instrument/symbology.rs` mints `HYPE/USDC` as the
//! workspace's symbol for that instrument, and `vike_paths::store_path::refuse_a_path_hostile_symbol`
//! REFUSES it at the store door: the slash would silently add a partition level. Calling
//! [`to_core_symbol`] at that venue's catalog would spell the same instrument `HYPE-USDC`, which is
//! a different series key, a different store partition and a different thing for a mount to name.
//! That is `docs/decisions/0061`'s STEP 2 — one venue, one row, its own argument and its own
//! migration question — and the playbook's rule is that STEP 1 declares and STEP 2 changes. This
//! module is the declaration; nothing here is licensed to rewrite a symbol a venue already answers
//! to.
//!
//! ## What moved down to `vike-venues`
//!
//! The suffix primitive itself ([`PERP_SUFFIX`], [`split_perp`]), the fee lane it keys
//! ([`fee_lane`]), the fiat-code set ([`fiat_code`]) and the pair-shape readers and markers
//! ([`PAIR_SEPARATOR`], [`DEX_MARKER`], `vike_venues::is_hyperliquid_spot`) live in `vike-venues`
//! beside the fee table, so a crate at this layer (`vike-strategy`) can resolve a lane too. What
//! reads this crate's addressing table stays here and calls them.

use vike_venues::{DEX_MARKER, PAIR_SEPARATOR, fiat_code, split_perp};
#[cfg(doc)]
use vike_venues::{PERP_SUFFIX, fee_lane};

/// Whether what an operator TYPED names a perpetual, in any case and with spaces around it: the one reading the
/// node's door and every client's preview of it address a default account's order by.
#[must_use]
pub fn names_perp(typed: &str) -> bool {
    split_perp(&typed.trim().to_ascii_uppercase()).1
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
/// module away from [`crate::addressing_for`], whose [`crate::VenueAddressingNaming::PerpSuffix`] rows name
/// EXACTLY those three venues and answer the SAME question. Nothing held the two equal. That is
/// `docs/decisions/0061`'s own objection to this axis — *"two vocabularies for one question is the
/// defect this tree gates elsewhere"*, and the record lists four partial encodings of it already —
/// so rather than add a fifth and a gate to hold it in step, this function now READS the table.
/// The addressing row is the authority because it is the one that carries the adapter citation per
/// venue and the `vike:new-venue:row` marker, so a new bridge answers this question once, there.
///
/// BYTE-IDENTICAL, and the pin is `derivation_matches_the_roster_it_replaced` below: the same three
/// roster venues answer `true`, every other roster venue answers `false`, and an OFF-roster string
/// still answers `false` (the addressing fallback is [`crate::VenueAddressingNaming::Unaddressable`], not
/// `PerpSuffix`), which is the direction that would have been easy to get wrong.
pub fn uses_perp_suffix(venue: &str) -> bool {
    matches!(crate::addressing_for(venue).naming, crate::VenueAddressingNaming::PerpSuffix)
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
/// (`crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`) already has the venue in
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

/// **Whether an engine mounted on `engine_symbol` at `venue` can hold a TP/SL bracket's
/// stop-loss** — the ONE rule the node and the Trade window both read (final review B, I-1): the
/// node refuses a bracket by it (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_engine_refusal`),
/// and the desktop's dispatcher and glue refuse and explain the same bracket before the click, so
/// the two sides cannot disagree about a lane. It lived in the node until the window needed it.
///
/// `false` exactly for an engine on the SPOT lane of a dual-lane venue (binance, aster): their
/// `vike_venues::capabilities_for` rows list `stop` as the spot+perp UNION, while the spot order builder
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

/// **Whether `symbol`, in `venue`'s engine spelling, names a SPOT pair**: a holding bought outright
/// with the account's own cash, never borrowed against, so no venue ever liquidates it (its positions
/// are `vike_model::MarginMode::Cash`). Read off the symbol's shape, as each adapter picks its book:
/// on a dual-lane venue (binance, aster) the bare spelling, read with [`split_perp_at`]; on bybit
/// `PAIR.S` (decision 0231, the spelling [`fee_lane`] prices; a bare name is its linear perpetual); on
/// okx a `BASE-QUOTE` instId (a SWAP, a dated future and an option have more parts, decision 0232); on
/// hyperliquid a pair (`HYPE/USDC`, `HYPE-USDC`). Every other venue answers `false`: a book this cannot
/// prove spot from its name is never called spot. One rule for every reader: the mount (the node's shared
/// tail) and the paper builders, an admitted symbol (the gatewayhub's facts), a journaled position (the boot
/// restore) and the backtest's spot account (the spot rule: one spot law in live, paper and backtest).
pub fn names_spot_pair(venue: &str, symbol: &str) -> bool {
    match venue {
        v if spot_lane_holds_no_stop(v) => !split_perp_at(v, symbol).1,
        "hyperliquid" => vike_venues::is_hyperliquid_spot(symbol),
        // bybit and okx: the spelling their row states (`vike_venues::spot_pair_row`), the one the fee lane reads.
        _ => vike_venues::spot_pair_row(venue).is_some_and(|row| row.spelling.names_spot(symbol)),
    }
}

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
///   `crates/bridges/alpaca/src/feed/mod.rs`'s `class_of` is `if symbol.contains('/') { Crypto } else
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
             slash — `crates/bridges/alpaca/src/feed/mod.rs`'s `class_of` reads it — so restoring it is \
             the one thing a string cannot decide. Pass the class; \
             `vike_model::Instrument::asset_class` carries it."
        )),
    }
}

/// Canonical symbol → OANDA instrument form (`"eurusd"` -> `"EUR_USD"`): [`six_letter_pair`] over `_`.
///
/// It lives here, not in the OANDA bridge, because a process that links no bridge (the data
/// daemon) still has to know the spelling OANDA's own catalog lists before it stores a series.
pub fn to_oanda_instrument(symbol: &str) -> String {
    six_letter_pair(symbol, '_')
}

/// A canonical symbol upper-cased, and when it is six ASCII letters split into its two
/// three-letter halves around `sep` (`("eurusd", '_')` -> `"EUR_USD"`). A symbol already carrying
/// `sep`, or of any other shape, comes back upper-cased and otherwise as given. The venue
/// spellings are this with their own separator: [`to_oanda_instrument`]'s `_`, fxcm's `/`.
pub fn six_letter_pair(symbol: &str, sep: char) -> String {
    let up = symbol.to_uppercase();
    if up.contains(sep) {
        return up;
    }
    if up.len() == 6 && up.chars().all(|c| c.is_ascii_alphabetic()) {
        format!("{}{sep}{}", &up[..3], &up[3..])
    } else {
        up
    }
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

/// **The fiat currency `symbol` is quoted in, read off its KEPT row where the row states one** (`row`: the symbol's
/// row of the datahub's `<project>/instruments/<venue>.json`, found by `crate::kept_row`): the row's quote when it is
/// a fiat code (`vike_venues::fiat_code`: binance's `IDTRY` is lira, which its name does not say); else
/// [`fiat_quote_of`], the name's answer, as with no row at all. A row quoted in a coin or a stablecoin does not say
/// which fiat that is, and the name may: binance's `EUREURI` is quoted in `EURI`, a euro stablecoin, and its name
/// keeps it off the dollar ledger. So a row can only ADD a quote the name misses, never drop one (measured over
/// the CI box's kept lists, 2026-11-13: 24 binance rows add one, `EUREURI` is the one a row alone would have dropped).
#[must_use]
pub fn kept_fiat_quote(symbol: &str, row: Option<&vike_model::Instrument>) -> Option<&'static str> {
    row.and_then(|r| fiat_code(&r.quote)).or_else(|| fiat_quote_of(symbol))
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
        && !(quote == LEDGER_CURRENCY && matches!(&token[6..], "T" | "C"))
    {
        return Some(quote);
    }
    tail.filter(|quote| *quote != LEDGER_CURRENCY)
}

/// The currency a book, run or engine books its money in when nothing has told it another one: the
/// dollar (the constructor's `USDT` label is its stablecoin spelling). A symbol quoted in anything
/// else is FOREIGN to it, and code with no rate source (a paper book, the simulator, the offline
/// statistics) refuses it instead of booking it at par.
pub const LEDGER_CURRENCY: &str = "USD";

/// What [`off_ledger_quote`] answers for a name that states no fiat quote on a ledger that is not the
/// dollar: the symbol is neither proven to be in that currency nor covered by the crypto convention.
const NO_FIAT_QUOTE: &str = "no fiat quote";

/// Why a symbol whose fiat `quote` is known ([`fiat_quote_of`] or [`kept_fiat_quote`]) cannot be booked in a
/// `ledger` ledger (a [`fiat_code`] spelling), or `None` when it can: that quote when it is not the ledger's,
/// or [`NO_FIAT_QUOTE`] when there is none and the ledger is not the dollar.
///
/// The asymmetry is the rule: a name that "does not say" is booked at par only on the dollar ledger
/// (the crypto convention: a stablecoin quote is a dollar). On a yen ledger `BTCUSDT` is not yen, and
/// `7203.T` (a name that really is yen) is not PROVEN yen by its name, so both are refused until a
/// caller holds the instrument row.
fn off_ledger_quote(quote: Option<&'static str>, ledger: &str) -> Option<&'static str> {
    match quote {
        Some(quote) if quote.eq_ignore_ascii_case(ledger.trim()) => None,
        Some(quote) => Some(quote),
        None if ledger.trim().eq_ignore_ascii_case(LEDGER_CURRENCY) => None,
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
    off_ledger_list_of(symbols.into_iter().map(|s| (s, fiat_quote_of(s))), ledger)
}

/// [`off_ledger_list`] over symbols whose fiat quote the caller already holds (`(symbol, quote)`, the quote
/// [`kept_fiat_quote`] read off a kept row where one is held): the same rule, phrase and order.
#[must_use]
pub fn off_ledger_list_of<'a>(
    quoted: impl IntoIterator<Item = (&'a str, Option<&'static str>)>,
    ledger: &str,
) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut named: Vec<String> = Vec::new();
    for (symbol, quote) in quoted {
        if seen.contains(&symbol) {
            continue;
        }
        seen.push(symbol);
        if let Some(why) = off_ledger_quote(quote, ledger) {
            named.push(format!("{symbol} ({why})"));
        }
    }
    (!named.is_empty()).then(|| named.join(", "))
}

/// The foreign-quoted ones among `symbols` (a fiat quote [`fiat_quote_of`] reads other than [`LEDGER_CURRENCY`]),
/// each with its quote and listed
/// once in first-seen order (`"USD_JPY (JPY), EUR_GBP (GBP)"`), or `None` when there are none:
/// [`off_ledger_list`] on the dollar ledger, the phrase the journal and paper doors refuse with.
#[must_use]
pub fn foreign_fiat_quoted_list<'a>(symbols: impl IntoIterator<Item = &'a str>) -> Option<String> {
    off_ledger_list(symbols, LEDGER_CURRENCY)
}

#[path = "symbol_tests.rs"]
#[cfg(test)]
mod symbol_tests;
