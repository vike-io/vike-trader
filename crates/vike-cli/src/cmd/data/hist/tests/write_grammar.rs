//! The write verbs' grammar: `fetch`'s window, `export`'s two routes, the engine argv, `--json`.

use super::*;
use crate::cmd::engine;

/// ⚠ The shape check is a TYPO catcher, not a roster. Three non-empty parts pass whatever they
/// name; two, four, or an empty part do not.
#[test]
fn the_spec_shape_is_checked_and_nothing_else_is() {
    assert!(check_spec("binance:BTCUSDT:1h").is_ok());
    assert!(check_spec("notavenue:WHATEVER:3q").is_ok(), "the engine judges the venue, not us");
    for bad in ["binance:BTCUSDT", "a:b:c:d", "binance::1h", ":BTCUSDT:1h", "binance:BTCUSDT:"] {
        let err = check_spec(bad).unwrap_err();
        assert!(err.contains(bad), "the message names what was typed: {err}");
        assert!(err.contains("VENUE:SYMBOL:INTERVAL"), "…and the shape it wanted: {err}");
    }
}

/// A fetch with no window is a usage error naming both spellings — the engine would refuse it
/// too, but only after a process spawn and in a binary the user did not name.
#[test]
fn a_fetch_needs_a_window() {
    let err = parse_of(&["hist", "fetch", "binance:BTCUSDT:1h"]).unwrap_err();
    assert!(err.contains("--days"), "{err}");
    assert!(err.contains("--from"), "{err}");
}

/// The two window forms are EXCLUSIVE, and a half-range is named for the half that is missing.
#[test]
fn the_window_forms_do_not_mix() {
    assert!(window_from(Some("7".into()), Some("a".into()), Some("b".into())).is_err());
    assert!(window_from(None, Some("a".into()), None).unwrap_err().contains("--to"));
    assert!(window_from(None, None, Some("b".into())).unwrap_err().contains("--from"));
    assert_eq!(
        window_from(None, Some("a".into()), Some("b".into())).unwrap(),
        Window::Range { from: "a".into(), to: "b".into() }
    );
}

/// `--days` is a whole positive number: a `--days 7.5` or a `--days 0` is caught here rather
/// than becoming a fetch that covers nothing.
#[test]
fn days_must_be_a_positive_whole_number() {
    assert!(window_from(Some("7".into()), None, None).is_ok());
    assert!(window_from(Some("7.5".into()), None, None).is_err());
    assert!(window_from(Some("-3".into()), None, None).is_err());
    assert!(window_from(Some("0".into()), None, None).unwrap_err().contains("no time"));
}

/// `seed-demo` refuses every fetch-shaped flag rather than ignoring it — a `--days 30` that
/// quietly did nothing would leave an operator believing they had seeded a month.
#[test]
fn seed_demo_refuses_a_window_and_a_spec() {
    assert!(
        parse_of(&["hist", "fetch", "--source", "demo", "--days", "30"])
            .unwrap_err()
            .contains("--days")
    );
    assert!(
        parse_of(&["hist", "fetch", "--source", "demo", "--from", "0"])
            .unwrap_err()
            .contains("--from")
    );
    assert!(
        parse_of(&["hist", "fetch", "--source", "demo", "binance:BTCUSDT:1h"])
            .unwrap_err()
            .contains("takes no spec")
    );
    // …and the one flag that DOES apply to it still parses.
    assert_eq!(
        parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s"]).unwrap().store.as_deref(),
        Some("/s")
    );
}

/// A second bare word is a shell-quoting accident far more often than an intention, and
/// ignoring it would fetch a series nobody asked for.
#[test]
fn a_second_positional_is_refused_naming_both() {
    let err = parse_of(&["hist", "fetch", "binance:BTCUSDT:1h", "okx:BTC-USDT:1h", "--days", "7"])
        .unwrap_err();
    assert!(err.contains("okx:BTC-USDT:1h") && err.contains("binance:BTCUSDT:1h"), "{err}");
}

/// THE translation: what the engine is actually asked to do. Pinned as argv because that is
/// the whole product of this module — everything else is the engine's.
#[test]
fn the_engine_argv_is_the_translation() {
    // ⚠ `fetch` was the first two cases here and is GONE from this test, because it is gone
    // from the engine: it asks a datahub now (see [`execute_fetch`]) and `parse` refuses the
    // `--store`/`--engine` these cases passed it. `engine_argv`'s `Sub::Fetch` arm went with
    // them — an arm nothing can reach is not coverage, it is a second answer waiting to drift
    // from the one that runs.
    assert_eq!(
        engine_argv(&parse_of(&["hist", "fetch", "--source", "demo"]).unwrap()),
        ["data", "seed-demo"]
    );
    assert_eq!(
        engine_argv(&parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s"]).unwrap()),
        ["data", "seed-demo", "--store", "/s"]
    );
    assert_eq!(
        engine_argv(&parse_of(&["hist", "fetch", "--source", "starter", "--store", "/s"]).unwrap()),
        ["data", "fetch-starter", "--store", "/s"]
    );

    // ⚠ `export`'s two bounds are INDEPENDENT — see [`ExportRange`]. Each of the four
    // combinations reaches the engine as itself, which is the whole difference from a
    // [`Window`].
    assert_eq!(
        engine_argv(&parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet"]).unwrap()),
        ["data", "export", "demo:D:1h", "--out", "s.parquet"]
    );
    assert_eq!(
        engine_argv(
            &parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet", "--from", "5"])
                .unwrap()
        ),
        ["data", "export", "demo:D:1h", "--out", "s.parquet", "--from", "5"],
        "a lone --from is a WELL-FORMED export bound, where on a fetch it is a usage error"
    );
    assert_eq!(
        engine_argv(
            &parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet", "--to", "9"]).unwrap()
        ),
        ["data", "export", "demo:D:1h", "--out", "s.parquet", "--to", "9"],
        "…and so is a lone --to"
    );
}

/// The two subcommands ruling 12 moved that had NO home here before: their grammar, and the
/// one place it deliberately diverges from `fetch`'s.
#[test]
fn export_and_fetch_starter_carry_their_own_grammar() {
    // `export` REQUIRES a spec and a destination; neither has a default a store could supply.
    assert!(
        parse_of(&["hist", "export", "--out", "s.parquet"]).unwrap_err().contains("needs a spec")
    );
    assert!(parse_of(&["hist", "export", "demo:D:1h"]).unwrap_err().contains("--out"));
    assert!(
        parse_of(&["hist", "export", "not-a-spec", "--out", "s"]).unwrap_err().contains("VENUE")
    );

    // ⚠ `--days` is refused BY NAME rather than folded into a range: it counts back from NOW,
    // which bounds a FETCH and says nothing about what a store already holds.
    let err = parse_of(&["hist", "export", "demo:D:1h", "--out", "s", "--days", "7"]).unwrap_err();
    assert!(err.contains("--days") && err.contains("FETCH"), "{err}");

    // `fetch-starter` is `seed-demo`'s shape: no spec, no window, `--store` and nothing else.
    assert!(
        parse_of(&["hist", "fetch", "--source", "starter", "d:S:1h"])
            .unwrap_err()
            .contains("takes no spec")
    );
    assert!(
        parse_of(&["hist", "fetch", "--source", "starter", "--days", "7"])
            .unwrap_err()
            .contains("--days")
    );
    assert_eq!(
        parse_of(&["hist", "fetch", "--source", "starter", "--store", "/s"])
            .unwrap()
            .store
            .as_deref(),
        Some("/s")
    );

    // `--out` belongs to `export` alone, and is refused elsewhere by name rather than ignored.
    // ⚠ The two absorbed verbs are now SOURCES, so they are exercised through the axis — a
    // bare `fetch-starter` would be refused for its NAME and prove nothing about `--out`.
    for argv in [
        &["hist", "fetch", "--source", "starter", "--out", "s.parquet"][..],
        &["hist", "fetch", "--source", "demo", "--out", "s.parquet"][..],
        &["hist", "ls", "--out", "s.parquet"][..],
        &["hist", "rm", "--out", "s.parquet"][..],
    ] {
        let err = parse_of(argv).unwrap_err();
        assert!(err.contains("--out"), "{argv:?}: {err}");
    }
}

/// D1 (0094 follow-ups): a VENUE fetch's `--addr` is its OWN route to a datahub — the same shape
/// `export`'s own route flag takes — and must not be refused as a foreign flag; `--source
/// starter|demo` still refuses it, because that axis drives the LOCAL engine and has no datahub in
/// its path at all. Before this fix `execute_fetch`'s own doc and the module's USAGE text both
/// already promised a VENUE fetch could reach `--addr`, while the parser refused it outright.
#[test]
fn a_venue_fetch_keeps_addr_and_an_engine_source_still_refuses_it() {
    let venue =
        parse_of(&["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--addr", "1.2.3.4:9"])
            .expect("a VENUE fetch's --addr is its own route, not a foreign flag");
    assert_eq!(venue.addr, "1.2.3.4:9");
    assert!(venue.addr_given);

    for source in ["starter", "demo"] {
        let err =
            parse_of(&["hist", "fetch", "--source", source, "--addr", "1.2.3.4:9"]).unwrap_err();
        assert!(err.contains("--addr") && err.contains("REMOTE"), "--source {source}: {err}");
    }
}

/// **THE ROUTE SWITCH.** `--addr` — TYPED, not merely resolved — is what selects the remote
/// export, and the two routes then take different grammars.
///
/// ⚠ The load-bearing half is the SECOND assertion: a line with no `--addr` builds NO
/// [`export::Plan`] even though [`Args::addr`] is always `Some`. Reading the resolved address
/// would send every local export over a socket.
#[test]
fn the_export_route_is_chosen_by_a_typed_addr_and_not_by_the_resolved_one() {
    let local = parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet"])
        .expect("the shipped local grammar is unchanged");
    assert!(local.export.is_none(), "no --addr is the ENGINE route");
    assert!(!local.addr_given, "…and the resolved default is not a request");
    assert!(!local.addr.is_empty(), "…while the address itself is still resolved");

    let remote = parse_of(&[
        "hist",
        "export",
        "demo:D:1h",
        "--out",
        "s.jsonl",
        "--addr",
        "h:1",
        "--format",
        "jsonl",
        "--from",
        "1000",
        "--to",
        "2000",
    ])
    .expect("the remote grammar");
    let e = remote.export.as_ref().expect("--addr selects the walk");
    assert_eq!(e.kind, export::Kind::Bar);
    assert_eq!(e.wire, export::Wire::Jsonl);
    assert_eq!(e.bounds, (1000, 2000));
    assert_eq!(e.step_ms, export::Kind::Bar.default_window_ms());
    assert!(e.step_defaulted);
    // ...and the LOCAL route's range field stays empty, so no arm can read a half-filled one.
    assert!(remote.export_range.is_none());
}

/// Each route refuses the OTHER's flags by name, and each refusal names the flag that would
/// reach the route the operator wanted.
#[test]
fn each_export_route_refuses_the_others_flags_and_names_the_way_across() {
    // REMOTE-only flags on the LOCAL route.
    for (argv, needle) in [
        (&["hist", "export", "d:S:1h", "--out", "o", "--kind", "trade"][..], "add --addr"),
        (&["hist", "export", "d:S:1h", "--out", "o", "--window", "1d"][..], "--addr"),
        (&["hist", "export", "d:S:1h", "--out", "o", "--format", "csv"][..], "--addr"),
    ] {
        let err = parse_of(argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must name the way across: {err}");
    }

    // ...and the ENGINE route's flag on the REMOTE one. (It named `--store` too until
    // 2026-09-26; that flag is refused on `export` outright now — see the test below.)
    let clash =
        parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--engine", "/e"])
            .unwrap_err();
    assert!(clash.contains("two DIFFERENT routes"), "{clash}");

    // ⚠ THE ANTI-VACUITY CONTROL: each of those flags is ACCEPTED on the route it belongs to,
    // so the refusals above are about the route rather than about the flag not existing.
    assert!(
        parse_of(&[
            "hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "csv", "--kind",
            "trade", "--window", "1d", "--from", "1", "--to", "2",
        ])
        .is_err_and(|e| e.contains("no INTERVAL")),
        "a THREE-part spec on a tick kind is the only thing wrong with that line"
    );
    assert!(
        parse_of(&[
            "hist", "export", "d:S", "--out", "o", "--addr", "h:1", "--format", "csv", "--kind",
            "trade", "--window", "1d", "--from", "1", "--to", "2",
        ])
        .is_ok(),
        "…and with the interval dropped, every one of those flags is accepted"
    );
    assert!(
        parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--engine", "/e"]).is_ok(),
        "--engine is accepted on the route it belongs to"
    );
}

/// ⚠ **`--store` on `export` is REFUSED BY NAME, on BOTH routes, since 2026-09-26** — decision
/// 0084's amendment closed the local READ door, and `export` was the one reader it had left
/// open. The sentence is `store_flag_removed`'s, so it names the one command that serves local
/// files, cites the record by NUMBER (the published surface cannot carry a `docs/` path), and
/// fires before the value is read — a trailing `--store` with no directory meets it too.
#[test]
fn the_store_flag_on_export_is_refused_by_name_on_both_routes() {
    let engine_route = ["hist", "export", "d:S:1h", "--out", "o"];
    let remote_route = ["hist", "export", "d:S", "--out", "o", "--addr", "h:1", "--format", "csv"];
    for base in [&engine_route[..], &remote_route[..]] {
        for store in [&["--store", "/srv/hist"][..], &["--store=/srv/hist"][..], &["--store"][..]] {
            let argv = [base, store].concat();
            let err = parse_of(&argv).unwrap_err();
            assert_eq!(
                err,
                vike_datahub_client::flag_vocab::store_flag_removed("data hist export"),
                "{argv:?} must meet the ONE sentence every history reader prints"
            );
            assert!(err.contains("VIKE_DATAHUB_STORE=DIR vike-backend datahub"), "{err}");
            assert!(!err.contains("docs/"), "no withheld path: {err}");
        }
    }
    // …and the WRITERS on the same plane keep it: the ruling was about readers.
    assert!(parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s"]).is_ok());
    assert!(
        parse_of(&[
            "hist", "repair", "--kind", "bar", "--venue", "d", "--symbol", "S", "--store", "/s"
        ])
        .is_ok()
    );
}

/// A remote export needs BOTH bounds, and the refusal names the verb that PRINTS them.
#[test]
fn a_remote_export_needs_both_bounds_and_names_where_to_read_them() {
    let base = ["hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "jsonl"];
    for extra in [&[][..], &["--from", "1"][..], &["--to", "9"][..]] {
        let mut argv = base.to_vec();
        argv.extend_from_slice(extra);
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains("BOTH --from and --to"), "{extra:?}: {err}");
        assert!(err.contains("data hist ls --venue d --name S"), "{extra:?}: {err}");
    }
    // ANTI-VACUITY: the pair together is accepted, so the refusal is about absence rather than
    // about the flags being rejected outright.
    let mut both = base.to_vec();
    both.extend_from_slice(&["--from", "1", "--to", "9"]);
    assert!(parse_of(&both).is_ok(), "both bounds is the accepted line");

    // ...and an INVERTED pair is refused rather than swapped, because the file it would write
    // is empty and an empty file reads exactly like an empty store.
    let mut inverted = base.to_vec();
    inverted.extend_from_slice(&["--from", "9", "--to", "1"]);
    let err = parse_of(&inverted).unwrap_err();
    assert!(err.contains("AFTER"), "{err}");
}

/// The ENGINE route is what shipped, which is the property the remote route had to preserve — a
/// second route may not alter the first.
///
/// ⚠ `--format parquet` is the one ADDITION to it, and it changes nothing: it names what this
/// route already writes. That is deliberately not the same as being ignored — see
/// [`export::refuse_a_wire_on_the_local_route`]. (⚠ These lines carried `--store /s` until
/// 2026-09-26, when that flag became a refusal on `export` — the one change to this route, and
/// the one decision 0084's amendment asked for.)
#[test]
fn the_local_export_route_is_unchanged_and_parquet_merely_names_what_it_writes() {
    let plain = parse_of(&["hist", "export", "demo:D:1h", "--out", "s.parquet", "--from", "5"])
        .expect("the shipped line");
    let named = parse_of(&[
        "hist",
        "export",
        "demo:D:1h",
        "--out",
        "s.parquet",
        "--from",
        "5",
        "--format",
        "parquet",
    ])
    .expect("…and the same line naming the format it already writes");
    assert_eq!(plain, named, "naming `parquet` here changes nothing about the request");
    assert!(plain.export.is_none());
    assert_eq!(
        plain.export_range,
        Some(ExportRange { from: Some("5".into()), to: None }),
        "a LONE --from still stands alone on this route"
    );
    // ANTI-VACUITY: the engine argv is still what it was, so the equality above is over a
    // request that genuinely reaches the child.
    assert!(engine_argv(&plain).contains(&"--out".to_string()));
}

/// `--json` parses on EVERY write subcommand, takes no value, and — like `--engine` — is consumed
/// here rather than forwarded. The engine's own `--json` is a different flag on a different
/// code path (it renders a `BacktestReport`), and passing this one through would ask `--fetch`
/// for a document it does not produce.
#[test]
fn json_parses_on_every_write_subcommand_and_is_not_forwarded() {
    for argv in [
        vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--json"],
        vec!["hist", "fetch", "--source", "demo", "--json"],
        vec!["hist", "fetch", "--source", "starter", "--json"],
        vec!["hist", "export", "demo:D:1h", "--out", "s.parquet", "--json"],
    ] {
        let a = parse_of(&argv).unwrap();
        assert!(a.json, "{argv:?}");
        assert!(!engine_argv(&a).iter().any(|s| s == "--json"), "{:?}", engine_argv(&a));
    }
    assert!(!parse_of(&["hist", "fetch", "--source", "demo"]).unwrap().json, "absent means absent");
    // A value is refused rather than swallowed — the same `no_value` rung every other
    // valueless flag in this crate uses.
    assert!(
        parse_of(&["hist", "fetch", "--source", "demo", "--json=1"])
            .unwrap_err()
            .contains("--json")
    );
}

/// The document, field by field. It is built from the SAME parsed `Args` and the SAME argv the
/// engine was handed, so a machine and a person cannot be told different things about one run.
#[test]
fn the_json_document_carries_the_request_the_engine_and_its_report() {
    // ⚠ This was a `fetch` until that verb left the engine route. `export` is the surviving
    // engine verb that carries a SPEC, so the series half is pinned exactly as before.
    //
    // ⚠ `report_json`'s `args.window` branch is still EXERCISED — the range block further down
    // builds a `fetch` and renders it directly — but it is no longer REACHABLE in production,
    // because `report_json` is called only from the engine route and `fetch` no longer takes
    // it. The renderer stays: `--from`/`--to` on an engine verb is a grammar this module may
    // grow again, and deleting a renderer is a wider change than retiring a route. Stated so
    // the difference between "tested" and "reachable" is known here rather than found later.
    let a = parse_of(&["hist", "export", "binance:BTCUSDT:1h", "--out", "s.parquet", "--json"])
        .unwrap();
    let argv = engine_argv(&a);
    let doc: serde_json::Value = serde_json::from_str(&report_json(
        &a,
        &engine::Engine::standalone("/opt/backtest"),
        &argv,
        &["12 bars".to_string()],
    ))
    .expect("report_json writes JSON");

    assert_eq!(doc["subcommand"], "export");
    // ⚠ NULL on an export, always, since 2026-09-26: `--store` is refused on that verb, and
    // the engine reads its bars through a datahub rather than a directory this side could name.
    // The document still carries the field — a machine must be able to tell "no store" from
    // "the field is gone" — and `fetch --source demo --store` below is where it is non-null.
    assert!(doc["store"].is_null(), "{doc}");
    assert_eq!(doc["series"]["venue"], "binance");
    assert_eq!(doc["series"]["symbol"], "BTCUSDT");
    assert_eq!(doc["series"]["interval"], "1h");
    assert_eq!(doc["series"]["spec"], "binance:BTCUSDT:1h");
    assert_eq!(doc["engine"], "/opt/backtest");
    assert_eq!(doc["engine_argv"][0], "data");
    assert_eq!(doc["engine_argv"][1], "export");
    assert_eq!(doc["report"][0], "12 bars");

    // The range window is the OTHER form, and a caller must be able to tell which it got
    // without re-parsing the argv.
    let range =
        parse_of(&["hist", "fetch", "okx:BTC-USDT:1h", "--from", "0", "--to", "100", "--json"])
            .unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&report_json(&range, &engine::Engine::standalone("b"), &[], &[]))
            .unwrap();
    assert_eq!(doc["window"]["from"], "0");
    assert_eq!(doc["window"]["to"], "100");
    assert!(doc["window"]["days"].is_null());

    // `seed-demo` takes neither a spec nor a window, and both are NULL rather than absent: a
    // machine can tell "no series" from "the field is gone" only if the field is there.
    let seed = parse_of(&["hist", "fetch", "--source", "demo", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&report_json(&seed, &engine::Engine::standalone("b"), &[], &[]))
            .unwrap();
    // ⚠ The verb is `fetch` now and the SOURCE carries what the old name did. Both are
    // asserted, because either alone would let the collapse lose a fact.
    assert_eq!(doc["subcommand"], "fetch");
    assert_eq!(doc["source"], "demo");
    assert!(doc["series"].is_null());
    assert!(doc["window"].is_null());
    // ...and an unnamed store is NULL, never a guessed path: the ENGINE resolves the root, in
    // another process, and this side would be inventing one.
    assert!(doc["store"].is_null());

    // ...while a NAMED one is carried verbatim — on a WRITER, the only verbs that still take
    // the flag. (This was the export case's assertion until `--store` left that verb.)
    let named =
        parse_of(&["hist", "fetch", "--source", "demo", "--store", "/s", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&report_json(&named, &engine::Engine::standalone("b"), &[], &[]))
            .unwrap();
    assert_eq!(doc["store"], "/s");
}

/// `--engine` never reaches the child: it says WHICH binary to run, not what to tell it.
#[test]
fn the_engine_flag_is_consumed_here_and_not_forwarded() {
    let a = parse_of(&["hist", "fetch", "--source", "demo", "--engine", "/opt/backtest"]).unwrap();
    assert_eq!(a.engine.as_deref(), Some("/opt/backtest"));
    assert!(!engine_argv(&a).iter().any(|s| s == "--engine"), "{:?}", engine_argv(&a));
}
