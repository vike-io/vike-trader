//! `venue_symbol` — **how each roster venue's ENGINE spells a symbol, and what is refused BY NAME before any
//! lookup**: the per-venue rule table (`SYMBOL_RULES`), the spellings (`VenueSymbolSpelling`) and the one grammar
//! (`canonical_symbol`).
//!
//! It was the tradehub control door's own (`crates/vike-tradehub/src/server/symbol_admission.rs`, decision 0174)
//! and moved here, bodies unchanged, so the door and `vike-cli secrets account set-symbol` read ONE grammar: an
//! account's stated symbol (`vike_secrets::AccountRow::symbol`) is refused at the CLI exactly as the door would refuse
//! it as an order's. What the door then DOES with a rule (admit through the lease's facts, fence, refuse) stays
//! the door's. Whether a name is LISTED is the venue's answer, from the facts or the adapter's row; this module
//! adds one offline reading for the CLI, `venue_engine_symbol`, which also refuses a bybit name its KEPT list
//! carries no row of (`BTCUSDX`), and that reading is the CLI's and the node's boot's alone.

use vike_model::{AssetClass, Instrument};

/// How an `Admits` venue spells a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueSymbolSpelling {
    /// Upper case with the `.P` lane suffix kept (binance, aster): `ETHUSDT` spot, `ETHUSDT.P` perp.
    LaneSuffixed,
    /// Upper case, no suffix (bybit's linear-only exec plane): `ETHUSDT`.
    BareLinear,
    /// The venue's own instId, upper case, nothing translated (okx): `ETH-USDT` (spot), `ETH-USDT-SWAP`,
    /// `ETH-USDT-261225` (a dated future). See `canonical_okx_instid`.
    Hyphenated,
    /// The name as the venue lists it, case kept, no lane suffix (hyperliquid): a perpetual `BTC`, `kPEPE`, a spot
    /// pair `HYPE/USDC`, a builder-dex coin `xyz:TSLA`. `kPEPE` and `KPEPE` are different spellings, so nothing is
    /// upper-cased. See `canonical_hyperliquid_symbol`.
    CaseKept,
    /// The venue's own PERPETUAL name, upper case, nothing translated (deribit): `ETH-PERPETUAL` (the
    /// coin-margined book) or `ETH_USDC-PERPETUAL` (the linear one), a dated future `ETH-27MAR26`, a USDC
    /// option `ETH_USDC-27MAR26-2500-C`. See `canonical_deribit_name`.
    InstrumentName,
    /// The venue's own asset symbol, upper case, nothing translated (alpaca): an equity ticker `AAPL` /
    /// `BRK.B`, or a crypto pair WITH its slash `BTC/USD`. See `canonical_alpaca_symbol`.
    AssetSymbol,
    /// The broker's own symbol name, CASE KEPT, nothing translated (ctrader): `EURUSD`, and the names a
    /// CFD list carries (`Wheat_H6`, `USCrude100`). A cTrader name is a case-sensitive string the account's
    /// symbol list defines, so nothing is upper-cased and the list refuses a wrong case by name. See
    /// `canonical_broker_name`.
    BrokerName,
    /// The engine's own spelling of an OANDA instrument, upper case: a six-letter FX pair WITHOUT its
    /// underscore (`GBPUSD`, as the mounts spell `EURUSD`) or a CFD name WITH it (`SPX500_USD`); `GBP_USD`
    /// is read as `GBPUSD`. The adapter and the gatewayhub's facts turn it into OANDA's `GBP_USD`
    /// (`vike_catalog::to_oanda_instrument`). See `canonical_oanda_instrument`.
    OandaInstrument,
    /// IG's own EPIC, upper case, nothing translated: `CS.D.EURUSD.MINI.IP`, dot-separated segments. See
    /// `canonical_epic`. Upper case is the engine's AND the reconcile client's spelling
    /// (`ReconcileAdmittedSymbols` holds epics upper-case), so an epic IG itself spells otherwise is refused by the
    /// facts (`vike_ig::instrument` compares the venue's spelling exactly), never admitted twice.
    Epic,
    /// An IBKR US stock by NAME (ibkr): `TICKER.SMART.CCY`, upper case; a bare ticker is read as the USD stock
    /// (`AAPL` is `AAPL.SMART.USD`). A forex pair and a dated future keep their own names, and an option and an
    /// explicit exchange are refused by name (`canonical_ibkr_stock`).
    StockName,
    /// A currency or metal PAIR, upper case (dukascopy, fxcm): written `EURUSD` or `EUR/USD`, any pair of two codes
    /// losing its slash (both venues' fills spell the pair without it; a coin too, `DOGE/USD` is `DOGEUSD`). A name of
    /// any other shape that is still made of the characters an instrument name has (`COPPER.CMD/USD`) is passed through
    /// unchanged for the gatewayhub's table to refuse by name. See `canonical_pair_name`.
    PairName,
    /// A Polymarket OUTCOME TOKEN (polymarket): the 20-or-more-digit ERC-1155 id, used unchanged (it is the one name
    /// of the position: fills, positions and every order carry it). Typed as a market NAME instead (`slug:Outcome`,
    /// the market's address), it is resolved to the id first by the door's `SymbolAdmitter::resolve` (the gatewayhub
    /// reads Gamma), so [`canonical_symbol`] only ever sees an id. The engine behind it is mounted on no symbol
    /// (the door's `account_wide`). See `canonical_outcome_token`.
    OutcomeToken,
}

/// What a venue's adapter does with the symbol an order names. What the door does with each rule is its own
/// (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `plan_submit_symbol`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueSymbolRule {
    /// Signs on the order's symbol: the door admits it.
    Admits(VenueSymbolSpelling),
    /// Signs on the engine's mounted symbol: the door refuses any other, for this reason. ⚠ No roster
    /// venue has this row today (deribit's was the last): the variant is kept because
    /// `just new-venue`'s classification hint offers it to the next adapter.
    MountedOnly(&'static str),
    /// Reads the order's own symbol already; not this gate's. ⚠ No roster venue has this row today (polymarket's was
    /// the last, until it became `Admits`): the variant is kept because `just new-venue`'s classification hint offers
    /// it, and `per_order_refusal`'s fences stay for the next venue that holds it.
    PerOrder,
    /// Reads the order's own symbol, but the ENGINE cannot judge a symbol it was not started on (its
    /// grid, multiplier or price are the mounted symbol's): the door refuses a foreign symbol, for this
    /// reason, until admission supports the venue. Deleting the row (to `PerOrder` or `Admits`) opens it.
    /// ⚠ No roster venue has this row today (fxcm's was the last, until its price lane): the variant is kept
    /// because `just new-venue`'s classification hint offers it. ⚠ NO TEST HOLDS WHAT IT DOES any more (the test
    /// that did went with the last row, and a rule cannot be injected into `SYMBOL_RULES` from outside): whoever
    /// gives a venue this row again writes one first (`per_order_refusal`'s `PerOrderUnsafe` arm).
    PerOrderUnsafe(&'static str),
}

/// The rule of every roster venue. `every_roster_venue_has_a_symbol_rule` fails on a venue in none,
/// so a new venue is classified by whoever reads its order builder.
// vike:new-venue:note do NOT add a row here by hand-copying one: read the new venue's ORDER BUILDER (does it sign on the order's symbol or on the one its client was bound to?), then add `("{venue}", VenueSymbolRule::PerOrder)` if it reads the order's symbol AND the engine judges a foreign symbol soundly (grid, multiplier, a price source), `VenueSymbolRule::PerOrderUnsafe("<why>")` if it reads the symbol but the engine cannot, or `VenueSymbolRule::MountedOnly("<why>")` if it does not. `Admits` is only for a venue whose adapter has a per-symbol router AND whose `TradeLane::symbol_facts` answers (the gatewayhub), and it also needs a `VenueSymbolSpelling`.
pub const SYMBOL_RULES: &[(&str, VenueSymbolRule)] = &[
    ("binance", VenueSymbolRule::Admits(VenueSymbolSpelling::LaneSuffixed)),
    ("aster", VenueSymbolRule::Admits(VenueSymbolSpelling::LaneSuffixed)),
    ("bybit", VenueSymbolRule::Admits(VenueSymbolSpelling::BareLinear)),
    ("okx", VenueSymbolRule::Admits(VenueSymbolSpelling::Hyphenated)),
    ("deribit", VenueSymbolRule::Admits(VenueSymbolSpelling::InstrumentName)),
    ("hyperliquid", VenueSymbolRule::Admits(VenueSymbolSpelling::CaseKept)),
    ("alpaca", VenueSymbolRule::Admits(VenueSymbolSpelling::AssetSymbol)),
    ("ctrader", VenueSymbolRule::Admits(VenueSymbolSpelling::BrokerName)),
    ("ig", VenueSymbolRule::Admits(VenueSymbolSpelling::Epic)),
    ("oanda", VenueSymbolRule::Admits(VenueSymbolSpelling::OandaInstrument)),
    ("fxcm", VenueSymbolRule::Admits(VenueSymbolSpelling::PairName)),
    ("ibkr", VenueSymbolRule::Admits(VenueSymbolSpelling::StockName)),
    ("polymarket", VenueSymbolRule::Admits(VenueSymbolSpelling::OutcomeToken)),
    ("dukascopy", VenueSymbolRule::Admits(VenueSymbolSpelling::PairName)),
];

/// The canonical spelling of `raw` for `spelling`, or why it is not a symbol.
///
/// # Errors
/// The words that say why `raw` is not a symbol of that spelling, naming it.
pub fn canonical_symbol(spelling: VenueSymbolSpelling, raw: &str) -> Result<String, String> {
    if spelling == VenueSymbolSpelling::BareLinear {
        return canonical_bybit_symbol(raw);
    }
    if spelling == VenueSymbolSpelling::Hyphenated {
        return canonical_okx_instid(raw);
    }
    if spelling == VenueSymbolSpelling::InstrumentName {
        return canonical_deribit_name(raw);
    }
    if spelling == VenueSymbolSpelling::AssetSymbol {
        return canonical_asset_symbol(raw);
    }
    if spelling == VenueSymbolSpelling::BrokerName {
        return canonical_broker_name(raw);
    }
    if spelling == VenueSymbolSpelling::OandaInstrument {
        return canonical_oanda_instrument(raw);
    }
    if spelling == VenueSymbolSpelling::Epic {
        return canonical_epic(raw);
    }
    if spelling == VenueSymbolSpelling::StockName {
        return canonical_ibkr_stock(raw);
    }
    if spelling == VenueSymbolSpelling::PairName {
        return canonical_pair_name(raw);
    }
    if spelling == VenueSymbolSpelling::OutcomeToken {
        return canonical_outcome_token(raw);
    }
    if spelling == VenueSymbolSpelling::CaseKept {
        return canonical_hyperliquid_symbol(raw);
    }
    let upper = raw.trim().to_ascii_uppercase();
    let (base, perp) = vike_venues::split_perp(&upper);
    if spelling == VenueSymbolSpelling::LaneSuffixed {
        lane_suffixed_refusal(raw, &upper, base, perp)?;
    }
    // A dated USDS-M future keeps its delivery date in the symbol (`BTCUSDT_261225`) and is spelled with the lane
    // suffix; the check above has already refused every other use of `_`.
    let name = lane_suffixed_name(spelling, base);
    if name.is_empty() || base.len() > 32 || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!(
            "`{raw}` is not an exchange symbol (letters and digits, e.g. `ETHUSDT`)"
        ));
    }
    Ok(match spelling {
        VenueSymbolSpelling::LaneSuffixed if perp => format!("{base}{}", vike_venues::PERP_SUFFIX),
        VenueSymbolSpelling::LaneSuffixed
        | VenueSymbolSpelling::BareLinear
        | VenueSymbolSpelling::Hyphenated
        | VenueSymbolSpelling::InstrumentName
        | VenueSymbolSpelling::AssetSymbol
        | VenueSymbolSpelling::BrokerName
        | VenueSymbolSpelling::OandaInstrument
        | VenueSymbolSpelling::Epic
        | VenueSymbolSpelling::StockName
        | VenueSymbolSpelling::PairName
        | VenueSymbolSpelling::OutcomeToken
        | VenueSymbolSpelling::CaseKept => base.to_string(),
    })
}

/// `("BTCUSDT", "261225")` for the base of a dated future, `BTCUSDT_261225`: a name and a six-digit delivery date.
/// `("BTCUSD", "PERP")` for `BTCUSD_PERP`, the COIN-M perpetual's own spelling: the door lets the name through to
/// the facts, which answer it from the COIN-M list (an inverse contract, decision 0258's binance amendment), where the
/// generic check would only say "not a symbol".
fn delivery_split(base: &str) -> Option<(&str, &str)> {
    let (name, tail) = base.rsplit_once('_')?;
    (tail == "PERP" || (tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit())))
        .then_some((name, tail))
}

/// The part of `base` that must be letters and digits: the symbol itself, or, for a lane-suffixed venue's dated
/// future, the name before its delivery date.
fn lane_suffixed_name(spelling: VenueSymbolSpelling, base: &str) -> &str {
    if spelling == VenueSymbolSpelling::LaneSuffixed {
        delivery_split(base).map_or(base, |(name, _)| name)
    } else {
        base
    }
}

/// **What a lane-suffixed venue's door (binance, aster) refuses BY NAME**, before the generic letters-and-digits
/// check would say only "not an exchange symbol": an OPTIONS symbol (`BTC-261225-85000-C`, the EAPI shape), and a
/// futures contract written without the lane suffix (`BTCUSDT_261225` would route to the spot lane; the USDS-M lane
/// is `.P`, `BTCUSDT_261225.P`). `Ok` for everything else, which the caller then judges as before.
fn lane_suffixed_refusal(raw: &str, upper: &str, base: &str, perp: bool) -> Result<(), String> {
    let parts: Vec<&str> = upper.split('-').collect();
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if let [underlying, expiry, strike, side] = parts.as_slice()
        && !underlying.is_empty()
        && underlying.bytes().all(|b| b.is_ascii_alphanumeric())
        && expiry.len() == 6
        && digits(expiry)
        && digits(strike)
        && matches!(*side, "C" | "P")
    {
        return Err(format!(
            "`{raw}` is an options symbol (the exchange's EAPI shape) and this adapter trades spot and USDS-M \
             futures only: no options order is signed. MEASURED 2026-10-30: the exchange's demo environment has \
             no options API (its demo-eapi host redirects to the public website), so an options order cannot be \
             proven on a demo account"
        ));
    }
    if !perp
        && let Some((name, _)) = delivery_split(base)
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(format!(
            "`{raw}` names a futures contract (a delivery date or `_PERP` in the symbol), and a symbol without the \
             lane suffix is the spot lane: write it `{upper}.P`"
        ));
    }
    Ok(())
}

/// **Bybit's engine symbol is the venue's own name, upper case**: a linear perpetual bare (`ETHUSDT`, a `.P` you
/// type is dropped: the lane suffix says PERPETUAL and the engine's symbol never carried it), or a DATED linear
/// future under its own name (`ETHUSDT-27NOV26`: the base and quote, a hyphen, the delivery day as `DDMMMYY`). Letters
/// and digits only apart from that one hyphen: the symbol reaches a signed query string downstream. Whether the name
/// is LISTED, and what it is, is the facts' to say.
fn canonical_bybit_symbol(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let plain =
        |s: &str| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric());
    // A SPOT pair is `PAIR.S` (the exec plane's one suffix, `crates/bridges/bybit/src/instrument/class.rs`'s `SPOT_SUFFIX`):
    // the bare name is the perpetual's, and the pair shares it.
    if let Some(pair) = upper.strip_suffix(".S") {
        return if plain(pair) {
            Ok(upper)
        } else {
            Err(format!(
                "`{raw}` is not a bybit spot pair (the pair as the venue names it, `DOGEUSDT.S`)"
            ))
        };
    }
    let name = upper.strip_suffix(vike_venues::PERP_SUFFIX).unwrap_or(&upper);
    // An option is `BTC-31OCT26-75500-C-USDT`: refused by name, not as a misspelling.
    if name.matches('-').count() > 1 {
        return Err(format!(
            "`{raw}` is a bybit option; options are not traded here (the engine has no option payoff or \
             short-option margin model)"
        ));
    }
    let ok = match name.split_once('-') {
        None => plain(name),
        Some((base, day)) => plain(base) && is_delivery_day(day),
    };
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "`{raw}` is not an exchange symbol (letters and digits, e.g. `ETHUSDT`; a dated future as the venue \
             names it, `ETHUSDT-27NOV26`; a spot pair `DOGEUSDT.S`)"
        ))
    }
}

/// Whether `day` is a bybit delivery day, `DDMMMYY` (`27NOV26`): two digits, three letters, two digits.
fn is_delivery_day(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 7
        && b[..2].iter().all(u8::is_ascii_digit)
        && b[2..5].iter().all(u8::is_ascii_uppercase)
        && b[5..].iter().all(u8::is_ascii_digit)
}

/// The most digits an outcome token id has: an ERC-1155 id is a uint256, 78 decimal digits at most.
const OUTCOME_TOKEN_MAX_DIGITS: usize = 78;

/// Whether `text` is an outcome token id: 20 or more decimal digits and no more than a uint256 holds. The twin of
/// `vike_polymarket::catalog::market_ref`'s `is_token_id` (the venue's own predicate, which this node cannot name: it links
/// no bridge), held to it by `crates/vike-tradehub/src/server/tests/outcome_token_door.rs`'s
/// `the_doors_token_test_agrees_with_the_bridges_classifier`.
#[must_use]
pub fn is_outcome_token(text: &str) -> bool {
    (20..=OUTCOME_TOKEN_MAX_DIGITS).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_digit())
}

/// **A Polymarket symbol is an outcome's TOKEN id**, used unchanged: the digits are the venue's own name for the
/// position (every order, fill and holding carries it), so there is nothing to translate and nothing to upper-case.
/// What an operator may type instead, a market name, never reaches this function: the door's `SymbolAdmitter::resolve` turned it
/// into the id before the planner canonicalised. So a text that is not an id here is a surface with no source to
/// resolve it, or a name no source could, and is refused with the spellings that work.
fn canonical_outcome_token(raw: &str) -> Result<String, String> {
    let token = raw.trim();
    if is_outcome_token(token) {
        return Ok(token.to_string());
    }
    Err(format!(
        "`{raw}` is not a Polymarket outcome token id (a number of 20 or more digits); a market name \
         (`market-slug:Yes`, or the market's polymarket.com address) is looked up by the gatewayhub, \
         which this surface did not reach"
    ))
}

/// **Hyperliquid's symbol is the venue's own name, case kept** (decision 0174, 0218): a perpetual's coin as the venue
/// lists it (`BTC`, `kPEPE`; `KPEPE` is another, unlisted name), a SPOT pair written `BASE/QUOTE` (`HYPE/USDC`), or a
/// builder-dex (HIP-3) coin written `dex:COIN` (`xyz:TSLA`). Letters and digits in every part, at most one `/` or one
/// `:`, nothing else: the symbol reaches a signed query and a JSON body downstream. The venue's wire name of a spot
/// pair (`@107`) is refused by name here, before any lookup, saying how to write the pair; a `.P` lane suffix has no
/// meaning on this venue and stays a refused name, never `BTC`. Whether the name is LISTED (and in which margin mode) is
/// the facts' to say.
fn canonical_hyperliquid_symbol(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.starts_with('@') {
        return Err(format!(
            "`{raw}` is not an exchange symbol: that is the venue's wire name of a spot pair, write the pair \
             as BASE/QUOTE (for example `HYPE/USDC`)"
        ));
    }
    let word =
        |p: &str| !p.is_empty() && p.len() <= 20 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let ok = name.len() <= 41
        && match (name.split_once('/'), name.split_once(':')) {
            (None, None) => {
                !name.is_empty()
                    && name.len() <= 32
                    && name.bytes().all(|b| b.is_ascii_alphanumeric())
            }
            (Some((a, b)), None) | (None, Some((a, b))) => word(a) && word(b),
            (Some(_), Some(_)) => false,
        };
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "`{raw}` is not an exchange symbol (a Hyperliquid coin as listed, `BTC`; a spot pair `HYPE/USDC`; a \
             builder-dex coin `dex:COIN`; letters and digits only)"
        ))
    }
}

/// **ctrader's symbol is the broker's own name**, case kept and nothing translated: the name the account's
/// symbol list gives (`EURUSD`; a CFD list also carries `Wheat_H6`, `USCrude100`), looked up by that exact
/// string. Letters, digits and `_`, at most 32, nothing else: the name reaches a log line and a protobuf
/// field downstream. A wrong case (`eurusd`) is another, unlisted name and the list refuses it by name.
fn canonical_broker_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty()
        || name.len() > 32
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(format!(
            "`{raw}` is not a ctrader symbol (the broker's name as the account's symbol list spells it, \
             case kept, letters, digits and `_`, e.g. `EURUSD`)"
        ));
    }
    Ok(name.to_string())
}

/// **IG's symbol is its EPIC**, upper-cased, nothing translated: three or more non-empty dot-separated
/// segments of letters, digits, `_` and `-` (`CS.D.EURUSD.MINI.IP`). A market NAME (`EURUSD`, `EUR/USD`),
/// which the adapter itself refuses (`non_epic_refusal`: IG answers it with an HTTP 500), is refused here
/// by name before any lookup. The symbol reaches a URL path downstream, so nothing else passes.
fn canonical_epic(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let segment = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    if upper.len() > 64 || upper.split('.').count() < 3 || !upper.split('.').all(segment) {
        return Err(format!(
            "`{raw}` is not an IG epic (a dot-separated market id such as `CS.D.EURUSD.MINI.IP`, \
             not a pair name like `EURUSD`)"
        ));
    }
    Ok(upper)
}

/// **A Dukascopy or FXCM instrument is a six-letter currency or metal pair**, upper-cased: `EURUSD`, written with
/// or without its slash (`EUR/USD` becomes `EURUSD`, the form both venues' fills and position lines carry), and a
/// pair of two codes of other lengths loses its slash the same way ([`bare_pair`]). What else the venue names
/// (`AAPL.US/USD`, `COPPER.CMD/USD`) is passed through UNCHANGED, upper-cased, when it is
/// made of letters, digits, `.` and `/` only, so the gatewayhub's table refuses it BY NAME (a CFD placeholder,
/// a CFD whose fill spelling differs, an instrument the table does not carry); the door itself knows no table.
/// Anything with another character is refused here, before any lookup: the symbol reaches the sidecar's JSON
/// line (dukascopy) or the Offers lookup (fxcm) downstream.
fn canonical_pair_name(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let bare = bare_pair(&upper);
    if is_pair(&bare) {
        return Ok(bare);
    }
    let name_made = !upper.is_empty()
        && upper.len() <= 32
        && upper.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'/');
    if name_made {
        Ok(bare)
    } else {
        Err(format!(
            "`{raw}` is not an instrument name (a currency or metal pair is six letters, \
             `EURUSD` or `EUR/USD`)"
        ))
    }
}

/// `symbol` upper-cased with the slash of a pair of two codes dropped (`EUR/USD`, `DOGE/USD`): the one spelling the
/// engine, the fills and the quotes share. A part with a dot is an SDK name, whose slash IS its spelling
/// (`COPPER.CMD/USD`): `crates/bridges/dukascopy/src/instrument/any_symbol_tests.rs`'s
/// `every_slashed_name_of_the_table_is_a_pair_of_three_letter_codes_or_carries_a_dot` holds that against that table.
#[must_use]
pub fn bare_pair(symbol: &str) -> String {
    let upper = symbol.trim().to_ascii_uppercase();
    let code = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric());
    match upper.split_once('/') {
        Some((a, b)) if code(a) && code(b) => format!("{a}{b}"),
        _ => upper,
    }
}

/// Whether `bare` is six ASCII letters, the shape of a currency or metal pair.
#[must_use]
pub fn is_pair(bare: &str) -> bool {
    bare.len() == 6 && bare.bytes().all(|b| b.is_ascii_alphabetic())
}

/// **oanda's symbol, in the engine's spelling**: upper case, a six-letter FX pair without an underscore
/// (`GBP_USD` and `gbpusd` are `GBPUSD`: the mounts spell a pair that way, and one instrument must be ONE
/// symbol in an engine), any other name as OANDA writes it, with its single underscore (`SPX500_USD`).
/// Letters and digits and at most that one underscore, nothing else: the symbol reaches a URL path and a
/// JSON body downstream. Nothing is guessed: a bare `BTCUSD`-shaped name is read as the FX pair it looks
/// like and the account's own list refuses it by name.
fn canonical_oanda_instrument(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "an OANDA instrument is a six-letter FX pair such as `GBPUSD` (or `GBP_USD`), or a CFD \
                name with its underscore such as `SPX500_USD`";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 12 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let ok = match upper.split_once('_') {
        None => upper.len() == 6 && upper.bytes().all(|b| b.is_ascii_alphabetic()),
        Some((base, quote)) => word(base) && word(quote),
    };
    if !ok {
        return Err(format!("`{raw}` is not an OANDA instrument ({hint})"));
    }
    Ok(match upper.split_once('_') {
        Some((base, quote))
            if base.len() == 3
                && quote.len() == 3
                && base.bytes().chain(quote.bytes()).all(|b| b.is_ascii_alphabetic()) =>
        {
            format!("{base}{quote}")
        }
        _ => upper,
    })
}

/// **alpaca's symbol is the venue's own asset symbol**, upper-cased, and nothing is translated: an equity
/// ticker (`AAPL`, with a share-class dot, `BRK.B`) or a crypto pair WITH its slash (`BTC/USD`). A bare
/// `BTCUSD` is NOT turned into `BTC/USD` here (the adapter does that on the wire, but guessing a pair
/// from letters at the door could sign a different instrument than the operator meant): it is read as
/// the ticker it looks like and the venue's list refuses it by name. Letters and digits, one inner `.` or
/// one inner `/`, nothing else: the symbol reaches a query string and a JSON body downstream.
fn canonical_asset_symbol(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let word =
        |p: &str| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let hint = "an alpaca symbol is an equity ticker such as `AAPL` (or `BRK.B`), or a crypto pair \
                written with its slash such as `BTC/USD`";
    let ok = match upper.split_once(['.', '/']) {
        None => word(&upper),
        Some((a, b)) => word(a) && word(b),
    };
    if ok { Ok(upper) } else { Err(format!("`{raw}` is not an alpaca symbol ({hint})")) }
}

/// **An IBKR stock is admitted by NAME**: `TICKER.SMART.CCY`, upper-cased, where the ticker is letters and
/// digits (a dot cannot be written in the engine grammar, `crates/bridges/vike-ibkr/src/instrument/contract.rs`) and the
/// currency is three letters; a bare ticker is read as the USD stock. A forex pair, a dated future and an option
/// are [`canonical_ibkr_class`]'s, asked first; what else a trader may type is refused here BY NAME, before any
/// lookup: a stock on a named exchange (`AAPL.NASDAQ.USD`: it is another way to name the stock the engine may
/// already hold). The gatewayhub's lookup checks the same form again and resolves the name to exactly one stock.
fn canonical_ibkr_stock(raw: &str) -> Result<String, String> {
    // A space and a hyphen are never a field separator: they are the class share's (`BRK B`, `BRK-B`).
    let upper: String = raw
        .trim()
        .to_ascii_uppercase()
        .chars()
        .map(|c| if matches!(c, ' ' | '-') { '_' } else { c })
        .collect();
    let hint = "an IBKR stock is `TICKER.SMART.CCY`, e.g. `AAPL.SMART.USD` (or just the ticker `AAPL`, read \
                as the USD stock; a class share is `BRK.B` or `BRK_B`; one contract is pinned with its id, \
                `TICKER.SMART.CCY.STK.<conId>`, e.g. `AAPL.SMART.USD.STK.265598`); an explicit exchange is \
                not admitted by name, a forex pair is `EUR.USD.IDEALPRO`, a future \
                `ES.CME.USD.FUT.20261218`, and an option is refused";
    if let Some(class) = canonical_ibkr_class(raw, &upper, hint) {
        return class;
    }
    let not_a_stock = || format!("`{raw}` is not an IBKR stock name ({hint})");
    let is_currency = |c: &str| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic());
    let parts: Vec<&str> = upper.split('.').collect();
    // The tail says the currency (and a pinned contract); what is left of it is the ticker, which is one word or
    // a class share's two (`BRK.B`), never more: `AAPL.NASDAQ.USD` is an exchange, not a three-word ticker.
    let (ticker, currency, pin) = match parts.as_slice() {
        [head @ .., "SMART", currency, "STK", id]
            if is_currency(currency)
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            if !id.parse::<i32>().is_ok_and(|n| n > 0) {
                return Err(format!(
                    "`{raw}` is not an IBKR stock name: the contract id {id:?} is not a positive whole \
                     number ({hint})"
                ));
            }
            (head, *currency, Some(*id))
        }
        [head @ .., "SMART", currency] if is_currency(currency) => (head, *currency, None),
        all if all.len() <= 2 && !all.contains(&"SMART") => (all, "USD", None),
        _ => return Err(not_a_stock()),
    };
    if !(1..=2).contains(&ticker.len()) {
        return Err(not_a_stock());
    }
    let ticker = ticker.join("_");
    if ticker.is_empty()
        || ticker.len() > 12
        || !ticker.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || ticker.starts_with('_')
        || ticker.ends_with('_')
        || ticker.contains("__")
    {
        return Err(not_a_stock());
    }
    Ok(match pin {
        Some(id) => format!("{ticker}.SMART.{currency}.STK.{id}"),
        None => format!("{ticker}.SMART.{currency}"),
    })
}

/// **An IBKR forex pair or dated future by NAME, or an option's refusal** (decision 0234), `None` for a name that is
/// neither (the stock grammar judges it). A pair is `EUR.USD.IDEALPRO` (typed `EUR.IDEALPRO.USD[.CASH]` or `EUR/USD`
/// too: one pair, one spelling); a future is `ROOT.EXCHANGE.CCY.FUT.YYYYMMDD[.MULTIPLIER]`, its month form
/// `YYYYMM` kept as typed (the gatewayhub spells the venue's day back). What each is NOT is judged by the facts, which
/// the gatewayhub resolves from the gateway (`vike_ibkr::instrument::any_symbol::forex_future`); this fn only reads the form.
fn canonical_ibkr_class(raw: &str, upper: &str, hint: &str) -> Option<Result<String, String>> {
    let is_currency = |c: &str| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic());
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((base, quote)) = upper.split_once('/')
        && is_currency(base)
        && is_currency(quote)
    {
        return Some(if base == quote {
            Err(format!("`{raw}` is not a forex pair: a pair is two different currencies"))
        } else {
            Ok(format!("{base}.{quote}.IDEALPRO"))
        });
    }
    let parts: Vec<&str> = upper.split('.').collect();
    if parts.iter().skip(1).any(|p| *p == "IDEALPRO") || parts.get(3) == Some(&"CASH") {
        let pair = match parts.as_slice() {
            [base, quote, "IDEALPRO"]
            | [base, "IDEALPRO", quote]
            | [base, "IDEALPRO", quote, "CASH"]
                if is_currency(base) && is_currency(quote) && base != quote =>
            {
                Some(format!("{base}.{quote}.IDEALPRO"))
            }
            _ => None,
        };
        return Some(pair.ok_or_else(|| {
            format!(
                "`{raw}` is not an IBKR forex pair: a pair is `BASE.QUOTE.IDEALPRO` (`EUR.USD.IDEALPRO`, or \
                 `EUR/USD`), two different three-letter currencies on IDEALPRO"
            )
        }));
    }
    match parts.as_slice() {
        [root, exchange, currency, "FUT", expiry]
        | [root, exchange, currency, "FUT", expiry, _]
            if !root.is_empty()
                && root.len() <= 12
                && root.bytes().all(|b| b.is_ascii_alphanumeric())
                && !exchange.is_empty()
                && exchange.bytes().all(|b| b.is_ascii_alphanumeric())
                && is_currency(currency)
                && digits(expiry)
                && matches!(expiry.len(), 6 | 8)
                && parts.get(5).is_none_or(|m| digits(m)) =>
        {
            Some(Ok(upper.to_string()))
        }
        [_, _, _, "FUT", ..] => Some(Err(format!(
            "`{raw}` is not an IBKR future: a future is `ROOT.EXCHANGE.CCY.FUT.YYYYMMDD`, e.g. \
             `ES.CME.USD.FUT.20261218` (the month `YYYYMM` is read too), with an optional multiplier last"
        ))),
        [_, _, _, "OPT", ..] => Some(Err(format!(
            "`{raw}` is not an IBKR stock name ({hint}). An IBKR option is refused by name: this engine has no \
             option payoff and no short-option margin model, and an option is never booked as a share"
        ))),
        _ => None,
    }
}

/// **deribit's symbol is the venue's own INSTRUMENT name**, upper-cased, and nothing is translated: a
/// perpetual `BASE-PERPETUAL` (`ETH-PERPETUAL`, the coin-margined book) or `BASE_QUOTE-PERPETUAL`
/// (`ETH_USDC-PERPETUAL`, the linear one); a DATED future `BASE-DDMMMYY` (`ETH-27MAR26`) or
/// `BASE_QUOTE-DDMMMYY`; a USDC-settled OPTION `BASE_QUOTE-DDMMMYY-STRIKE-C|P` (`ETH_USDC-27MAR26-2500-C`;
/// a decimal strike keeps the venue's lower-case `d`: `XRP_USDC-27MAR26-2d5-C`). What else a trader may type
/// is refused here BY NAME, before any lookup: a coin-settled option (`BTC-27MAR26-60000-C`: its premium is
/// coin, which a one-currency ledger books as dollars), a spot pair (`BTC_USDC`: the venue reports no
/// position for one), a combo and anything else (decision 0233). An expiry is [`deribit_expiry`]'s, so a day
/// the calendar does not have (`ETH-31FEB26`) is refused as no deribit name, as the venue would refuse it.
/// Letters, digits, `-` and `_` only: the symbol reaches a JSON-RPC frame downstream.
fn canonical_deribit_name(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "a deribit instrument is a perpetual `BASE-PERPETUAL` (`ETH-PERPETUAL`), a dated future \
                `BASE-DDMMMYY` (`ETH-27MAR26`) or a USDC option `BASE_USDC-DDMMMYY-STRIKE-C` \
                (`ETH_USDC-27MAR26-2500-C`)";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let base_ok = |b: &str| b.split('_').count() <= 2 && b.split('_').all(word);
    if upper.len() > 48
        || !upper.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("`{raw}` is not a deribit instrument name ({hint})"));
    }
    // `DDMMMYY`: one or two digits, a month, two digits (`3NOV26`, `27MAR26`), on a day the calendar has.
    let expiry_ok = |e: &str| deribit_expiry(e).is_some();
    // Digits with at most one `D` (the venue's decimal point), neither first nor last.
    let strike_ok = |s: &str| {
        s.bytes().next().is_some_and(|b| b.is_ascii_digit())
            && s.bytes().last().is_some_and(|b| b.is_ascii_digit())
            && s.bytes().all(|b| b.is_ascii_digit() || b == b'D')
            && s.bytes().filter(|b| *b == b'D').count() <= 1
    };
    let parts: Vec<&str> = upper.split('-').collect();
    match parts.as_slice() {
        [base, "PERPETUAL"] if base_ok(base) => Ok(upper.clone()),
        [base, expiry] if base_ok(base) && expiry_ok(expiry) => Ok(upper.clone()),
        [base, expiry, strike, cp @ ("C" | "P")]
            if base_ok(base) && expiry_ok(expiry) && strike_ok(strike) =>
        {
            if !base.contains('_') {
                return Err(format!(
                    "`{raw}` is a coin-settled option: its premium is quoted in {base}, so its notional \
                     and PnL are coin amounts that an account's one-currency ledger would book as \
                     dollars. Trade the USDC-settled options of the same underlying ({hint})"
                ));
            }
            Ok(format!("{base}-{expiry}-{}-{cp}", strike.to_ascii_lowercase()))
        }
        [_, _, _, "C" | "P"] => Err(format!("`{raw}` is not a deribit option name ({hint})")),
        [_] => Err(format!(
            "`{raw}` is a spot pair or a bare symbol: Deribit reports no position for a spot pair (a \
             holding is only a currency balance, the same wallet that is the collateral of the \
             coin-margined contracts), so the node could not reconcile one ({hint})"
        )),
        _ => {
            Err(format!("`{raw}` is not a deribit perpetual, dated future or USDC option ({hint})"))
        }
    }
}

/// **okx's symbol is its own instId, upper-cased, and nothing is translated** (`BTCUSDT` and `BTCUSDT.P`
/// are binance's names, not okx's: guessing a pair from them would sign a different instrument than
/// the operator meant). Three shapes are okx's: a spot pair `BASE-QUOTE` (`BTC-USDT`), a perpetual
/// `BASE-QUOTE-SWAP` (`BTC-USDT-SWAP`) and a dated future `BASE-QUOTE-YYMMDD` (`BTC-USDT-261225`;
/// the quote of the USD-margined contracts is spelled `USD_UM`, an underscore allowed in that slot
/// alone). What else a trader may type is refused here BY NAME, before any lookup: an option
/// (`BTC-USD-261106-76000-C`, whose premium the engine cannot value) and anything else. Letters and
/// digits between hyphens only: the symbol reaches a signed query string downstream. Which pair or
/// contract is admitted is the facts' to say (`okx_class_facts` in `vike_okx::symbol_properties`), not the spelling's.
fn canonical_okx_instid(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('-').collect();
    let hint = "an okx instrument id is `BASE-QUOTE` (a spot pair), `BASE-QUOTE-SWAP` or \
                `BASE-QUOTE-YYMMDD` (a dated future), e.g. `ETH-USDT`, `ETH-USDT-SWAP`, \
                `ETH-USDT-261225`";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    // the quote slot of a three-part id may carry the venue's own underscore (`USD_UM`)
    let quote = |p: &str| word(&p.replace('_', "")) && !p.starts_with('_') && !p.ends_with('_');
    let wellformed = upper.len() <= 48
        && match parts.as_slice() {
            [a, b] => word(a) && word(b) && *b != "SWAP",
            [a, b, c] => word(a) && quote(b) && word(c),
            [a, b, c, d, e] => [a, b, c, d, e].into_iter().all(|p| word(p)),
            _ => false,
        };
    if !wellformed {
        return Err(format!(
            "`{raw}` is not an okx instrument id ({hint}; okx's names are its own, so \
             `BTCUSDT` and `BTCUSDT.P` are not translated)"
        ));
    }
    match parts.as_slice() {
        [_, _] | [_, _, "SWAP"] => Ok(upper),
        // a real date, the bridge's own reading (`InstrumentType::of_inst_id`): six digits that are no date
        // (`ETH-USDT-261399`) used to pass here and be signed by the bridge as a SWAP (owner ruling F-8)
        [_, _, tail] if is_okx_delivery_date(tail) => Ok(upper),
        [_, _, _, _, "C" | "P"] => Err(format!(
            "`{raw}` is an option and this adapter does not trade options: the premium is quoted in \
             the underlying coin and the engine has no option payoff or margin model ({hint})"
        )),
        _ => Err(format!("`{raw}` is not an okx instrument id ({hint})")),
    }
}

/// **The delivery date of an okx dated future's id** (`BTC-USDT-261225`'s `261225`): six digits that read
/// as `YYMMDD`, a month 1 to 12 and a day 1 to 31. The ONE reading of that tail: the door's grammar
/// ([`canonical_symbol`]) and the okx bridge's class of an id (`crates/bridges/okx/src/instrument.rs`'s
/// `InstrumentType::of_inst_id`) both ask it, so the door never admits an id the bridge would sign as another class.
#[must_use]
pub fn is_okx_delivery_date(tail: &str) -> bool {
    let b = tail.as_bytes();
    b.len() == 6
        && b.iter().all(u8::is_ascii_digit)
        && matches!(
            &tail[2..4],
            "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
        )
        && (1..=31).contains(&tail[4..6].parse::<u32>().unwrap_or(0))
}

/// **A deribit `DDMMMYY` expiry as `(year, month, day)`** (`27MAR26` is 2026-03-27, `3NOV26` 2026-11-03): one or
/// two day digits, an upper-case month, two year digits (2000 to 2099), and a day the calendar has
/// (`vike_utility::calendar::is_real_day`: `31FEB26`, `00JAN26` and `29FEB25` are `None`). The ONE reading of that
/// grammar: the door's `canonical_deribit_name` and the deribit bridge's
/// `crates/bridges/deribit/src/feed/options/chain.rs`'s `parse_instrument_name` both ask it.
#[must_use]
pub fn deribit_expiry(expiry: &str) -> Option<(i64, u32, u32)> {
    const MONTHS: [&str; 12] =
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    if !expiry.is_ascii() || !(6..=7).contains(&expiry.len()) {
        return None;
    }
    let (day, rest) = expiry.split_at(expiry.len() - 5);
    let (month, year) = rest.split_at(3);
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(day) || !digits(year) {
        return None;
    }
    let month = (1_u32..).zip(MONTHS).find(|(_, word)| *word == month)?.0;
    let (year, day) = (2000 + year.parse::<i64>().ok()?, day.parse::<u32>().ok()?);
    vike_utility::calendar::is_real_day(year, month, day).then_some((year, month, day))
}

/// **`raw` in `venue`'s door spelling**: the control door's own reading of an order's symbol ([`SYMBOL_RULES`] through
/// [`canonical_symbol`]), for every OTHER door that takes a symbol (a mount, a strategy's leg, an account's stated
/// symbol, the gatewayhub's wire), so one instrument has one spelling wherever it enters. `None` for a venue whose rule
/// admits no symbol by name (no `Admits` row); else the canonical spelling, or the grammar's refusal in its words. What a
/// door does with each answer is the door's.
#[must_use]
pub fn door_symbol(venue: &str, raw: &str) -> Option<Result<String, String>> {
    match SYMBOL_RULES.iter().find(|(v, _)| *v == venue) {
        Some((_, VenueSymbolRule::Admits(spelling))) => Some(canonical_symbol(*spelling, raw)),
        _ => None,
    }
}

/// **The row of `symbol` on `venue` in `rows`, a KEPT instrument list** (`<project>/instruments/<venue>.json`, the
/// datahub's): the row spelled exactly so, else the ONE row the door's grammar ([`door_symbol`]) spells as it spells
/// `symbol` (`EUR/USD` and `EURUSD` are one instrument on fxcm: one instrument, one spelling). `None` when the list
/// carries no such row, or two rows that one spelling cannot tell apart: an ambiguous answer is no answer, never a
/// pick. A grammar, never a guess: no row is made up for a name the list does not carry.
#[must_use]
pub fn kept_row<'a>(rows: &'a [Instrument], venue: &str, symbol: &str) -> Option<&'a Instrument> {
    let listed = || rows.iter().filter(move |r| r.venue == venue);
    if let Some(exact) = listed().find(|r| r.raw_symbol == symbol) {
        return Some(exact);
    }
    let spelled = |raw: &str| door_symbol(venue, raw).and_then(Result::ok);
    let want = spelled(symbol)?;
    let mut same = listed().filter(|r| spelled(&r.raw_symbol).as_deref() == Some(want.as_str()));
    let first = same.next()?;
    same.next().is_none().then_some(first)
}

/// **Why `raw` is refused where a symbol is TAKEN AS WRITTEN** (a mount, a strategy's leg, the gatewayhub's wire): the
/// door grammar ([`door_symbol`]) reads it as a symbol of `venue` it spells ANOTHER way (`ethusdt.s` is `ETHUSDT.S` on
/// bybit, `EUR/USD` is `EURUSD` on fxcm), so the same instrument would carry two names. Such a door refuses rather
/// than respelling, because what it takes as written (a strategy's own orders, an engine's journal) keeps the name it
/// was given. `None` when the grammar spells it exactly so, or does not judge it: a venue with no rule, a blank, a
/// name the grammar refuses (an engine's own symbol is never read by the order grammar at the door: the venue or its
/// facts refuse a name they do not list), and the two venues whose ENGINE spelling the control door keeps as it is:
/// ibkr ([`VenueSymbolSpelling::StockName`]: an engine's own symbol is matched ignoring case, and its mounts are written in the
/// bridge's wider grammar, `EUR.IDEALPRO.USD`, `VOD.LSE.USD.STK`) and oanda ([`VenueSymbolSpelling::OandaInstrument`]: `EUR_USD`
/// and `EURUSD` are one instrument to the door, which sends an order on the engine's own spelling, and the venue and
/// the data store write `EUR_USD`).
#[must_use]
pub fn respelled_symbol(venue: &str, raw: &str) -> Option<String> {
    let Some((_, VenueSymbolRule::Admits(spelling))) =
        SYMBOL_RULES.iter().find(|(v, _)| *v == venue)
    else {
        return None;
    };
    if raw.trim().is_empty()
        || matches!(spelling, VenueSymbolSpelling::StockName | VenueSymbolSpelling::OandaInstrument)
    {
        return None;
    }
    match canonical_symbol(*spelling, raw) {
        Ok(canonical) if canonical != raw => Some(format!(
            "`{raw}` is written `{canonical}` on {venue} (the control door's spelling): one instrument has one \
             spelling, so write it `{canonical}` here and wherever the strategy names it"
        )),
        _ => None,
    }
}

/// **The engine symbol `raw` names on `venue`, or why it names none**: the reading `vike-cli secrets account
/// set-symbol` makes of an account's stated symbol before anything is written, with no network, and the node's
/// reading of a stated symbol at boot.
///
/// The venue's [`SYMBOL_RULES`] spelling through [`canonical_symbol`], the door's own grammar (so the CLI refuses
/// what the door would, in the door's words), and then, on bybit, its KEPT row ([`bybit_kept_row`] over `kept`, the
/// venue's list the datahub keeps): a name the grammar takes but that the list carries no row of (`BTCUSDX`) is
/// refused by name. A name is never read for a book (the owner's ruling of 2026-11-13). Whether the venue still
/// LISTS the name is its answer at the account's start. The door does NOT ask this: an order's name is judged by the
/// lease's facts, which read the listing itself.
///
/// # Errors
/// A venue no rule admits symbols for, a name the venue's grammar refuses, or a bybit name its kept list carries no
/// row of, each in words naming `raw` and the venue.
pub fn venue_engine_symbol(venue: &str, raw: &str, kept: &[Instrument]) -> Result<String, String> {
    let Some(read) = door_symbol(venue, raw) else {
        return Err(format!(
            "`{raw}` cannot be read as a {venue} symbol: no spelling is declared for {venue}"
        ));
    };
    let symbol = read?;
    if venue == "bybit" && bybit_kept_row(kept, &symbol).is_none() {
        let why = if kept.is_empty() {
            "no bybit instrument list is kept on this box yet (the datahub's \
             `<project>/instruments/bybit.json`): its weekly refresh, or the Data Manager's Refresh, writes it"
        } else {
            "the kept list (the datahub's `<project>/instruments/bybit.json`) carries no such row: a perpetual is \
             its bare name (`BTCUSDT`), a dated contract the venue's own name (`ETHUSDT-27NOV26`), a spot pair \
             `DOGEUSDT.S`"
        };
        return Err(format!("`{raw}` names no bybit book: {why}"));
    }
    Ok(symbol)
}

/// **The kept row a bybit ENGINE symbol names**, in `rows` (bybit's KEPT list, the datahub's
/// `<project>/instruments/bybit.json`), by the two lane suffixes both planes already spell: a spot pair `X.S` is the
/// SPOT row `X`; a bare name is the perpetual's row `X.P`, else a DATED contract's row `X` (`ETHUSDT-27NOV26`,
/// `BTCUSDZ26`: the venue's own name). The row carries its book (`wire.group`, the venue's category, and its typed
/// contract), which is what the bybit bridge signs in and `venue_engine_symbol` checks; a bare name never matches
/// the spot row of the same string (on the exec plane the bare `BTCUSDT` is the perpetual). `None`: the list carries
/// no such row, or no list is held. Never read off the name.
#[must_use]
pub fn bybit_kept_row<'a>(rows: &'a [Instrument], symbol: &str) -> Option<&'a Instrument> {
    let name = symbol.trim().to_ascii_uppercase();
    let row = |key: &str, class: AssetClass| {
        rows.iter().find(|r| r.venue == "bybit" && r.raw_symbol == key && r.asset_class() == class)
    };
    if let Some(pair) = name.strip_suffix(".S") {
        return row(pair, AssetClass::CryptoSpot);
    }
    let perpetual = format!("{name}{}", vike_venues::PERP_SUFFIX);
    row(&perpetual, AssetClass::CryptoPerp).or_else(|| row(&name, AssetClass::CryptoFuture))
}

#[path = "venue_symbol_tests.rs"]
#[cfg(test)]
mod venue_symbol_tests;
