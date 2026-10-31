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
// THE BRACKET LANE RULE: which engine can hold a bracket's stop-loss.
//
// It lived in the node (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_engine_refusal`) until the
// Trade window needed the same answer before the click (final review B, I-1): one predicate below
// both sides, so the window and the node cannot disagree about a lane.
// -----------------------------------------------------------------------------------------

/// On binance and aster an engine mounted on the SPOT lane holds no stop-loss, whatever a bracket
/// names; one mounted on the perp lane does. The lane is read off the ENGINE's symbol with
/// [`split_perp_at`], so a lower-case `.p` is the spot lane, exactly as the adapter reads it. Every
/// other roster venue has one lane, so any engine symbol holds a stop there.
#[test]
fn an_engine_on_a_dual_lane_venues_spot_lane_holds_no_stop() {
    let dual: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| spot_lane_holds_no_stop(v)).collect();
    assert!(!dual.is_empty(), "the rule is vacuous: no venue has a spot lane");
    for &venue in &dual {
        assert!(!engine_lane_holds_stop(venue, "BTCUSDT"), "{venue}: a spot engine");
        assert!(engine_lane_holds_stop(venue, "BTCUSDT.P"), "{venue}: a perp engine");
        assert!(!engine_lane_holds_stop(venue, "BTCUSDT.p"), "{venue}: `.p` is the spot lane");
    }
    for &venue in vike_model::VENUES.iter().filter(|v| !dual.contains(v)) {
        for symbol in ["BTCUSDT", "BTCUSDT.P", "BTC-USDT-SWAP", "BTC"] {
            assert!(engine_lane_holds_stop(venue, symbol), "{venue}/{symbol}: one lane");
        }
    }
}

/// ⚠ The spot-lane venue set is a SECOND spelling of [`fee_lane`]'s dual-lane set, so it is pinned
/// to it over the whole roster: a venue whose exec routes spot vs perp on the `.P` suffix gets its
/// own perp fee lane, and must also refuse a spot-lane bracket. A new dual-lane venue reddens this
/// rather than silently accepting spot brackets. (Moved here from the node's `server::tests::lower_command`
/// with the rule it pins.)
#[test]
fn the_spot_lane_venues_are_exactly_fee_lanes_dual_lane_venues() {
    for &venue in vike_model::VENUES {
        let dual_lane = fee_lane(venue, "X.P") != venue;
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
    assert!(!suffixed.is_empty(), "the derivation is vacuous — uses_perp_suffix matched nothing");
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
// USD1-settled 0.5 bps (live sweep, 2026-08-05; `vike_venues::fees` carries the table). Only the
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
        vike_venues::fee_schedule_for(fee_lane("aster", "BTCUSDT")),
        PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
        "aster SPOT BTCUSDT moved — measured live at 0.5/4 bps on 2026-08-05"
    );
    assert_eq!(fee_lane("aster", "BTCUSDT.P"), "aster-perp");
    assert_eq!(
        vike_venues::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
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
            vike_venues::fee_schedule_for(fee_lane("aster", sym)),
            PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
            "{sym}: USD1-Perp is 0/0.005% (published) and measured 0/0.5 bps live 2026-08-05"
        );
        assert_ne!(
            vike_venues::fee_schedule_for(fee_lane("aster", sym)),
            vike_venues::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
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
        vike_venues::fee_schedule_for("aster-perp-usd1"),
        vike_model::FeeSchedule::Free,
        "the USD1 lane key has no arm in vike_model::money::fees — every fee on it is silently zero"
    );
}

// -----------------------------------------------------------------------------------------
// THE SHAPE LANES: deribit's perpetuals and hyperliquid's spot pairs (FEES-AUDIT 2026-10-26
// §4.11). Neither venue uses the `.P` suffix; each fronts two books under one id and prices them
// apart, and the symbol's shape decides which one a mount trades.
// -----------------------------------------------------------------------------------------

/// A Deribit PERPETUAL resolves to its own lane and nothing else on that venue does: dated futures,
/// options, the empty string and a name that only CONTAINS the word keep the bare (options) row.
#[test]
fn a_deribit_perpetual_takes_its_lane_and_no_other_instrument_does() {
    for sym in ["BTC-PERPETUAL", "ETH-PERPETUAL", "BTC_USDC-PERPETUAL", "eth-perpetual"] {
        assert_eq!(fee_lane("deribit", sym), "deribit-perp", "{sym}");
    }
    for sym in [
        "BTC-27MAR26",
        "BTC-27MAR26-80000-C",
        "BTC-PERPETUAL-X",
        "PERPETUAL",
        "-PERPETUAL",
        "",
        "BTCUSDT.P",
        "BTCUSDT",
    ] {
        assert_eq!(fee_lane("deribit", sym), "deribit", "{sym} keeps the bare options row");
    }
    // the shape is deribit's alone: another venue's `...-PERPETUAL` is not a lane (hyperliquid is not in
    // this list: its own `BASE-QUOTE` spot shape reads any two-part hyphen name as a pair)
    for v in ["binance", "bybit", "okx", "ibkr"] {
        assert_eq!(fee_lane(v, "BTC-PERPETUAL"), v, "{v}");
    }
}

/// A Bybit SPOT pair (`PAIR.S`, decision 0231) takes the spot lane; the bare name and a typed `.P` stay the
/// perpetual's row, and no other venue reads the suffix.
#[test]
fn a_bybit_spot_pair_takes_the_spot_lane_by_its_suffix() {
    assert_eq!(fee_lane("bybit", "DOGEUSDT.S"), "bybit-spot");
    assert_eq!(fee_lane("bybit", "DOGEUSDT"), "bybit");
    assert_eq!(fee_lane("bybit", "DOGEUSDT.P"), "bybit");
    assert_eq!(fee_lane("bybit", "ETHUSDT-27NOV26"), "bybit");
    assert_eq!(fee_lane("bybit", "BTCUSD"), "bybit");
    assert_eq!(fee_lane("okx", "DOGEUSDT.S"), "okx");
    assert_eq!(fee_lane("binance", "DOGEUSDT.S"), "binance");
}

/// A Hyperliquid SPOT pair resolves to the spot lane in BOTH spellings; a perp (a bare coin), a
/// builder-dex perp in either spelling, another venue's multi-hyphen name and a half-pair do not.
#[test]
fn a_hyperliquid_spot_pair_takes_the_spot_lane_in_both_spellings() {
    for sym in ["HYPE/USDC", "HYPE-USDC", "PURR/USDC", "HYPE/USDT0"] {
        assert_eq!(fee_lane("hyperliquid", sym), "hyperliquid-spot", "{sym}");
    }
    for sym in [
        "BTC",
        "kPEPE",
        "xyz:TSLA",
        "TSLA.d-xyz",
        "",
        "/USDC",
        "HYPE/",
        "A/B/C",
        "BTC-USDT-SWAP",
        "BTCUSDT.P",
    ] {
        assert_eq!(fee_lane("hyperliquid", sym), "hyperliquid", "{sym} keeps the perp row");
    }
    // the shape is hyperliquid's alone: alpaca writes pairs with a slash too, and keeps its row
    for v in ["alpaca", "binance", "okx", "deribit"] {
        assert_eq!(fee_lane(v, "HYPE/USDC"), v, "{v}");
    }
}

/// THE REGRESSION GATE for both lanes, through the REAL path (`fee_lane` then
/// `vike_venues::fee_schedule_for`): the shape lane is priced apart from the bare row, and its key has
/// a real arm (a typo would ride `fee_schedule_for`'s `_ => Free` fallback and zero every fee).
#[test]
fn a_shape_lane_prices_apart_from_the_bare_row() {
    use vike_model::FeeSchedule::PercentMakerTaker;
    for (venue, bare, lane) in
        [("deribit", "BTC-27MAR26-80000-C", "ETH-PERPETUAL"), ("hyperliquid", "BTC", "HYPE/USDC")]
    {
        let (a, b) = (fee_lane(venue, bare), fee_lane(venue, lane));
        assert_eq!(a, venue, "{venue}: {bare} is the bare row");
        assert_ne!(b, venue, "{venue}: {lane} is a lane");
        assert_ne!(vike_venues::fee_schedule_for(a), vike_venues::fee_schedule_for(b), "{venue}");
        assert_ne!(
            vike_venues::fee_schedule_for(b),
            vike_model::FeeSchedule::Free,
            "{b} has an arm"
        );
    }
    // the measured perpetual taker (5.0 bps of the 10 USD contract) and the published spot pair
    assert_eq!(
        vike_venues::fee_schedule_for(fee_lane("deribit", "ETH-PERPETUAL")),
        PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 }
    );
    assert_eq!(
        vike_venues::fee_schedule_for(fee_lane("hyperliquid", "HYPE/USDC")),
        PercentMakerTaker { maker_bps: 4.0, taker_bps: 7.0 }
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
        to_exchange_symbol("alpaca", "BTC-USD", Some(vike_model::AssetClass::Equity)).as_deref(),
        Ok("BTC-USD")
    );
    // Hyperliquid is NOT asked, because its shape decides — no HL name carries a hyphen.
    assert_eq!(to_exchange_symbol("hyperliquid", "HYPE-USDC", None).as_deref(), Ok("HYPE/USDC"));
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
    assert_eq!(LEDGER_CCY, "USD");
    assert_eq!(foreign_fiat_quote_of("EURUSD"), None);
    assert_eq!(foreign_fiat_quote_of("BTCUSDT"), None);
    assert_eq!(foreign_fiat_quote_of("USDJPY"), Some("JPY"));
    let fills = ["EUR_USD", "USD_JPY", "USD_JPY", "BTCUSDT", "EUR_GBP", "USD_JPY"];
    assert_eq!(foreign_fiat_quoted_list(fills).as_deref(), Some("USD_JPY (JPY), EUR_GBP (GBP)"));
    assert_eq!(foreign_fiat_quoted_list(["EURUSD", "BTCUSDT", "AAPL"]), None);
    assert_eq!(foreign_fiat_quoted_list(std::iter::empty::<&str>()), None);
}

/// A ledger currency a person typed goes through `fiat_code`: trimmed, any case, upper-cased out, and
/// only a currency the recogniser knows (a coin, a stablecoin, junk and the empty string are not).
#[test]
fn a_typed_ledger_currency_is_read_or_refused() {
    assert_eq!(fiat_code("JPY"), Some("JPY"));
    assert_eq!(fiat_code(" jpy "), Some("JPY"));
    assert_eq!(fiat_code("Usd"), Some("USD"));
    for bad in ["", "  ", "JP", "JPYY", "BTC", "USDT", "XAU", "xyz"] {
        assert_eq!(fiat_code(bad), None, "{bad:?}");
    }
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
