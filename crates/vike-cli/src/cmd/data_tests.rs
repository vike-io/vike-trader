// ── §7: the group split, and the refusals that make it survivable ───────────────────────

/// EVERY pre-group spelling is refused BY NAME and NAMES ITS REPLACEMENT.
///
/// ⚠ Driven from [`RETIRED_SPELLINGS`], never a hand-written list — the same discipline
/// `a_missing_subcommand_names_every_subcommand_that_exists` exists for. A table row added
/// without a message, or a message that forgets to name where the verb went, fails here.
#[test]
fn every_retired_flat_spelling_is_refused_and_names_its_replacement() {
    for (was, now) in RETIRED_SPELLINGS {
        let err = parse_of(&[was]).unwrap_err();
        assert!(
            err.contains(now),
            "refusing `data {was}` must NAME its replacement `{now}`: {err}"
        );
    }
}

/// ...and every replacement it names actually PARSES. A refusal that points at a spelling the
/// parser rejects is worse than the spelling it replaced — it costs the operator two attempts
/// instead of one, and there is no third message to correct them.
#[test]
fn every_named_replacement_is_a_spelling_that_parses() {
    for (_, now) in RETIRED_SPELLINGS {
        // The replacement is a full command line minus `vike-cli data`; the verbs that need
        // arguments get the minimum that makes a NAME failure the only possible one.
        let mut argv: Vec<&str> = now.split_whitespace().skip(1).collect();
        match *argv.last().unwrap() {
            "fetch" => argv.extend(["d:S:1h", "--days", "1"]),
            "export" => argv.extend(["d:S:1h", "--out", "s.parquet"]),
            "rm" => argv.extend(["--kind", "bar", "--venue", "d", "--symbol", "S"]),
            "repair" => argv.extend(["--kind", "trade", "--venue", "d", "--symbol", "S"]),
            _ => {}
        }
        parse_of(&argv).unwrap_or_else(|e| {
            panic!(
                "the replacement named for a retired verb, {now:?}, \
                                            does not parse: {e}"
            )
        });
    }
}

/// The two verbs the split RENAMED get their own message. A reader who typed the GROUP
/// correctly and the verb by its old name should not be handed the whole roster to diff.
#[test]
fn a_renamed_verb_under_the_right_group_names_its_new_spelling() {
    for (old, new) in [("list", "ls"), ("tape-health", "health")] {
        let err = parse_of(&["hist", old]).unwrap_err();
        assert!(err.contains(new), "`data hist {old}` must name `{new}`: {err}");
    }
}

/// A group nobody has heard of names the GROUPS, not the verbs — the roster that is one level
/// up from where the reader went wrong.
#[test]
fn the_groups_are_named_when_one_is_missing() {
    let err = parse_of(&["nonsense"]).unwrap_err();
    for group in ["hist", "realtime", "catalog", "source"] {
        assert!(err.contains(group), "the unknown-group error must name {group}: {err}");
    }
}

/// The sub-verb is REQUIRED, as it is on `backtest`. There is no bare `vike-cli data hist`.
#[test]
fn the_group_alone_is_not_a_command() {
    assert!(parse_of(&["hist"]).is_err());
}

// ── §9.3.2: the account-kind exclusion, its disclosure, and its refusal ──────────────────

use std::collections::BTreeSet;

fn note(count: usize, kinds: &[&str]) -> Option<String> {
    let set: BTreeSet<&str> = kinds.iter().copied().collect();
    super::account_exclusion_note(count, &set)
}

/// A note that fires on every run stops being read, so nothing withheld means nothing said.
#[test]
fn nothing_withheld_says_nothing() {
    assert_eq!(note(0, &[]), None);
}

/// The spec's own example sentence, §9.3.2. It names the KINDS rather than only a count,
/// because a count alone cannot be acted on — an operator who sees "3 series are hidden" has
/// to guess whether the thing they came for is among them.
#[test]
fn the_disclosure_names_the_kinds_and_the_plane_that_will_serve_them() {
    let n = note(3, &["exec_fill", "exec_order", "equity"]).unwrap();
    assert!(n.starts_with("note: 3 series of kinds "), "{n}");
    for kind in ["exec_fill", "exec_order", "equity"] {
        assert!(n.contains(kind), "the note must NAME {kind}: {n}");
    }
    assert!(n.contains("account data"), "{n}");
    assert!(n.contains("vike-cli account"), "the note must say WHERE they will be served: {n}");
}

/// One series is "1 series of kind X is", not "1 series of kinds X are".
#[test]
fn the_disclosure_inflects_for_one() {
    let n = note(1, &["equity"]).unwrap();
    assert!(n.contains("1 series of kind equity is in this store"), "{n}");
    assert!(!n.contains("kinds"), "{n}");
}

/// ⚠ The exclusion and the refusal answer DIFFERENT questions, and this is the refusal: a
/// listing that ASKED for an account kind by name must not answer "no series match", because
/// that sentence is false — the series exist and this plane declines to serve them.
#[test]
fn a_kind_filter_naming_account_data_is_refused_by_name() {
    for kind in vike_model::ACCOUNT_KINDS {
        let args = parse_of(&["hist", "ls", "--kind", kind]).expect("the FILTER still parses");
        let err = super::refuse_an_account_kind_filter(&args)
            .expect_err("an account kind must be refused, not filtered to nothing");
        assert!(err.contains(kind), "{err}");
        assert!(err.contains("vike-cli account"), "{err}");
    }
}

/// ⚠ The MARKET funding RATE is not account data and must never be refused as such. It is a
/// BAR series under `interval=funding`; `docs/decisions/0080` separated the two names so that
/// this test can be written at all.
#[test]
fn the_market_funding_rate_is_not_refused() {
    for kind in ["bar", "funding"] {
        let args = parse_of(&["hist", "ls", "--kind", kind]).expect("parses");
        assert!(
            super::refuse_an_account_kind_filter(&args).is_ok(),
            "{kind:?} is MARKET data — the funding RATE is a bar series under \
                 `interval=funding`, and refusing it would hide market data behind an account \
                 refusal, which is the collision 0079 exists to have resolved"
        );
    }
}

/// EXACT match only. `--kind` on a listing is a substring filter, and refusing every substring
/// that could reach an account kind would put a roster's worth of guessing into a filter.
#[test]
fn a_substring_that_merely_overlaps_an_account_kind_stays_a_filter() {
    for kind in ["exec", "equ", "fill", "exec_f"] {
        let args = parse_of(&["hist", "ls", "--kind", kind]).expect("parses");
        assert!(
            super::refuse_an_account_kind_filter(&args).is_ok(),
            "{kind:?} is a substring, not a name — it must stay a filter"
        );
    }
}

use super::*;
use crate::cmd::args::HELP_SENTINEL;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()), None)
}

/// The address ladder, all three rungs — the twin of
/// `crate::cmd::backtest::resolve_addr`'s `the_address_ladder_is_cli_then_configured_then_default`,
/// one plane over.
///
/// ⚠ **What would make this fail:** dropping the `configured_addr` rung from `parse`, which is
/// how this verb behaved until that parameter existed — `config.datahub_addr` was read by
/// `crates/vike-desktop/src/app_methods.rs` and by nothing here, so a box whose datahub is not
/// on [`DEFAULT_ADDR`] reached it from the GUI and dialled the compiled-in default from the CLI.
#[test]
fn the_address_ladder_is_cli_then_configured_then_default() {
    let cli = parse(
        ["hist", "ls", "--addr", "1.2.3.4:9"].iter().map(|s| s.to_string()),
        Some("5.6.7.8:9"),
    )
    .unwrap();
    assert_eq!(cli.addr, "1.2.3.4:9", "the flag outranks the setting");

    let configured =
        parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("5.6.7.8:9")).unwrap();
    assert_eq!(configured.addr, "5.6.7.8:9", "the setting is the middle rung");

    let neither = parse(["hist", "ls"].iter().map(|s| s.to_string()), None).unwrap();
    assert_eq!(neither.addr, DEFAULT_ADDR, "and the compiled-in default is the floor");

    // A blank rung is SKIPPED, not honoured: an `Environment=` line that set nothing must not
    // aim this client at an empty address.
    let blank = parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("   ")).unwrap();
    assert_eq!(blank.addr, DEFAULT_ADDR, "a blank setting falls through to the default");
}

/// ⚠ The setting says WHERE to dial, never WHETHER to — and `rm` is where the difference
/// deletes something. `execute_rm` routes on `addr_given`, so a configured `config.datahub_addr`
/// must leave it FALSE: a box that merely names its datahub has not asked for every `data hist rm` to
/// run against that datahub instead of its local store.
///
/// **What would make this fail:** setting `addr_given` from the resolved address rather than
/// from the flag — the tidying a reader who saw only the ladder above would reach for.
#[test]
fn a_configured_address_does_not_ask_for_the_remote_route() {
    let configured =
        parse(["hist", "ls"].iter().map(|s| s.to_string()), Some("5.6.7.8:9")).unwrap();
    assert_eq!(configured.addr, "5.6.7.8:9", "the address resolved…");
    assert!(!configured.addr_given, "…and the ROUTE was still not asked for");

    let flagged =
        parse(["hist", "ls", "--addr", "5.6.7.8:9"].iter().map(|s| s.to_string()), None).unwrap();
    assert!(flagged.addr_given, "naming it on the line IS asking");
}

#[test]
fn each_subcommand_parses() {
    assert_eq!(
        parse_of(&["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7"]).unwrap().sub,
        Sub::Fetch
    );
    // ⚠ `seed-demo` is no longer a VERB — it is a source. The verb is `fetch` and the
    // axis carries what used to be the name.
    let demo = parse_of(&["hist", "fetch", "--source", "demo"]).unwrap();
    assert_eq!(demo.sub, Sub::Fetch);
    assert_eq!(demo.source, Source::Demo);
    assert_eq!(parse_of(&["hist", "ls"]).unwrap().sub, Sub::List);
    assert_eq!(parse_of(&["hist", "coverage"]).unwrap().sub, Sub::Coverage);
}

#[test]
fn help_short_circuits_at_both_levels() {
    assert_eq!(parse_of(&["--help"]).unwrap_err(), HELP_SENTINEL);
    assert_eq!(parse_of(&["hist", "fetch", "-h"]).unwrap_err(), HELP_SENTINEL);
}

/// The verb takes no default action, so a bare `data` must name every subcommand there is —
/// this message is the only place a user who typed the verb alone learns what it can do.
#[test]
fn a_missing_subcommand_names_every_subcommand_that_exists() {
    // ⚠ `data hist` with no verb, not bare `data` — the group split moved the roster one level
    // down, so THIS is now the message whose whole job is to name what the group can do. Bare
    // `data` names the GROUPS instead, which `the_groups_are_named_when_one_is_missing` covers.
    let err = parse_of(&["hist"]).unwrap_err();
    // ⚠ EVERY row of [`SUBCOMMANDS`], not a hand-written list — the hand-written list in this
    // test AND in the message it checked both omitted `rm`, which had shipped months earlier.
    // A test that names its own expectations cannot catch a roster going short.
    for sub in SUBCOMMANDS {
        assert!(
            err.contains(sub.as_str()),
            "the missing-subcommand error must name {}: {err}",
            sub.as_str()
        );
    }
}

/// Every advertised subcommand is REACHABLE by the name it advertises, both directions:
/// [`SUBCOMMANDS`] round-trips through [`parse`]'s match, and nothing is in one and not the
/// other. A verb in [`USAGE`] that `parse` answers with "unknown `data` subcommand" is the
/// failure this exists to make impossible.
#[test]
fn all_subcommands_are_reachable_by_the_name_they_advertise() {
    for sub in SUBCOMMANDS {
        // Parsed with the flags each one REQUIRES, so a refusal here can only be about the
        // NAME rather than about a missing argument.
        let argv: Vec<&str> = match sub {
            Sub::Fetch => vec!["hist", "fetch", "d:S:1h", "--days", "1"],
            Sub::Export => vec!["hist", "export", "d:S:1h", "--out", "s.parquet"],
            Sub::Rm => vec!["hist", "rm", "--kind", "bar", "--venue", "d", "--symbol", "S"],
            // ⚠ `--symbol` is REQUIRED here where it is optional on `rm` — the one shape
            // difference between the two selectors, and the reason this arm cannot reuse
            // `rm`'s.
            Sub::Repair => {
                vec!["hist", "repair", "--kind", "trade", "--venue", "d", "--symbol", "S"]
            }
            // A spec AND a criterion: the one read verb that requires both, and the only one
            // for which `hist gate` alone is two separate refusals rather than a name lookup.
            Sub::Gate => vec!["hist", "gate", "d:S:1h", "--require-days", "30"],
            // A spec AND a window: the READ verb that requires a bound rather than merely
            // accepting one — §8.2's first rule, which `hist get d:S:1h` alone would trip.
            Sub::Get => vec!["hist", "get", "d:S:1h", "--days", "1"],
            // A spec and nothing else: the running fetch it names already has its window.
            Sub::Cancel => vec!["hist", "cancel", "d:S:1h"],
            // TWO positionals — a format and a dataset — and nothing else: the window defaults to
            // the whole dataset and the bars to 1m.
            Sub::Import => vec!["hist", "import", "dukascopy-bi5", "EURUSD"],
            other => vec!["hist", other.as_str()],
        };
        let parsed = parse_of(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
        assert_eq!(parsed.sub, *sub, "{} parsed as a different subcommand", sub.as_str());
    }
}

/// **The running-fetch door's grammar**: `running` takes nothing but the address and the format,
/// and `cancel` takes `fetch`'s own three-part spec. Neither is a store verb, so neither carries a
/// window, a store, an engine or a listing filter into [`Args`].
#[test]
fn running_and_cancel_parse_to_the_running_fetch_door() {
    let a = parse_of(&["hist", "running"]).unwrap();
    assert_eq!(a.sub, Sub::Running);
    assert_eq!(a.spec, None);
    assert_eq!(a.addr, DEFAULT_ADDR, "it dials the datahub default");
    assert!(!a.json);
    let a = parse_of(&["hist", "running", "--addr", "<host>:7878", "--json"]).unwrap();
    assert_eq!(a.addr, "<host>:7878");
    assert!(a.json);
    assert!(parse_of(&["hist", "running", "--format", "json"]).unwrap().json);

    let a = parse_of(&["hist", "cancel", "oanda:EUR_USD:5s"]).unwrap();
    assert_eq!(a.sub, Sub::Cancel);
    assert_eq!(a.spec.as_deref(), Some("oanda:EUR_USD:5s"));
    assert_eq!(a.window, None, "a running fetch already has its window");
    assert!(a.rm.is_none() && a.store.is_none() && a.engine.is_none());
}

/// `cancel` needs the spec — three non-empty parts, `fetch`'s own shape — and `running` takes none,
/// and each refusal names the other verb so the operator lands on the one they meant.
#[test]
fn cancel_needs_fetchs_spec_and_running_takes_none() {
    let e = parse_of(&["hist", "cancel"]).unwrap_err();
    assert!(e.contains("cancel needs a spec") && e.contains("data hist running"), "{e}");
    for bad in ["oanda:EUR_USD", "oanda:EUR_USD:5s:x", "oanda::5s"] {
        let e = parse_of(&["hist", "cancel", bad]).unwrap_err();
        assert!(e.contains("VENUE:SYMBOL:INTERVAL"), "{bad}: {e}");
    }
    let e = parse_of(&["hist", "running", "oanda:EUR_USD:5s"]).unwrap_err();
    assert!(e.contains("`running` takes no spec"), "{e}");
    assert!(e.contains("vike-cli data hist cancel oanda:EUR_USD:5s"), "{e}");
}

/// ⚠ **Every store, window, source and listing flag is refused on the running-fetch door BY NAME,
/// with a reason that is true of THESE verbs** — not the store-verb sentences a read verb gets,
/// which would tell an operator to "narrow the listing with --kind" on a verb that takes no
/// filter. Both verbs, every flag.
#[test]
fn the_running_fetch_door_refuses_every_store_window_and_listing_flag_by_name() {
    let rows: &[(&[&str], &str)] = &[
        (&["--store", "/srv/hist"], "touches no store"),
        (&["--engine", "/opt/backtest"], "touches no store"),
        (&["--source", "demo"], "starts no fetch"),
        (&["--days", "7"], "already has one"),
        (&["--from", "2024-01-01"], "already has one"),
        (&["--to", "2024-01-01"], "already has one"),
        (&["--kind", "bar"], "reads no store"),
        (&["--venue", "oanda"], "reads no store"),
        (&["--name", "EUR"], "reads no store"),
        (&["--class"], "reads no store"),
        (&["--partial-only"], "reads no store"),
    ];
    let mut refused = 0;
    for verb in [&["hist", "running"][..], &["hist", "cancel", "oanda:EUR_USD:5s"][..]] {
        for &(flag, why) in rows {
            let argv = [verb, flag].concat();
            let e = parse_of(&argv).unwrap_err();
            assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
            assert!(e.contains(why), "{argv:?} must say why ({why}): {e}");
            assert!(!e.contains("unknown option"), "{argv:?} must not read as a typo: {e}");
            refused += 1;
        }
        // …and the confirmation pair of the verbs that plan a write: a cancel removes nothing, so
        // there is nothing for `--yes` to confirm. The refusal names ALL THREE verbs that take it.
        for flag in ["--yes", "--dry-run"] {
            let e = parse_of(&[verb, &[flag][..]].concat()).unwrap_err();
            assert!(e.contains(flag) && e.contains("`rm`, `repair` and `import`"), "{flag}: {e}");
        }
    }
    assert_eq!(refused, rows.len() * 2, "both verbs, every row");
}

// ── `import`: a vendor archive, read on the datahub's own box ─────────────────────────────────

/// Midnight UTC of `y-m-d`, in epoch-ms.
fn utc_midnight(y: i64, m: u32, d: u32) -> i64 {
    vike_model::time::days_from_civil(y, m, d) * vike_model::MS_PER_DAY
}

fn import_of(args: &[&str]) -> ImportArgs {
    let mut argv = vec!["hist", "import"];
    argv.extend_from_slice(args);
    parse_of(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}")).import.expect("an ImportArgs")
}

/// **The import door's grammar**: a FORMAT and a DATASET, the window defaulting to the whole
/// dataset and the bars to `1m` — and nothing of it left in the fields the other verbs read.
#[test]
fn import_takes_a_format_and_a_dataset_and_defaults_the_rest() {
    let a = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD"]).unwrap();
    assert_eq!(a.sub, Sub::Import);
    assert_eq!(a.addr, DEFAULT_ADDR, "it dials the datahub default");
    assert_eq!(a.spec, None, "the FORMAT is moved into ImportArgs, never left as a spec");
    assert_eq!(a.window, None);
    assert!(a.rm.is_none() && a.store.is_none() && a.engine.is_none() && !a.json);
    assert_eq!(
        a.import.expect("an ImportArgs"),
        ImportArgs {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: None,
            to_day: None,
            bars: vec!["1m".to_string()],
            dry_run: false,
            verify: false,
            yes: false,
        }
    );
    let a = parse_of(&[
        "hist",
        "import",
        "dukascopy-bi5",
        "EURUSD",
        "--bars",
        "1m,1h",
        "--dry-run",
        "--verify",
        "--yes",
        "--json",
        "--addr",
        "127.0.0.1:9",
    ])
    .unwrap();
    assert!(a.json && a.addr_given && a.addr == "127.0.0.1:9");
    let im = a.import.expect("an ImportArgs");
    assert_eq!(im.bars, vec!["1m".to_string(), "1h".to_string()]);
    assert!(im.dry_run && im.verify && im.yes);
    assert!(import_of(&["dukascopy-bi5", "EURUSD", "--bars", "none"]).bars.is_empty());
}

/// **`--from` and `--to` are INCLUSIVE DAYS** — `--from D --to D` is the one day D, each bound the
/// midnight of the day it names, where a fetch's `--to` is an instant — and the request carries
/// exactly those two midnights. Either bound stands alone.
#[test]
fn an_imports_bounds_are_inclusive_days_and_reach_the_request_unchanged() {
    let d = utc_midnight(2024, 1, 15);
    let one = import_of(&["dukascopy-bi5", "EURUSD", "--from", "2024-01-15", "--to", "2024-01-15"]);
    assert_eq!((one.from_day, one.to_day), (Some(d), Some(d)), "one day, whole");
    let req = import_request(&one, one.from_day, one.to_day, import::Mode::Import);
    assert_eq!((req.from_day, req.to_day), (Some(d), Some(d)));
    vike_datahub_client::archive::validate_import_spec(&req)
        .expect("a one-day import is a request the wire accepts");
    let from_only = import_of(&["dukascopy-bi5", "EURUSD", "--from", "2024-01-15"]);
    assert_eq!((from_only.from_day, from_only.to_day), (Some(d), None));
    let to_only = import_of(&["dukascopy-bi5", "EURUSD", "--to", &d.to_string()]);
    assert_eq!((to_only.from_day, to_only.to_day), (None, Some(d)), "an epoch-ms midnight");

    for (args, needle) in [
        (&["--from", "2024-01-16", "--to", "2024-01-15"][..], "AFTER --to"),
        (&["--to", "2024-02-30"][..], "not a calendar date"),
        (&["--from", "1705276800001"][..], "not the START of a UTC day"),
        (&["--to", "2024-01-15T00"][..], "YYYY-MM-DD"),
    ] {
        let mut argv = vec!["hist", "import", "dukascopy-bi5", "EURUSD"];
        argv.extend_from_slice(args);
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(needle), "{args:?} must say {needle:?}: {e}");
    }
}

/// **A dry run never writes, and only `--verify` decodes without writing**: the request a run sends
/// is decided by its MODE alone, and `--yes` loses to `--dry-run`, as on `rm`.
#[test]
fn a_dry_run_never_writes_and_only_verify_decodes_without_writing() {
    use import::Mode;
    let im = import_of(&["dukascopy-bi5", "EURUSD"]);
    for (mode, dry_run, verify) in
        [(Mode::Plan, true, false), (Mode::Verify, true, true), (Mode::Import, false, false)]
    {
        let req = import_request(&im, None, None, mode);
        assert_eq!((req.dry_run, req.verify), (dry_run, verify), "{mode:?}");
        assert_eq!(req.writes(), mode == Mode::Import, "{mode:?}");
    }
    assert_eq!(Mode::of(true, false), Mode::Plan);
    assert_eq!(Mode::of(true, true), Mode::Verify);
    assert_eq!(Mode::of(false, false), Mode::Import);
    let both = import_of(&["dukascopy-bi5", "EURUSD", "--dry-run", "--yes"]);
    assert_eq!(Mode::of(both.dry_run, both.verify), Mode::Plan, "--dry-run wins over --yes");
}

/// `--verify` is a mode of a DRY RUN, and beside an import it is refused rather than read one way
/// or the other — the wire's own rule, made a usage error here.
#[test]
fn verify_without_dry_run_is_refused() {
    let e = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "--verify"]).unwrap_err();
    assert!(e.contains("--verify is a mode of --dry-run"), "{e}");
}

/// The two positionals are both required, a third is refused in the verb's own words, and every
/// command line the refusals tell an operator to type parses.
#[test]
fn an_import_needs_a_format_and_a_dataset_and_refuses_a_third() {
    let none = parse_of(&["hist", "import"]).unwrap_err();
    assert!(none.contains("import needs a FORMAT and a DATASET"), "{none}");
    let no_dataset = parse_of(&["hist", "import", "dukascopy-bi5"]).unwrap_err();
    assert!(no_dataset.contains("import needs a DATASET"), "{no_dataset}");
    assert!(no_dataset.contains("imports/dukascopy-bi5/"), "{no_dataset}");
    let third = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "GBPUSD"]).unwrap_err();
    assert!(third.contains("`import` takes TWO") && third.contains("'GBPUSD'"), "{third}");
    for message in [none, no_dataset] {
        let commands: Vec<&str> =
            message.split('`').skip(1).step_by(2).filter(|c| c.starts_with("vike-cli ")).collect();
        assert!(!commands.is_empty(), "the refusal names no command to type: {message}");
        for command in commands {
            let argv: Vec<&str> = command.split_whitespace().collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| panic!("`{command}`: {e}"));
        }
    }
}

/// The DATASET meets the wire's own validator at parse time — the function the client runs before
/// it sends and the server runs at its door — so a name that can never be a dataset is a usage
/// error before a socket opens.
#[test]
fn the_dataset_meets_the_wires_validator_before_a_socket_opens() {
    let long = "A".repeat(33);
    for bad in ["eurusd", "..", "EUR/USD", ".EURUSD", "CON", long.as_str()] {
        let e = parse_of(&["hist", "import", "dukascopy-bi5", bad]).unwrap_err();
        assert!(e.contains("import dataset"), "{bad:?}: {e}");
    }
    let e = parse_of(&["hist", "import", "dukascopy-bi5", "EURUSD", "--bars", "7m"]).unwrap_err();
    assert!(e.contains("--bars") && e.contains("does not divide a UTC day"), "{e}");
}

/// ⚠ **Every flag that is not the import door's own is refused BY NAME there, with a reason true
/// of an import** — never "unknown option", and never another verb's sentence about a listing.
#[test]
fn the_import_door_refuses_every_flag_that_is_not_its_own_by_name() {
    let rows: &[(&[&str], &str)] = &[
        (&["--store", "/srv/hist"], "no local route"),
        (&["--engine", "/opt/backtest"], "no local route"),
        (&["--source", "demo"], "its FORMAT"),
        (&["--days", "7"], "counts back from NOW"),
        (&["--kind", "quote"], "names what it writes exactly"),
        (&["--venue", "dukascopy"], "names what it writes exactly"),
        (&["--name", "EUR"], "names what it writes exactly"),
        (&["--class"], "names what it writes exactly"),
        (&["--partial-only"], "names what it writes exactly"),
        (&["--symbol", "EURUSD"], "two arguments"),
        (&["--group", "G"], "two arguments"),
        (&["--interval", "1m"], "two arguments"),
        (&["--produced-by", "dukascopy:"], "only `rm` deletes"),
        (&["--out", "f.parquet"], "only `export` writes one"),
        (&["--limit", "5"], "ROWS"),
        (&["--window", "1d"], "WINDOWED WALK"),
        (&["--require-days", "3"], "CRITERION"),
    ];
    for &(flag, why) in rows {
        let argv = [&["hist", "import", "dukascopy-bi5", "EURUSD"][..], flag].concat();
        let e = parse_of(&argv).unwrap_err();
        assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
        assert!(e.contains(why), "{argv:?} must say why ({why}): {e}");
        assert!(!e.contains("unknown option"), "{argv:?} must not read as a typo: {e}");
    }
}

/// …and the import door's OWN two flags are refused by name on every other verb, naming the verb
/// that takes them.
#[test]
fn the_import_flags_are_refused_by_name_on_every_other_verb() {
    let mut refused = 0;
    for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Import) {
        for flag in [&["--bars", "1m"][..], &["--verify"][..]] {
            let argv = [&["hist", sub.as_str()][..], flag].concat();
            let e = parse_of(&argv).unwrap_err();
            assert!(e.contains(flag[0]), "{argv:?} must name {}: {e}", flag[0]);
            assert!(e.contains("`vike-cli data hist import`"), "{argv:?}: {e}");
            refused += 1;
        }
    }
    assert_eq!(refused, (SUBCOMMANDS.len() - 1) * 2, "every other verb, both flags");
}

/// Every `vike-cli data hist import` line the get-market-data skill tells an agent to run PARSES —
/// read from the RENDERED copy, the one that ships and that an agent executes rather than reads.
#[test]
fn every_import_line_in_the_skill_parses() {
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/get-market-data/SKILL.md");
    let skill = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));
    let lines: Vec<&str> = skill
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("vike-cli data hist import "))
        .collect();
    assert!(lines.len() >= 3, "the skill's import examples were not found: {lines:?}");
    for line in lines {
        let argv: Vec<&str> = line.split_whitespace().collect();
        crate::cmd::accepts(&argv[1..])
            .unwrap_or_else(|e| panic!("the skill tells an agent to run `{line}`: {e}"));
    }
}

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

// ---- the read half: the grammar ----

/// A per-symbol bar row and a grouped tick row — every rendering property below is visible in
/// one pair: the four dimensions, the `symbol`/`group` alternative, and an absent interval.
fn bar_row() -> SeriesRow {
    SeriesRow {
        kind: "bar".to_string(),
        venue: "binance".to_string(),
        name: "BTCUSDT".to_string(),
        grouped: false,
        symbol: "BTCUSDT".to_string(),
        group: None,
        interval: Some("1h".to_string()),
        coverage: Coverage {
            first_ts: 0,
            last_ts: 86_400_000,
            rows: 48,
            bytes: 900,
            parts: 2,
            dates: 2,
        },
        gaps: None,
        gaps_error: None,
        class: None,
    }
}

fn grouped_row() -> SeriesRow {
    SeriesRow {
        kind: "trade".to_string(),
        venue: "polymarket".to_string(),
        name: "fam".to_string(),
        grouped: true,
        // ⚠ EMPTY, exactly as the store reports it for a grouped series. The whole point of
        // the fixture: anything that renders `symbol` would render nothing here.
        symbol: String::new(),
        group: Some("fam".to_string()),
        interval: None,
        coverage: Coverage {
            first_ts: 172_800_000,
            last_ts: 259_200_000,
            rows: 7,
            bytes: 40,
            parts: 1,
            dates: 2,
        },
        gaps: None,
        gaps_error: None,
        class: None,
    }
}

#[test]
fn the_read_subcommands_default_the_addr_and_take_the_filter_flags() {
    let a = parse_of(&["hist", "ls"]).unwrap();
    assert_eq!(a.addr, DEFAULT_ADDR);
    assert_eq!(a.filter, Filter::default());
    assert!(!a.gaps && !a.class && !a.partial_only && !a.json);

    let a = parse_of(&[
        "hist",
        "ls",
        "--addr",
        "1.2.3.4:9",
        "--kind",
        "bar",
        "--venue",
        "binance",
        "--name",
        "BTC",
        "--class",
        "--json",
    ])
    .unwrap();
    assert_eq!(a.addr, "1.2.3.4:9");
    assert_eq!(a.filter.kind.as_deref(), Some("bar"));
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    assert_eq!(a.filter.name.as_deref(), Some("BTC"));
    assert!(a.class && a.json && !a.gaps, "`ls` never probes gaps");

    // ...and the same filters on `gaps`, where the probe is on because the VERB is.
    let a = parse_of(&["hist", "gaps", "--kind", "bar", "--venue", "binance", "--name", "BTC"])
        .unwrap();
    assert_eq!(a.filter.kind.as_deref(), Some("bar"));
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    assert_eq!(a.filter.name.as_deref(), Some("BTC"));
    assert!(a.gaps && !a.class, "the verb arms the probe, and only the probe");

    let a = parse_of(&["hist", "coverage", "--venue", "binance", "--partial-only"]).unwrap();
    assert!(a.partial_only);
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
}

/// The bare booleans reject an inline value, on the same rung every valueless flag in this
/// crate uses.
#[test]
fn the_read_booleans_take_no_value() {
    assert!(parse_of(&["hist", "ls", "--class=1"]).unwrap_err().contains("--class"));
    assert!(parse_of(&["hist", "ls", "--json=1"]).unwrap_err().contains("--json"));
    assert!(
        parse_of(&["hist", "coverage", "--partial-only=yes"])
            .unwrap_err()
            .contains("--partial-only")
    );
}

/// `--class` is accepted on `ls` ALONE, and every other subcommand refuses it BY NAME with
/// the reason that subcommand has — never silently ignores it.
///
/// ⚠ The three read siblings are the interesting rows and they do NOT share a reason, which is
/// why each is asserted on its own words rather than on "is an error": `coverage` refuses it
/// because its roster is the TICK kinds and a bar-only instrument is in none of them;
/// `tape-health` because every finding it makes is folded from the one inventory it already
/// fetched; `universe` because its cells are as-of a window the operator wrote down and this
/// probe is as-of now.
#[test]
fn the_class_flag_belongs_to_list_alone_and_every_refusal_says_why() {
    assert!(parse_of(&["hist", "ls", "--class"]).unwrap().class);

    let err = |args: &[&str]| parse_of(args).unwrap_err();

    // ⚠ The SIBLING that is easiest to get wrong: `gaps` takes the same filters as `ls` and
    // deliberately not this annotation — every line it prints is about an ABSENCE.
    let gaps = err(&["hist", "gaps", "--class"]);
    assert!(gaps.contains("--class"), "{gaps}");
    assert!(gaps.contains("does NOT"), "the absence reason: {gaps}");
    assert!(gaps.contains("data hist ls --class"), "…and where to go: {gaps}");

    let coverage = err(&["hist", "coverage", "--class"]);
    assert!(coverage.contains("--class"), "{coverage}");
    assert!(coverage.contains("TICK kinds"), "the MEASURED reason: {coverage}");
    assert!(coverage.contains("data hist ls --class"), "…and where to go: {coverage}");

    let health = err(&["hist", "health", "--class"]);
    assert!(health.contains("inventory"), "{health}");

    let uni = err(&["hist", "universe", "--class"]);
    assert!(uni.contains("as of NOW") || uni.contains("as of the window"), "{uni}");

    for args in [
        &["hist", "fetch", "binance:BTCUSDT:1h", "--days", "2", "--class"][..],
        &["hist", "fetch", "--source", "demo", "--class"][..],
        &["hist", "fetch", "--source", "starter", "--class"][..],
        &["hist", "export", "demo:X:1h", "--out", "o.parquet", "--class"][..],
        &["hist", "rm", "--kind", "bar", "--venue", "demo", "--class"][..],
        &[
            "hist",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "demo",
            "--symbol",
            "X",
            "--interval",
            "1h",
            "--class",
        ][..],
    ] {
        let e = err(args);
        assert!(e.contains("--class"), "{args:?} must refuse it by name: {e}");
    }
}

/// ⚠ **THE refusal this module exists to make loud.** `--store` on a read verb is the mistake
/// an operator makes first, because the sibling subcommand takes one — and a silently-ignored
/// `--store` would answer about a completely different store with no sign that it had. The
/// message must name the flag, the verb, and the flag that reaches the other store.
#[test]
fn a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store() {
    // ⚠ EVERY read subcommand, not just the two that shipped first. A verb that inherited
    // `is_read()` without inheriting this refusal would accept `--store` and answer about a
    // completely different store, which is the exact failure this test is named for.
    let mut exercised = 0;
    for sub in SUBCOMMANDS.iter().filter(|s| s.is_read()) {
        for (flag, value) in [("--store", "/srv/hist"), ("--engine", "/opt/backtest")] {
            let argv = ["hist", sub.as_str(), flag, value];
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("--addr"), "{argv:?} must point at --addr: {err}");
        }
        // ⚠ `--store` meets the ONE sentence every history reader prints — the same one
        // `export` prints, never a hand-spelled copy of its replacement — in every spelling,
        // including a trailing one with no directory, which would otherwise be asked for a
        // value that is then refused. The shared sentence LEADS, so it names the verb as typed.
        let shared = vike_datahub_client::flag_vocab::store_flag_removed(&format!(
            "data hist {}",
            sub.as_str()
        ));
        for store in [&["--store", "/srv/hist"][..], &["--store=/srv/hist"][..], &["--store"][..]] {
            let argv = [&["hist", sub.as_str()][..], store].concat();
            let err = parse_of(&argv).unwrap_err();
            assert!(err.starts_with(&shared), "{argv:?} must lead with the shared sentence: {err}");
            assert!(err.contains("VIKE_DATAHUB_STORE=DIR vike-backend datahub"), "{err}");
            assert!(!err.contains("docs/"), "no withheld path: {err}");
        }
        exercised += 1;
    }
    assert!(exercised >= 5, "the read roster lost its rows: {exercised}");
    // …and `export` gets the shared sentence EXACTLY, with no `--addr` clause: its `--addr`
    // selects a different route rather than a different source — see [`store_refusal`].
    assert_eq!(
        parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--store", "/s"]).unwrap_err(),
        vike_datahub_client::flag_vocab::store_flag_removed("data hist export")
    );
}

/// ⚠ **The WINDOW refusal moved out of the test above, and the move is the point.** `--days`
/// and `--from`/`--to` used to ride the store-side refusal and so were asserted to name
/// `--addr` — which was never apt for them: neither flag names a store, and pointing an
/// operator at `--addr` answers a question they did not ask. They bound a FETCH, and the
/// verbs that refuse them fold each series' WHOLE recorded span, so the useful thing to name
/// is the read verb whose question IS a window. See [`Sub::refuses_a_window`].
#[test]
fn a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one() {
    for sub in SUBCOMMANDS.iter().filter(|s| s.refuses_a_window()) {
        for (flag, value) in [("--days", "7"), ("--from", "0"), ("--to", "0")] {
            let argv = ["hist", sub.as_str(), flag, value];
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("universe"), "{argv:?} must name the verb: {err}");
        }
    }
}

/// …and `universe` ACCEPTS the pair, parses both bounds HERE, and keeps each one independent.
/// The parse is what makes it different from `export`'s forwarded strings — see
/// [`membership_window`].
#[test]
fn universe_takes_an_independent_pair_of_bounds_and_parses_them_here() {
    let a = parse_of(&["hist", "universe"]).unwrap();
    assert_eq!(
        a.universe_window,
        Some(universe::MembershipWindow::default()),
        "a bare `universe` is unbounded, not an error — the store's own span is the window"
    );

    let a = parse_of(&["hist", "universe", "--from", "2026-01-01"]).unwrap();
    let window = a.universe_window.expect("universe always carries a window");
    assert_eq!(window.from, Some(1_767_225_600_000), "YYYY-MM-DD resolves to UTC midnight");
    assert_eq!(window.to, None, "one bound stands alone, as on `export`");

    let a = parse_of(&["hist", "universe", "--to", "0"]).unwrap();
    let window = a.universe_window.expect("universe always carries a window");
    assert_eq!(window.from, None);
    assert_eq!(window.to, Some(0), "a bare epoch-ms stays an epoch-ms, including zero");
}

/// An unreadable bound is a USAGE error here rather than a silently-discarded flag, because
/// nothing is forwarded: the comparison happens in this process.
#[test]
fn an_unreadable_universe_bound_is_refused_by_name() {
    let err = parse_of(&["hist", "universe", "--from", "last-tuesday"]).unwrap_err();
    assert!(err.contains("--from"), "{err}");
    assert!(err.contains("last-tuesday"), "the message names what was typed: {err}");
    assert!(err.contains("YYYY-MM-DD"), "…and the spellings it wanted: {err}");
}

/// An INVERTED window is refused rather than swapped: it would report every instrument in the
/// store `absent`, which reads exactly like an empty store.
#[test]
fn an_inverted_universe_window_is_refused_rather_than_swapped() {
    let err =
        parse_of(&["hist", "universe", "--from", "2026-06-01", "--to", "2026-01-01"]).unwrap_err();
    assert!(err.contains("--from") && err.contains("--to"), "{err}");
    assert!(err.contains("absent"), "…and what it would have produced: {err}");
}

/// `--days` is refused on `universe` for a reason of its OWN, and the message has to carry it:
/// a window counted back from now answers a different question every day it is run, and a
/// point-in-time universe exists to be re-askable.
#[test]
fn days_is_refused_on_universe_with_the_re_askability_reason() {
    let err = parse_of(&["hist", "universe", "--days", "30"]).unwrap_err();
    assert!(err.contains("--days"), "{err}");
    assert!(err.contains("--from"), "…and the spelling that works: {err}");
    assert!(err.contains("NOW"), "…and why: {err}");
}

/// The two new read verbs refuse the ABSENCE flag, and the message says which verb answers
/// absence — `health`'s whole distinction is present-and-impossible versus missing.
#[test]
fn the_new_read_verbs_refuse_the_absence_flags_and_name_the_verbs_that_answer_absence() {
    for sub in ["health", "universe"] {
        let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
        assert!(err.contains("--partial-only"), "{sub}: {err}");
        assert!(err.contains("hist gaps"), "{sub} must name the verb that answers: {err}");
    }
}

/// **THE OUTPUT DOOR.** `--format` is the axis, `--json` is its shorthand, the two are refused
/// only where they DISAGREE, and every value this plane names but this verb does not serve is
/// refused BY NAME.
///
/// ⚠ **[`UNBUILT_FORMATS`] IS EMPTY NOW, and the loop over it was therefore VACUOUS** — it
/// asserted three things about every row of a roster with no rows, which passes whatever the
/// refusals say. That is the shape this suite exists to catch, so the roster's emptiness is
/// asserted as a CLAIM (with the reason a row would be added back) and each of the three
/// departed values is exercised through the arm that actually refuses it:
///
/// * `jsonl` — BUILT, on [`ROW_VERB`]. Refused here because a catalog is not rows.
/// * `csv`/`parquet` — BUILT, on [`FILE_VERB`]. Refused here because this verb PRINTS.
///
/// None of the three may read as "waiting on a phase", which is what each of them said once.
#[test]
fn the_format_axis_carries_json_and_refuses_the_other_verbs_formats_by_name() {
    assert!(!parse_of(&["hist", "ls"]).unwrap().json, "table is the default");
    assert!(parse_of(&["hist", "ls", "--format", "json"]).unwrap().json);
    assert!(!parse_of(&["hist", "ls", "--format", "table"]).unwrap().json);
    assert!(parse_of(&["hist", "ls", "--json"]).unwrap().json, "the shorthand still works");
    assert!(
        parse_of(&["hist", "ls", "--json", "--format", "json"]).unwrap().json,
        "agreeing spellings are not a contradiction"
    );

    // ...and the ONE way they can disagree is refused rather than resolved.
    let err = parse_of(&["hist", "ls", "--json", "--format", "table"]).unwrap_err();
    assert!(err.contains("--json") && err.contains("--format table"), "{err}");
    assert!(err.contains("pass one"), "…and what to do: {err}");

    // THE ROSTER'S EMPTINESS AS A CLAIM. Every value it once held is now written by some verb
    // of this plane, so there is nothing left that is "designed but not built". A row added
    // back is a value nothing writes, and it gets the arm that names what it is waiting on.
    assert!(
        UNBUILT_FORMATS.is_empty(),
        "every value this roster held is now WRITTEN by a verb; a row here must name what it \
             is waiting on: {UNBUILT_FORMATS:?}"
    );
    // ...but the loop over it is vacuous, so each departed value is exercised through its own
    // arm — and none of the three may read as waiting on a phase.
    for (name, needle) in [("jsonl", ROW_VERB), ("csv", FILE_VERB), ("parquet", FILE_VERB)] {
        let err = parse_of(&["hist", "ls", "--format", name]).unwrap_err();
        assert!(err.contains(name), "{name}: {err}");
        assert!(err.contains(needle), "{name} must name the verb that serves it: {err}");
        assert!(!err.contains("not built"), "{name} SHIPS: {err}");
        // ⚠ …and must not send the operator to a LOCAL store for it: `export` reads through a
        // datahub on both routes since 2026-09-26 (decision 0084's amendment), and this
        // message said `parquet` came "from a store on this machine" for a day after that.
        if needle == FILE_VERB {
            assert!(!err.contains("on this machine"), "{name}: no local-store route: {err}");
            assert!(err.contains("datahub"), "{name}: names where export reads: {err}");
        }
    }
    // ⚠ THE ANTI-VACUITY CONTROL for all three at once: each is genuinely ACCEPTED somewhere,
    // so the refusals above are about THIS verb rather than about a value nothing serves.
    parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", "jsonl"])
        .expect("`get` is the verb that serves jsonl on stdout");
    parse_of(&[
        "hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "csv", "--from",
        "1", "--to", "2",
    ])
    .expect("`export --addr` is the verb that writes csv");
    parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--format", "parquet"])
        .expect("`export` is the verb that writes parquet");

    // An unrecognised one IS a spelling mistake, and says so — unlike `--source`, whose roster
    // lives in a process this crate cannot see.
    let err = parse_of(&["hist", "ls", "--format", "yaml"]).unwrap_err();
    assert!(err.contains("unknown") && err.contains("table | json"), "{err}");
    assert!(parse_of(&["hist", "ls", "--format", ""]).unwrap_err().contains("EMPTY"));

    // The axis is on EVERY verb, not just the read half — a `fetch` report is a document too.
    assert!(parse_of(&["hist", "fetch", "--source", "demo", "--format", "json"]).unwrap().json);
}

/// **THE PROMOTION.** `gaps` is a verb, `--gaps` is refused on EVERY verb by name, and the
/// refusal names the verb that replaced it.
///
/// ⚠ The `ls` row is the one that matters. A refusal that fired only on the siblings would
/// leave the ONE verb that used to accept the flag answering `unknown option '--gaps'` — which
/// tells an operator the flag never existed and sends them to check their spelling. Every verb
/// is asserted rather than a sample, for [`RETIRED_SPELLINGS`]'s reason one layer up.
#[test]
fn gaps_is_a_verb_and_the_retired_flag_is_refused_everywhere_by_name() {
    let a = parse_of(&["hist", "gaps"]).unwrap();
    assert_eq!(a.sub, Sub::Gaps);
    assert!(a.gaps, "the verb IS the probe");
    assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so it resolves a datahub");

    for sub in SUBCOMMANDS {
        // Each verb needs its own minimum argv before the flag can be judged, so the refusal
        // is reached with the line that verb would otherwise accept.
        let mut argv: Vec<&str> = match sub {
            Sub::Fetch => vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "1"],
            Sub::Export => vec!["hist", "export", "demo:X:1h", "--out", "o.parquet"],
            Sub::Rm => vec!["hist", "rm", "--kind", "bar", "--venue", "demo"],
            Sub::Repair => {
                vec!["hist", "repair", "--kind", "bar", "--venue", "demo", "--symbol", "X"]
            }
            other => vec!["hist", other.as_str()],
        };
        argv.push("--gaps");
        let err = parse_of(&argv).unwrap_err();
        assert!(!err.contains("unknown option"), "{argv:?} must not read as a typo: {err}");
        assert!(err.contains("--gaps"), "{argv:?} must name the flag: {err}");
        assert!(
            err.contains("data hist gaps"),
            "{argv:?} must name the verb that replaced it: {err}"
        );
    }
}

/// `--kind` is ACCEPTED on `tape-health` and on `universe` while `coverage` refuses it, and the
/// asymmetry is deliberate: a coverage row IS the join across kinds, while a universe narrowed
/// to `--kind bar` is exactly the set a bar-driven profile reads.
#[test]
fn kind_narrows_the_new_read_verbs_while_coverage_still_refuses_it() {
    for sub in ["health", "universe"] {
        let a = parse_of(&["hist", sub, "--kind", "bar"]).unwrap();
        assert_eq!(a.filter.kind.as_deref(), Some("bar"), "{sub}");
    }
    assert!(parse_of(&["hist", "coverage", "--kind", "bar"]).is_err());
}

/// …and the mirror image: a datahub flag on the write half is refused rather than ignored,
/// naming the half it belongs to. Ignoring one would let `data fetch --addr the CI box:7878` read
/// as "fetch into the remote store", which is not a thing this verb can do.
///
/// ⚠ **`export` used to be in this loop and has been split out, deliberately.** `--addr` and
/// `--kind` are that verb's own now (the route switch and the row shape), so the blanket
/// sentence would be a true no for a false reason. The LISTING aids still are foreign there and
/// still refuse — with a sentence of their own, which is what the second half asserts.
///
/// ⚠ **`--addr` LEFT this loop too, on `fetch` ITSELF — D1 of the 0094 follow-ups, and the same
/// shape as `export`'s departure above.** It used to share this loop's read-half sentence on
/// every source, `--source starter|demo` included — the "`fetch` KEEPS both of the rows `export`
/// took back" comment this replaced was the control for that claim, and D1 is exactly what makes
/// it false: a VENUE fetch's `--addr` is now its own route (it never reaches this refusal, or any
/// refusal, at all — see `a_venue_fetch_keeps_addr_and_an_engine_source_still_refuses_it`), and
/// `--source starter|demo` refuses it with a DIFFERENT sentence of its own (that source has no
/// REMOTE route, not that the flag belongs to the read half) — asserted separately below, the
/// same split the export section below argues for its own two rows.
#[test]
fn a_datahub_flag_on_a_write_verb_is_refused_and_names_the_read_half() {
    for (argv, flag) in [
        (vec!["hist", "fetch", "--source", "demo", "--class"], "--class"),
        (vec!["hist", "fetch", "--source", "demo", "--venue", "binance"], "--venue"),
        (vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--name", "BTC"], "--name"),
        (vec!["hist", "fetch", "--source", "demo", "--kind", "bar"], "--kind"),
    ] {
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
        assert!(err.contains("READ half"), "{argv:?} must name the half it belongs to: {err}");
    }

    // `--addr` on an engine source is still refused, but no longer with the read half's
    // sentence — see the doc comment above for why, and
    // `a_venue_fetch_keeps_addr_and_an_engine_source_still_refuses_it` for the VENUE half of the
    // split this is the other half of.
    let err = parse_of(&["hist", "fetch", "--source", "demo", "--addr", "h:1"]).unwrap_err();
    assert!(err.contains("--addr") && err.contains("REMOTE"), "{err}");
    // ANTI-VACUITY: it is a DIFFERENT sentence, which is the whole point of the split.
    assert!(
        !err.contains("READ half"),
        "an engine source's --addr refusal must not borrow the read half's reason: {err}"
    );

    // `export` refuses the four LISTING aids too, with the sentence that is true of IT.
    for flag in ["--venue", "--name"] {
        let err = parse_of(&["hist", "export", "d:S:1h", "--out", "o", flag, "x"]).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
        assert!(err.contains("names ONE series"), "{flag}: {err}");
        // ANTI-VACUITY: it is a DIFFERENT sentence, which is the whole point of the split.
        assert!(!err.contains("READ half"), "{flag} must not inherit fetch's reason: {err}");
    }
}

/// The intra-half refusals, each about a UNIT rather than about tidiness — see the arms in
/// [`parse`]. `--kind` on `coverage` would filter away the disagreement the report exists to
/// show; `--partial-only` on a per-series listing has no cross-kind verdict to filter.
#[test]
fn each_read_verb_refuses_the_others_flag_with_the_reason() {
    for sub in ["ls", "gaps"] {
        let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
        assert!(err.contains("--partial-only") && err.contains("coverage"), "{sub}: {err}");
    }

    let err = parse_of(&["hist", "coverage", "--kind", "trade"]).unwrap_err();
    assert!(err.contains("--kind") && err.contains("across kinds"), "{err}");
}

/// A colon-string on a read verb is refused with the REASON: a stored series is four
/// dimensions with an alternative inside them, and no `VENUE:SYMBOL:INTERVAL` can spell one.
/// It is the single likeliest thing to type after using `fetch`.
#[test]
fn a_read_verb_refuses_a_fetch_shaped_spec_and_says_why() {
    let err = parse_of(&["hist", "ls", "binance:BTCUSDT:1h"]).unwrap_err();
    assert!(err.contains("binance:BTCUSDT:1h"), "the message names what was typed: {err}");
    assert!(err.contains("--kind"), "…and the flags that do narrow a listing: {err}");
}

// ---- the filter ----

/// Case-insensitive SUBSTRING, ANDed, and an absent dimension matches everything. The grouped
/// case is the load-bearing one: `--name` matches a group, which is why it is not `--symbol`.
#[test]
fn the_filter_is_case_insensitive_substring_and_reaches_a_group_by_name() {
    let f = |args: &[&str]| parse_of(args).unwrap().filter;

    let all = Filter::default();
    assert!(all.matches(Some("bar"), "binance", "BTCUSDT"));
    assert!(all.is_empty());

    let by_name = f(&["hist", "ls", "--name", "btc"]);
    assert!(by_name.matches(Some("bar"), "binance", "BTCUSDT"), "case-insensitive substring");
    assert!(!by_name.matches(Some("bar"), "binance", "ETHUSDT"));
    assert!(!by_name.is_empty());

    // A GROUPED series' label is its group; `--name fam` must reach it.
    let grouped = f(&["hist", "ls", "--name", "FAM"]);
    assert!(grouped.matches(Some("trade"), "polymarket", "fam"));

    // ANDed: every named dimension has to agree.
    let both = f(&["hist", "ls", "--kind", "bar", "--venue", "okx"]);
    assert!(both.matches(Some("bar"), "okx", "BTC-USDT"));
    assert!(!both.matches(Some("trade"), "okx", "BTC-USDT"));
    assert!(!both.matches(Some("bar"), "binance", "BTCUSDT"));

    // A row with NO kind dimension (the `coverage` caller) passes the kind test — and `--kind`
    // cannot reach that path at all, because `parse` refuses it there.
    assert!(f(&["hist", "ls", "--kind", "bar"]).matches(None, "binance", "BTCUSDT"));
}

// ---- the `list` rendering ----

/// ⚠ The identity is rendered as COLUMNS, never as a colon-string: `kind` and a `SCOPE` cell
/// are their own cells, so a grouped series reads as a group rather than as a venue with two
/// empties after it.
#[test]
fn list_lines_render_four_dimensions_and_never_a_colon_string() {
    let rows = [bar_row(), grouped_row()];
    let lines = list_lines(&rows, 2, false, false, false);

    let cells =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
    assert_eq!(cells(&lines[0])[..5], ["KIND", "VENUE", "SCOPE", "NAME", "INTERVAL"]);
    assert_eq!(cells(&lines[1])[..7], ["bar", "binance", "symbol", "BTCUSDT", "1h", "48", "2"]);
    // The grouped row: its NAME comes from the group, and its interval is genuinely absent
    // rather than an empty cell — the two things a colon-string identity gets wrong.
    assert_eq!(cells(&lines[2])[..7], ["trade", "polymarket", "group", "fam", "-", "7", "2"]);
    // …and the columns actually line up, which is what makes the table skimmable at all.
    let scope_col = lines[0].find("SCOPE").expect("the header names the column");
    assert_eq!(lines[1].find("symbol"), Some(scope_col), "{}", lines[1]);
    assert_eq!(lines[2].find("group"), Some(scope_col), "{}", lines[2]);
    for line in &lines {
        assert!(!line.contains("binance:BTCUSDT"), "no colon-string identity: {line}");
    }
    assert_eq!(lines.last().unwrap(), "2 series · 55 rows");
}

/// A filtered listing says how many the SERVER reported beside how many survived — otherwise
/// "3 series" is unreadable without knowing whether it was 3 of 3 or 3 of 3000.
#[test]
fn a_filtered_listing_counts_both_sides() {
    let lines = list_lines(&[bar_row()], 9, true, false, false);
    assert_eq!(lines.last().unwrap(), "1 of 9 series · 48 rows");
}

/// A zero-row series renders `-` for its span rather than a well-formed `1970-01-01` folded
/// from an all-zero coverage — while [`list_json`] keeps the store's own numbers untouched.
#[test]
fn a_zero_row_series_shows_a_dash_span_and_the_document_still_carries_the_zeroes() {
    let mut row = bar_row();
    row.coverage = Coverage::default();
    let lines = list_lines(std::slice::from_ref(&row), 1, false, false, false);
    assert!(!lines[1].contains("1970"), "a sentinel span is not a date: {}", lines[1]);
    let cells: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(&cells[cells.len() - 2..], ["-", "-"], "both span cells: {}", lines[1]);

    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, std::slice::from_ref(&row), 1)).unwrap();
    assert_eq!(doc["series"][0]["coverage"]["first_ts"], 0, "the document is not edited");
    assert_eq!(doc["series"][0]["coverage"]["rows"], 0);
}

/// All THREE gap outcomes are said out loud. An unasked question, a clean series and a probe
/// that failed must not share a rendering — the middle one is what an operator ran `gaps`
/// to learn.
#[test]
fn gap_lines_distinguish_holes_from_none_from_unanswerable() {
    let mut row = bar_row();
    assert!(gap_lines(&row).is_empty(), "an `ls` row carries no annotation at all");

    row.gaps = Some(Vec::new());
    assert_eq!(gap_lines(&row), vec!["      no gaps".to_string()]);

    row.gaps = Some(vec![(86_400_000, 172_800_000)]);
    assert_eq!(gap_lines(&row), vec!["      gap 1970-01-02 .. 1970-01-03".to_string()]);

    row.gaps = None;
    row.gaps_error = Some("manifest unreadable".to_string());
    let lines = gap_lines(&row);
    assert!(lines[0].contains("gaps unavailable"), "{lines:?}");
    assert!(lines[0].contains("manifest unreadable"), "the server's own words: {lines:?}");
}

/// The `--gaps` degrade, seen through the renderer that carries it: the LISTING still renders
/// in full and the failure lands on the row it belongs to.
#[test]
fn a_failed_gap_probe_annotates_its_row_and_leaves_the_listing_whole() {
    let mut broken = bar_row();
    broken.gaps_error = Some("series_gaps: manifest unreadable".to_string());
    let mut clean = grouped_row();
    clean.gaps = Some(Vec::new());

    let lines = list_lines(&[broken, clean], 2, false, true, false);
    assert!(lines.iter().any(|l| l.contains("gaps unavailable")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("no gaps")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row itself survives: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("fam")), "{lines:?}");
}

// ---- `--class`: the reader `SymbolProperties::asset_class` did not have ----

/// Every one of the five probe outcomes renders as its own WORD, and none of them as a blank.
///
/// ⚠ **What would make this fail, and why it matters more than it looks:** folding
/// `unclassified` and `no-properties` into one cell. They are the two halves of "there is no
/// class here" and they send an operator to different places — the venue's producer, or the
/// recorder that never ran for this instrument. `vike_model::SymbolProperties::asset_class`
/// keeps them apart in the store by being an `Option` at all; a renderer that collapsed them
/// would undo that on the last hop.
#[test]
fn every_class_outcome_has_its_own_word_and_none_of_them_is_blank() {
    let cell = |probe: Option<ClassProbe>| {
        let mut row = bar_row();
        row.class = probe;
        class_cell(&row)
    };

    assert_eq!(cell(Some(ClassProbe::Classified("CryptoPerp"))), "CryptoPerp");
    assert_eq!(cell(Some(ClassProbe::Unclassified)), "unclassified");
    assert_eq!(cell(Some(ClassProbe::Unrecorded)), "no-properties");
    assert_eq!(cell(Some(ClassProbe::Grouped)), "(group)");
    assert_eq!(cell(Some(ClassProbe::Failed("boom".into()))), "(error)");
    assert_eq!(cell(None), "-", "the unreachable arm is still a cell, never a panic");

    // The two that mean "no class" are DIFFERENT words — the assertion the fold above would
    // break, stated on its own so the failure names the defect rather than a string.
    assert_ne!(
        cell(Some(ClassProbe::Unclassified)),
        cell(Some(ClassProbe::Unrecorded)),
        "a producer that named no class and a recorder that never ran are different findings"
    );
}

/// The class word is the MODEL's own spelling, taken from `vike_model::AssetClass::sql_word`
/// (which is also its serde word) — never a lowercase or prettified second form minted here.
/// A third spelling of one vocabulary is the exact defect `crates/vike-model/src/asset_class.rs`
/// opens its module doc with.
#[test]
fn the_rendered_class_word_is_the_models_own_spelling() {
    for class in vike_model::AssetClass::ALL {
        let mut row = bar_row();
        row.class = Some(ClassProbe::Classified(class.sql_word()));
        assert_eq!(class_cell(&row), class.sql_word());
    }
}

/// The CLASS column appears ONLY under `--class`, and without it every line is byte-identical
/// to the listing this verb rendered before the flag existed.
///
/// ⚠ That equality is the point of the test, not tidiness: `list_lines` grew a width that is
/// zero in the unasked case, and a regression there would silently add trailing whitespace to
/// every row of every `data hist list` anybody has ever piped into a diff.
#[test]
fn the_class_column_is_absent_unless_asked_and_changes_nothing_when_it_is() {
    let mut classified = bar_row();
    classified.class = Some(ClassProbe::Classified("CryptoPerp"));
    let mut grouped = grouped_row();
    grouped.class = Some(ClassProbe::Grouped);
    let rows = [classified, grouped];

    let without = list_lines(&rows, 2, false, false, false);
    assert!(!without[0].contains("CLASS"), "no header without the flag: {}", without[0]);
    assert!(!without[1].contains("CryptoPerp"), "…and no cell either: {}", without[1]);
    // The rows carry a probe, so this proves the RENDERER is gated and not merely the caller.
    assert_eq!(
        without,
        list_lines(&[bar_row(), grouped_row()], 2, false, false, false),
        "an unasked class may not change one byte of the listing"
    );

    let with = list_lines(&rows, 2, false, false, true);
    assert!(with[0].trim_end().ends_with("CLASS"), "the column is last: {}", with[0]);
    assert!(with[1].ends_with("CryptoPerp"), "{}", with[1]);
    assert!(with[2].ends_with("(group)"), "a grouped row was never asked: {}", with[2]);
    // The cells sit UNDER the header — the whole reason `last_w` is measured rather than
    // assumed, since a `LAST` column of varying width would otherwise ragged the one after it.
    let class_col = with[0].find("CLASS").expect("the header names the column");
    assert!(with[1][class_col..].starts_with("CryptoPerp"), "aligned: {}", with[1]);
    assert!(with[2][class_col..].starts_with("(group)"), "…both rows: {}", with[2]);
}

/// A failed probe degrades the ROW and leaves the listing whole — the same contract the gap
/// probe has, and the reason both are annotations rather than run failures.
#[test]
fn a_failed_class_probe_annotates_its_row_and_leaves_the_listing_whole() {
    let mut broken = bar_row();
    broken.class = Some(ClassProbe::Failed("properties_as_of: store unreadable".to_string()));
    let mut fine = grouped_row();
    fine.class = Some(ClassProbe::Unclassified);

    let lines = list_lines(&[broken, fine], 2, false, false, true);
    assert!(lines.iter().any(|l| l.contains("class unavailable")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("store unreadable")), "the server's words: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row survives: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("unclassified")), "…and so does its sibling");

    // Only the FAILURE gets a line — every other verdict is already a word in the row, which is
    // the asymmetry with `gap_lines` and the reason it is written down there.
    for probe in [ClassProbe::Classified("Fx"), ClassProbe::Unclassified, ClassProbe::Grouped] {
        let mut row = bar_row();
        row.class = Some(probe);
        assert!(class_error_line(&row).is_empty(), "{:?} needs no line", row.class);
    }
}

/// The `--json` wire: `asset_class_status` names WHICH answer each row got, and `asset_class`
/// is non-null for EXACTLY the `classified` verdict.
///
/// ⚠ This is the compatibility assertion the module doc argues for. Without the status field a
/// `null` class would mean four different things at once — not asked, no grid, a grid naming no
/// class, and a failed probe — and the last three are what an operator acts on differently.
#[test]
fn the_class_fields_are_a_status_and_a_word_that_cannot_disagree() {
    let args = parse_of(&["hist", "ls", "--class", "--json"]).unwrap();
    let mut rows = Vec::new();
    for probe in [
        ClassProbe::Classified("CryptoPerp"),
        ClassProbe::Unclassified,
        ClassProbe::Unrecorded,
        ClassProbe::Grouped,
        ClassProbe::Failed("store unreadable".to_string()),
    ] {
        let mut row = bar_row();
        row.class = Some(probe);
        rows.push(row);
    }
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, &rows, rows.len())).unwrap();

    assert_eq!(doc["class_requested"], true);
    let series = doc["series"].as_array().expect("an array of rows");
    let statuses: Vec<&str> =
        series.iter().map(|s| s["asset_class_status"].as_str().unwrap()).collect();
    assert_eq!(
        statuses,
        ["classified", "unclassified", "unrecorded", "grouped", "error"],
        "one status per outcome, and all five distinct"
    );
    for row in series {
        let classified = row["asset_class_status"] == "classified";
        assert_eq!(
            !row["asset_class"].is_null(),
            classified,
            "asset_class is non-null for exactly the classified verdict: {row}"
        );
        assert_eq!(
            !row["asset_class_error"].is_null(),
            row["asset_class_status"] == "error",
            "…and asset_class_error for exactly the error one: {row}"
        );
    }
    assert_eq!(series[0]["asset_class"], "CryptoPerp");
    assert_eq!(series[4]["asset_class_error"], "store unreadable");
}

/// WITHOUT `--class` the three keys are present and null on every row and `class_requested` is
/// false — the shape `gaps`/`gaps_requested` already has, so the addition is ADDITIVE for a
/// consumer that predates it and needs no new convention from one that does not.
#[test]
fn an_unasked_class_is_null_on_the_wire_and_says_it_was_not_asked() {
    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
    assert_eq!(doc["class_requested"], false);
    let row = &doc["series"][0];
    for key in ["asset_class", "asset_class_status", "asset_class_error"] {
        assert!(row.get(key).is_some(), "{key} is a KEY, so a consumer can read it uniformly");
        assert!(row[key].is_null(), "…and null, because nothing was asked: {row}");
    }
}

/// The document carries the RAW `symbol` and `group` beside the resolved `name`/`grouped`, so
/// a machine reader gets the identity rather than this side's reading of it — and the gap
/// ranges are objects rather than positional pairs.
#[test]
fn list_json_carries_the_raw_identity_beside_the_resolved_name() {
    let mut per_symbol = bar_row();
    per_symbol.gaps = Some(Vec::new());
    let mut grouped = grouped_row();
    grouped.gaps = Some(vec![(0, 86_400_000)]);
    // ⚠ `gaps`, not `ls --gaps`: the probe is DERIVED from the verb now, so this is also the
    // assertion that `gaps_requested` still follows it.
    let args = parse_of(&["hist", "gaps", "--venue", "poly", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, &[per_symbol, grouped], 5)).unwrap();

    assert_eq!(doc["subcommand"], "gaps");
    assert_eq!(doc["addr"], DEFAULT_ADDR);
    assert_eq!(doc["filter"]["venue"], "poly");
    assert!(doc["filter"]["kind"].is_null(), "an unset filter dimension is null, not absent");
    assert_eq!(doc["gaps_requested"], true);
    assert_eq!(doc["series_reported"], 5);
    assert_eq!(doc["count"], 2);

    let per_symbol = &doc["series"][0];
    assert_eq!(per_symbol["kind"], "bar");
    assert_eq!(per_symbol["name"], "BTCUSDT");
    assert_eq!(per_symbol["symbol"], "BTCUSDT");
    assert!(per_symbol["group"].is_null());
    assert_eq!(per_symbol["grouped"], false);
    assert_eq!(per_symbol["interval"], "1h");
    assert_eq!(per_symbol["coverage"]["rows"], 48);
    assert!(per_symbol["gaps"].as_array().expect("the verb asked for them").is_empty());

    // ⚠ The grouped row is where a `VENUE:SYMBOL:INTERVAL` document would fall apart: its
    // symbol is EMPTY and its name comes from the group.
    let grouped = &doc["series"][1];
    assert_eq!(grouped["name"], "fam");
    assert_eq!(grouped["symbol"], "");
    assert_eq!(grouped["group"], "fam");
    assert_eq!(grouped["grouped"], true);
    assert!(grouped["interval"].is_null());
    assert_eq!(grouped["gaps"][0]["from_ts"], 0);
    assert_eq!(grouped["gaps"][0]["to_ts"], 86_400_000);
}

/// A series whose gaps were NOT asked for carries `null`, never an empty array — "nobody
/// asked" and "this series has no holes" are different facts and a fold over them differs.
#[test]
fn an_unasked_gap_field_is_null_rather_than_an_empty_array() {
    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
    assert!(doc["series"][0]["gaps"].is_null());
    assert!(doc["series"][0]["gaps_error"].is_null());
    assert_eq!(doc["gaps_requested"], false);
}

/// The EMPTY answer's two causes are told apart, in both verbs' nouns.
#[test]
fn the_empty_answer_separates_an_empty_store_from_an_over_narrow_filter() {
    assert_eq!(
        list_lines(&[], 0, false, false, false),
        vec!["the datahub reported no series at all"]
    );
    assert_eq!(
        list_lines(&[], 12, true, false, false),
        vec!["no series match the filter (12 reported)"]
    );
    assert_eq!(coverage_lines(&[], 0, false), vec!["the datahub reported no instruments at all"]);
    assert_eq!(coverage_lines(&[], 4, true), vec!["no instruments match the filter (4 reported)"]);
}

// ---- the `coverage` rendering ----

/// One instrument with a real disagreement (`trade` on every day, `depth` missing one) and one
/// that is complete — the two dispositions the report separates.
fn coverage_rows() -> Vec<InstrumentRow> {
    vec![
        InstrumentRow {
            venue: "binance".to_string(),
            name: "BTCUSDT".to_string(),
            grouped: false,
            kinds: vec![
                KindRow { kind: "trade".to_string(), days: 3 },
                KindRow { kind: "depth".to_string(), days: 2 },
            ],
            spanned_days: 3,
            partial: vec![PartialRow {
                day: 2,
                start_ms: 172_800_000,
                missing: vec!["depth".to_string()],
            }],
        },
        InstrumentRow {
            venue: "polymarket".to_string(),
            name: "fam".to_string(),
            grouped: true,
            kinds: vec![KindRow { kind: "trade".to_string(), days: 2 }],
            spanned_days: 2,
            partial: Vec::new(),
        },
    ]
}

#[test]
fn coverage_lines_line_the_kinds_up_and_detail_only_the_partial_days() {
    let lines = coverage_lines(&coverage_rows(), 2, false);
    assert!(lines[0].contains("INSTRUMENT") && lines[0].contains("PARTIAL"), "{}", lines[0]);
    assert!(lines[1].contains("trade:3, depth:2"), "{}", lines[1]);
    assert!(lines[1].ends_with("  1"), "the partial COUNT is on the row: {}", lines[1]);
    assert_eq!(lines[2], "      1970-01-03  missing: depth");
    // The complete instrument gets its row and NO detail lines — a "nothing missing" line
    // under every healthy instrument is how a report teaches an operator to skim past it.
    assert!(lines[3].contains("fam") && lines[3].ends_with("  -"), "{}", lines[3]);
    assert_eq!(lines.last().unwrap(), "2 instruments · 1 with partial days");
}

/// The human table CAPS the day detail and says how many it withheld; the document does not
/// cap at all (see [`MAX_PARTIAL_DAYS_SHOWN`]).
#[test]
fn the_human_table_caps_the_partial_days_while_the_document_carries_them_all() {
    let mut rows = coverage_rows();
    rows[0].partial = (0..MAX_PARTIAL_DAYS_SHOWN as i64 + 2)
        .map(|d| PartialRow {
            day: d,
            start_ms: d * 86_400_000,
            missing: vec!["depth".to_string()],
        })
        .collect();

    let lines = coverage_lines(&rows, 2, false);
    let detail = lines.iter().filter(|l| l.contains("missing:")).count();
    assert_eq!(detail, MAX_PARTIAL_DAYS_SHOWN, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("… and 2 more partial days")), "{lines:?}");

    let args = parse_of(&["hist", "coverage", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&coverage_json(&args, &rows, 2)).unwrap();
    assert_eq!(
        doc["instruments"][0]["partial_days"].as_array().unwrap().len(),
        MAX_PARTIAL_DAYS_SHOWN + 2,
        "a truncated array would be a wrong answer, not a long one"
    );
}

/// The document's `complete` flag is computed from the same list it ships, and a partial day
/// carries its day INDEX, its epoch-ms midnight and the rendered date — the index is what the
/// store indexes by, the ms is what everything else derives from.
#[test]
fn coverage_json_carries_the_verdict_and_both_spellings_of_a_day() {
    let args = parse_of(&["hist", "coverage", "--partial-only", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&coverage_json(&args, &coverage_rows(), 7)).unwrap();

    assert_eq!(doc["subcommand"], "coverage");
    assert_eq!(doc["partial_only"], true);
    assert_eq!(doc["instruments_reported"], 7);
    assert_eq!(doc["count"], 2);

    let partial = &doc["instruments"][0];
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["venue"], "binance");
    assert_eq!(partial["spanned_days"], 3);
    assert_eq!(partial["kinds"][0], serde_json::json!({ "kind": "trade", "days": 3 }));
    assert_eq!(partial["partial_days"][0]["day"], 2);
    assert_eq!(partial["partial_days"][0]["start_ms"], 172_800_000);
    assert_eq!(partial["partial_days"][0]["date"], "1970-01-03");
    assert_eq!(partial["partial_days"][0]["missing_kinds"][0], "depth");

    let complete = &doc["instruments"][1];
    assert_eq!(complete["complete"], true);
    assert_eq!(complete["grouped"], true, "a grouped instrument says so");
    assert!(complete["partial_days"].as_array().unwrap().is_empty());
}

/// `gate`'s whole line, resolved: the spec becomes a SELECTOR, the duration becomes
/// milliseconds, and the kind roster is never empty. Everything downstream is then a pure fold
/// with no parse left in it — see [`GateArgs`].
#[test]
fn a_gate_line_resolves_its_spec_its_duration_and_its_kind_roster() {
    let a = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "365",
        "--max-gap",
        "4h",
    ])
    .expect("a well-formed gate");
    let g = a.gate.as_ref().expect("a GateArgs");
    assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");
    assert_eq!(g.require_days, 365);
    assert_eq!(g.max_gap_ms, Some(4 * 3_600_000));
    assert_eq!(g.kinds, vec!["bar".to_string()], "defaulted, never empty");
    assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so the addr is resolved");
    assert!(a.spec.is_none(), "the spec is MOVED into GateArgs, never left in both");

    // Repeatable, deduped, and the default is replaced rather than added to.
    let a = parse_of(&[
        "hist",
        "gate",
        "polymarket:@election-2026",
        "--require-days",
        "7",
        "--require-kind",
        "book",
        "--require-kind",
        "trade",
        "--require-kind",
        "book",
    ])
    .expect("a grouped gate");
    let g = a.gate.as_ref().expect("a GateArgs");
    assert_eq!(g.kinds, vec!["book".to_string(), "trade".to_string()]);
    assert_eq!(g.max_gap_ms, None, "the holes were not asked about");
    assert!(g.spec.grouped, "the @ reached the selector: {:?}", g.spec);
}

/// The two refusals that keep a gate from asserting nothing: no spec, and no criterion. Each
/// names what to type instead, so the refusal is never a dead end.
///
/// ⚠ **The criterion refusal used to advertise `--require-days 1` as "the PRESENCE-only
/// spelling", and that label was FALSE.** `judge_days` compares `span_ms >= 86_400_000`, so a
/// series that exists and is six hours old breaches it — an operator who wrote the advertised
/// line into an `ExecStartPre=` had a unit refusing to start over exactly the tape it had just
/// fetched. There is no presence-only spelling, and the message says so rather than implying
/// one.
#[test]
fn a_gate_with_no_subject_or_no_criterion_is_refused_at_the_door() {
    let e = parse_of(&["hist", "gate", "--require-days", "30"]).expect_err("no spec");
    assert!(e.contains("gate needs a spec"), "{e}");
    assert!(e.contains("VENUE:@GROUP"), "…and names the grouped spelling too: {e}");

    let e = parse_of(&["hist", "gate", "binance:BTCUSDT:1h"]).expect_err("no criterion");
    assert!(e.contains("--require-days"), "{e}");
    assert!(e.contains("checked nothing"), "{e}");
    assert!(e.contains("no PRESENCE-only spelling"), "the label that was false is gone: {e}");
    assert!(e.contains("`--require-days 1`"), "…and the narrowest gate is still named: {e}");
    assert!(e.contains("WHOLE DAY"), "…with what it actually asserts: {e}");
}

/// `--require-kind` naming ACCOUNT data is refused in the SAME words `ls --kind` is refused in
/// — one rule, one sentence. The control is the second half: the market funding RATE is not an
/// account kind and still parses.
///
/// ⚠ **The control USED TO BE UNABLE TO FAIL.** It spelled `binance:BTCUSDT:funding` and
/// `--require-kind bar` — putting the word in the spec's INTERVAL slot, which this crate
/// deliberately does not validate, and passing the flag the DEFAULT value that the first half
/// of this file already proves parses. Flip `vike_model::store_plane::plane_of` so `funding`
/// classifies as `StorePlane::Account` and that spelling stays green while the boundary it
/// claims to guard has moved. The word now reaches `--require-kind` itself, which is the only
/// place the refusal reads.
#[test]
fn a_required_kind_naming_account_data_is_refused_in_the_readers_own_words() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "exec_fill",
    ])
    .expect_err("account data");
    assert!(e.contains("ACCOUNT data"), "{e}");
    assert!(e.contains("vike-cli account"), "…and the plane that will serve it: {e}");
    assert_eq!(
        e,
        refuse_an_account_kind_on_a_read("exec_fill").expect_err("the shared sentence"),
        "the two flags must meet ONE sentence"
    );

    // The CONTROL, through the same flag the refusal reads. `funding` is the MARKET rate and
    // must pass `refuse_an_account_kind_on_a_read`; the spec carries NO interval, because the
    // market rate lives in the `bar` kind under `interval=funding` and a third part here would
    // meet `refuse_a_kind_the_spec_can_never_select` for a different reason entirely.
    assert!(
        refuse_an_account_kind_on_a_read("funding").is_ok(),
        "the MARKET funding rate is not account data — `docs/decisions/0080` separated the \
             two names so this can be asserted at all"
    );
    let ok = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT",
        "--require-days",
        "1",
        "--require-kind",
        "funding",
    ]);
    let g = ok.as_ref().expect("the market funding RATE parses as a criterion").gate.as_ref();
    assert_eq!(
        g.expect("a GateArgs").kinds,
        vec!["funding".to_string()],
        "…and it reaches the criterion roster rather than being swallowed: {ok:?}"
    );
}

/// A `--require-kind` the SPEC can never select is refused HERE, before a socket opens — the
/// wiring of [`gate::refuse_a_kind_the_spec_can_never_select`], whose own unit tests carry the
/// argument and the anti-vacuity control.
///
/// ⚠ The order matters and this is where it is asserted: the refusal runs AFTER the default
/// kind is folded in, so a bare `gate binance:BTCUSDT:1h` — whose roster is `[bar]` — is not
/// refused by the check meant for the kind an operator NAMED.
#[test]
fn a_required_kind_the_spec_can_never_select_is_refused_before_a_socket_opens() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "bar",
        "--require-kind",
        "trade",
    ])
    .expect_err("an interval-bearing spec cannot select a trade series");
    assert!(e.contains("--require-kind trade"), "{e}");
    assert!(e.contains("`binance:BTCUSDT` gates"), "…and the spec that would work: {e}");

    // The two controls: the default roster under the same spec, and the same roster under a
    // spec that named no step.
    assert!(
        parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"]).is_ok(),
        "the DEFAULT roster is the one kind a third part can select"
    );
    assert!(
        parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
        ])
        .is_ok(),
        "…and with no step named, a tick kind is exactly what the spec reaches"
    );
}

/// An EMPTY `--require-kind` is refused rather than collapsing into the default. It is
/// reachable from a script (`--require-kind="$K"` with `K` unset), and silently gating `bar`
/// where the operator meant a variable's value is a green over the wrong tape.
#[test]
fn a_blank_required_kind_is_refused_rather_than_defaulted() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "",
    ])
    .expect_err("a blank kind");
    assert!(e.contains("EMPTY value"), "{e}");
    assert!(e.contains("omit the flag"), "…and what to do instead: {e}");
}

/// Every flag that does not apply to `gate` is refused BY NAME, and `--kind` — the one an
/// operator reaches for first — names the criterion flag that replaced it.
#[test]
fn gate_refuses_the_listing_flags_and_names_what_replaced_the_kind_filter() {
    let line = |extra: &[&str]| {
        let mut v = vec!["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"];
        v.extend_from_slice(extra);
        parse_of(&v)
    };
    assert!(line(&[]).is_ok(), "the negative control: the bare line parses");
    for (extra, needle) in [
        (vec!["--kind", "bar"], "--require-kind"),
        (vec!["--venue", "binance"], "EXACTLY"),
        (vec!["--name", "BTC"], "EXACTLY"),
        (vec!["--class"], "data hist ls --class"),
        (vec!["--partial-only"], "verdict"),
        (vec!["--store", "/srv/hist"], "--addr"),
        (vec!["--engine", "/bin/backtest"], "--addr"),
        (vec!["--days", "30"], "bounds a FETCH window"),
        (vec!["--from", "0"], "bounds a FETCH window"),
        (vec!["--out", "x.parquet"], "only `export` writes one"),
        (vec!["--symbol", "S"], "belongs to `rm` or `repair`"),
        (vec!["--produced-by", "p:"], "only `rm` deletes"),
        (vec!["--source", "demo"], "`fetch`'s axis"),
        (vec!["--gaps"], "data hist gaps"),
    ] {
        let e = line(&extra).expect_err(&format!("{extra:?} must be refused"));
        assert!(e.contains(needle), "{extra:?} must say {needle:?}: {e}");
    }
}

/// ...and the mirror: every CRITERION flag is refused by name on the verbs that judge nothing,
/// with the verb that does judge in the message.
#[test]
fn a_criterion_flag_on_a_rendering_verb_is_refused_and_names_the_gate() {
    for sub in ["ls", "gaps", "coverage", "health", "universe", "fetch", "export"] {
        for extra in
            [vec!["--require-days", "30"], vec!["--max-gap", "1d"], vec!["--require-kind", "bar"]]
        {
            let mut v = vec!["hist", sub];
            v.extend_from_slice(&extra);
            let e = parse_of(&v).expect_err(&format!("{sub} {extra:?}"));
            assert!(e.contains(extra[0]), "{sub} must name the flag: {e}");
            assert!(e.contains("data hist gate"), "{sub} must name the gate: {e}");
        }
    }
}

/// A malformed `--max-gap` is refused HERE, before a socket is opened, because nothing is
/// forwarded: this flag is consumed in this process, so the far side would never see it.
#[test]
fn a_max_gap_that_is_not_fixed_time_is_refused_before_anything_is_dialled() {
    let line = |v: &str| {
        parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1", "--max-gap", v])
    };
    assert!(line("1d").is_ok(), "the negative control");
    assert!(line("500bars").expect_err("bars").contains("BAR COUNT"));
    assert!(line("3mo").expect_err("months").contains("CALENDAR"));
    assert!(line("").expect_err("blank").contains("--max-gap"));
}

// ── §8.2: `get`, the ROW verb ───────────────────────────────────────────────────────────

/// **§8.2 RULE 1 REACHES THE GRAMMAR.** A `get` with no window is refused BY NAME, and the
/// three flag spellings that ARE a window all parse.
///
/// ⚠ The refusal lives in [`get::parse_window`] and is unit-tested there; what this case adds
/// is that the flags REACH it — `--days`/`--from`/`--to` survive the read half's
/// [`Sub::refuses_a_window`] refusal, which every other read verb but `universe` trips.
#[test]
fn get_requires_a_window_and_the_window_flags_reach_it() {
    let err = parse_of(&["hist", "get", "binance:BTCUSDT:1h"]).expect_err("no window");
    assert!(err.contains("get needs a window"), "{err}");
    for window in [vec!["--days", "7"], vec!["--from", "2026-01-01"], vec!["--to", "2026-02-01"]] {
        let mut v = vec!["hist", "get", "binance:BTCUSDT:1h"];
        v.extend_from_slice(&window);
        let a = parse_of(&v).unwrap_or_else(|e| panic!("{window:?}: {e}"));
        assert_eq!(a.sub, Sub::Get);
        assert!(a.get.is_some(), "{window:?} must build a GetArgs");
        assert!(a.spec.is_none(), "the spec is MOVED into GetArgs, never left in both");
    }
}

/// The RESOLVED defaults `GetArgs` promises: the ceiling folded in, the fold recorded, and the
/// rendering decided — so nothing downstream re-decides one.
#[test]
fn get_args_arrive_resolved_with_the_ceiling_folded_in() {
    let a = parse_of(&["hist", "get", "binance:BTCUSDT:1h", "--days", "7"]).expect("a line");
    let g = a.get.as_ref().expect("a GetArgs");
    assert_eq!(g.limit, get::ROW_CEILING);
    assert!(g.limit_defaulted, "nobody typed --limit, and the disclosure has to know");
    assert_eq!(g.render, get::Render::Table, "the plane's default");
    assert_eq!(g.window, get::Window::Days(7));
    assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");

    let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "5", "--json"])
        .expect("a line");
    let g = a.get.as_ref().expect("a GetArgs");
    assert_eq!(g.limit, 5);
    assert!(!g.limit_defaulted, "a named limit says so rather than reading as the default");
    assert_eq!(g.render, get::Render::Json);
    assert!(a.json, "…and `Args::json` is its PROJECTION, not a second decision");
}

/// **`jsonl` is a THIRD state and `Args::json` has two**, so the render is the authority for
/// this verb — the reason [`GetArgs::render`] exists at all. A `jsonl` run must not be
/// indistinguishable from a `table` one downstream.
#[test]
fn a_jsonl_get_is_not_a_json_get_and_not_a_table_one_either() {
    let line = |f: &str| {
        parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", f])
            .unwrap_or_else(|e| panic!("{f}: {e}"))
    };
    let jsonl = line("jsonl");
    assert_eq!(jsonl.get.as_ref().expect("a GetArgs").render, get::Render::Jsonl);
    assert!(!jsonl.json, "jsonl is NOT the one-document form");
    // THE CONTROLS: the two states `Args::json` CAN express, so the assertion above is about
    // `jsonl` being a third rather than about the field always being false.
    assert!(!line("table").json);
    assert!(line("json").json);

    // ...and because it is a third state, `--json --format jsonl` is a SECOND way the two
    // spellings can disagree — refused with a sentence of its own rather than resolved, for
    // the same reason `--json --format table` is.
    let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
        .expect_err("a sequence is not one document");
    assert!(err.contains("SEQUENCE"), "{err}");
    assert!(err.contains("Pass one"), "…and what to do: {err}");
    // THE CONTROL: the AGREEING pair is not a contradiction, so the refusal is about the
    // values rather than about naming both spellings at all.
    assert!(
        parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "json"])
            .expect("agreeing spellings")
            .json
    );
}

/// **The `jsonl` contradiction is [`Sub::Get`]'s ALONE, and on every other verb the answer is
/// the one the sibling group already gives.**
///
/// ⚠ It shipped UNCONDITIONAL, above the verb dispatch, so `data hist ls --json --format
/// jsonl` answered with a sentence describing GET's document and ending "Pass one" — advice
/// that is false there, since dropping `--json` leaves a `--format jsonl` `ls` also refuses.
/// Meanwhile `data catalog ls --json --format jsonl` answered with [`ROW_VERB`], because that
/// parser reads `--format` eagerly and its contradiction check never sees a `jsonl`. Two
/// groups, one plane, one question, two answers — which is exactly what
/// `catalog`'s `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words` holds
/// for the `table` spelling and nothing held for this one.
#[test]
fn a_jsonl_contradiction_on_a_catalog_verb_names_the_row_verb_in_both_groups() {
    let hist = parse_of(&["hist", "ls", "--json", "--format", "jsonl"])
        .expect_err("a catalog verb emits no rows");
    assert!(hist.contains(ROW_VERB), "it must name where `jsonl` works: {hist}");
    assert!(
        !hist.contains("SEQUENCE"),
        "…and must NOT hand over `get`'s document sentence, whose advice is false here: {hist}"
    );
    // ⚠ The CROSS-GROUP half of this pairing lives in `catalog`'s tests
    // (`the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words`) and not here,
    // for a visibility reason rather than a taste one: `catalog::parse` is private to its own
    // module, so only a descendant can reach BOTH parsers.
    //
    // ...and it must not merely be refused: dropping `--json` has to leave the SAME answer,
    // because the arm above is gone on this verb and the format parser is what refuses it.
    let without = parse_of(&["hist", "ls", "--format", "jsonl"]).expect_err("still refused");
    assert_eq!(hist, without, "`--json` must not change what `--format jsonl` means on `ls`");
    // THE CONTROL: the same pair on the ROW verb DOES get the document sentence, so the
    // assertions above are about the VERB rather than about the arm being dead.
    let get = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
        .expect_err("a sequence is not one document");
    assert!(get.contains("SEQUENCE"), "{get}");
    assert_ne!(get, hist, "the two verbs answer with different sentences");
}

/// **THE ACCOUNT-KIND REFUSAL, and the ORDER it is applied in.** `--kind` does not apply to
/// `get` at all — but for an ACCOUNT kind the §9.3.2 sentence comes FIRST, because "this plane
/// does not serve your fills" outranks "that flag belongs elsewhere".
#[test]
fn an_account_kind_on_get_is_the_plane_s_refusal_and_not_a_flag_note() {
    for kind in vike_model::ACCOUNT_KINDS {
        let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", kind])
            .expect_err("account data is not this plane's");
        assert!(err.contains("ACCOUNT data"), "{kind}: {err}");
        assert!(err.contains("vike-cli account"), "…and where it will live: {kind}: {err}");
    }
    // THE CONTROL: a MARKET kind gets the flag-placement answer instead, so the refusal above
    // is about the kind rather than about `--kind` being refused with one sentence for all.
    let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", "trade"])
        .expect_err("`get` reads bars");
    assert!(!err.contains("ACCOUNT data"), "{err}");
    assert!(err.contains("reads BARS"), "{err}");
    assert!(err.contains("data hist ls"), "…and names where a kind IS a filter: {err}");
}

/// The LISTING flags are refused on `get` by name, and `--limit` is refused everywhere else by
/// name — both directions of the one rule that this verb's noun is a ROW and its siblings' is a
/// series.
///
/// ⚠ **The `--limit` half DERIVES its roster**, as its two neighbours upfile already do
/// ([`a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store`],
/// [`a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one`]). It
/// shipped hand-typed and three verbs short — `gate`, `rm` and `repair` were never exercised.
/// The production guard is `sub != Sub::Get`, so it was right for them anyway; what was wrong
/// was the PROOF, which would have stayed green through a new [`SUBCOMMANDS`] row or a guard
/// refined into an enumeration.
///
/// ⚠ …and it asserts the RENDERED [`ROW_VERB`], not the words inside it. `contains("data hist
/// get")` is satisfied by a stale hand copy, which is the drift the const exists to stop.
#[test]
fn the_row_flag_and_the_listing_flags_are_refused_on_each_other() {
    for (flag, value) in [("--venue", Some("binance")), ("--name", Some("BTC")), ("--class", None)]
    {
        let mut v = vec!["hist", "get", "d:S:1h", "--days", "1", flag];
        v.extend(value);
        let err = parse_of(&v).expect_err(flag);
        assert!(err.contains(flag), "{flag}: {err}");
        assert!(err.contains("data hist ls"), "{flag} must name the listing verb: {err}");
    }
    let mut refused = 0usize;
    for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Get) {
        let err = parse_of(&["hist", sub.as_str(), "--limit", "10"]).expect_err(sub.as_str());
        assert!(err.contains("--limit"), "{}: {err}", sub.as_str());
        assert!(err.contains(ROW_VERB), "{} must RENDER the row verb: {err}", sub.as_str());
        refused += 1;
    }
    // Anti-vacuity: a `SUBCOMMANDS` that had lost its rows, or a filter that kept none, would
    // leave the loop above asserting nothing at all.
    assert_eq!(refused, SUBCOMMANDS.len() - 1, "every verb but `get` was exercised");
    assert!(refused > 5, "and there are enough of them for that to mean something");
    // THE CONTROL: `get` itself TAKES the flag, so the refusals above are about the verb
    // rather than about `--limit` being rejected wherever it appears.
    let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "10"])
        .expect("`get` is the verb that bounds rows");
    assert_eq!(a.get.as_ref().expect("a GetArgs").limit, 10);
}

/// The usage roster names every subcommand and every flag the parser accepts — `crate::cmd::
/// mcp`'s `the_instructions_name_only_real_commands` reads this text to check the MCP
/// surface's `vike-cli data …` mentions, so a subcommand missing from it makes that check
/// unable to confirm a command that does exist.
#[test]
fn the_usage_names_every_subcommand_and_flag_this_parser_accepts() {
    for sub in SUBCOMMANDS {
        assert!(USAGE.contains(sub.as_str()), "USAGE must name {}", sub.as_str());
    }
    for token in [
        "--out",
        "--produced-by",
        "--dry-run",
        "--yes",
        "--symbol",
        "--group",
        "--interval",
        "--days",
        "--from",
        "--to",
        "--store",
        "--engine",
        "--addr",
        "--kind",
        "--venue",
        "--name",
        "--class",
        "--partial-only",
        "--require-days",
        "--max-gap",
        "--require-kind",
        "--limit",
        "--format",
        "--json",
        "--bars",
        "--verify",
    ] {
        assert!(USAGE.contains(token), "USAGE must name {token}");
    }
    assert!(USAGE.contains(DEFAULT_ADDR), "…and the datahub default it resolves to");
    // ⚠ …and `get`'s ROW CEILING, which this page states as a bare NUMBER twice while
    // [`get::ROW_CEILING`] declares it. A `const &'static str` cannot interpolate, so the
    // duplication is unavoidable and this assertion is what stops it rotting — the same shape
    // the rung above gives `DEFAULT_ADDR`, and the same defect this file has watched a
    // hand-copied count produce more than once.
    assert!(
        USAGE.contains(&get::ROW_CEILING.to_string()),
        "USAGE states the row ceiling as a number and it no longer matches get::ROW_CEILING"
    );
}

// ── D2 (0094 follow-ups): the `--features backfill-serve` hint is not true of every failure ────

/// The hint is true of exactly ONE of [`vike_datahub_client::DatahubClient::backfill`]'s two
/// client-side capability refusals — the server never advertised `backfill` at all — and false of
/// its sibling, which means a `backfill-serve` server simply predates the funding lane. Spelled
/// against the two REAL messages that function formats, not a paraphrase of them.
#[test]
fn is_missing_backfill_feature_discriminates_the_two_capability_refusals() {
    assert!(
        is_missing_backfill_feature(
            "datahub server does not advertise `backfill` (advertised: []) — nothing was sent. \
             Backfill needs a server built with `--features backfill-serve` (or a newer server; \
             this verb is capability-negotiated, not version-gated)."
        ),
        "the server never advertised `backfill` at all — the hint belongs here"
    );
    assert!(
        !is_missing_backfill_feature(
            "datahub server does not advertise `backfill_funding` (advertised: [\"backfill\"]) — \
             nothing was sent. A funding-rate backfill (`VENUE:SYMBOL:funding`) needs a \
             `backfill-serve` server from a release that carries the funding lane."
        ),
        "this server DOES run backfill-serve — appending the hint here would contradict its own \
         sentence"
    );
}

/// A server-side `Response::Error` — a funding/spot/unknown-venue refusal from a server that
/// plainly runs `backfill-serve`, since it answered the verb at all — must not be read as the
/// missing-feature case either.
#[test]
fn is_missing_backfill_feature_is_false_for_an_ordinary_server_side_refusal() {
    assert!(!is_missing_backfill_feature("unknown venue 'not-a-venue'"));
    assert!(!is_missing_backfill_feature(
        "binance: \"BTCUSDT\" names SPOT, and funding is a perpetual's series — ask for \
         \"BTCUSDT.P\""
    ));
}

// ── the date help is what the parser takes (the OANDA history design's follow-up 3) ──────────

/// Every concrete `YYYY-MM-DD` date in `text`, with its `THH` hour suffix when it carries one, in
/// order. The placeholder spelling itself (`YYYY-MM-DD`) has no digits and is not a token.
fn date_tokens(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= b.len() {
        let starts_a_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if starts_a_word
            && digits(i..i + 4)
            && b[i + 4] == b'-'
            && digits(i + 5..i + 7)
            && b[i + 7] == b'-'
            && digits(i + 8..i + 10)
        {
            let mut end = i + 10;
            if end + 3 <= b.len() && b[end] == b'T' && digits(end + 1..end + 3) {
                end += 3;
            }
            out.push(&text[i..end]);
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// `USAGE`'s option row for `flag` — from its line to the next option row.
fn usage_row(flag: &str) -> &'static str {
    let at = USAGE.find(&format!("\n  {flag} ")).unwrap_or_else(|| panic!("no {flag} row"));
    let rest = &USAGE[at + 1..];
    let end = rest[1..].find("\n  -").map_or(rest.len(), |n| n + 1);
    &rest[..end]
}

/// **Every bound this verb's help documents is one [`parse_date_label`] takes** — the parser
/// `fetch_window_ms` reads `--from`/`--to` through. It takes epoch-ms or a UTC date `YYYY-MM-DD`
/// and REFUSES an hour label, and the help said `YYYY-MM-DDTHH` in three places — the window error,
/// the skill's prose and the skill's own fetch example — from the day the route moved to a datahub
/// until this test fed each documented example through the parser. Each half below would have gone
/// red on that text.
///
/// ⚠ The `--from` row documents TWO spellings because two processes parse it: this binary for
/// `fetch`/`universe`/a remote export, the ENGINE for an engine export (`parse_ts`, which takes
/// the hour label and refuses a bare date). So the row's hour example must be one THIS parser
/// refuses — that is what the row says about it — and its date examples ones it takes.
#[test]
fn a_documented_fetch_bound_is_one_the_parser_takes() {
    // The window error: names the date spelling, an example the parser takes, and no hour label.
    let err = window_from(None, None, None).unwrap_err();
    assert!(err.contains("YYYY-MM-DD") && !err.contains("YYYY-MM-DDTHH"), "{err}");
    let examples = date_tokens(&err);
    assert!(!examples.is_empty(), "the window error names no example a reader can copy: {err}");
    for ex in examples {
        parse_date_label(ex).unwrap_or_else(|e| panic!("the window error's {ex:?}: {e}"));
    }

    // USAGE's `--from` row (and `--to` says "same spellings").
    let row = usage_row("--from LABEL");
    let (hours, dates): (Vec<&str>, Vec<&str>) =
        date_tokens(row).into_iter().partition(|t| t.contains('T'));
    assert!(!dates.is_empty() && !hours.is_empty(), "the row names both spellings: {row}");
    for d in dates {
        parse_date_label(d).unwrap_or_else(|e| panic!("the --from row's date {d:?}: {e}"));
    }
    for h in hours {
        assert!(parse_date_label(h).is_err(), "{h:?} is said to be refused HERE and is not");
        assert!(vike_model::time::parse_hour_label(h).is_some(), "{h:?} is not even an hour label");
    }
    assert!(usage_row("--to LABEL").contains("same spellings"), "{}", usage_row("--to LABEL"));

    // The get-market-data skill, as RENDERED (the copy that ships and that an agent executes):
    // every `--from`/`--to` value on a `data hist fetch` line, and the sentence describing them.
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/get-market-data/SKILL.md");
    let skill = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));
    let mut bounds = 0;
    for line in skill.lines().filter(|l| l.trim_start().starts_with("vike-cli data hist fetch ")) {
        let words: Vec<&str> = line.split_whitespace().collect();
        for pair in words.windows(2).filter(|p| p[0] == "--from" || p[0] == "--to") {
            parse_date_label(pair[1])
                .unwrap_or_else(|e| panic!("the skill's fetch example {line:?}: {e}"));
            bounds += 1;
        }
    }
    assert!(bounds >= 2, "the skill's fetch examples carry no --from/--to to check");
    let at = skill.find("OR `--from` and").expect("the skill's window sentence");
    let sentence = &skill[at..at + skill[at..].find("never a mixture").expect("its end")];
    assert!(
        sentence.contains("`YYYY-MM-DD`") && !sentence.contains("YYYY-MM-DDTHH"),
        "the skill's fetch window sentence: {sentence}"
    );
}

// ── `fetch`'s per-year cut (`super::fetch_split`) ──────────────────────────────────────────────

/// The tests of `crate::cmd::data`'s `fetch_split` module — here, in a module of their own, rather
/// than in a `fetch_split_tests.rs` file: `crates/vike-ops/tests/compile_time_path_gate.rs` ratchets
/// the number of `#[cfg(test)] mod NAME;` files, and a new one would have had to raise that shared
/// ceiling.
mod fetch_split_cases {
    use std::cell::RefCell;
    use std::time::Duration;

    use vike_catalog::{ChannelState, HistoryLane, history_channels_for};
    use vike_datahub_client::BackfillDone;
    use vike_model::{
        MS_PER_DAY, VENUES,
        time::{civil_from_days, days_from_civil},
    };

    use crate::cmd::data::fetch_split::{
        PieceDone, Plan, elapsed, header_line, plan, progress_line, run_pieces, splits_by_year,
        stores_whole_utc_days,
    };

    /// Midnight UTC of `y-m-d`, in epoch-ms.
    fn day(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * MS_PER_DAY
    }

    fn pieces(plan: &Plan) -> Vec<(i64, i64)> {
        plan.pieces().collect()
    }

    /// The properties every cut must have, whatever the window: the pieces tile `[start, end]`
    /// with no instant in two and none in neither, every interior cut is a 1 January midnight UTC,
    /// and `Plan::count` — computed without walking — agrees with the walk.
    fn assert_tiles(plan: &Plan) {
        let (start, end) = plan.window();
        let got = pieces(plan);
        assert_eq!(got.len() as u64, plan.count(), "count() disagrees with the walk: {got:?}");
        assert_eq!(got.first().map(|p| p.0), Some(start), "the first piece keeps the user's start");
        assert_eq!(got.last().map(|p| p.1), Some(end), "the last piece keeps the user's end");
        for pair in got.windows(2) {
            let ((_, to), (from, _)) = (pair[0], pair[1]);
            assert_eq!(to + 1, from, "a gap or an overlap between {pair:?}");
            assert_eq!(from.rem_euclid(MS_PER_DAY), 0, "a cut off a UTC midnight: {from}");
            let (_, m, d) = civil_from_days(from.div_euclid(MS_PER_DAY));
            assert_eq!((m, d), (1, 1), "a cut that is not 1 January: {from}");
        }
        for (from, to) in &got {
            assert!(from <= to, "an inverted piece {from}..{to} in {got:?}");
        }
    }

    // ─── the rule: WHICH venues may be cut ─────────────────────────────────────────────────────

    /// **A one-shot lane's window is NEVER cut** — the hazard `fetch_split` exists for. Each of
    /// these venues' lanes keys a commit by the request's own bounds (or, for dukascopy, by a
    /// partial chunk's own cut and the recent tail's request bounds), so a per-year request would
    /// store a long window's rows a second time beside a later whole-window request's. Three years
    /// is far past the one-year floor, so only the lane rule can be what keeps each of these one
    /// request.
    #[test]
    fn a_one_shot_lanes_window_is_one_request_however_long() {
        let (start, end) = (day(2021, 7, 1), day(2024, 3, 1));
        for venue in ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "dukascopy"] {
            let plan = plan(venue, start, end);
            assert!(!plan.is_split(), "{venue}'s lane keys by the request, and was cut: {plan:?}");
            assert_eq!(pieces(&plan), vec![(start, end)], "{venue} must send the window as typed");
            assert_eq!(plan.count(), 1, "{venue}");
        }
    }

    /// ...and the day-grid lane IS cut, at the same window. Without this the case above would pass
    /// for a module that never split anything.
    #[test]
    fn the_day_grid_lane_is_cut_into_years() {
        let (start, end) = (day(2021, 7, 1), day(2024, 3, 1));
        let plan = plan("oanda", start, end);
        assert!(plan.is_split(), "oanda's lane stores whole UTC days and was not cut: {plan:?}");
        assert_eq!(
            pieces(&plan),
            vec![
                (start, day(2022, 1, 1) - 1),
                (day(2022, 1, 1), day(2023, 1, 1) - 1),
                (day(2023, 1, 1), day(2024, 1, 1) - 1),
                (day(2024, 1, 1), end),
            ]
        );
        assert_tiles(&plan);
    }

    /// The rule is DERIVED from the history table, never a venue list: across the whole roster, a
    /// venue is cut exactly when the table claims at least one built lane for it and every one it
    /// claims is the day-grid lane. An unknown venue string declares nothing and is one request.
    #[test]
    fn a_venue_is_cut_exactly_when_every_lane_it_claims_stores_whole_days() {
        let mut cut = Vec::new();
        for &venue in VENUES {
            let lanes: Vec<HistoryLane> = history_channels_for(venue)
                .iter()
                .filter_map(|row| match row.state {
                    ChannelState::Built(lane) => Some(lane),
                    ChannelState::Designed(_) => None,
                })
                .collect();
            let expected =
                !lanes.is_empty() && lanes.iter().all(|l| *l == HistoryLane::CredentialedKlines);
            assert_eq!(splits_by_year(venue), expected, "{venue}: lanes {lanes:?}");
            if expected {
                cut.push(venue);
            }
        }
        assert_eq!(cut, vec!["oanda"], "the venues cut today — a new one is a decision, not drift");
        assert!(!splits_by_year("not-a-venue"));
        assert!(!plan("not-a-venue", day(2000, 1, 1), day(2010, 1, 1)).is_split());
    }

    /// `stores_whole_utc_days` answers each lane by its ingest, read in `fetch_split`'s module doc.
    #[test]
    fn only_the_credentialed_lane_stores_whole_days() {
        assert!(stores_whole_utc_days(HistoryLane::CredentialedKlines));
        for lane in [HistoryLane::Klines, HistoryLane::TickBars, HistoryLane::Funding] {
            assert!(!stores_whole_utc_days(lane), "{lane:?} keys by the request");
        }
    }

    // ─── the cut: arithmetic ───────────────────────────────────────────────────────────────────

    /// A window of one year or less is never cut, even when it crosses a 1 January — a split exists
    /// to report progress on a LONG fetch, and a year is the unit an operator ran by hand before it.
    #[test]
    fn a_year_or_less_is_one_piece_even_across_new_year() {
        // Exactly one calendar year, crossing 2024-01-01.
        let exact = Plan::by_year(day(2023, 3, 15), day(2024, 3, 15));
        assert!(!exact.is_split());
        assert_eq!(pieces(&exact), vec![(day(2023, 3, 15), day(2024, 3, 15))]);
        // Two months across new year.
        assert!(!Plan::by_year(day(2023, 12, 1), day(2024, 2, 1)).is_split());
        // A whole calendar year written the way the operator page writes it.
        assert!(!Plan::by_year(day(2024, 1, 1), day(2024, 12, 31)).is_split());
        // ...and with `--to` on the next year's first day.
        assert!(!Plan::by_year(day(2024, 1, 1), day(2025, 1, 1)).is_split());
        // One millisecond past a year IS longer than a year.
        let over = Plan::by_year(day(2023, 3, 15), day(2024, 3, 15) + 1);
        assert!(over.is_split());
        assert_eq!(
            pieces(&over),
            vec![(day(2023, 3, 15), day(2024, 1, 1) - 1), (day(2024, 1, 1), day(2024, 3, 15) + 1)]
        );
    }

    /// A window ending EXACTLY on 1 January 00:00 keeps that instant in its last piece: the cut
    /// points are strictly inside the window, so no one-millisecond piece is minted for it.
    #[test]
    fn a_window_ending_on_new_year_keeps_that_instant_in_its_last_piece() {
        let plan = Plan::by_year(day(2023, 1, 1), day(2025, 1, 1));
        assert_eq!(plan.count(), 2);
        assert_eq!(
            pieces(&plan),
            vec![(day(2023, 1, 1), day(2024, 1, 1) - 1), (day(2024, 1, 1), day(2025, 1, 1))]
        );
        assert_tiles(&plan);
    }

    /// A window STARTING on a 1 January cuts at the NEXT one — a cut at the start would be an
    /// inverted first piece.
    #[test]
    fn a_window_starting_on_new_year_cuts_at_the_next_one() {
        let plan = Plan::by_year(day(2020, 1, 1), day(2021, 6, 1));
        assert_eq!(
            pieces(&plan),
            vec![(day(2020, 1, 1), day(2021, 1, 1) - 1), (day(2021, 1, 1), day(2021, 6, 1))]
        );
        assert_tiles(&plan);
    }

    /// The first and last pieces keep the operator's bounds to the MILLISECOND — a `--days` window
    /// starts and ends mid-day, and the outer edges must be what one request would have sent.
    #[test]
    fn the_outer_pieces_keep_mid_day_bounds() {
        let start = day(2019, 5, 17) + 13 * 3_600_000 + 1_234;
        let end = day(2021, 8, 2) + 22 * 3_600_000 + 59_999;
        let plan = Plan::by_year(start, end);
        let got = pieces(&plan);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0], (start, day(2020, 1, 1) - 1));
        assert_eq!(got[2], (day(2021, 1, 1), end));
        assert_tiles(&plan);
    }

    /// Leap years: a leap year's piece is 366 days, and "one year after 29 February" is 1 March.
    #[test]
    fn leap_years_are_cut_and_measured_by_the_calendar() {
        let plan = Plan::by_year(day(2023, 6, 1), day(2025, 6, 1));
        let got = pieces(&plan);
        assert_eq!(got[1], (day(2024, 1, 1), day(2025, 1, 1) - 1));
        assert_eq!((got[1].1 + 1 - got[1].0) / MS_PER_DAY, 366, "2024 is a leap year");
        assert_tiles(&plan);

        // From 29 February, one year later is 1 March — so a window to that instant is NOT longer
        // than a year, and one millisecond more is.
        let feb29 = day(2024, 2, 29) + 12 * 3_600_000;
        let mar1 = day(2025, 3, 1) + 12 * 3_600_000;
        assert!(!Plan::by_year(feb29, mar1).is_split());
        assert!(Plan::by_year(feb29, mar1 + 1).is_split());
    }

    /// Before 1970 the calendar is the same calendar: the cut uses `div_euclid`, so a negative
    /// instant finds its year like any other.
    #[test]
    fn a_pre_1970_window_is_cut_at_its_own_new_years() {
        let plan = Plan::by_year(day(1968, 6, 1), day(1970, 6, 1));
        assert_eq!(
            pieces(&plan),
            vec![
                (day(1968, 6, 1), day(1969, 1, 1) - 1),
                (day(1969, 1, 1), day(1970, 1, 1) - 1),
                (day(1970, 1, 1), day(1970, 6, 1)),
            ]
        );
        assert_tiles(&plan);
    }

    /// The whole of OANDA's dense 5-second history, the case this exists for: 2005-01-03 to a day
    /// in 2026 is 22 requests, the first and last ragged and the twenty between them whole years.
    #[test]
    fn the_whole_oanda_history_is_twenty_two_requests() {
        let plan = plan("oanda", day(2005, 1, 3), day(2026, 9, 29));
        assert_eq!(plan.count(), 22);
        let got = pieces(&plan);
        assert_eq!(got[0], (day(2005, 1, 3), day(2006, 1, 1) - 1));
        assert_eq!(got[21], (day(2026, 1, 1), day(2026, 9, 29)));
        assert_tiles(&plan);
    }

    /// A sweep of windows, including ragged edges on both sides and every relation to new year,
    /// holds every tiling property — and `Plan::count`'s arithmetic agrees with the walk on each.
    #[test]
    fn every_window_in_a_sweep_tiles_its_range() {
        let offsets = [0, 1, MS_PER_DAY - 1, 40 * MS_PER_DAY + 7, 200 * MS_PER_DAY];
        for start_year in [1969, 1999, 2004, 2020] {
            for years in 0..4 {
                for &a in &offsets {
                    for &b in &offsets {
                        let start = day(start_year, 1, 1) + a;
                        let end = day(start_year + years, 1, 1) + b;
                        if start > end {
                            continue;
                        }
                        assert_tiles(&Plan::by_year(start, end));
                        assert_tiles(&Plan::whole(start, end));
                    }
                }
            }
        }
    }

    /// The edges of `i64` neither panic nor overflow: a window at the very top cannot be longer
    /// than a year past its start, so it is one piece; a window across all of `i64` counts without
    /// walking and yields its first pieces lazily.
    #[test]
    fn the_edges_of_i64_neither_panic_nor_allocate() {
        let top = Plan::by_year(i64::MAX - 10, i64::MAX);
        assert_eq!(pieces(&top), vec![(i64::MAX - 10, i64::MAX)]);
        let all = Plan::by_year(i64::MIN, i64::MAX);
        assert!(all.is_split());
        assert!(all.count() > 500_000_000, "{}", all.count());
        let first: Vec<_> = all.pieces().take(3).collect();
        assert_eq!(first[0].0, i64::MIN);
        assert_eq!(first[0].1 + 1, first[1].0);
    }

    // ─── the run: stop at the first failure ────────────────────────────────────────────────────

    fn done(rows: u64, first: Option<i64>, last: Option<i64>) -> BackfillDone {
        BackfillDone { rows_written: rows, first_ts: first, last_ts: last }
    }

    fn three_years() -> Plan {
        Plan::by_year(day(2021, 7, 1), day(2023, 3, 1))
    }

    /// Every piece answers: the requests go out in order with each piece's own bounds, one progress
    /// line follows the header per piece, and the merge reads like one request's answer.
    #[test]
    fn every_piece_is_sent_in_order_and_reported() {
        let plan = three_years();
        let sent = RefCell::new(Vec::new());
        let lines = RefCell::new(Vec::new());
        let out = run_pieces(
            &plan,
            "HEADER",
            |from, to| {
                sent.borrow_mut().push((from, to));
                Ok(match sent.borrow().len() {
                    1 => done(184, Some(from + 5), Some(to - 5)),
                    // An empty year: nothing stored in its range.
                    2 => done(0, None, None),
                    _ => done(60, Some(from + 9), Some(to - 9)),
                })
            },
            |line| lines.borrow_mut().push(line.to_string()),
        )
        .expect("every piece answered");

        assert_eq!(*sent.borrow(), pieces(&plan));
        assert_eq!(out.rows_written, 244);
        assert_eq!(out.first_ts, Some(day(2021, 7, 1) + 5), "the earliest piece that held rows");
        assert_eq!(out.last_ts, Some(day(2023, 3, 1) - 9), "the latest piece that held rows");
        assert_eq!(out.pieces.len(), 3);

        let lines = lines.borrow();
        assert_eq!(lines.len(), 4, "the header, then one line per piece: {lines:?}");
        assert_eq!(lines[0], "HEADER");
        for (line, want) in lines[1..].iter().zip([
            "  1/3 [2021-07-01 .. 2021-12-31]: 184 rows written in ",
            "  2/3 [2022-01-01 .. 2022-12-31]: 0 rows written in ",
            "  3/3 [2023-01-01 .. 2023-03-01]: 60 rows written in ",
        ]) {
            assert!(line.starts_with(want), "{line:?} is not {want:?}…");
        }
    }

    /// **A failed piece STOPS the run.** Piece 2 of 3 fails: piece 3 is never sent, no line is
    /// printed for the failed piece, and the failure names the piece, the rows the pieces before it
    /// wrote and the datahub's own text. A run that carried on past a failure would report a whole
    /// window while leaving a year out of it — and would send requests after the venue or the store
    /// had said no.
    #[test]
    fn a_failed_piece_stops_the_run_and_names_what_is_stored() {
        let plan = three_years();
        let sent = RefCell::new(Vec::new());
        let lines = RefCell::new(Vec::new());
        let failed = run_pieces(
            &plan,
            "HEADER",
            |from, to| {
                sent.borrow_mut().push((from, to));
                match sent.borrow().len() {
                    1 => Ok(done(184, Some(from), Some(to))),
                    _ => Err("backfill oanda/EUR_USD@1h failed: chunk 166 of 365 failed".into()),
                }
            },
            |line| lines.borrow_mut().push(line.to_string()),
        )
        .expect_err("piece 2 failed");

        let sent = sent.borrow();
        assert_eq!(sent.len(), 2, "piece 3 was sent after piece 2 failed: {sent:?}");
        assert_eq!(lines.borrow().len(), 2, "the header and piece 1 only: {:?}", lines.borrow());
        assert_eq!((failed.index, failed.count), (2, 3));
        assert_eq!((failed.from, failed.to), (day(2022, 1, 1), day(2023, 1, 1) - 1));
        assert_eq!(failed.rows_before, 184);

        let msg = failed.message("oanda");
        assert!(msg.contains("piece 2 of 3 [2022-01-01 .. 2022-12-31] failed"), "{msg}");
        assert!(msg.contains("the piece before it wrote 184 rows, which stay stored"), "{msg}");
        assert!(msg.contains("Re-run the same command to resume"), "{msg}");
        assert!(msg.contains("chunk 166 of 365 failed"), "the datahub's own text survives: {msg}");
    }

    /// A first piece that fails is said to be the first, rather than claiming rows were written.
    #[test]
    fn a_failed_first_piece_says_nothing_was_written_before_it() {
        let failed = run_pieces(&three_years(), "H", |_, _| Err("refused".to_string()), |_| {})
            .expect_err("piece 1 failed");
        assert_eq!((failed.index, failed.rows_before), (1, 0));
        let msg = failed.message("oanda");
        assert!(msg.contains("piece 1 of 3"), "{msg}");
        assert!(msg.contains("it was the first, so this run wrote nothing before it"), "{msg}");
        assert!(msg.ends_with("refused"), "{msg}");
    }

    /// A later failure counts every piece before it, by number and by rows.
    #[test]
    fn a_later_failure_counts_every_piece_before_it() {
        let plan = Plan::by_year(day(2019, 1, 1), day(2023, 6, 1));
        let mut n = 0;
        let failed = run_pieces(
            &plan,
            "H",
            |_, _| {
                n += 1;
                if n < 4 { Ok(done(10, None, None)) } else { Err("x".into()) }
            },
            |_| {},
        )
        .expect_err("piece 4 failed");
        assert_eq!((failed.index, failed.count, failed.rows_before), (4, 5, 30));
        assert!(failed.message("oanda").contains("the 3 pieces before it wrote 30 rows"));
    }

    // ─── the lines ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn elapsed_reads_the_way_an_operator_reads_a_long_fetch() {
        assert_eq!(elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(elapsed(Duration::from_millis(4_240)), "4.2s");
        assert_eq!(elapsed(Duration::from_secs(59)), "59.0s");
        assert_eq!(elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(elapsed(Duration::from_secs(252)), "4m12s");
        assert_eq!(elapsed(Duration::from_secs(3_600)), "1h00m");
        assert_eq!(elapsed(Duration::from_secs(5_430)), "1h30m");
    }

    #[test]
    fn the_header_names_the_window_the_count_and_why_the_cut_is_safe() {
        let plan = plan("oanda", day(2005, 1, 3), day(2026, 9, 29));
        let line = header_line("oanda:EUR_USD:5s", "oanda", &plan);
        let want = "fetching oanda:EUR_USD:5s [2005-01-03 .. 2026-09-29] as 22 requests";
        assert!(line.starts_with(want), "{line}");
        assert!(line.contains("one per calendar year"), "{line}");
        assert!(line.contains("stores whole UTC days"), "{line}");
    }

    #[test]
    fn a_progress_line_carries_the_window_the_rows_and_the_time() {
        let piece = PieceDone {
            from: day(2006, 1, 1),
            to: day(2007, 1, 1) - 1,
            done: done(3_012_345, None, None),
            elapsed: Duration::from_secs(252),
        };
        assert_eq!(
            progress_line(2, 22, &piece),
            "  2/22 [2006-01-01 .. 2006-12-31]: 3012345 rows written in 4m12s"
        );
    }
}
