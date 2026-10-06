//! `--produced-by`: the assertion's own rules, on both routes.

use super::*;

// ---- `--produced-by`: the assertion's own rules, on BOTH routes ----

/// ⚠ **A BLANK `--produced-by` is refused on BOTH routes, in BOTH spellings, with the reason
/// that is actually true of it.**
///
/// It was already refused — as a row in the SELECTOR loop, whose message argues about the
/// store's grouped-series `symbol=` sentinel and tells the operator to *omit the flag to
/// wildcard the dimension*. Both halves are false here: this is not a dimension, and omitting
/// it turns the assertion OFF, which on a sweep is refused outright. So the only guard between
/// a blank prefix and the wire was an untested line that did not know what it was guarding —
/// and lifting `--produced-by` out of that loop, which is what a `rm` refactor does first, was
/// enough to drop it silently.
///
/// The needle is the CONSEQUENCE ("matches every key"), not the flag name, because that is
/// what distinguishes the right refusal from the wrong one that was already passing.
#[test]
fn a_blank_produced_by_is_refused_on_both_routes_with_the_real_reason() {
    for route in [vec![], vec!["--addr", "1.2.3.4:9"]] {
        for blank in ["", "   "] {
            let mut argv =
                vec!["hist", "rm", "--kind", "bar", "--venue", "binance", "--produced-by", blank];
            argv.extend(route.iter().copied());
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains("--produced-by"), "{argv:?}: {err}");
            assert!(
                err.contains("matches every key"),
                "the refusal must give the reason that is TRUE of a provenance prefix, not \
                     the grouped-series sentinel argument it inherited: {err}"
            );
            assert!(
                !err.contains("wildcard the dimension"),
                "…and must not tell the operator to omit the flag, which on a sweep is the \
                     one thing that is refused: {err}"
            );
        }
    }
}

/// ⚠ **A PRODUCER PATH is refused on the REMOTE route and accepted on the LOCAL one, because
/// only one of the two resolves it.**
///
/// `vike_data::store::store_kind::resolve_produced_by` has one caller in the tree —
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`, i.e. the local route.
/// `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` calls nothing of the kind, so the
/// same line under `--addr` asserted the PATH as a literal prefix, matched no commit key, and
/// told the operator their store had foreign provenance — a serious-looking finding about the
/// data that was really an unresolved flag.
#[test]
fn a_producer_path_is_refused_on_the_remote_route_and_forwarded_on_the_local_one() {
    let path = "crates/vike-data/src/demo.rs";
    let base = ["hist", "rm", "--kind", "bar", "--venue", "demo", "--produced-by", path];

    let err = parse_of(&[&base[..], &["--addr", "1.2.3.4:9"][..]].concat()).unwrap_err();
    assert!(err.contains("PRODUCER PATH"), "{err}");
    assert!(err.contains("--store"), "…and it names the route that DOES resolve one: {err}");

    // The local route forwards it verbatim for the engine to resolve — the behaviour this
    // whole asymmetry exists to preserve rather than to flatten.
    let local = parse_of(&base).expect("a producer path is legitimate on the local route");
    let rm = local.rm.as_ref().expect("rm args");
    assert_eq!(rm.produced_by.as_deref(), Some(path));
    assert!(
        rm_engine_argv(&local, rm).iter().any(|a| a == path),
        "the path reaches the engine unresolved: {:?}",
        rm_engine_argv(&local, rm)
    );
}

/// A LITERAL prefix is untouched on both routes — the refusals above are narrow, and a prefix
/// carrying no `/` is the ordinary spelling both sides understand.
#[test]
fn a_literal_prefix_reaches_both_routes_unchanged() {
    for route in [vec![], vec!["--addr", "1.2.3.4:9"]] {
        let mut argv =
            vec!["hist", "rm", "--kind", "bar", "--venue", "demo", "--produced-by", "panel_bars:"];
        argv.extend(route.iter().copied());
        let a = parse_of(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        assert_eq!(a.rm.as_ref().and_then(|r| r.produced_by.as_deref()), Some("panel_bars:"));
    }
}

/// A glob in `--produced-by` is refused with the reason that fits a PREFIX: it is matched with
/// `starts_with`, so `pmxt:*` is a literal that matches nothing rather than a pattern.
#[test]
fn a_glob_in_the_prefix_is_refused_as_a_prefix_rather_than_as_a_dimension() {
    let err = parse_of(&["hist", "rm", "--kind", "bar", "--venue", "d", "--produced-by", "pmxt:*"])
        .unwrap_err();
    assert!(err.contains("--produced-by") && err.contains("starts_with"), "{err}");
}
