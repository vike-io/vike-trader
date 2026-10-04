use super::TifOutcome::{Coerced, Ignored, Mapped, NotEmitted};
use super::{TifOutcome, venue_tif};
use crate::orders::order::TimeInForce::{self, Day, Fok, Gtc, Gtd, Ioc};

const ALL_TIFS: [TimeInForce; 5] = [Gtc, Ioc, Fok, Gtd, Day];

/// The CURRENT matrix, verbatim — any drift in a venue row is loud here. THE CONTRADICTION,
/// pinned on purpose: polymarket's `Ioc -> FOK` row coerces UP (partial fills forbidden)
/// while hyperliquid's `Fok -> Ioc` row coerces DOWN (partial fills allowed) — the same
/// Ioc/Fok pair folded in OPPOSITE directions. Step 2 resolves that per venue behind demo
/// smokes; until then this test IS the documentation of record.
#[test]
fn current_tif_matrix_is_pinned() {
    #[rustfmt::skip]
        let rows: &[(&str, TimeInForce, TifOutcome)] = &[
            // polymarket — maps/coerces every TIF; Ioc coerced UP to FOK (contradiction, side A)
            ("polymarket", Gtc, Mapped("GTC")),
            ("polymarket", Ioc, Coerced { from: Ioc, to: Fok, wire: "FOK" }),
            ("polymarket", Fok, Mapped("FOK")),
            ("polymarket", Gtd, Mapped("GTD")), // + a real wire `expiration`; not yet live-proven
            ("polymarket", Day, Coerced { from: Day, to: Gtc, wire: "GTC" }),
            // hyperliquid — Fok coerced DOWN to Ioc (contradiction, side B — same pair!)
            ("hyperliquid", Gtc, Mapped("Gtc")),
            ("hyperliquid", Ioc, Mapped("Ioc")),
            ("hyperliquid", Fok, Coerced { from: Fok, to: Ioc, wire: "Ioc" }),
            ("hyperliquid", Gtd, Coerced { from: Gtd, to: Gtc, wire: "Gtc" }),
            ("hyperliquid", Day, Coerced { from: Day, to: Gtc, wire: "Gtc" }),
            // binance SPOT lane — FLIPPED (smoke-proven): GTC/IOC/FOK map 1:1; GTD/Day loud-deny
            ("binance", Gtc, Mapped("GTC")),
            ("binance", Ioc, Mapped("IOC")),
            ("binance", Fok, Mapped("FOK")),
            ("binance", Gtd, TifOutcome::Unsupported),
            ("binance", Day, TifOutcome::Unsupported),
            // binance PERP lane (the "binance-perp" LANE sub-key — not a roster venue): native
            // fapi GTD is Mapped (goodTillDate rides from `gtd_expiry`, validity gated
            // venue-side); Day stays loud-denied. Smoke-proven (`binance_tif_smoke.rs`).
            ("binance-perp", Gtc, Mapped("GTC")),
            ("binance-perp", Ioc, Mapped("IOC")),
            ("binance-perp", Fok, Mapped("FOK")),
            ("binance-perp", Gtd, Mapped("GTD")),
            ("binance-perp", Day, TifOutcome::Unsupported),
            // the ignores — request TIF never read; LIMIT orders silently rest GTC
            ("aster", Gtc, Ignored { wire: "GTC" }), // shares binance's family builder, unflipped
            ("aster", Ioc, Ignored { wire: "GTC" }),
            ("aster", Fok, Ignored { wire: "GTC" }),
            ("aster", Gtd, Ignored { wire: "GTC" }),
            ("aster", Day, Ignored { wire: "GTC" }),
            // bybit — FLIPPED (smoke-proven): GTC/IOC/FOK map 1:1; GTD/Day loud-deny
            ("bybit", Gtc, Mapped("GTC")),
            ("bybit", Ioc, Mapped("IOC")),
            ("bybit", Fok, Mapped("FOK")),
            ("bybit", Gtd, TifOutcome::Unsupported),
            ("bybit", Day, TifOutcome::Unsupported),
            ("ig", Gtc, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Ioc, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Fok, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Gtd, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Day, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            // okx — FLIPPED (smoke-proven): TIF rides ordType (no separate field) — Gtc stays
            // NotEmitted (ordType "limit", byte-identical); Ioc/Fok map to the ioc/fok
            // ordTypes; Gtd/Day loud-deny
            ("okx", Gtc, NotEmitted),
            ("okx", Ioc, Mapped("ioc")),
            ("okx", Fok, Mapped("fok")),
            ("okx", Gtd, TifOutcome::Unsupported),
            ("okx", Day, TifOutcome::Unsupported),
            // deribit — FLIPPED (smoke-proven): Gtc stays NotEmitted (venue default,
            // byte-identical); Ioc/Fok/Day map to Deribit's TIF vocabulary; Gtd loud-deny
            ("deribit", Gtc, NotEmitted),
            ("deribit", Ioc, Mapped("immediate_or_cancel")),
            ("deribit", Fok, Mapped("fill_or_kill")),
            ("deribit", Gtd, TifOutcome::Unsupported),
            ("deribit", Day, Mapped("good_til_day")),
            // the genuine mappers — rows consumed by their venue fns (oanda: working arm only)
            ("oanda", Gtc, Mapped("GTC")),
            ("oanda", Ioc, Mapped("IOC")),
            ("oanda", Fok, Mapped("FOK")),
            ("oanda", Gtd, Mapped("GTD")),
            ("oanda", Day, Mapped("GFD")),
            ("alpaca", Gtc, Mapped("gtc")),
            ("alpaca", Ioc, Mapped("ioc")),
            ("alpaca", Fok, Mapped("fok")),
            ("alpaca", Gtd, Coerced { from: Gtd, to: Gtc, wire: "gtc" }),
            ("alpaca", Day, Mapped("day")),
            ("ibkr", Gtc, Mapped("GTC")),
            ("ibkr", Ioc, Mapped("IOC")),
            ("ibkr", Fok, Mapped("FOK")),
            ("ibkr", Gtd, Mapped("GTD")), // stub: no backend wires the good-till date (gtd_expiry)
            ("ibkr", Day, Mapped("DAY")),
        ];
    for (venue, tif, want) in rows {
        assert_eq!(venue_tif(venue, *tif), *want, "{venue}/{tif:?}");
    }
    // completeness: every table venue (incl. the binance-perp lane sub-key) is covered for
    // all five TIFs (5 rows each)
    let venues = [
        "polymarket",
        "hyperliquid",
        "binance",
        "binance-perp",
        "aster",
        "bybit",
        "ig",
        "okx",
        "deribit",
        "oanda",
        "alpaca",
        "ibkr",
    ];
    for v in venues {
        assert_eq!(rows.iter().filter(|(rv, _, _)| rv == &v).count(), ALL_TIFS.len(), "{v}");
    }
    assert_eq!(rows.len(), venues.len() * ALL_TIFS.len());
}

/// The Ioc/Fok contradiction, spelled out as its own gate so it cannot be "fixed" silently:
/// resolving EITHER side is a step-2 wire change that must flip this test deliberately.
#[test]
fn polymarket_and_hyperliquid_coerce_the_same_pair_in_opposite_directions() {
    assert_eq!(venue_tif("polymarket", Ioc), Coerced { from: Ioc, to: Fok, wire: "FOK" });
    assert_eq!(venue_tif("hyperliquid", Fok), Coerced { from: Fok, to: Ioc, wire: "Ioc" });
}

/// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue is
/// classified exactly once — either it has an explicit `venue_tif` arm (pinned verbatim in
/// `current_tif_matrix_is_pinned` above) or it is in the DECLARED tif-less set (its protocol
/// carries no TIF, so it deliberately rides the `NotEmitted` fallthrough). Adding a venue to
/// the roster fails here until it is classified one way or the other.
#[test]
fn every_roster_venue_is_classified() {
    // ROSTER venues with an explicit arm in `venue_tif` (the pinned matrix additionally
    // covers the non-roster "binance-perp" LANE sub-key — lane keys are not classified
    // here because they are not roster ids)
    const MAPPED: &[&str] = &[
        "polymarket",
        "hyperliquid",
        "binance",
        "aster",
        "bybit",
        "ig",
        "okx",
        "deribit",
        "oanda",
        "alpaca",
        "ibkr",
    ];
    // venues whose protocol carries no TIF — the declared `NotEmitted` fallthrough set
    #[rustfmt::skip]
        const TIFLESS: &[&str] = &[
            "ctrader", "fxcm", "dukascopy",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): if the protocol DOES carry a TIF, delete this and add a
            // vike:new-venue:row // `venue_tif` arm + five `current_tif_matrix_is_pinned` rows instead. Leaving it here
            // vike:new-venue:row // declares `NotEmitted` for all five TIFs, which is what `venue_caps`' scaffolded
            // vike:new-venue:row // `supported_tifs`/`accepted_tifs` of `&[Gtc]` cross-pins against.
        ];
    assert_eq!(
        MAPPED.len() + TIFLESS.len(),
        crate::venues::VENUES.len(),
        "every roster venue classified exactly once"
    );
    for &v in crate::venues::VENUES {
        let mapped = MAPPED.contains(&v);
        let tifless = TIFLESS.contains(&v);
        assert!(
            mapped ^ tifless,
            "roster venue {v} must be classified exactly once (explicit venue_tif arm or \
                 declared tif-less)"
        );
        if tifless {
            for t in ALL_TIFS {
                assert_eq!(venue_tif(v, t), NotEmitted, "{v}/{t:?} declared tif-less");
            }
        }
    }
}

/// CROSS-PIN with the sibling `venue_caps` registry (w2-task-5): the TWO TIF authorities
/// — this table (what each adapter DOES per TIF) and each venue's `VenueCaps` row (what a
/// caller may offer/submit) — are derived views of the same reality, so they must never
/// drift. Per roster venue, for every TIF:
///
/// - `supported_tifs` (the HONORED set) == `Mapped` rows plus the venue-default `Gtc` of an
///   `Ignored`/`NotEmitted` row (a GTC request matches what actually rests). Binance is the
///   lane INTERSECTION (spot ∧ perp — the single-keyed registry must not offer perp-only
///   GTD); polymarket's `Gtd` is the ONE declared exception (Mapped here, and it now DOES
///   emit a real wire `expiration`, but that path has never been exercised against the live
///   CLOB — so the caps row still refuses to ADVERTISE it on a mainnet money venue).
/// - `accepted_tifs` (the core-preflight ADMIT set) == `Mapped` ∪ `Coerced` rows plus the
///   same default-Gtc rule; binance is the lane UNION (perp GTD must not be core-refused).
///   No stub exception: a stub TIF is still ACCEPTED today, and the preflight refuses
///   nothing a venue accepts.
#[test]
fn venue_caps_cross_pin_the_tif_table() {
    let honored = |venue: &str, t: TimeInForce| match venue_tif(venue, t) {
        TifOutcome::Mapped(_) => true,
        TifOutcome::Ignored { .. } | TifOutcome::NotEmitted => t == Gtc,
        TifOutcome::Coerced { .. } | TifOutcome::Unsupported => false,
    };
    let accepted = |venue: &str, t: TimeInForce| match venue_tif(venue, t) {
        TifOutcome::Mapped(_) | TifOutcome::Coerced { .. } => true,
        TifOutcome::Ignored { .. } | TifOutcome::NotEmitted => t == Gtc,
        TifOutcome::Unsupported => false,
    };
    for &v in crate::venues::VENUES {
        let caps = crate::venues::venue_caps::caps_for(v);
        for t in ALL_TIFS {
            let want_supported = if v == "binance" {
                honored("binance", t) && honored("binance-perp", t)
            } else {
                honored(v, t) && !(v == "polymarket" && t == Gtd) // declared: not live-proven
            };
            assert_eq!(
                caps.supported_tifs.contains(&t),
                want_supported,
                "{v}/{t:?}: supported_tifs drifted from the venue_tif table"
            );
            let want_accepted = if v == "binance" {
                accepted("binance", t) || accepted("binance-perp", t)
            } else {
                accepted(v, t)
            };
            assert_eq!(
                caps.accepted_tifs.contains(&t),
                want_accepted,
                "{v}/{t:?}: accepted_tifs drifted from the venue_tif table"
            );
        }
    }
}

/// TIF-less protocols and unknown venues fall through to `NotEmitted`.
#[test]
fn tifless_and_unknown_venues_are_not_emitted() {
    for v in ["ctrader", "fxcm", "dukascopy", "no-such-venue"] {
        for t in ALL_TIFS {
            assert_eq!(venue_tif(v, t), NotEmitted, "{v}/{t:?}");
        }
    }
}

/// `wire()` — Some for Mapped/Coerced (the request TIF reaches the wire), None otherwise.
#[test]
fn wire_projection() {
    assert_eq!(Mapped("GTC").wire(), Some("GTC"));
    assert_eq!(Coerced { from: Ioc, to: Fok, wire: "FOK" }.wire(), Some("FOK"));
    assert_eq!(Ignored { wire: "GTC" }.wire(), None);
    assert_eq!(NotEmitted.wire(), None);
    assert_eq!(TifOutcome::Unsupported.wire(), None);
}

/// The submit-side deny gate fires ONLY on `(limit, Unsupported-row)` pairs: every
/// non-Unsupported row passes through (`None`), and non-limit order types never deny (their
/// path carries no TIF axis). The per-venue `Some` cases are pinned in each flipped venue's
/// own tests next to its adapter.
#[test]
fn deny_gate_passes_supported_rows_and_non_limit_paths() {
    let req =
        |venue: &str, order_type: &str, tif: TimeInForce| crate::orders::order::OrderRequest {
            client_order_id: "c-deny".to_string(),
            venue: venue.to_string(),
            symbol: "X".to_string(),
            side: 1,
            qty: 1.0,
            order_type: order_type.to_string(),
            price: Some(1.0),
            time_in_force: tif,
            ..Default::default()
        };
    for v in ["polymarket", "hyperliquid", "oanda", "alpaca", "ibkr", "ig", "no-such-venue"] {
        for t in ALL_TIFS {
            assert!(
                super::deny_unsupported_tif(v, &req(v, "limit", t)).is_none(),
                "{v}/{t:?} has no Unsupported row — must pass"
            );
        }
    }
    // non-limit paths never deny, even where the venue's limit row is Unsupported
    for v in ["binance", "bybit", "deribit", "okx", "binance-perp"] {
        for ot in ["market", "stop"] {
            assert!(super::deny_unsupported_tif(v, &req(v, ot, Gtd)).is_none(), "{v}/{ot}");
        }
    }
    // the binance lane split: the perp lane's Gtd row is Mapped so the Unsupported gate
    // passes it (date VALIDITY is gated venue-side, `vike_binance::perp::deny_invalid_gtd`);
    // its Day row — and the spot lane's Gtd — still deny.
    assert!(super::deny_unsupported_tif("binance-perp", &req("binance", "limit", Gtd)).is_none());
    assert!(super::deny_unsupported_tif("binance-perp", &req("binance", "limit", Day)).is_some());
    assert!(super::deny_unsupported_tif("binance", &req("binance", "limit", Gtd)).is_some());
}
