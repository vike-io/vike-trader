//! `vike-cli indicators` — the roster a user is pointed at must be EXACTLY the roster the host
//! binds, asserted through the shipped binary.
//!
//! # Why this drives the real binary
//!
//! The claim under test is not "a function returns the right vector" — the unit tests in
//! `crates/vike-cli/src/cmd/indicators.rs` cover that. It is that the STDOUT a user reads, from the
//! binary they installed, names every callable indicator and nothing else. The shipped
//! `user_data/strategies/rhai/README.md` (`crates/vike-cli/src/cmd/init/content.rs`'s
//! `RHAI_README`) stopped listing indicator names and now points here instead, so this listing IS
//! the documentation: if it under-reports, a user is told a callable indicator does not exist, and
//! if it over-reports they are handed a name whose script compiles and then fails on every bar
//! until the strategy switches itself off.
//!
//! # Derived on both sides
//!
//! Every expectation is `vike_script::RHAI_INDICATORS`, the same const
//! `crates/vike-script/src/engine.rs`'s `register_indicators` iterates when it registers the host
//! functions. No name is written down here, so these tests cannot pass while the binding says
//! something else — and they keep working when the bound set changes, which is the whole reason the
//! roster is derived rather than hand-listed.

use std::process::{Command, Output};

/// Run the shipped binary in an environment that resolves no project settings, so the dispatcher's
/// `resolve_policy` (which runs before EVERY subcommand) cannot pick up this machine's real
/// `policy.toml`. The listing itself depends on the BUILD alone, but the dispatcher still runs.
///
/// ⚠ `VIKE_USER_DATA_DIR` is pinned at an empty directory for the same reason, and it is
/// load-bearing rather than tidiness: this listing also prints the USER's own indicators
/// (`user_data/indicators/*.rhai`), whose rows are indented call forms exactly like a built-in's.
/// Unpinned, every `printed_names` assertion below would silently start counting whatever the
/// developer running the tests happens to have written in their own checkout — a suite that passes
/// on CI and fails on one machine for a reason nothing in it names.
/// `crates/vike-cli/tests/user_indicators_cli.rs` is where that block IS the subject, over a
/// fixture it writes itself.
fn run(args: &[&str]) -> Output {
    let empty_settings = std::env::temp_dir().join("vike_cli_indicators_no_settings");
    let empty_user_data = std::env::temp_dir().join("vike_cli_indicators_no_user_data");
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", &empty_settings)
        .env("VIKE_USER_DATA_DIR", &empty_user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn stdout_of(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "`vike-cli {}` must exit 0; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The indicator NAMES a listing printed: every row is indented two spaces and opens with
/// `name(`, so the name is what precedes the first `(`. Parsed rather than substring-matched —
/// a `contains(name)` check would be satisfied by a name appearing inside a category header, a
/// label, or another indicator's name.
fn printed_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter_map(|l| l.split_once('('))
        .map(|(name, _)| name.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// ⚠ **The gate the shipped README leans on.** Exactly the host-bound set, in both directions.
#[test]
fn the_listing_is_exactly_the_host_bound_set() {
    let mut printed = printed_names(&stdout_of(&["indicators"]));
    // ⚠ `is_callable`, not `RHAI_INDICATORS`. Those were the same question until per-line
    // accessors split them: `bollinger` has no bare call and three working accessors, so a roster
    // built from the bare names alone would UNDER-report — the exact failure the message below
    // names, and the one a user would hit trying to find the middle band.
    let mut bound: Vec<String> = vike_indicators::registry()
        .iter()
        .map(|m| m.name)
        .filter(|n| vike_script::is_callable(n))
        .map(|n| n.to_string())
        .collect();
    printed.sort();
    bound.sort();
    assert_eq!(
        printed, bound,
        "the printed roster and the host-bound set must be the same names — over-reporting hands a \
         user a script that compiles and never trades, under-reporting hides a working indicator"
    );
    assert!(!printed.is_empty(), "a build binding nothing would make every assertion here vacuous");
}

/// The count in the footer is COMPUTED from what was printed — the one number this feature states,
/// and it is never written into prose anywhere.
#[test]
fn the_footer_counts_what_it_printed() {
    let listing = stdout_of(&["indicators"]);
    let n = printed_names(&listing).len();
    assert!(
        listing.contains(&format!("{n} callable")),
        "the footer must report the number of rows above it; listing tail: {:?}",
        listing.lines().last()
    );
}

/// Each row carries the two facts a caller cannot guess: the parameters (with defaults, so a period
/// is not a mystery) and the label. Derived — the assertion walks whatever the build binds.
#[test]
fn every_row_shows_its_parameters_and_its_label() {
    let listing = stdout_of(&["indicators"]);
    for line in listing.lines().filter_map(|l| l.strip_prefix("  ")) {
        let Some((call, rest)) = line.split_once(')') else {
            panic!("a row must print a complete call form: {line:?}");
        };
        assert!(call.contains('('), "{line:?}");
        assert!(!rest.trim().is_empty(), "a row must carry its label: {line:?}");
    }
}

/// `--category` narrows to a family that the full listing itself named, and `--json` answers with
/// the same names in a machine shape. Both derived from the default listing, so neither test
/// hard-codes a category that a future registry could rename away.
#[test]
fn category_narrows_and_json_agrees_with_the_listing() {
    let listing = stdout_of(&["indicators"]);
    // A category header is the un-indented, non-empty line the rows hang under.
    let category = listing
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with(' '))
        .expect("the listing groups by category")
        .to_string();

    let narrowed = printed_names(&stdout_of(&["indicators", "--category", &category]));
    assert!(!narrowed.is_empty(), "the category the listing itself printed must match rows");
    let all = printed_names(&listing);
    assert!(narrowed.len() <= all.len());
    assert!(narrowed.iter().all(|n| all.contains(n)));
    // Case-insensitive, because a user retypes what they read without matching its case.
    let lowered =
        printed_names(&stdout_of(&["indicators", "--category", &category.to_lowercase()]));
    assert_eq!(lowered, narrowed);

    let json: serde_json::Value = serde_json::from_str(&stdout_of(&["indicators", "--json"]))
        .expect("--json must print valid JSON");
    let rows = json["indicators"].as_array().expect("an `indicators` array");
    let mut json_names: Vec<String> =
        rows.iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
    let mut all_sorted = all;
    json_names.sort();
    all_sorted.sort();
    assert_eq!(json_names, all_sorted, "--json and the listing must answer the same set");
    for row in rows {
        assert!(row["params"].is_array(), "a machine row must carry its parameters: {row}");
        assert!(row["outputs"].as_array().is_some_and(|o| !o.is_empty()), "{row}");
    }
}

/// ⚠ `--name` is what the shipped README tells a user to run when an indicator they expected is
/// missing, so it must separate the two cases the README promises it separates: a name that is
/// callable prints its row, and a real indicator the host holds back prints the REASON and exits
/// non-zero. Without the second, every absence looks like a typo and the user re-checks a spelling
/// that was already right.
///
/// Derived: which name is held back comes from `vike_script`, never from a list here.
#[test]
fn name_answers_for_a_bound_indicator_and_explains_a_held_back_one() {
    let bound = vike_script::RHAI_INDICATORS.first().expect("the host binds something");
    let one = stdout_of(&["indicators", "--name", bound]);
    assert_eq!(printed_names(&one), vec![bound.to_string()]);
    assert!(!one.contains("callable in this build"), "a single lookup is not a roster: {one:?}");

    // Genuinely held back = reachable by NO spelling. `bollinger` is absent from RHAI_INDICATORS
    // and perfectly callable, so the old predicate now picks a name the CLI rightly answers for.
    let held = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name));
    let Some(held) = held else {
        return; // nothing is held back in this build, so there is nothing to explain
    };
    let out = run(&["indicators", "--name", held.name]);
    assert!(!out.status.success(), "a name a script cannot call must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let why = vike_script::unbound_reason(held.name).expect("a held-back name must carry a reason");
    assert!(stderr.contains(why), "the CLI must quote the host's own reason: {stderr}");

    // ...and a name that is in no registry at all is the OTHER error — a typo, with a different
    // repair.
    let typo = run(&["indicators", "--name", "sma_typo"]);
    assert!(!typo.status.success());
    assert!(String::from_utf8_lossy(&typo.stderr).contains("no indicator named"));
}

/// An unknown category FAILS and names the ones that exist. An empty listing would read as "this
/// build binds nothing in that family", which is the answer that sends somebody into a config file.
#[test]
fn an_unknown_category_fails_and_names_the_real_ones() {
    let out = run(&["indicators", "--category", "not-a-category"]);
    assert!(!out.status.success(), "an unknown category must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not-a-category"), "{stderr}");
    let listing = stdout_of(&["indicators"]);
    let category = listing.lines().find(|l| !l.is_empty() && !l.starts_with(' ')).unwrap();
    assert!(stderr.contains(category), "the error must name a category that DOES exist: {stderr}");
}
