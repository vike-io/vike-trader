use super::*;
use vike_venues::fee_lane;

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
        for symbol in ["BTCUSDT.P", "BTCUSDT", "BTCUSD_PERP.P", "", ".P", "BTC.PERP", "XPUSDT"] {
            assert_eq!(split_perp_at(v, symbol), split_perp(symbol), "{v}/{symbol:?}");
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
        for symbol in ["BTC-PERPETUAL", "BTC-USDT-SWAP", "HYPE/USDC", "SOMETHING.P", ".P"] {
            assert_eq!(
                split_perp_at(v, symbol),
                (symbol, false),
                "{v} has no suffix convention, so {symbol:?} must pass through whole"
            );
        }
    }
    // An unknown venue string takes the same inert path — the addressing fallback refuses to
    // claim a naming convention, so nothing is stripped on a venue nobody has classified.
    assert_eq!(split_perp_at("no-such-venue", "BTCUSDT.P"), ("BTCUSDT.P", false));
}

// -----------------------------------------------------------------------------------------
// THE BRACKET LANE RULE: which engine can hold a bracket's stop-loss.
//
// It lived in the node (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_engine_refusal`) until the
// Trade window needed the same answer before the click (final review B, I-1): one predicate below
// both sides, so the window and the node cannot disagree about a lane.
// -----------------------------------------------------------------------------------------

/// On binance and aster an engine mounted on the SPOT lane holds no stop-loss, whatever a bracket
/// names; one mounted on the futures lane does. The lane is read off the ENGINE's user name with
/// [`on_futures_lane`] (its kept row, else its grammar: a dated `BTCUSDT_261225` is the futures lane, decision
/// 0290). Every other roster venue has one lane, so any engine symbol holds a stop there.
#[test]
fn an_engine_on_a_dual_lane_venues_spot_lane_holds_no_stop() {
    let dual: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| spot_lane_holds_no_stop(v)).collect();
    assert!(!dual.is_empty(), "the rule is vacuous: no venue has a spot lane");
    for &venue in &dual {
        assert!(!engine_lane_holds_stop(venue, "BTCUSDT"), "{venue}: a spot engine");
        assert!(engine_lane_holds_stop(venue, "BTCUSDT.P"), "{venue}: a perp engine");
        assert!(engine_lane_holds_stop(venue, "BTCUSDT_261225"), "{venue}: a dated engine");
    }
    for &venue in vike_model::VENUES.iter().filter(|v| !dual.contains(v)) {
        for symbol in ["BTCUSDT", "BTCUSDT.P", "BTC-USDT-SWAP", "BTC"] {
            assert!(engine_lane_holds_stop(venue, symbol), "{venue}/{symbol}: one lane");
        }
    }
}

/// A SPOT pair is told from a derivative by its engine spelling, per venue, exactly as the adapter
/// picks its book; a venue whose names prove nothing is never spot.
#[test]
fn a_spot_pair_is_told_by_its_engine_spelling() {
    for (venue, symbol, spot) in [
        ("binance", "BTCUSDT", true),
        ("binance", "BTCUSDT.P", false),
        ("binance", "BTCUSD.P", false),
        ("binance", "BTCUSDT_261225", false),
        ("binance", "BTCUSD_261225", false),
        ("aster", "BTCUSDT", true),
        ("aster", "BTCUSDT.P", false),
        // bybit by decision 0290's user names: the unmarked pair, a perpetual, a dated contract, the bare lookup.
        ("bybit", "BTCUSDT", true),
        ("bybit", "BTCUSDT.P", false),
        ("bybit", "BTCUSD.P", false),
        ("bybit", "ETHUSDT_261127", false),
        ("bybit", "", false),
        // okx by decision 0290's user names too: the pair `BTCUSDT` (the venue's `BTC-USDT`), its perpetual, a dated
        // contract.
        ("okx", "BTCUSDT", true),
        ("okx", "BTCUSDT.P", false),
        ("okx", "BTCUSDT_261225", false),
        // hyperliquid by decision 0290's user names, as bybit: the unmarked pair, a perpetual, a builder-dex coin.
        ("hyperliquid", "HYPEUSDC", true),
        ("hyperliquid", "BTCUSDC.P", false),
        ("hyperliquid", "XYZ:TSLAUSDC.P", false),
        ("hyperliquid", "HYPE/USDC", false),
        ("deribit", "BTCUSD.P", false),
        ("alpaca", "AAPL", false),
        ("polymarket", "", false),
    ] {
        assert_eq!(names_spot_pair(venue, symbol), spot, "{venue}/{symbol}");
    }
}

/// ⚠ The spot-lane venue set (the venue table's `vike_venues::spot_pair_lanes_for` column) and [`fee_lane`]'s `.P`
/// split encode one fact twice, so the column is pinned to the split over the whole roster: a venue whose exec routes
/// spot vs perp on the `.P` suffix gets its own perp fee lane, and must also refuse a spot-lane bracket. A new
/// dual-lane venue reddens this rather than silently accepting spot brackets. (Moved here from the node's
/// `server::tests::lower_command` with the rule it pins.)
#[test]
fn the_spot_lane_venues_are_exactly_fee_lanes_dual_lane_venues() {
    for &venue in vike_model::VENUES {
        // Against the bare twin, not the venue id: a shape lane (ctrader's CFDs) reads both alike. A venue whose spot
        // pair is a stated row (`vike_venues::spot_pair_row`: bybit's unmarked pair since decision 0290) prices the
        // twins apart but routes each by its kept row, never by `.P`, and refuses a pair's stop in its own bridge.
        let dual_lane = fee_lane(venue, "X.P") != fee_lane(venue, "X")
            && vike_venues::spot_pair_row(venue).is_none();
        assert_eq!(spot_lane_holds_no_stop(venue), dual_lane, "{venue}");
    }
}

// -----------------------------------------------------------------------------------------
// FEE-LANE GATE.
//
// The defect this exists to catch: `vike_venues::fee_schedule_for` carried ONE row per venue id
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
/// to, so a fee lane key would be noise. **bybit**: its exec half is V5 LINEAR-PERP
/// only (its module doc says so, and there is no `run_spot` arm anywhere in the crate) — its
/// single `"bybit"` row already IS the perp row.
const SINGLE_EXECUTION_LANE: &[&str] = &["bybit"];

/// Every `.P`-suffix roster venue is classified into exactly ONE of the three sets above. A new
/// suffix venue fails here until someone decides whether its fee table needs a lane — the
/// question that went unasked when binance grew its perp lane.
#[test]
fn fee_lane_is_declared_for_every_perp_suffix_venue() {
    let suffixed: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| uses_perp_suffix(v)).collect();
    assert!(!suffixed.is_empty(), "the derivation is vacuous — uses_perp_suffix matched nothing");
    for v in suffixed {
        let n = [LANE_PRICED, LANE_NAMED_SAME_PRICE, SINGLE_EXECUTION_LANE]
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
/// `fee_lane(venue, symbol)` then `vike_venues::fee_schedule_for` — must give a DIFFERENT answer
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
            vike_venues::fee_schedule_for(spot_key),
            vike_venues::fee_schedule_for(perp_key),
            "{v}: both lanes resolve to the SAME schedule — a perp mount is being charged its \
                 spot fees (or vice versa). Give {perp_key} its own row in vike_model::money::fees."
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
            vike_venues::fee_schedule_for(v),
            vike_venues::fee_schedule_for(perp_key),
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
            vike_venues::fee_schedule_for(perp_key),
            vike_model::FeeSchedule::Free,
            "{perp_key} has no arm in vike_model::money::fees — it fell through to the Free fallback"
        );
    }
}

/// `fee_lane` is the IDENTITY for every venue that is not lane-keyed, on BOTH symbol forms — so
/// wiring it into a call site that used to pass a bare venue string is byte-identical
/// everywhere except the two lanes it exists to split.
#[test]
fn fee_lane_is_the_identity_for_every_other_venue() {
    let lane_keyed: Vec<&str> = LANE_PRICED.iter().chain(LANE_NAMED_SAME_PRICE).copied().collect();
    for &v in vike_model::VENUES.iter().chain(["ibkr_cpapi", "no-such-venue"].iter()) {
        for symbol in ["BTCUSDT", "BTCUSDT.P", "", "BTC-USDT-SWAP"] {
            if lane_keyed.contains(&v) && split_perp(symbol).1 {
                continue;
            }
            // ctrader's CFD lane is every name that is not a currency or metal pair, so only the
            // empty bare-venue lookup keeps its row (vike-venues' `a_ctrader_cfd_takes_the_free_lane`).
            if v == "ctrader" && !symbol.is_empty() {
                continue;
            }
            // deribit prices a perpetual's user name on its futures lane (decision 0290: `BTCUSD.P`; vike-venues'
            // `every_deribit_instrument_but_an_option_leaves_the_options_row`).
            if v == "deribit" && split_perp(symbol).1 {
                continue;
            }
            // A venue's stated spot pair is its own lane (bybit's unmarked `BTCUSDT`, decision 0290: vike-venues'
            // `a_bybit_spot_pair_takes_the_spot_lane_by_its_unmarked_name`).
            if vike_venues::spot_pair_row(v).is_some_and(|row| row.spelling.names_spot(symbol)) {
                continue;
            }
            assert_eq!(fee_lane(v, symbol), v, "{v}/{symbol} must resolve to the bare venue id");
        }
    }
    // SINGLE_EXECUTION_LANE venues are the interesting case: they DO carry `.P` symbols, and must
    // still resolve to the bare id (their one row is their only row).
    for &v in SINGLE_EXECUTION_LANE {
        assert_eq!(fee_lane(v, "BTCUSDT.P"), v);
    }
}

#[test]
fn six_letter_pair_splits_around_the_separator_it_is_given() {
    // A `/` separator: the slashed, the bare and the non-pair shapes.
    assert_eq!(six_letter_pair("eurusd", '/'), "EUR/USD");
    assert_eq!(six_letter_pair("eur/usd", '/'), "EUR/USD");
    assert_eq!(six_letter_pair("XAUUSD", '/'), "XAU/USD");
    assert_eq!(
        six_letter_pair("EUR_USD", '/'),
        "EUR_USD",
        "another venue's separator is not this one"
    );
    assert_eq!(six_letter_pair("ABCDEFG", '/'), "ABCDEFG");
}

#[test]
fn to_oanda_instrument_covers_the_conversion_table() {
    assert_eq!(to_oanda_instrument("eurusd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EURUSD"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EurUsd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EUR_USD"), "EUR_USD"); // already OANDA-shaped, just upcased
    assert_eq!(to_oanda_instrument("eur_usd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("XAUUSD"), "XAU_USD");
    // not exactly 6 alpha chars -> left alone (just upcased), no split inserted
    assert_eq!(to_oanda_instrument("ABCDE"), "ABCDE");
    assert_eq!(to_oanda_instrument("ABCDEFG"), "ABCDEFG");
    // 6 chars but not all alphabetic -> left alone too (the split guard requires all-alpha)
    assert_eq!(to_oanda_instrument("123456"), "123456");
    assert_eq!(to_oanda_instrument("EUR12"), "EUR12");
}

/// The two positive spellings, each with the currency it states: the pair token (any case, inside an
/// IG epic too) and the last separated token.
#[test]
fn a_name_that_states_a_fiat_quote_is_read() {
    for (name, quote) in [
        ("EURUSD", "USD"),
        ("eurgbp", "GBP"),
        ("USDJPY", "JPY"),
        ("EUR_GBP", "GBP"),
        ("USD/JPY", "JPY"),
        ("EUR.USD", "USD"),
        ("CS.D.GBPJPY.CFD.IP", "JPY"),
        ("SPX500_USD", "USD"),
        ("XAU_USD", "USD"),
        ("DE30_EUR", "EUR"),
        ("AAPL.SMART.EUR", "EUR"),
        ("BTC-EUR", "EUR"),
        ("BTC-USD", "USD"),
        // fxcm's exotic forex rows, read as stating no quote (and so booked at par) until 2026-11-08
        ("USDCLP", "CLP"),
        ("USD/COP", "COP"),
        ("USDTWD", "TWD"),
    ] {
        assert_eq!(fiat_quote_of(name), Some(quote), "{name}");
    }
}

/// ⚠ **A crypto asset quoted in a fiat currency states it without a separator**, and `None` there
/// read as "dollars" on the dollar ledger: `BTCEUR` booked euros as dollars with no label. An asset
/// ticker (not itself a fiat code) on a fiat code, a longer base on a non-dollar code, the
/// no-separator index spellings, and the broker-suffixed pair spellings are all read.
#[test]
fn a_fiat_quoted_asset_without_a_separator_is_read() {
    for (name, quote) in [
        // six letters: asset + fiat
        ("BTCEUR", "EUR"),
        ("ETHEUR", "EUR"),
        ("BTCTRY", "TRY"),
        ("BTCJPY", "JPY"),
        ("BTCGBP", "GBP"),
        ("btceur", "EUR"),
        ("XAUUSD", "USD"),
        ("BTCUSD", "USD"),
        // a fiat PAIR keeps reading as the pair, not as "asset + fiat"
        ("USDTRY", "TRY"),
        ("EURUSD", "USD"),
        // longer base on a non-dollar code, and the no-separator index spellings
        ("DOGEEUR", "EUR"),
        ("SHIBTRY", "TRY"),
        ("MATICEUR", "EUR"),
        ("DE30EUR", "EUR"),
        ("DEUIDXEUR", "EUR"),
        ("JPNIDXJPY", "JPY"),
        // a broker suffix on a fiat pair, in a token or after a separator
        ("USDJPYm", "JPY"),
        ("USDJPYM", "JPY"),
        ("EURUSDc", "USD"),
        ("EURUSDecn", "USD"),
        ("EURUSDpro", "USD"),
        ("EURUSD.pro", "USD"),
        ("USDJPY.m", "JPY"),
    ] {
        assert_eq!(fiat_quote_of(name), Some(quote), "{name}");
    }
}

/// The other side of the same rule: a name the recogniser must NOT read, because reading it would
/// refuse a run that worked or book a stablecoin as a fiat. The dollar-suffixed long names are
/// dollar-like already (`None` and `USD` are the same answer on the dollar ledger), and the
/// stablecoin tails after a dollar quote are not broker suffixes.
#[test]
fn a_stable_or_dollar_suffixed_long_name_is_left_alone() {
    for name in [
        "AAPLUSD",
        "BTCFDUSD",
        "ETHBUSD",
        "DOGEUSDT",
        "DOGEUSDC",
        "BTCUSDT",
        "EURUSDT",
        "EURUSDC",
        "GBPUSDT",
        "AUDUSDT",
        "ZARUSDT",
        "USDUSDT",
        "ETHBTC",
        "EURBTC",
        "BTCEURT",
        "EURUSD#",
        "EURUSDmicro",
        "EUR1",
        "BTC€",
    ] {
        assert_eq!(fiat_quote_of(name), None, "{name:?}");
    }
}

/// The ledger is the dollar: a USD-quoted name is not foreign to it, a yen- or pound-quoted one is, and
/// the list names each offender once, with its quote, in first-seen order.
#[test]
fn the_foreign_list_names_each_non_dollar_quote_once_in_first_seen_order() {
    let fills = ["EUR_USD", "USD_JPY", "USD_JPY", "BTCUSDT", "EUR_GBP", "USD_JPY"];
    assert_eq!(foreign_fiat_quoted_list(fills).as_deref(), Some("USD_JPY (JPY), EUR_GBP (GBP)"));
    assert_eq!(foreign_fiat_quoted_list(["EURUSD", "BTCUSDT", "AAPL"]), None);
    assert_eq!(foreign_fiat_quoted_list(std::iter::empty::<&str>()), None);
}

/// On the dollar ledger the new list IS the old one; on any other ledger only a name that states
/// EXACTLY that quote fits, and "does not say" is no longer par (decision 0183 item 13).
#[test]
fn a_non_dollar_ledger_accepts_only_its_own_quote() {
    let mix = ["EUR_USD", "USD_JPY", "BTCUSDT", "EUR_GBP", "7203.T"];
    assert_eq!(off_ledger_list(mix, "USD"), foreign_fiat_quoted_list(mix));
    assert_eq!(off_ledger_list(mix, " usd "), foreign_fiat_quoted_list(mix));
    assert_eq!(
        off_ledger_list(mix, "JPY").as_deref(),
        Some("EUR_USD (USD), BTCUSDT (no fiat quote), EUR_GBP (GBP), 7203.T (no fiat quote)")
    );
    // The crosses of one quote share a ledger; the pair token and the separated spelling agree.
    assert_eq!(off_ledger_list(["USDJPY", "EUR_JPY", "CS.D.GBPJPY.CFD.IP"], "JPY"), None);
    assert_eq!(off_ledger_list(["EURGBP"], "gbp"), None);
    assert_eq!(off_ledger_list(["EURGBP"], "USD").as_deref(), Some("EURGBP (GBP)"));
    assert_eq!(off_ledger_list(std::iter::empty::<&str>(), "JPY"), None);
}

/// `None` is "does not say", never "dollars": crypto concatenations, a stablecoin quote, a bare
/// ticker, a perp suffix, a lone code and the degenerate inputs.
#[test]
fn a_name_that_states_no_fiat_quote_is_not_guessed() {
    for name in [
        "BTCUSDT",
        "BTCUSDT.P",
        "BTC-USDT-SWAP",
        "HYPE-USDC",
        "BTC-PERPETUAL",
        "BTC",
        "AAPL",
        "BRK.B",
        "7203.T",
        "EURUSDT",
        "ABCDEF",
        "EUR12",
        "USD",
        "",
        "_",
        "-EUR-",
    ] {
        assert_eq!(fiat_quote_of(name), None, "{name:?}");
    }
}

/// What an operator typed trades on the futures lane in any case and with spaces around it: a perpetual by its trailing
/// marker, a dated contract by its `_YYMMDD` (decision 0290: `BTCUSDT_261225` carries no `.P`), with no kept list.
#[test]
fn a_typed_symbol_names_the_futures_lane_in_any_case() {
    for typed in ["BTCUSDT.P", "ethusdt.p", " BTCUSDT.p ", "BTCUSDT_261225", "btcusd_261225"] {
        assert!(on_futures_lane("aster", typed), "{typed:?}");
    }
    for typed in ["BTCUSDT", "BTC.PUSDT", "", "BTC-USDT-SWAP", "BTCUSDT_PERP", "BTCUSDT_26122"] {
        assert!(!on_futures_lane("aster", typed), "{typed:?}");
    }
}

/// **The futures lane is the KEPT row's** where this process holds the venue's list: binance's recorded COIN-M rows
/// (`crate::recorded_kept_lists`) are the futures lane by their user names, whatever the grammar would say.
#[test]
fn a_kept_row_puts_a_name_on_the_futures_lane() {
    crate::hold_recorded_kept_lists();
    for user in ["BTCUSD.P", "ETHUSD.P", "BTCUSD_261225", "btcusd_270326"] {
        assert!(on_futures_lane("binance", user), "{user}");
        assert!(!names_spot_pair("binance", user), "{user}");
        assert!(engine_lane_holds_stop("binance", user), "{user}");
    }
}

/// A KEPT row's fiat quote decides; a row quoted in a coin or a stablecoin, and no row, leave the name's answer.
/// `off_ledger_list_of` refuses by the quotes it is handed, in `off_ledger_list`'s words.
#[test]
fn a_kept_rows_fiat_quote_decides_and_a_stablecoin_row_leaves_the_names() {
    let row = |symbol: &str, quote: &str| {
        let pair = vike_model::Contract::Fx(vike_model::ContractFx::default());
        vike_model::Instrument::new("fxcm", symbol, "X", quote, pair)
    };
    let code = |c: &'static str| Some(FiatQuote::Code(c));
    assert_eq!(kept_fiat_quote("fxcm", "GER30", Some(&row("GER30", "EUR"))), code("EUR"));
    assert_eq!(kept_fiat_quote("binance", "GER30", None), None, "an index's name states no quote");
    assert_eq!(kept_fiat_quote("binance", "BTCUSDT", Some(&row("BTCUSDT", "USDT"))), None);
    assert_eq!(kept_fiat_quote("binance", "IDTRY", Some(&row("IDTRY", "TRY"))), code("TRY"));
    assert_eq!(
        kept_fiat_quote("binance", "IDTRY", None),
        None,
        "five letters: the name says nothing"
    );
    assert_eq!(kept_fiat_quote("binance", "EUREURI", Some(&row("EUREURI", "EURI"))), code("EUR"));
    assert_eq!(kept_fiat_quote("fxcm", "USDJPY", None), code("JPY"));
    assert_eq!(kept_fiat_quote("fxcm", "USD/JPY", Some(&row("USD/JPY", "jpy"))), code("JPY"));
    let names = ["USDJPY", "BTCUSDT", "USDJPY"];
    assert_eq!(
        off_ledger_list_of(
            names.iter().map(|s| (*s, fiat_quote_of(s).map(FiatQuote::Code))),
            "USD"
        ),
        off_ledger_list(names, "USD")
    );
    assert_eq!(
        off_ledger_list_of([("GER30", code("EUR")), ("BTCUSDT", None)], "USD").as_deref(),
        Some("GER30 (EUR)")
    );
}

/// **IG-1: a broker's symbol whose currency is not known is REFUSED, never booked at par**: an IG epic with no kept
/// row (`IX.D.DAX.IFD.IP`, a euro index) and any FX-week broker's name that states no currency answer
/// `FiatQuote::Unknown`, which every ledger refuses; a name ending in a currency, the dollar included, states it.
#[test]
fn a_brokers_symbol_of_no_known_currency_is_refused_on_every_ledger() {
    for (venue, symbol) in [
        ("ig", "IX.D.DAX.IFD.IP"),
        ("ig", "CS.D.BITCOIN.CFD.IP"),
        ("ctrader", "US500"),
        ("fxcm", "GER30"),
        ("dukascopy", "DEU.IDX"),
        ("oanda", "SPX500"),
    ] {
        assert_eq!(
            kept_fiat_quote(venue, symbol, None),
            Some(FiatQuote::Unknown),
            "{venue} {symbol}"
        );
    }
    for (venue, symbol, quote) in [
        ("ig", "CS.D.USDJPY.MINI.IP", "JPY"),
        ("dukascopy", "AAPLUSUSD", "USD"),
        ("dukascopy", "EURUSD_BID", "USD"),
        ("dukascopy", "DEU.IDX/EUR", "EUR"),
        ("ctrader", "XTIUSD", "USD"),
        ("oanda", "DE30_EUR", "EUR"),
    ] {
        assert_eq!(
            kept_fiat_quote(venue, symbol, None),
            Some(FiatQuote::Code(quote)),
            "{venue} {symbol}"
        );
    }
    // A crypto venue keeps the crypto convention: a name that states nothing passes on the dollar ledger.
    assert_eq!(kept_fiat_quote("binance", "BTCUSDT", None), None);
    assert_eq!(kept_fiat_quote("alpaca", "AAPL", None), None);
    let quoted = [("IX.D.DAX.IFD.IP", kept_fiat_quote("ig", "IX.D.DAX.IFD.IP", None))];
    for ledger in ["USD", "EUR", "JPY"] {
        assert_eq!(
            off_ledger_list_of(quoted, ledger).as_deref(),
            Some("IX.D.DAX.IFD.IP (unknown quote currency)"),
            "{ledger}"
        );
    }
}

/// **IBKR-1: an ibkr symbol is quoted in its KEPT row's currency** (decision 0290's O-10: ibkr's rows are the
/// operator's, and a user name states no currency), never booked as dollars; with no row every name is refused, an
/// old canonical spelling included.
#[test]
fn an_ibkr_symbol_is_quoted_in_its_kept_rows_currency() {
    let row = |raw: &str, quote: &str| {
        let contract = vike_model::Contract::Fx(vike_model::ContractFx::default());
        vike_model::Instrument::new("ibkr", raw, "X", quote, contract)
    };
    for (user, raw, quote) in [
        ("USDJPY", "USD.JPY.IDEALPRO", "JPY"),
        ("SAP", "SAP.IBIS.EUR.STK", "EUR"),
        ("VOD", "VOD.LSE.GBP.STK", "GBP"),
        ("ES_261218", "ES.CME.USD.FUT.20261218", "USD"),
    ] {
        assert_eq!(
            kept_fiat_quote("ibkr", user, Some(&row(raw, quote))),
            Some(FiatQuote::Code(quote)),
            "{user}"
        );
    }
    assert_eq!(
        kept_fiat_quote("ibkr", "BTC", Some(&row("BTC.PAXOS.USDT.CRYPTO", "USDT"))),
        Some(FiatQuote::Unknown)
    );
    for symbol in ["AAPL", "USDJPY", "ES_261218", "USD.JPY.IDEALPRO", "AAPL.SMART.USD"] {
        assert_eq!(kept_fiat_quote("ibkr", symbol, None), Some(FiatQuote::Unknown), "{symbol}");
    }
}

/// **DUK-3: a broker's KEPT row quoted in something that is not a currency is refused** (dukascopy's pence, `GBX`,
/// would book prices a hundred times too large as dollars); its name is not read in its place, and every ledger,
/// the pound's included, refuses it in the unknown quote's words.
#[test]
fn a_brokers_row_quoted_in_no_currency_is_refused() {
    let cfd = vike_model::Contract::Fx(vike_model::ContractFx::default());
    let pence =
        vike_model::Instrument::new("dukascopy", "LSE.GB/GBX", "LSE.GB", "GBX", cfd.clone());
    let gbp = MinorUnit::of_minor("GBX").expect("pence is a minor unit");
    let pence_quote = kept_fiat_quote("dukascopy", "LSEGBGBX", Some(&pence));
    assert_eq!(pence_quote, Some(FiatQuote::Minor(gbp)));
    assert_eq!(
        off_ledger_list_of([("LSEGBGBX", pence_quote)], "GBP").as_deref(),
        Some("LSEGBGBX (unknown quote currency)")
    );
    let pounds = vike_model::Instrument::new("dukascopy", "VOD.GB/GBP", "VOD.GB", "GBP", cfd);
    assert_eq!(
        kept_fiat_quote("dukascopy", "VODGBGBP", Some(&pounds)),
        Some(FiatQuote::Code("GBP"))
    );
}

/// **The typed reading is the untyped one with the sentinel as `Unknown`**, except a kept row quoted in a minor
/// unit, which is `Minor` (its major named) on every venue instead of the sentinel or the name's answer.
#[test]
fn the_typed_fiat_quote_names_the_unknown_and_the_minor_unit() {
    use vike_model::{FiatQuote, MinorUnit};
    let cfd = vike_model::Contract::Fx(vike_model::ContractFx::default());
    let row = |venue: &str, quote: &str| {
        vike_model::Instrument::new(venue, "X/Y", "X", quote, cfd.clone())
    };
    let gbp = MinorUnit::of_minor("GBX").expect("pence is a minor unit");
    assert_eq!(gbp.major, "GBP");
    for venue in ["dukascopy", "ibkr", "binance"] {
        let pence = row(venue, "GBX");
        assert_eq!(kept_fiat_quote(venue, "LSEGBGBX", Some(&pence)), Some(FiatQuote::Minor(gbp)));
    }
    assert_eq!(
        kept_fiat_quote("dukascopy", "VODGBGBP", Some(&row("dukascopy", "GBP"))),
        Some(FiatQuote::Code("GBP"))
    );
    assert_eq!(kept_fiat_quote("ig", "IX.D.DAX.IFD.IP", None), Some(FiatQuote::Unknown));
    assert_eq!(kept_fiat_quote("ibkr", "AAPL", None), Some(FiatQuote::Unknown));
    assert_eq!(kept_fiat_quote("oanda", "DE30_EUR", None), Some(FiatQuote::Code("EUR")));
    assert_eq!(kept_fiat_quote("binance", "BTCUSDT", None), None);
}

/// The typed off-ledger list words a minor unit exactly like an unknown quote and is the untyped list otherwise.
#[test]
fn the_off_ledger_list_words_a_minor_unit_as_an_unknown_quote() {
    use vike_model::{FiatQuote, MinorUnit};
    let gbp = MinorUnit::of_minor("GBX").expect("pence is a minor unit");
    let typed = [
        ("LSEGBGBX", Some(FiatQuote::Minor(gbp))),
        ("US500", Some(FiatQuote::Unknown)),
        ("USD_JPY", Some(FiatQuote::Code("JPY"))),
        ("EURGBP", Some(FiatQuote::Code("GBP"))),
        ("BTCUSDT", None),
    ];
    assert_eq!(
        off_ledger_list_of(typed, "GBP").as_deref(),
        Some(
            "LSEGBGBX (unknown quote currency), US500 (unknown quote currency), USD_JPY (JPY), \
             BTCUSDT (no fiat quote)"
        )
    );
    assert_eq!(
        off_ledger_list_of(typed, "USD").as_deref(),
        Some(
            "LSEGBGBX (unknown quote currency), US500 (unknown quote currency), USD_JPY (JPY), \
             EURGBP (GBP)"
        )
    );
}

/// A fee is in the fill's money when the venue named no asset, named a dollar asset, or named the quote the
/// symbol ends in; an exchange's own coin, a spot buy's base coin and an inverse contract's settle coin are not.
#[test]
fn a_fee_is_in_the_fills_money_only_in_its_quote_or_a_dollar() {
    for (symbol, asset) in [
        ("BTCUSDT", "USDT"),
        ("BTCUSDT", "usdt"),
        ("BTCUSDT", ""),
        ("BTC", "USDC"),
        ("BTC-USDT-SWAP", "USDT"),
        ("ETHBTC", "BTC"),
        ("ETH-BTC", "BTC"),
        ("EUR_USD", "USD"),
        ("AAPL", "USD"),
    ] {
        assert!(fee_in_the_fills_money(symbol, asset), "{symbol} charging {asset:?}");
    }
    for (symbol, asset) in
        [("BTCUSDT", "BNB"), ("BTCUSDT", "BTC"), ("BTC", "BTC"), ("BTC-PERPETUAL", "BTC")]
    {
        assert!(!fee_in_the_fills_money(symbol, asset), "{symbol} charging {asset:?}");
    }
}
