use super::*;

/// Every registry venue paired with its declared const — the completeness test below asserts
    /// this covers `crate::venues::VENUES` exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, AmendSemantics)] = &[
        ("binance", BINANCE),
        ("bybit", BYBIT),
        ("okx", OKX),
        ("deribit", DERIBIT),
        ("oanda", OANDA),
        ("ig", IG),
        ("fxcm", FXCM),
        ("dukascopy", DUKASCOPY),
        ("polymarket", POLYMARKET),
        ("ibkr", IBKR),
        ("ctrader", CTRADER),
        ("alpaca", ALPACA),
        ("aster", ASTER),
        ("hyperliquid", HYPERLIQUID),
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): declared-row completeness
    ];

/// The full matrix, pinned verbatim (the `venue_margin_support` idiom). Any drift in a row is
/// loud here — and a row moving INTO `InPlaceTotal` is the only kind of drift that can change
/// an admit/deny verdict anywhere.
#[test]
fn amend_matrix_is_pinned() {
    use AmendSemantics::{CancelReplace, InPlaceTotal, Unknown, Unsupported};
    #[rustfmt::skip]
        let rows: &[(&str, AmendSemantics)] = &[
            // in-place amend: the qty is the order's new TOTAL, executions stay attached
            ("binance",     InPlaceTotal),
            ("okx",         InPlaceTotal),
            ("bybit",       InPlaceTotal),
            // native cancel-replace (proven from the adapter's own new-oid swap)
            ("hyperliquid", CancelReplace),
            // amendable, convention NOT established — held conservative
            ("aster",       Unknown),
            ("ctrader",     Unknown),
            ("ibkr",        Unknown),
            // no native amend at all (the ExecutionClient default no-op)
            ("deribit",     Unsupported),
            ("oanda",       Unsupported),
            ("ig",          Unsupported),
            ("fxcm",        Unsupported),
            ("dukascopy",   Unsupported),
            ("polymarket",  Unsupported),
            ("alpaca",      Unsupported),
            // vike:new-venue:row ("{venue}",  Unsupported), // TODO(new-venue: {venue}): move with the const above
        ];
    for (venue, want) in rows {
        assert_eq!(amend_semantics(venue), *want, "{venue}");
    }
    assert_eq!(rows.len(), MATRIX.len(), "matrix test row count == registry size");
}

/// Completeness vs the canonical roster: every `crate::venues::VENUES` entry has a NAMED
/// declared row, and the registry serves exactly it. Adding a venue fails here until its row
/// exists — including when the honest answer is [`AmendSemantics::Unknown`], which must be
/// DECLARED rather than inherited from the fallback arm.
#[test]
fn every_roster_venue_has_a_declared_row() {
    assert_eq!(MATRIX.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
    for &v in crate::venues::VENUES {
        let (_, want) = MATRIX
            .iter()
            .find(|(rv, _)| *rv == v)
            .unwrap_or_else(|| panic!("no AmendSemantics row declared for roster venue {v}"));
        assert_eq!(amend_semantics(v), *want, "{v}: registry must serve its declared row");
    }
}

/// THE SAFETY PROPERTY, stated as a test rather than as prose: exactly one variant subtracts
/// anything, so a row misclassified among the other three cannot move a single verdict. This is
/// what makes an `Unknown` row cost nothing and an `InPlaceTotal` row the only one needing
/// evidence.
#[test]
fn only_in_place_total_subtracts_anything() {
    assert_eq!(AmendSemantics::InPlaceTotal.already_in_position(4.0), 4.0);
    for s in [
        AmendSemantics::CancelReplace,
        AmendSemantics::InPlaceRemaining,
        AmendSemantics::Unsupported,
        AmendSemantics::Unknown,
    ] {
        assert_eq!(s.already_in_position(4.0), 0.0, "{s:?} must keep today's arithmetic");
    }
}

/// [`AmendSemantics::InPlaceRemaining`] is a CLIENT-declared row, never a venue one: the paper
/// exchange is the only thing in this workspace that implements it. A venue string that started
/// resolving to it would mean somebody keyed a client fact onto the venue axis.
#[test]
fn the_registry_never_serves_the_client_only_row() {
    for &v in crate::venues::VENUES {
        assert_ne!(
            amend_semantics(v),
            AmendSemantics::InPlaceRemaining,
            "{v}: InPlaceRemaining is declared by the CLIENT, not by this table"
        );
    }
    for v in ["sim", "paper", ""] {
        assert_ne!(amend_semantics(v), AmendSemantics::InPlaceRemaining, "{v}");
    }
}

/// A garbage `filled_qty` yields `0.0` — the conservative answer — rather than propagating.
/// A NaN subtracted from an order qty makes `projected > cap` FALSE, which would vacate the
/// ceiling this value exists to tighten.
#[test]
fn a_non_finite_or_negative_filled_qty_subtracts_nothing() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
        assert_eq!(
            AmendSemantics::InPlaceTotal.already_in_position(bad),
            0.0,
            "filled_qty {bad} must subtract nothing"
        );
    }
}

/// An unrecognised venue is [`AmendSemantics::Unknown`], i.e. byte-identical to the behaviour
/// before this table existed. ⚠ This is NOT the paper-mount story: a paper mount carries the
/// REAL venue string (`vike_mount::make_engine`), so it never reaches this arm — see
/// `the_registry_never_serves_the_client_only_row` and the module doc.
#[test]
fn an_unknown_venue_is_conservative() {
    for v in ["sim", "paper", "nasdaq", ""] {
        assert_eq!(amend_semantics(v), AmendSemantics::Unknown, "{v}");
        assert_eq!(amend_semantics(v).already_in_position(9.0), 0.0, "{v}");
    }
    assert_eq!(AmendSemantics::default(), AmendSemantics::Unknown);
}

/// The in-place set is exactly the three venues whose vendor doc speaks to the partially-filled
/// case. Spelled as an explicit both-directions assertion so that adding a fourth venue to the
/// anti-conservative arm cannot be a one-line diff nobody notices.
#[test]
fn the_in_place_set_is_exactly_the_evidence_backed_three() {
    let in_place: Vec<&str> = crate::venues::VENUES
        .iter()
        .copied()
        .filter(|v| amend_semantics(v) == AmendSemantics::InPlaceTotal)
        .collect();
    assert_eq!(in_place, vec!["binance", "bybit", "okx"]);
}

/// ⚠ **THE SECOND TABLE ON THIS AXIS.** `crate::venues::venue_caps`'s `supports_modify` already declares
/// per venue whether an amend exists at all, and nothing tied the two together — the exact shape
/// this tree has been burned by (`caps.rs` drifted three times while every declaration-pinning
/// test stayed green). So they are cross-checked, BOTH directions:
///
/// * `supports_modify == false` ⇒ [`AmendSemantics::Unsupported`] — there is no convention to
///   declare when no amend leaves the process;
/// * `supports_modify == true` ⇒ anything BUT `Unsupported` — a venue that amends has a
///   convention, even when the honest value for it is `Unknown`.
///
/// [`CAPS_DISAGREEMENTS`] is the declared-exception list, and it exists because the two tables
/// are keyed differently: `caps_for` resolves a per-BACKEND row this table has no axis for.
#[test]
fn amend_semantics_agrees_with_venue_caps_supports_modify() {
    for &v in crate::venues::VENUES {
        let sem = amend_semantics(v);
        let modifiable = crate::venues::venue_caps::caps_for(v).supports_modify;
        if CAPS_DISAGREEMENTS.iter().any(|(rv, _)| *rv == v) {
            assert!(
                !modifiable && sem != AmendSemantics::Unsupported,
                "{v}: declared as a caps disagreement, but the tables now agree — delete the \
                     CAPS_DISAGREEMENTS row"
            );
            continue;
        }
        if modifiable {
            assert_ne!(
                sem,
                AmendSemantics::Unsupported,
                "{v}: venue_caps says it amends, so its amend convention cannot be Unsupported"
            );
        } else {
            assert_eq!(
                sem,
                AmendSemantics::Unsupported,
                "{v}: venue_caps says it has no amend, so there is no convention to declare"
            );
        }
    }
}

/// [`CAPS_DISAGREEMENTS`] is well-formed: every row names a ROSTER venue (a typo'd id would
/// silently exempt nothing while looking like a granted exception), carries a non-empty reason
/// — the string a consumer RENDERS, so an empty one ships a blank explanation to a reader — and
/// names each venue once, so `amend_caps_note`'s first-match lookup cannot hide a second row.
#[test]
fn every_declared_disagreement_names_a_roster_venue_once_with_a_reason() {
    for (v, why) in CAPS_DISAGREEMENTS {
        assert!(crate::venues::VENUES.contains(v), "{v}: not a roster venue");
        assert!(!why.trim().is_empty(), "{v}: a disagreement row must carry its reason");
        assert_eq!(
            CAPS_DISAGREEMENTS.iter().filter(|(rv, _)| rv == v).count(),
            1,
            "{v}: declared twice — amend_caps_note would serve only the first"
        );
    }
}

/// [`amend_caps_note`] is exactly [`CAPS_DISAGREEMENTS`] as a lookup — `Some(reason)` for a
/// declared row and `None` for every other venue, roster or not. This is the accessor the
/// docs-data export renders through, so "the tables agree" and "nobody wrote the reason down"
/// must not be the same answer.
#[test]
fn amend_caps_note_serves_exactly_the_declared_rows() {
    for &v in crate::venues::VENUES {
        let declared = CAPS_DISAGREEMENTS.iter().find(|(rv, _)| *rv == v).map(|(_, why)| *why);
        assert_eq!(amend_caps_note(v), declared, "{v}");
    }
    for v in ["sim", "paper", "nasdaq", ""] {
        assert_eq!(amend_caps_note(v), None, "{v}: not a roster venue, so no declared note");
    }
}

#[test]
fn amend_semantics_serde_round_trips() {
    for s in [
        AmendSemantics::InPlaceTotal,
        AmendSemantics::CancelReplace,
        AmendSemantics::InPlaceRemaining,
        AmendSemantics::Unsupported,
        AmendSemantics::Unknown,
    ] {
        let json = serde_json::to_string(&s).expect("serialize");
        let back: AmendSemantics = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
    }
    assert_eq!(serde_json::to_string(&AmendSemantics::InPlaceTotal).unwrap(), "\"InPlaceTotal\"");
}
