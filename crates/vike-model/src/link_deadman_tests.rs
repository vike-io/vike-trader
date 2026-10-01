use super::*;
use crate::VENUES;

/// The per-venue completeness gate, exactly as `crate::venues`' contract requires: every roster
/// venue has a NAMED row, so adding a bridge crate fails here until its link-dead-man default
/// is classified.
#[test]
fn every_roster_venue_has_a_named_link_deadman_row() {
    let fallback = link_deadman_default("no-such-venue-anywhere");
    for venue in VENUES {
        assert_ne!(
            link_deadman_default(venue),
            fallback,
            "{venue} rides the unknown-venue fallback — add a NAMED row saying whether the \
                 connection-state dead-man defaults ON for it, even if the answer is the same"
        );
    }
}

/// **The verbatim matrix pin** (the `venue_caps`/`tif` discipline): the whole table as one
/// list of `(venue, is_on)` pairs, in roster order. A row that flips direction has to flip
/// here too, in a diff a reviewer reads as "this venue now cancels its book on a link death".
#[test]
fn the_link_deadman_matrix_is_pinned() {
    let matrix: Vec<(&str, bool)> =
        VENUES.iter().map(|v| (*v, link_deadman_default(v).is_on())).collect();
    assert_eq!(
        matrix,
        vec![
            ("binance", true),
            ("bybit", true),
            ("okx", true),
            ("deribit", false),
            ("oanda", false),
            ("ig", false),
            ("fxcm", false),
            ("dukascopy", false),
            ("polymarket", true),
            ("ibkr", false),
            ("ctrader", false),
            ("alpaca", false),
            ("aster", true),
            ("hyperliquid", false),
        ],
        "the link-dead-man default flipped for some venue — if that is intended, the row's \
             own reason must say what was OBSERVED to justify it"
    );
}

/// A row that is not armed must SAY why in terms an operator can act on. An empty or one-word
/// reason is how a silent default gets reintroduced wearing a variant name.
#[test]
fn every_row_reason_is_an_argument() {
    for venue in VENUES {
        match link_deadman_default(venue) {
            LinkDeadMan::Armed { emitter, scope } => {
                assert!(
                    emitter.contains(".rs") && emitter.contains('\''),
                    "{venue}'s Armed row must cite the emitter as `path`'s `symbol`: \
                         {emitter:?}"
                );
                assert!(scope.len() > 30, "{venue}'s Armed row states no scope: {scope:?}");
            }
            LinkDeadMan::SessionBounded { why } | LinkDeadMan::Inert { why } => assert!(
                why.len() > 30 && why.contains(' '),
                "{venue}'s off reason is not something an operator can act on: {why:?}"
            ),
        }
    }
}

/// The off/on split IS the `off_reason` split — every unarmed venue reports one, every armed
/// one reports none. The mount-time report reads exactly this pair.
#[test]
fn only_an_unarmed_venue_carries_a_reason() {
    for venue in VENUES {
        let row = link_deadman_default(venue);
        assert_eq!(
            row.is_on(),
            row.off_reason().is_none(),
            "{venue}: an armed row must carry no off-reason and an unarmed one must carry one"
        );
    }
}

/// ⚠ **An ARMED venue must have answered its OWN close-analogue**, not merely failed to be an
/// FX venue. "24/7" is a property of an exchange's hours; every venue still has SOMETHING that
/// looks like a close from a feed's point of view, and the row has to say what it is and how
/// it is disclosed. Polymarket's is a market RESOLVING (its scheduled CLOB maintenance is the
/// other one), and the sibling test below can never catch a missing answer here because an
/// armed venue is by construction not in its list.
#[test]
fn an_armed_venue_states_how_its_own_close_analogue_is_disclosed() {
    let LinkDeadMan::Armed { scope, .. } = link_deadman_default("polymarket") else {
        panic!("polymarket is expected to be an Armed row — the matrix pin above says so")
    };
    assert!(
        scope.contains("RESOLVES") && scope.contains("maintenance"),
        "the polymarket row must state BOTH of its close-analogues and how each is \
             disclosed — a resolution and a maintenance window: {scope:?}"
    );
}

/// ⚠ **The four CEX rows must cite the TICK PUMP, not the DOM lane alone** — because the tick
/// pump is the lane `vike-tradehub`'s live mount subscribes, and the DOM lane is the one it
/// does not. A row that names only `depth_main` is the state this table shipped in on
/// 2026-09-05: honest about the adapter, and describing an emitter the daemon holding real
/// orders could not hear. Asserted on the row's own text because that text is what an operator
/// is shown, and because dropping the pump's `disclose_link` would otherwise leave this table
/// green while the daemon silently stopped arming.
#[test]
fn the_cex_rows_cite_the_tick_pump_the_daemon_actually_subscribes() {
    for (venue, needle) in [
        ("binance", "family/depth.rs's md_main"),
        ("aster", "family/depth.rs's md_main"),
        ("bybit", "market_data.rs's spawn_bybit_market_data"),
        ("okx", "market_data.rs's spawn_okx_market_data"),
    ] {
        let LinkDeadMan::Armed { emitter, scope } = link_deadman_default(venue) else {
            panic!("{venue} is expected to be an Armed row — the matrix pin above says so")
        };
        assert!(
            emitter.contains(needle),
            "{venue}'s row must cite the tick-pump emitter ({needle}), not the DOM lane \
                 alone — that lane is the one vike-tradehub does NOT subscribe: {emitter:?}"
        );
        assert!(
            emitter.contains("depth_main"),
            "{venue}'s row must ALSO keep citing the DOM lane — vike-app subscribes that one, \
                 and a row naming one emitter of two describes half the roster of mounts: \
                 {emitter:?}"
        );
        assert!(
            scope.contains("tick pump"),
            "{venue}'s scope must name the tick pump, since that is the subscription a live \
                 daemon's arming actually rests on: {scope:?}"
        );
    }
}

/// ⚠ **The rule the whole table exists for**: no venue whose market has SESSIONS is armed.
/// Stated as its own test over the FX/equity set rather than left implicit in the matrix,
/// because arming one of these is precisely the defect that made the silence-observing switch
/// opt-in (`vike_config::Policy::deadman_timeout_ms`'s doc, and decision 0038).
#[test]
fn no_session_bounded_venue_is_armed() {
    for venue in ["oanda", "ig", "ctrader", "fxcm", "alpaca", "ibkr", "dukascopy"] {
        assert!(
            !link_deadman_default(venue).is_on(),
            "{venue}'s market CLOSES — arming a link dead-man there needs an observation that \
                 the close is disclosed as Stale rather than as a disconnect, written into the row"
        );
    }
}
