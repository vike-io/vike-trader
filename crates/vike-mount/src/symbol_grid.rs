//! Per-symbol PRICE/SIZE GRID wiring — the mount-side producer for
//! `vike_exec::RiskLimits::grid_by_symbol`.
//!
//! ## The defect this closes
//!
//! Every live arm of [`crate::make_engine_with_legs`] builds its `RiskGate` with
//! `vike_exec::RiskLimits::from_properties(&f)` from **ONE** symbol's `SymbolProperties` — the
//! mounted `(venue, symbol)` pair. `vike_exec::RiskGate::check` then rounds EVERY order onto those
//! scalars, whatever instrument it names. That is exactly right for a single-symbol engine and
//! wrong the moment one admits a second symbol: a coarse mount lot destroys a valid finer-grid
//! order outright (`vike_exec::round_to(0.5, Some(1.0))` is `0.0`), and the gate then denies it
//! `"non-positive-size"` — a reason that names nothing about the real cause.
//!
//! `vike_exec::RiskLimits::grid_by_symbol` is the per-symbol override map the gate has consulted
//! since it was added (`vike_exec::RiskLimits::grid_for`, called once per `check`), with
//! field-by-field fallback to the scalars. Until this module it had **no producer at all** outside
//! unit tests: every mount left it empty, so the fix existed and was unreachable.
//!
//! ## The reference semantics — the BACKTEST twin
//!
//! `crates/vike-backtest/src/engine/sim_broker.rs`'s `grid_for` already resolves a grid
//! PER SYMBOL (and point-in-time) and feeds each one through the same
//! `vike_exec::RiskLimits::from_properties`. The live side deliberately reuses that mapping —
//! `vike_exec::SymbolGrid::from_properties` is its per-leg twin, pinned equal to the scalar builder
//! by `a_symbol_grid_matches_the_scalar_builder_field_for_field` — rather than inventing a second
//! reading of the same venue payload. The ONE semantic the live side adds is the fallback: a
//! `SymbolGrid` field left `None` inherits the mount scalar, where the backtest's per-symbol
//! `RiskLimits` are independent. See `vike_exec::SymbolGrid::from_properties`' ⚠ for the residual
//! that creates.
//!
//! ## ⚠ Which arms are wired, and why the rest are NOT
//!
//! [`declared_grid_source`] is the STEP-1 capability declaration (CLAUDE.md's per-venue
//! capability-map playbook): one row per `vike_model::VENUES` roster venue, naming where THAT
//! arm's grid for a NON-mounted symbol would have to come from, cited to the arm that reads it.
//! Only [`DeclaredGridSource::InHand`] venues are wired here, and the reason is a hard rule rather
//! than a scheduling accident:
//!
//! **A mount must not gain a blocking network round trip it did not already have.** The crypto-CEX
//! / deribit / aster / alpaca / ibkr pre-fetches are all symbol-SCOPED requests (`?symbol=` on
//! `exchangeInfo`/`instruments-info`, a throwaway OAuth2 lifecycle per Alpaca call, a fresh
//! transient TWS socket per IBKR call), so a second symbol costs a second round trip — per leg,
//! serially, before the first feed is up. A degraded arm that SAYS so (`warn_ungridded_legs`,
//! one line at the mount naming the venue and the legs) beats a mount that hangs. Wiring one of
//! those arms is a one-line change here — `declared_symbol_grids` takes the arm's own fetch as a
//! CLOSURE — but it belongs to a PR that can weigh the startup cost per venue, not to this one.
//!
//! The arms that need no round trip are wired today; `declared_grid_source` is the authority for which, and `every_roster_venue_declares_a_grid_source` is the partition. Writing the list here is how it would rot.rs`'s `hyperliquid_live_client`) and cTrader (its
//! `SymbolsList` is resolved during the exec handshake — see
//! `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`, whose own doc says "Needs NO
//! network").

use indexmap::IndexMap;

use vike_exec::SymbolGrid;

/// Where a venue arm would have to get the instrument grid for a symbol OTHER than the mounted
/// one — the declaration behind which arms of [`crate::make_engine_with_legs`] populate
/// `vike_exec::RiskLimits::grid_by_symbol` (see this module's ⚠ section).
///
/// Declaring TODAY'S REALITY, per the capability-map playbook: the rows are read off the arms, no
/// arm's behaviour is inferred from a row, and `every_roster_venue_declares_a_grid_source` iterates
/// `vike_model::VENUES` so a new bridge crate cannot join the roster without classifying itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredGridSource {
    /// The arm ALREADY holds a resolved instrument table covering every symbol of the venue, so a
    /// declared leg's grid costs no network at all. These are the arms wired today.
    InHand,
    /// The arm's only per-symbol source is a blocking, symbol-SCOPED network round trip — the very
    /// call it already makes once for the mounted symbol. A declared leg would cost one more, at
    /// mount time, before any feed is up. NOT wired: see this module's ⚠ section.
    PerSymbolFetch,
    /// The arm fetches no instrument grid at all (or the venue publishes none of that shape), so
    /// there is nothing to read per symbol and nothing to wire. Its mounted symbol has no grid
    /// either, which is why a leg here inherits nothing harmful.
    NoGrid,
}

/// One row per roster venue (`vike_model::VENUES`). Each cites the arm it was read from.
pub fn declared_grid_source(venue: &str) -> DeclaredGridSource {
    match venue {
        // `crates/vike-mount/src/hyperliquid.rs`'s `hyperliquid_live_client` loads
        // `HyperliquidInstruments` for the WHOLE venue before it resolves the mounted symbol's
        // properties, so every other asset is already in the same response.
        "hyperliquid" => DeclaredGridSource::InHand,
        // The ctrader arm reads `handle.symbols.risk_properties(symbol)` off the exec handshake's
        // own `SymbolsList` — `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`.
        "ctrader" => DeclaredGridSource::InHand,
        // Symbol-scoped blocking pre-fetches, one round trip each:
        // `crates/bridges/binance/src/exec.rs`'s `fetch_binance_properties` (`?symbol=` on
        // exchangeInfo), `crates/bridges/bybit/src/exec.rs`'s `fetch_bybit_properties`,
        // `crates/bridges/okx/src/exec.rs`'s `fetch_okx_instrument`,
        // `crates/bridges/deribit/src/exec.rs`'s `fetch_deribit_properties`,
        // `crates/bridges/aster/src/exec.rs`'s `fetch_aster_properties`.
        "binance" | "bybit" | "okx" | "deribit" | "aster" => DeclaredGridSource::PerSymbolFetch,
        // `crates/bridges/alpaca/src/instruments.rs`'s `fetch_alpaca_properties` builds a THROWAWAY
        // OAuth2 client-credentials lifecycle per call — the most expensive per-symbol source on
        // the roster.
        "alpaca" => DeclaredGridSource::PerSymbolFetch,
        // `crates/bridges/vike-ibkr/src/properties.rs`'s `fetch_ibkr_properties` opens its OWN
        // transient socket connection to TWS/Gateway per call (IBKR publishes no keyless grid
        // endpoint), and is socket-backend-only.
        "ibkr" => DeclaredGridSource::PerSymbolFetch,
        // ig / oanda: exec-only arms with NO grid pre-fetch at all today — their mounted symbol
        // keeps `RiskLimits::new()`'s permissive `None`s, so a leg inherits nothing.
        // polymarket: shares are whole and price is a probability; the venue publishes no
        // instrument grid of this shape (the arm's own comment says so).
        // fxcm / dukascopy: no `make_engine` arm exists at all (see the crate module doc) — they
        // reach the paper `_` arm, whose limits are likewise all-`None`.
        // vike:new-venue:row // TODO(new-venue: {venue}): a fresh bridge has no `make_engine` arm, so it fetches no grid.
        // vike:new-venue:row // A NAMED `NoGrid` says that was decided; the `_` fallback below says nothing.
        // vike:new-venue:row "{venue}" => DeclaredGridSource::NoGrid,
        _ => DeclaredGridSource::NoGrid,
    }
}

/// Build the per-symbol grid map for the EXTRA symbols a mount declared beyond its own, from that
/// arm's per-symbol properties source.
///
/// `properties_for` is the arm's OWN lookup, passed as a closure rather than resolved here: this
/// crate must not learn a second way to ask a venue for an instrument grid, and taking the closure
/// is also what makes the laziness below directly observable offline (call it with a counting
/// closure — `an_empty_declaration_never_touches_the_venue`), with no network and no credentials.
///
/// Rules, each load-bearing:
/// * **An empty `declared` never calls `properties_for` and returns an empty map.** That is the
///   BYTE-IDENTICAL property for every mount that exists today: `grid_by_symbol` stays empty,
///   `vike_exec::RiskLimits::grid_for` returns the scalars verbatim for every symbol, and the
///   serialized `RiskLimits` is unchanged (the field carries `skip_serializing_if`, so an empty map
///   does not even reach `vike_exec::engine_snapshot::state_hash`).
/// * **A leg naming the MOUNTED symbol gets no row.** The scalars already ARE that symbol's grid,
///   so a row could only ever be a redundant copy that drifts. Skipping it is also what makes a
///   caller that includes the primary in its list identical to one that does not.
/// * **A failed lookup gets no row, never a zeroed one.** `SymbolProperties::default()` is
///   all-`0.0`, which `vike_exec::SymbolGrid::from_properties` folds to all-`None` — a row that
///   inherits every scalar, i.e. exactly today's wrong answer wearing a declaration that it was
///   resolved. No row leaves the same wrong answer, but `warn_ungridded_legs` then SAYS so.
pub(crate) fn declared_symbol_grids<F>(
    primary: &str,
    declared: &[String],
    properties_for: F,
) -> IndexMap<String, SymbolGrid>
where
    F: Fn(&str) -> Option<vike_model::SymbolProperties>,
{
    let mut grids = IndexMap::new();
    for leg in declared {
        // ⚠ The key is the RAW leg, not a trimmed copy. `vike_core`'s `resolve_intent_symbol`
        // matches `l.symbol == r` and stamps the raw declared string onto the `OrderRequest`, and
        // `RiskLimits::grid_for` then looks the map up by THAT string. A trimmed key would file a
        // row the gate can never hit for a leg declared with stray whitespace — the wrong-instrument
        // rounding this map exists to prevent, dressed as a resolved grid.
        // `trim` still DECIDES: an all-whitespace leg is a declaration error, not an instrument.
        if leg.trim().is_empty() || leg == primary || grids.contains_key(leg.as_str()) {
            continue;
        }
        if let Some(properties) = properties_for(leg) {
            grids.insert(leg.to_string(), SymbolGrid::from_properties(&properties));
        }
    }
    grids
}

/// Does this mount's `limits` carry a REAL instrument grid — i.e. did a venue pre-fetch land?
///
/// The distinction matters only for `warn_ungridded_legs`: with every scalar `None` (a paper
/// mount, a failed pre-fetch, or an arm that fetches no grid at all) an ungridded leg inherits
/// NOTHING, so there is no defect to report and a warning would be pure noise on the most common
/// mount in the tree.
fn carries_a_venue_grid(limits: &vike_exec::RiskLimits) -> bool {
    limits.tick_size.is_some()
        || limits.lot_size.is_some()
        || limits.min_qty.is_some()
        || limits.min_notional.is_some()
}

/// Say, ONCE per mount, that a declared leg is being judged on the MOUNTED symbol's grid.
///
/// This is the "a degraded arm that says so" half of the rule in this module's ⚠ section: the leg
/// still trades (it is refused nothing it was not refused before), but its tick/lot/floors are
/// another instrument's, and an operator has no other way to learn that. Emitted only when the
/// mount actually fetched a grid (`carries_a_venue_grid`) — with all-`None` scalars the leg
/// inherits nothing and there is nothing to warn about.
pub(crate) fn warn_ungridded_legs(
    venue: &str,
    primary: &str,
    declared: &[String],
    grids: &IndexMap<String, SymbolGrid>,
    limits: &vike_exec::RiskLimits,
) {
    if declared.is_empty() || !carries_a_venue_grid(limits) {
        return;
    }
    // Raw here too, matching `declared_symbol_grids`' key — a trimmed lookup would report a leg as
    // ungridded that IS gridded, or the reverse, which is the same identity split one level up.
    let ungridded: Vec<&str> = declared
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty() && *s != primary && !grids.contains_key(*s))
        .collect();
    if ungridded.is_empty() {
        return;
    }
    let why = match declared_grid_source(venue) {
        DeclaredGridSource::InHand => {
            "this venue's resolved instrument table does not know the symbol (misspelled, or not \
             listed)"
        }
        DeclaredGridSource::PerSymbolFetch => {
            "this venue's only per-symbol grid source is a blocking, symbol-scoped network \
             round-trip, which a mount deliberately does not add per declared leg (see \
             vike_mount::symbol_grid's module doc)"
        }
        DeclaredGridSource::NoGrid => "this venue arm fetches no instrument grid at all",
    };
    tracing::warn!(
        venue,
        mounted = primary,
        legs = ?ungridded,
        "declared leg(s) have NO per-symbol risk grid → they are rounded onto `{primary}`'s tick \
         and lot and judged against its floors, which is the wrong instrument's grid: {why}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn properties(tick: f64, step: f64) -> vike_model::SymbolProperties {
        vike_model::SymbolProperties { tick_size: tick, step_size: step, ..Default::default() }
    }

    /// THE BYTE-IDENTICAL PROPERTY, and the one that cannot be observed from the RESULT: an empty
    /// map comes back from "no legs", from "every leg was the primary" and from "every lookup
    /// failed" alike, so only the CALL COUNT distinguishes a mount that touched the venue from one
    /// that did not.
    ///
    /// NON-VACUOUS: the closure panics rather than merely counting, so any future edit that probes
    /// the venue "just to warm a cache" — the exact laziness bug `recon_if_enabled` exists to
    /// prevent one layer up — fails here instead of costing a silent round trip at every mount.
    #[test]
    fn an_empty_declaration_never_touches_the_venue() {
        let grids = declared_symbol_grids("BTCUSDT", &[], |_| {
            panic!("a mount with no declared legs must not ask the venue anything")
        });
        assert!(grids.is_empty());
    }

    /// A declared leg gets ITS OWN grid, keyed by symbol.
    ///
    /// NON-VACUOUS: it asserts the ETH values (`0.01`/`0.0001`), not merely that a row exists, so
    /// a helper that inserted the PRIMARY's properties under the leg's name — the shape of the bug
    /// being fixed — fails.
    #[test]
    fn a_declared_leg_carries_its_own_tick_and_lot() {
        let legs = vec!["ETHUSDT".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &legs, |s| match s {
            "ETHUSDT" => Some(properties(0.01, 0.0001)),
            _ => None,
        });
        let eth = grids.get("ETHUSDT").expect("the declared leg is gridded");
        assert_eq!(eth.tick_size, Some(0.01));
        assert_eq!(eth.lot_size, Some(0.0001));
        assert_eq!(grids.len(), 1);
    }

    /// The mounted symbol is never given a row of its own: the scalars already ARE its grid, and a
    /// second copy could only drift from them. A caller that passes the primary through is
    /// therefore identical to one that filters it out first.
    ///
    /// NON-VACUOUS: the closure records what it was asked, so this fails on a helper that skipped
    /// the ROW while still paying for the LOOKUP — the network cost being avoided, not just the
    /// duplicate key.
    #[test]
    fn the_mounted_symbol_is_never_re_gridded() {
        let asked = RefCell::new(Vec::new());
        let legs = vec!["BTCUSDT".to_string(), "  ".to_string(), String::new()];
        let grids = declared_symbol_grids("BTCUSDT", &legs, |s| {
            asked.borrow_mut().push(s.to_string());
            Some(properties(0.1, 0.001))
        });
        assert!(grids.is_empty(), "no row for the mounted symbol, none for blanks");
        assert!(asked.borrow().is_empty(), "and no lookup was paid for either");
    }

    /// A repeated leg is looked up ONCE. Two mounts declaring the same hedge symbol is the ordinary
    /// case, and on a venue whose lookup is a network call the duplicate would be a duplicate round
    /// trip.
    #[test]
    fn a_repeated_leg_is_looked_up_once() {
        let calls = RefCell::new(0usize);
        let legs = vec!["ETHUSDT".to_string(), "ETHUSDT".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &legs, |_| {
            *calls.borrow_mut() += 1;
            Some(properties(0.01, 0.0001))
        });
        assert_eq!(grids.len(), 1);
        assert_eq!(*calls.borrow(), 1);
    }

    /// A failed lookup leaves NO row rather than an all-`None` one. The difference is invisible to
    /// `grid_for` (both resolve to the scalars) but not to `warn_ungridded_legs`, which is the
    /// only thing that tells an operator the leg is on the wrong instrument's grid.
    ///
    /// NON-VACUOUS: `SymbolProperties::default()` folds to an all-`None` `SymbolGrid`, so a helper
    /// that inserted `unwrap_or_default()` would produce a map of length 1 here and pass every
    /// `grid_for` assertion — the emptiness IS the observable.
    #[test]
    fn a_failed_lookup_leaves_no_row_at_all() {
        let legs = vec!["NOPE".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &legs, |_| None);
        assert!(grids.is_empty());
    }

    /// THE END-TO-END PROPERTY, composed exactly as `make_engine_with_legs` composes it: a
    /// TWO-symbol mount rounds each leg onto ITS OWN lot.
    ///
    /// The mount is binance `BTCUSDT` (lot `0.01`) with `ETHUSDT` (lot `0.001`) declared. An
    /// `ETHUSDT` order of `0.004` is exactly four ETH lots and must survive untouched.
    ///
    /// NON-VACUOUS — IT FAILS WITH THE CHANGE REVERTED: drop the `grid_by_symbol` assignment (or
    /// make `declared_symbol_grids` return an empty map) and the order is rounded on BTC's lot
    /// instead — `round_to(0.004, Some(0.01))` is `0.4` ties-even'd to `0`, i.e. `0.0` — so the gate
    /// denies it `"non-positive-size"`. That pre-change verdict is asserted directly below rather
    /// than merely claimed, and by REASON rather than by `!ok`, so a future weakening cannot
    /// satisfy it by denying for some unrelated cause.
    ///
    /// The values are chosen so every division is EXACT in `f64` (`0.004 / 0.001` is exactly `4.0`
    /// — the scale factor is a power of two, so both literals round to doubles with the same
    /// mantissa): a ties-even test whose ratio lands a half-ULP either side of `.5` would be a
    /// coin-flip, not a pin.
    #[test]
    fn a_two_symbol_mount_judges_each_leg_on_its_own_lot() {
        let btc = properties(0.1, 0.01);
        let eth = properties(0.01, 0.001);
        let legs = vec!["ETHUSDT".to_string()];

        let mut limits = vike_exec::RiskLimits::from_properties(&btc);
        limits.grid_by_symbol = declared_symbol_grids("BTCUSDT", &legs, |s| match s {
            "ETHUSDT" => Some(eth),
            _ => None,
        });

        let order = vike_model::OrderRequest {
            client_order_id: "t".into(),
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            side: 1,
            qty: 0.004,
            order_type: "market".into(),
            ..Default::default()
        };
        let verdict = vike_exec::RiskGate::new(limits.clone())
            .check(&order, &vike_exec::RiskContext::default());
        assert!(verdict.ok, "ETH's own 0.001 lot admits it: {verdict:?}");
        let admitted = verdict.request.expect("an admitted verdict carries the request");
        assert_eq!(admitted.qty.to_bits(), 0.004_f64.to_bits(), "qty {} != 0.004", admitted.qty);

        // …and the SAME limits with the map emptied — literally the pre-change mount — deny it.
        // Asserted here rather than trusted, because "this test would fail without the change" is
        // the claim, and this is the cheapest way to make the claim itself machine-checked.
        limits.grid_by_symbol.clear();
        let pre_change =
            vike_exec::RiskGate::new(limits).check(&order, &vike_exec::RiskContext::default());
        assert!(!pre_change.ok);
        assert_eq!(pre_change.reason, "non-positive-size");
    }

    /// The mounted symbol keeps the scalars — an override must not leak across symbols, and this is
    /// the mount-side composition of `an_override_does_not_leak_to_other_symbols`.
    #[test]
    fn the_mounted_symbol_still_uses_the_mount_scalars() {
        let legs = vec!["ETHUSDT".to_string()];
        let mut limits = vike_exec::RiskLimits::from_properties(&properties(0.1, 0.01));
        limits.grid_by_symbol =
            declared_symbol_grids("BTCUSDT", &legs, |_| Some(properties(0.01, 0.001)));
        let g = limits.grid_for("BTCUSDT");
        assert_eq!(g.lot_size, Some(0.01));
        assert_eq!(g.tick_size, Some(0.1));
    }

    /// Every roster venue is CLASSIFIED — the capability-map completeness gate. A new bridge crate
    /// joining `vike_model::VENUES` must state where its arm's per-symbol grid comes from (even
    /// when the answer equals the fallback), rather than silently riding `NoGrid`.
    ///
    /// NON-VACUOUS in the direction that matters: `declared_grid_source` has a catch-all `_` arm,
    /// so a missing row could never be caught by exhaustiveness. This asserts the NAMED partition
    /// instead — the `InHand` set is spelled out here, so an arm that stops holding its table in
    /// hand (or a new one that starts) turns this red.
    #[test]
    fn every_roster_venue_declares_a_grid_source() {
        let sorted = |want: DeclaredGridSource| {
            let mut v: Vec<&str> = vike_model::VENUES
                .iter()
                .copied()
                .filter(|venue| declared_grid_source(venue) == want)
                .collect();
            v.sort_unstable(); // roster ORDER is not the subject here; membership is
            v
        };
        assert_eq!(
            sorted(DeclaredGridSource::InHand),
            vec!["ctrader", "hyperliquid"],
            "exactly these arms hold a whole-venue instrument table at mount time; adding one is a \
             deliberate change to what make_engine_with_legs wires"
        );
        // …and no roster venue is unclassified by accident: the PerSymbolFetch family is named too,
        // so the remainder is a DECLARED `NoGrid` set rather than an assumed one.
        assert_eq!(
            sorted(DeclaredGridSource::PerSymbolFetch),
            vec!["alpaca", "aster", "binance", "bybit", "deribit", "ibkr", "okx"],
            "these arms already make a symbol-scoped blocking pre-fetch for the mounted symbol; a \
             declared leg would cost one more at mount time, so they stay on the scalar fallback"
        );
        assert_eq!(
            sorted(DeclaredGridSource::NoGrid),
            vec!["dukascopy", "fxcm", "ig", "oanda", "polymarket"],
            "these arms fetch no instrument grid at all, so a leg inherits nothing from the mount"
        );
        // vike:new-venue:note add `"{venue}"` to the sorted vec above (or to the InHand/PerSymbolFetch one, if the new arm holds or fetches a grid) — this assertion compares against a SORTED literal, so the scaffold cannot append the row for you: crates/vike-mount/src/symbol_grid.rs's `every_roster_venue_declares_a_grid_source`
    }

    /// An UNGRIDDED leg on a mount that really fetched a grid is reported; the same leg on a mount
    /// with no grid at all is not. Exercised through the predicate the warning is gated on, since
    /// `tracing` output is not assertable here without a subscriber.
    #[test]
    fn only_a_mount_with_a_real_grid_has_anything_to_report() {
        assert!(carries_a_venue_grid(&vike_exec::RiskLimits::from_properties(&properties(
            0.1, 0.001
        ))));
        assert!(
            !carries_a_venue_grid(&vike_exec::RiskLimits::new()),
            "a paper mount's all-None scalars leak nothing onto a leg, so a warning would be noise"
        );
        // `warn_ungridded_legs` must be inert on both of those, and on an empty declaration — this
        // only proves it does not panic or index out of bounds, which is what a log-only helper can
        // be tested for.
        let legs = vec!["ETHUSDT".to_string()];
        warn_ungridded_legs(
            "binance",
            "BTCUSDT",
            &legs,
            &IndexMap::new(),
            &vike_exec::RiskLimits::from_properties(&properties(0.1, 0.001)),
        );
        warn_ungridded_legs(
            "binance",
            "BTCUSDT",
            &[],
            &IndexMap::new(),
            &vike_exec::RiskLimits::new(),
        );
    }
}

#[cfg(test)]
mod symbol_identity_tests {
    use super::*;

    fn props(tick: f64) -> vike_model::SymbolProperties {
        vike_model::SymbolProperties { tick_size: tick, ..Default::default() }
    }

    /// ⚠ **The grid key must be the SAME BYTES the runtime routes on.**
    ///
    /// `vike_core`'s `resolve_intent_symbol` matches a declared leg with `l.symbol == r` and stamps
    /// the RAW declared string onto the `OrderRequest`; `RiskLimits::grid_for` then looks the map up
    /// by that string. So a key normalised on the way in is a row the gate can never hit — the
    /// wrong-instrument rounding this map exists to prevent, wearing a row that says it was
    /// resolved. This asserts the two rules agree on the one input where they could differ.
    ///
    /// Non-vacuous: with the `trim()` this file used to apply, the key would be `"ETHUSDT"` and the
    /// lookup below — by the raw declared spelling — would miss.
    #[test]
    fn a_leg_declared_with_whitespace_is_keyed_under_the_bytes_the_runtime_will_route() {
        let declared = vec![" ETHUSDT".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &declared, |s| {
            // The venue is asked for whatever was declared; what matters here is the KEY.
            (s == " ETHUSDT").then(|| props(0.05))
        });
        assert!(
            grids.contains_key(" ETHUSDT"),
            "the row must be filed under the declared spelling, not a normalised one: {:?}",
            grids.keys().collect::<Vec<_>>()
        );
    }

    /// An all-whitespace leg is a declaration error, not an instrument — `trim` still DECIDES even
    /// though it no longer forms the key.
    #[test]
    fn an_all_whitespace_leg_is_dropped_rather_than_gridded() {
        let declared = vec!["   ".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &declared, |_| Some(props(0.05)));
        assert!(
            grids.is_empty(),
            "a blank leg must not become a row: {:?}",
            grids.keys().collect::<Vec<_>>()
        );
    }

    /// The mounted symbol is never re-gridded — it already IS the scalar limits.
    #[test]
    fn the_primary_symbol_is_skipped() {
        let declared = vec!["BTCUSDT".to_string()];
        let grids = declared_symbol_grids("BTCUSDT", &declared, |_| Some(props(0.05)));
        assert!(grids.is_empty());
    }
}
