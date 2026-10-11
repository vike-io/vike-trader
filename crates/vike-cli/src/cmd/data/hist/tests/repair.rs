//! `repair`'s grammar: exactly one named series, on the engine route alone.

use super::*;

// ---- `repair`: the grammar ----

/// ⚠ **The one shape rule that differs from `rm`'s, and the reason this verb exists.** An
/// omitted dimension WILDCARDS on `rm`; here it names nothing, because a repair rebuilds
/// exactly one series' index. The refusal must carry the argument (a per-series lock hold, a
/// per-series verdict) rather than only the rule, because "why can't I repair a whole venue"
/// is the first question an operator asks.
#[test]
fn repair_refuses_a_wildcard_and_argues_for_one_series() {
    let err = parse_of(&["hist", "repair", "--kind", "bar", "--venue", "binance"]).unwrap_err();
    assert!(err.contains("--symbol S or --group G"), "{err}");
    assert!(err.contains("per-series"), "the refusal must carry the argument: {err}");
    assert!(err.contains("run this verb several times"), "…and the way out: {err}");
}

/// Both spellings of a named series parse, and the `symbol`/`group` alternative is refused as a
/// PAIR exactly as `rm` refuses it — one store, one rule.
#[test]
fn repair_takes_either_spelling_of_a_named_series() {
    let a = parse_of(&[
        "hist",
        "repair",
        "--kind",
        "bar",
        "--venue",
        "b",
        "--symbol",
        "S",
        "--interval",
        "1m",
    ])
    .unwrap();
    let r = a.repair.as_ref().expect("a RepairArgs");
    assert_eq!(r.kind, "bar");
    assert_eq!(r.symbol.as_deref(), Some("S"));
    assert_eq!(r.interval.as_deref(), Some("1m"));
    assert!(r.group.is_none());
    // ...and the filter it was PARSED into is left empty, so no listing code can read a
    // selector out of it.
    assert_eq!(a.filter, Filter::default());

    let g = parse_of(&[
        "hist",
        "repair",
        "--kind",
        "book",
        "--venue",
        "polymarket",
        "--group",
        "btc-5m",
    ])
    .unwrap();
    assert_eq!(g.repair.as_ref().unwrap().group.as_deref(), Some("btc-5m"));

    for (argv, needle) in [
        (
            vec!["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S", "--group", "G"],
            "ALTERNATIVES",
        ),
        (
            vec![
                "hist",
                "repair",
                "--kind",
                "b",
                "--venue",
                "v",
                "--group",
                "G",
                "--interval",
                "1m",
            ],
            "no `interval=` segment",
        ),
        (vec!["hist", "repair", "--venue", "v", "--symbol", "S"], "repair needs --kind"),
        (vec!["hist", "repair", "--kind", "b", "--symbol", "S"], "repair needs --venue"),
    ] {
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must say {needle:?}: {err}");
    }
}

/// ⚠ **`--addr` is REFUSED, not ignored**, and the refusal carries the reason that would
/// otherwise be re-litigated: a datahub can only name series it ENUMERATED, and the series this
/// repairs is by definition in no enumeration.
#[test]
fn repair_refuses_the_remote_route_and_says_why() {
    let err = parse_of(&[
        "hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S", "--addr", "h:1",
    ])
    .unwrap_err();
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("ENUMERATED"), "the load-bearing half of the argument: {err}");
    assert!(err.contains("--store"), "…and the route that does work: {err}");
}

/// `--produced-by` guards a DELETE and guards nothing here, so it is refused by name rather
/// than accepted as a flag that looks like a safety rail and is not one.
#[test]
fn repair_refuses_produced_by_because_it_removes_no_row() {
    let err = parse_of(&[
        "hist",
        "repair",
        "--kind",
        "b",
        "--venue",
        "v",
        "--symbol",
        "S",
        "--produced-by",
        "panel_bars:",
    ])
    .unwrap_err();
    assert!(err.contains("--produced-by"), "{err}");
    assert!(err.contains("only `rm` deletes"), "{err}");
    assert!(err.contains("removes no row"), "{err}");
}

/// The read half's browse aids and the fetch half's window are refused here for the reasons
/// they do not apply, never as "unknown option" — this module's standing rule.
#[test]
fn repair_refuses_the_other_halves_flags_with_their_reasons() {
    let base = ["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "S"];
    for (extra, needle) in [
        (vec!["--name", "BTC"], "may be one no listing can show you"),
        (vec!["--days", "7"], "no time range to rebuild"),
        (vec!["--from", "0"], "no time range to rebuild"),
    ] {
        let argv: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must say {needle:?}: {err}");
    }
    let err = parse_of(&[
        "hist",
        "repair",
        "--kind",
        "b",
        "--venue",
        "v",
        "--symbol",
        "S",
        "binance:BTCUSDT:1h",
    ])
    .unwrap_err();
    assert!(err.contains("no VENUE:SYMBOL:INTERVAL spec"), "{err}");
}

/// An EMPTY or GLOBBED dimension is refused — and with `repair`'s own reason, not `rm`'s: there
/// is no wildcard here to fall back to.
#[test]
fn repair_refuses_a_blank_or_globbed_dimension() {
    let err =
        parse_of(&["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", ""]).unwrap_err();
    assert!(err.contains("EMPTY value") && err.contains("no wildcard to fall back to"), "{err}");
    let err = parse_of(&["hist", "repair", "--kind", "b", "--venue", "v", "--symbol", "BTC*"])
        .unwrap_err();
    assert!(err.contains("glob character") && err.contains("ONE series exactly"), "{err}");
}

/// ⚠ **THE REHEARSAL DEFAULT, in the argv the child actually receives.** A line with neither
/// flag is forwarded as `--dry-run`, so the child is TOLD what this side decided rather than
/// relying on a default spelled in two places; `--dry-run` WINS over `--yes`; and only
/// `--yes` alone produces a write.
#[test]
fn the_repair_engine_argv_forwards_the_rehearsal_decision() {
    let argv_of = |extra: &[&str]| {
        let base = [
            "hist",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1m",
        ];
        let all: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
        let a = parse_of(&all).unwrap();
        repair_engine_argv(&a, a.repair.as_ref().unwrap())
    };
    assert_eq!(
        argv_of(&[]),
        vec![
            "data",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1m",
            "--dry-run"
        ],
        "the BARE form rehearses, and says so to the child"
    );
    assert!(argv_of(&["--dry-run"]).contains(&"--dry-run".to_string()));
    assert!(!argv_of(&["--dry-run"]).contains(&"--yes".to_string()));
    assert_eq!(
        argv_of(&["--yes"]).last().map(String::as_str),
        Some("--yes"),
        "--yes alone performs it"
    );
    let both = argv_of(&["--yes", "--dry-run"]);
    assert!(both.contains(&"--dry-run".to_string()), "--dry-run WINS over --yes: {both:?}");
    assert!(!both.contains(&"--yes".to_string()), "{both:?}");
}

/// `--json` is FORWARDED (like `rm`'s and unlike `fetch`'s) because the engine owns the only
/// document that can carry the RESOLVED store root, and `--store`/`--engine` keep their usual
/// asymmetry: the store is forwarded, the engine path is consumed here.
#[test]
fn repair_forwards_json_and_the_store_but_consumes_the_engine_path() {
    let a = parse_of(&[
        "hist",
        "repair",
        "--kind",
        "trade",
        "--venue",
        "d",
        "--symbol",
        "S",
        "--json",
        "--store",
        "/srv/hist",
        "--engine",
        "/opt/backtest",
    ])
    .unwrap();
    let argv = repair_engine_argv(&a, a.repair.as_ref().unwrap());
    assert!(argv.contains(&"--json".to_string()), "{argv:?}");
    assert_eq!(
        argv.iter().position(|s| s == "--store").map(|i| argv[i + 1].clone()),
        Some("/srv/hist".to_string())
    );
    assert!(!argv.iter().any(|s| s == "--engine"), "consumed HERE, never forwarded: {argv:?}");
}

/// The selector flags are refused on the verbs that have no series identity — and the message
/// now names BOTH verbs that take them, because `repair` joined the set.
#[test]
fn the_selector_flags_name_both_verbs_that_take_them() {
    // ⚠ The demo source is exercised through the AXIS, not as a verb — a bare `seed-demo`
    // would be refused for its name and prove nothing about `--symbol`.
    for argv in [
        &["hist", "fetch", "--symbol", "S"][..],
        &["hist", "fetch", "--source", "demo", "--symbol", "S"][..],
        &["hist", "ls", "--symbol", "S"][..],
        &["hist", "coverage", "--symbol", "S"][..],
    ] {
        let err = parse_of(argv).unwrap_err();
        assert!(err.contains("`rm` or `repair`"), "{argv:?}: {err}");
    }
}
