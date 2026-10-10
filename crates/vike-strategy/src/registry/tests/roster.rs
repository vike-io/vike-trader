//! The rosters and `LIVE_CAPABLE`: every name resolves, is `Send`, and carries one argued verdict.

use super::*;
use std::assert_matches;

#[test]
fn registry_lists_every_match_arm() {
    // Every name PORTABLE_STRATEGIES advertises must actually resolve with DEFAULT params —
    // keeps the const in sync with the match by construction, not by convention.
    for name in PORTABLE_STRATEGIES {
        assert!(resolve(name, &empty()).is_ok(), "{name} should resolve");
    }
}

#[test]
fn unknown_name_is_a_registry_error_naming_the_roster() {
    match resolve("nope", &empty()) {
        Err(e @ RegistryError::Unknown(_)) => {
            let msg = e.to_string();
            assert!(msg.contains("nope"), "names the typo: {msg}");
            assert!(msg.contains("buy_hold"), "names the roster: {msg}");
        }
        other => panic!("expected Unknown, got Ok={}", other.is_ok()),
    }
}

/// The registry's whole reason for existing: ONE resolver, TWO brokers. A generic function's
/// body is type-checked at its DEFINITION, so this compiling IS the proof that every arm holds
/// for every `B: HftBroker` — including `vike_core::LiveBroker`, which this crate cannot name.
#[test]
fn the_resolver_is_generic_over_every_hft_broker() {
    fn probe<B: HftBroker + 'static>() {
        let _ = strategy_by_name::<B>("buy_hold", &Value::Table(Default::default()));
    }
    probe::<RecordingBroker>();
}

/// The returned box must satisfy `vike_core::StrategyMount::strategy`'s `+ Send` bound — the
/// live core moves the strategy onto its own thread. Asserted here rather than trusted: a
/// future arm holding an `Rc` would compile everywhere else and fail only at the live mount.
#[test]
fn every_resolved_strategy_is_send() {
    fn assert_send<T: Send>(_t: &T) {}
    for name in PORTABLE_STRATEGIES {
        let s = resolve(name, &empty()).expect("resolves");
        assert_send(&s);
    }
}

#[test]
fn live_capable_table_is_exhaustive() {
    // Every roster name has exactly one verdict row, and no row names a strategy that is not on
    // the roster. Adding an arm without classifying it fails HERE, which is the point: an
    // unclassified strategy is one nobody decided could trade.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            LIVE_CAPABLE.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one LIVE_CAPABLE row"
        );
    }
    for (name, _) in LIVE_CAPABLE {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "LIVE_CAPABLE names {name}, which is not on the roster"
        );
    }
}

#[test]
fn every_not_live_row_carries_a_nonempty_reason() {
    // A `no` with no reason is an opinion; a `no` with a named missing input is a claim
    // somebody can check and later disprove.
    for (name, verdict) in LIVE_CAPABLE {
        if let Some(reason) = verdict.blocker() {
            assert!(reason.len() > 20, "{name}'s reason is too thin to act on: {reason:?}");
        }
    }
}

/// The PERMISSIVE arm's twin: a live row means MOUNTABLE BY THE ORDER-SIGNING DAEMON, so it needs a
/// real written argument just as a refusal does — 80 characters, matching
/// `crates/vike-tradehub/src/hot_reload_tests.rs`'s `every_hot_row_carries_a_written_reason`.
#[test]
fn every_live_row_carries_a_written_reason() {
    for (name, verdict) in LIVE_CAPABLE {
        if let Liveness::Live { why_safe } = verdict {
            assert!(
                why_safe.len() >= 80 && !why_safe.to_ascii_lowercase().contains("todo"),
                "{name} is declared live-mountable and needs a real written argument for why \
                     that is safe — what its inputs are on a LIVE mount and why its orders are \
                     correct there; got {why_safe:?}"
            );
        }
    }
}

/// Names carrying [`Liveness::LiveUnargued`] — mountable live with no written argument.
///
/// ⚠ It lives HERE, in the test module, because a module-level copy read by one test is dead code
/// in a plain `cargo clippy` lib build, and `-D warnings` then fails the gates.
///
/// ⚠ A RATCHET: it may shrink, never grow. A new strategy writes its `why_safe` or its `blocker`;
/// this only keeps an inherited gap visible instead of retiring it silently or inventing a
/// justification for it. `momentum` is here because its row never carried an argument: whether it
/// is safe live is a question for whoever knows; what is recorded is that the tree does not say.
const UNARGUED_LIVE: [&str; 1] = ["momentum"];

/// [`UNARGUED_LIVE`] is a RATCHET: it may shrink, never grow (see its doc).
#[test]
fn the_unargued_live_set_does_not_grow() {
    let found: Vec<&str> = LIVE_CAPABLE
        .iter()
        .filter(|(_, v)| matches!(v, Liveness::LiveUnargued))
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(
        found, UNARGUED_LIVE,
        "the unargued-live set changed. A row may LEAVE it — write that strategy's `why_safe` \
             (or its `blocker`) and delete its name from UNARGUED_LIVE, shrinking the array's \
             declared length. A row may not JOIN it: a new strategy carries its own argument."
    );
}

#[test]
fn simulator_only_and_portable_rosters_are_disjoint() {
    for (name, _) in SIMULATOR_ONLY {
        assert!(
            !PORTABLE_STRATEGIES.contains(name),
            "{name} is claimed by BOTH rosters — one of them is wrong"
        );
    }
}

#[test]
fn capability_distinguishes_the_four_answers() {
    assert_eq!(capability("spread_maker"), Capability::Live);
    assert_matches!(capability("funding_capture"), Capability::NotLive(_));
    assert_matches!(capability("rotation_top_k"), Capability::SimulatorOnly(_));
    assert_eq!(capability("nope"), Capability::Unknown);
}

/// The SCRIPT path's NAME is a real registry arm this crate cannot resolve — not a typo — and the
/// reason must ALSO point at the spelling that DOES mount a script live (`rhai = "<path>"`, since
/// decision 0024), or it reads as "scripts cannot go live".
///
/// ⚠ It must NOT cite the record itself: this string is EXPORTED (`templates.json`, rendered on
/// `vike.io/docs/trader/strategies/simulator-only`) and the public mirror publishes no `docs/`, so
/// a citation is a dead link on a public page. `crates/vike-docs/tests/docs_data_gate.rs`'s
/// `no_rendered_asset_cites_a_path_the_mirror_withholds` is the gate for the whole class.
#[test]
fn the_script_strategy_is_named_rather_than_called_a_typo() {
    match capability("rhai") {
        Capability::SimulatorOnly(why) => {
            assert!(why.contains("src"), "names the param the NAME arm needs: {why}");
            assert!(why.contains("vike-script"), "names what does not link here: {why}");
            assert!(why.contains("rhai = "), "points at the daemon's live path spelling: {why}");
            assert!(
                !why.contains("docs/"),
                "an EXPORTED string may not cite a path the public mirror withholds: {why}"
            );
        }
        other => panic!("rhai must not read as {other:?}"),
    }
}

#[test]
fn script_only_is_disjoint_from_both_rosters() {
    for (name, why) in SCRIPT_ONLY {
        assert!(!PORTABLE_STRATEGIES.contains(name), "{name} is on the portable roster");
        assert!(
            !SIMULATOR_ONLY.iter().any(|(n, _)| n == name),
            "{name} is claimed by SIMULATOR_ONLY too — one of the two rows is wrong"
        );
        assert!(why.len() > 20, "{name}'s reason is too thin to act on: {why:?}");
    }
}

/// The `NotLive` rows are the ones that would SILENTLY never trade (or, for
/// `trailing_scalper`, trade HALF of what was backtested). Pin them by name: a future PR that
/// wires the missing input flips the row deliberately and updates this list, rather than a
/// rename quietly making a footgun mountable.
#[test]
fn the_not_live_set_is_exactly_the_known_gaps() {
    let not_live: Vec<&str> =
        LIVE_CAPABLE.iter().filter(|(_, v)| v.blocker().is_some()).map(|(n, _)| *n).collect();
    assert_eq!(
        not_live,
        vec!["trailing_scalper", "funding_carry", "funding_capture", "pairs_zscore"]
    );
}
