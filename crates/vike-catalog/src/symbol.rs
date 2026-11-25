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
//! ## The SUFFIX half, and the renderer that is gone
//!
//! The SUFFIX half ([`PERP_SUFFIX`], [`split_perp`], [`split_perp_at`], [`uses_perp_suffix`],
//! [`fee_lane`]) is wired: every wire-facing site on binance, bybit and aster calls it, and
//! `crates/vike-datahub/src/server/seed_series.rs`'s `refuse_unhonourable_class` calls it at its own door.
//!
//! The RENDERER half decision 0079 declared (`to_core_symbol`, `to_exchange_symbol`, `writes_a_pair_with_a_slash`,
//! and `vike_venues`' `PAIR_SEPARATOR` and `DEX_MARKER`: hyperliquid's `HYPE/USDC` as `HYPE-USDC`, `xyz:TSLA` as
//! `TSLA.d-xyz`) never had a caller and was DELETED with decision 0290: an instrument's user, store and wire names
//! are `crate::instrument_names`' (`HYPEUSDC`, `XYZ:TSLAUSDC.P` stored as `XYZ-TSLAUSDC.P`), read off its kept row.
//!
//! ## What moved down to `vike-venues`
//!
//! The suffix primitive itself ([`PERP_SUFFIX`], [`split_perp`]), the fee lane it keys
//! ([`fee_lane`]) and the spot spelling a venue states (`vike_venues::spot_pair_row`) live in `vike-venues`
//! beside the fee table, so a crate at this layer (`vike-strategy`) can resolve a lane too. What reads this
//! crate's addressing table stays here and calls them. The fiat-code set is currency vocabulary, `vike_model`'s
//! [`fiat_code`], beside the minor units and [`FiatQuote`].

use vike_model::{DEFAULT_CURRENCY, FiatQuote, MinorUnit, fiat_code, same_currency};
use vike_venues::split_perp;
#[cfg(doc)]
use vike_venues::{PERP_SUFFIX, fee_lane};

/// **Whether the USER name `typed` on `venue` trades on the venue's FUTURES lane** (a perpetual or a dated future,
/// never a spot pair), in any case and with spaces around it: the one reading the node's door and every client's
/// preview of it address a default account's order by (binance's lane twin), and the dual-lane venues' spot law
/// ([`names_spot_pair`], [`engine_lane_holds_stop`]) reads. The name's KEPT row's class decides where this process holds
/// the venue's list and a row of it (decision 0290: a dated `BTCUSDT_261225` carries no `.P`); with no row, the
/// grammar's: a perpetual's [`PERP_SUFFIX`] or a dated contract's `_YYMMDD`.
#[must_use]
pub fn on_futures_lane(venue: &str, typed: &str) -> bool {
    let name = typed.trim().to_ascii_uppercase();
    if let Some(row) = crate::kept_row_of(venue, &name) {
        return matches!(
            row.contract,
            vike_model::Contract::Perp(_) | vike_model::Contract::Future(_)
        );
    }
    split_perp(&name).1 || vike_venues::is_dated(&name)
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
/// is a spot order. The lane is read with [`on_futures_lane`], the user name's kept row as the adapter reads it (a
/// dated `BTCUSDT_261225` is the futures lane, decision 0290).
pub fn engine_lane_holds_stop(venue: &str, engine_symbol: &str) -> bool {
    !spot_lane_holds_no_stop(venue) || on_futures_lane(venue, engine_symbol)
}

/// The venues whose SPOT lane cannot hold a bracket's stop-loss: the DUAL-LANE ones, whose exec
/// routes spot vs perp on the `.P` suffix and whose one caps row is the union of both lanes. Read off the
/// venue table's column (`vike_venues::spot_pair_lanes_for`, `SpotPairLanes::TwoExecLanes`), never a name list.
///
/// ⚠ [`fee_lane`]'s `.P` split (a perpetual lane key apart from the bare row) encodes the same set a second way,
/// so the column is PINNED to that function over the whole roster
/// (`the_spot_lane_venues_are_exactly_fee_lanes_dual_lane_venues`): a new dual-lane venue reddens
/// that test rather than silently accepting spot brackets.
fn spot_lane_holds_no_stop(venue: &str) -> bool {
    vike_venues::spot_pair_lanes_for(venue).lanes == vike_venues::SpotPairLanes::TwoExecLanes
}

/// **Whether `symbol`, in `venue`'s engine spelling, names a SPOT pair**: a holding bought outright
/// with the account's own cash, never borrowed against, so no venue ever liquidates it (its positions
/// are `vike_model::MarginMode::Cash`). Read as each adapter picks its book: on a dual-lane venue (binance, aster)
/// a user name off the futures lane ([`on_futures_lane`]: its kept row's class, else its grammar); on bybit, okx,
/// hyperliquid and deribit the unmarked user name (decision 0290: the pair `BTCUSDT` beside `BTCUSDT.P` and
/// `ETHUSDT_261127`, hyperliquid's `HYPEUSDC` beside `BTCUSDC.P`, the spelling [`fee_lane`] prices). Every other venue
/// answers `false`: a book this cannot
/// prove spot from its name is never called spot. One rule for every reader: the mount (the node's shared
/// tail) and the paper builders, an admitted symbol (the gatewayhub's facts), a journaled position (the boot
/// restore) and the backtest's spot account (the spot rule: one spot law in live, paper and backtest).
pub fn names_spot_pair(venue: &str, symbol: &str) -> bool {
    match venue {
        v if spot_lane_holds_no_stop(v) => !on_futures_lane(v, symbol),
        // bybit, hyperliquid, okx and deribit: the spelling their row states (`vike_venues::spot_pair_row`), the one
        // the fee lane reads.
        _ => vike_venues::spot_pair_row(venue).is_some_and(|row| row.spelling.names_spot(symbol)),
    }
}

/// **Whether the node books `symbol` on `venue` fully funded (margin mode `Cash`)**, by the rule its mount and its
/// boot restore apply (`crates/vike-tradehub-core/src/engine/assemble.rs`'s `assemble_engine`, "A FULLY FUNDED MOUNT
/// BOOKS `Cash`", and `crates/vike-tradehub-core/src/node/accounts.rs`'s `restore_spot_positions_cash`):
/// a spot pair ([`names_spot_pair`]), or any symbol of a venue whose only margin mode is `Cash`
/// (polymarket's outcome shares). Read off the venue and the symbol, so it needs no frame and holds on an older node.
/// ONE intent rule on both surfaces: the CLI's and MCP's order ids (`crates/vike-client/src/verbs.rs`'s
/// `fill_order_client_id`) and the desktop's (`crates/vike-order-entry/src/dispatch/mod.rs`'s `order_client_id_intent`).
/// It lives here, beside the spot rule it reads, and not in `vike-venues`: [`names_spot_pair`] reads this crate's
/// kept rows ([`on_futures_lane`]), which a lower crate cannot.
pub fn books_cash(venue: &str, symbol: &str) -> bool {
    names_spot_pair(venue, symbol.trim())
        || matches!(
            vike_venues::capabilities_for(venue).margin_modes,
            [vike_model::MarginMode::Cash]
        )
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
/// spelling is this with its own separator: [`to_oanda_instrument`]'s `_`.
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

/// **The fiat money `symbol` on `venue` is quoted in, read off its KEPT row where the row states one** (`row`: the
/// symbol's row of the datahub's `<project>/instruments/<venue>.json`, found by `crate::kept_row`), the name only a
/// fallback (the owner's ruling of 2026-11-16), as a [`FiatQuote`]:
///
/// - **a kept row quoted in the minor unit of a fiat currency** (dukascopy's `GBX`, pence of `GBP`; [`MinorUnit`]),
///   on every venue: [`FiatQuote::Minor`], never the major and never dollars (flag DUK-3);
/// - **an FX or CFD broker** (`vike_venues::session_for` is the FX week: dukascopy, oanda, ig, fxcm, ctrader) **and
///   ibkr** list instruments in many currencies, so their quote is always a currency: the row's quote when it is a
///   fiat code, and a row quoted in anything else is [`FiatQuote::Unknown`], never dollars. With no row, the name's:
///   none on ibkr (its user name states no currency, decision 0290's O-10: its rows are the operator's, flag IBKR-1),
///   else [`fiat_quote_of`] or the fiat code the name's last token ends in, the dollar included (`AAPLUSUSD`); a name
///   that states none (IG's `IX.D.DAX.IFD.IP`, a euro index; `US500`) is [`FiatQuote::Unknown`] (flag IG-1), so the
///   dollar ledger refuses it instead of booking it at par;
/// - **every other venue** (crypto, alpaca, polymarket): the row's quote when it is a fiat code (binance's `IDTRY`
///   is lira, which its name does not say); else [`fiat_quote_of`], the name's answer, as with no row. A row quoted
///   in a coin or a stablecoin does not say which fiat that is, and the name may: binance's `EUREURI` is quoted in
///   `EURI`, a euro stablecoin, and its name keeps it off the dollar ledger. So a row can only ADD a quote the name
///   misses there, never drop one (measured over the CI box's kept lists, 2026-11-13: 24 binance rows add one, `EUREURI`
///   is the one a row alone would have dropped), and `None` ("does not say") keeps the crypto convention on the
///   dollar ledger, never dollars.
#[must_use]
pub fn kept_fiat_quote(
    venue: &str,
    symbol: &str,
    row: Option<&vike_model::Instrument>,
) -> Option<FiatQuote> {
    if let Some(unit) = row.and_then(|row| MinorUnit::of_minor(&row.quote)) {
        return Some(FiatQuote::Minor(unit));
    }
    let broker =
        venue == "ibkr" || vike_venues::session_for(venue) == vike_venues::session::FX_WEEK;
    let named = || match venue {
        "ibkr" => None,
        _ if broker => fiat_quote_of(symbol).or_else(|| fiat_tail(symbol)),
        _ => fiat_quote_of(symbol),
    };
    let quote = match row {
        Some(row) => fiat_code(&row.quote).or_else(|| if broker { None } else { named() }),
        None => named(),
    };
    match quote {
        Some(code) => Some(FiatQuote::Code(code)),
        None => broker.then_some(FiatQuote::Unknown),
    }
}

/// The words every ledger refuses a [`FiatQuote::Unknown`] and a [`FiatQuote::Minor`] with: never a currency code,
/// so no conversion pair books it.
const UNKNOWN_QUOTE: &str = "unknown quote currency";

/// A [`FiatQuote`] in a refusal's words: its code, or [`UNKNOWN_QUOTE`] for an unknown quote and a minor unit alike
/// (on main a broker's `GBX` row already read as the unknown quote, so every refusal keeps its bytes).
fn quote_words(quote: FiatQuote) -> &'static str {
    match quote {
        FiatQuote::Code(code) => code,
        FiatQuote::Unknown | FiatQuote::Minor(_) => UNKNOWN_QUOTE,
    }
}

/// The fiat code the LAST token of `symbol` ends in, the dollar included (`AAPLUSUSD`, `USA500IDXUSD`): what a
/// broker's CFD name states where [`fiat_quote_of`] leaves the dollar unsaid (that reader's dollar exception is for
/// crypto names such as `BTCFDUSD`, which no broker lists).
fn fiat_tail(symbol: &str) -> Option<&'static str> {
    let last = symbol.rsplit(['_', '/', '.', '-']).next()?.trim();
    fiat_code(last.get(last.len().checked_sub(3)?..)?)
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
        && !(quote == DEFAULT_CURRENCY && matches!(&token[6..], "T" | "C"))
    {
        return Some(quote);
    }
    tail.filter(|quote| *quote != DEFAULT_CURRENCY)
}

/// What [`off_ledger_quote`] answers for a name that states no fiat quote on a ledger that is not the
/// dollar: the symbol is neither proven to be in that currency nor covered by the crypto convention.
const NO_FIAT_QUOTE: &str = "no fiat quote";

/// Why a symbol whose fiat `quote` is known ([`fiat_quote_of`] or [`kept_fiat_quote`]) cannot be booked in a
/// `ledger` ledger (a [`fiat_code`] spelling), or `None` when it can: that quote when it is not the ledger's (an
/// unknown quote and a minor unit in [`quote_words`]' words, on every ledger), or [`NO_FIAT_QUOTE`] when there is
/// none and the ledger is not the dollar.
///
/// The asymmetry is the rule: a name that "does not say" is booked at par only on the dollar ledger
/// (the crypto convention: a stablecoin quote is a dollar). On a yen ledger `BTCUSDT` is not yen, and
/// `7203.T` (a name that really is yen) is not PROVEN yen by its name, so both are refused until a
/// caller holds the instrument row.
fn off_ledger_quote(quote: Option<FiatQuote>, ledger: &str) -> Option<&'static str> {
    match quote {
        Some(FiatQuote::Code(code)) if same_currency(code, ledger) => None,
        Some(quote) => Some(quote_words(quote)),
        None if same_currency(ledger, DEFAULT_CURRENCY) => None,
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
    off_ledger_list_of(
        symbols.into_iter().map(|s| (s, fiat_quote_of(s).map(FiatQuote::Code))),
        ledger,
    )
}

/// [`off_ledger_list`] over symbols whose fiat quote the caller already holds (`(symbol, quote)`, the quote
/// [`kept_fiat_quote`] read off a kept row where one is held): the same rule, phrase and order, a
/// [`FiatQuote::Minor`] worded exactly like a [`FiatQuote::Unknown`] (`"unknown quote currency"`), so every refusal
/// reads as it did before the quote was typed.
#[must_use]
pub fn off_ledger_list_of<'a>(
    quoted: impl IntoIterator<Item = (&'a str, Option<FiatQuote>)>,
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

/// The foreign-quoted ones among `symbols` (a fiat quote [`fiat_quote_of`] reads other than
/// [`DEFAULT_CURRENCY`]), each with its quote and listed once in first-seen order (`"USD_JPY (JPY), EUR_GBP (GBP)"`),
/// or `None` when there are none: [`off_ledger_list`] on the dollar ledger, the phrase the journal and paper doors
/// refuse with.
#[must_use]
pub fn foreign_fiat_quoted_list<'a>(symbols: impl IntoIterator<Item = &'a str>) -> Option<String> {
    off_ledger_list(symbols, DEFAULT_CURRENCY)
}

/// **Whether a fill's fee is in the fill's own money**, read off the symbol's NAME and the fee's asset: a fold
/// of fills that adds the fee to its quote totals may add only these (a fee of 0.01 BNB on a USDT fill is no
/// 0.01 USDT).
///
/// - an EMPTY asset: the venue stated none, so the fee is the fill's money, as every fold has read it;
/// - a dollar asset ([`crate::underlying`]'s `DOLLAR_QUOTES`): the money of the one-currency sheet
///   (`vike_analytics::without_non_dollar_ledgers` keeps a dollar ledger's fills only);
/// - the quote the name ends in, separators ignored (`ETHBTC` and `ETH-BTC` charging BTC).
///
/// Anything else (an exchange's own coin, the base coin of a spot buy, an inverse contract's settle coin) is
/// another asset, and `false`.
///
/// shortcut: the name, not a catalog row: a fill row names no quote asset. A venue whose name states no quote and
/// charges in a non-dollar quote reads as another asset (left out, said, never added at par); read the
/// instrument's row here the day a fill carries one.
#[must_use]
pub fn fee_in_the_fills_money(symbol: &str, fee_asset: &str) -> bool {
    let Some(asset) = vike_model::currency_code(fee_asset) else {
        return true;
    };
    if crate::underlying::DOLLAR_QUOTES.contains(&asset.as_str()) {
        return true;
    }
    let name: String =
        symbol.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_uppercase();
    name.len() > asset.len() && name.ends_with(&asset)
}

#[path = "symbol_tests.rs"]
#[cfg(test)]
mod symbol_tests;
