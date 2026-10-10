//! The group split, the account-kind exclusion, the address ladder and the subcommand roster.

use super::*;
use crate::cmd::args::HELP_SENTINEL;

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
