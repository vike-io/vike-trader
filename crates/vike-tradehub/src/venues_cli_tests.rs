use super::*;
use vike_config::ArmingBlock;
use vike_model::account_keys::AccountLabel;

fn arg(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn row(
    venue: &'static str,
    label: Option<&str>,
    ceiling: VenueMode,
    effective: VenueMode,
    block: ArmingBlock,
) -> VenueArming {
    VenueArming {
        venue,
        label: label
            .map_or(AccountLabel::Default, |l| AccountLabel::parse(l).expect("a legal label")),
        ceiling,
        effective,
        block,
    }
}

/// Decision 0095: the projection sees the Polymarket gates exactly as the daemon folds them — a
/// `flags.poly_exec` row reaches the map under the name the arm reads, and nothing the credential
/// map carried is lost.
#[test]
fn the_projection_folds_the_flags_the_daemon_folds() {
    let creds = HashMap::from([("some_credential".to_string(), "k".to_string())]);
    let flags = vike_config::Flags { poly_exec: true, ..Default::default() };
    let vars = arming_vars(Some(&creds), flags);
    assert_eq!(vars.get(vike_config::flags::POLY_EXEC_ENV).map(String::as_str), Some("1"));
    assert_eq!(vars.get(vike_config::flags::POLY_RECONCILE_ENV).map(String::as_str), Some("0"));
    assert_eq!(vars.get("some_credential").map(String::as_str), Some("k"));
    assert!(
        arming_vars(None, vike_config::Flags::default())
            .contains_key(vike_config::flags::POLY_EXEC_ENV),
        "no store still folds the flags"
    );
}

// ------------------------------------------------------------------------------------------
// The flag grammar
// ------------------------------------------------------------------------------------------

#[test]
fn no_flags_is_the_whole_table() {
    assert_eq!(parse_args(&[]).unwrap(), Opts::default());
}

/// ⚠ `--venue` takes its value in BOTH spellings — the pair every other verb here accepts, so
/// an operator who learned one at `submit` does not have to learn the other at this prompt.
#[test]
fn the_venue_flag_takes_both_spellings() {
    assert_eq!(parse_args(&arg("--venue binance")).unwrap().venue.as_deref(), Some("binance"));
    assert_eq!(parse_args(&arg("--venue=binance")).unwrap().venue.as_deref(), Some("binance"));
}

/// ⚠ **A flag where a VALUE should be is a refusal, not a value.** `--venue --json` means the
/// operator's venue went missing and the next flag was eaten as one; accepting it would filter
/// on a venue called `--json` and print an empty table, which reads as "nothing to see".
#[test]
fn a_flag_in_the_value_slot_is_refused() {
    let e = parse_args(&arg("--venue --json")).expect_err("the value went missing");
    assert!(e.contains("--venue"), "{e}");
    assert!(parse_args(&arg("--venue")).is_err(), "a trailing --venue has no value at all");
}

#[test]
fn help_wins_over_everything_else_on_the_line() {
    assert!(parse_args(&arg("--json --help")).unwrap().help);
    assert!(parse_args(&arg("--help --venue")).unwrap().help, "…and parses no further");
}

#[test]
fn an_unknown_flag_is_refused_by_name() {
    let e = parse_args(&arg("--verbose")).expect_err("not a flag this verb has");
    assert!(e.contains("--verbose"), "the refusal must NAME it: {e}");
}

// ------------------------------------------------------------------------------------------
// The rendering
// ------------------------------------------------------------------------------------------

/// ⚠ **THE COLUMN THAT EARNS THE SCREEN**: a venue whose ceiling says `live` and whose engine
/// reaches only `paper` must render BOTH, side by side, plus the cause. A table that showed the
/// ceiling alone would generate the complaint this command exists to answer.
#[test]
fn a_capped_row_shows_the_ceiling_the_effective_tier_and_the_cause() {
    let rows =
        [row("binance", None, VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials)];
    let out = render_table(None, true, &rows, false);
    let line = out.lines().find(|l| l.starts_with("binance")).expect("a binance row");
    assert!(line.contains("live"), "the CEILING somebody asked for: {line}");
    assert!(line.contains("paper"), "…the tier it actually reaches: {line}");
    assert!(line.contains("NoCredentials"), "…and the cause: {line}");
}

/// ⚠ **The key LEADS the sentence, and this test is the reason.** `VenueArming::why` names the
/// VENUE, not the account, so two accounts of one venue blocked for one reason render the same
/// sentence twice — which reads as a rendering bug. The keys differ, so leading with them is
/// what keeps the two entries distinguishable.
#[test]
fn two_accounts_blocked_for_one_reason_are_still_told_apart() {
    let rows = [
        row("binance", None, VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials),
        row("binance", Some("ALT"), VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials),
    ];
    let out = render_table(None, true, &rows, false);
    assert!(out.contains("policy.venues.binance"), "the venue's own line: {out}");
    assert!(out.contains("policy.accounts.binance.ALT"), "…and the account's: {out}");
}

/// ⚠ **`ENGINE` is not "effective is not paper".** The venue's DEFAULT account is mounted even
/// on paper; a labelled one capped to paper is not. Using the obvious predicate would print
/// `no` beside every paper venue on a box where those engines exist.
#[test]
fn the_default_account_has_an_engine_even_on_paper_and_a_capped_label_does_not() {
    let rows = [
        row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
        row("binance", Some("ALT"), VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials),
    ];
    let out = render_table(None, true, &rows, false);
    let mut lines = out.lines().filter(|l| l.starts_with("binance "));
    assert!(lines.next().expect("the default row").contains("yes"), "{out}");
    assert!(lines.next().expect("the ALT row").contains("no"), "{out}");
}

/// An ABSENT store is ONE fact, said once, not one per row — a table of `NoCredentials` with no
/// line above it reads as fourteen separate venue problems.
#[test]
fn an_unread_credential_store_is_one_line_above_the_table() {
    let rows =
        [row("binance", None, VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials)];
    assert!(render_table(None, false, &rows, false).contains("NO STORE READ"));
    assert!(
        !render_table(None, true, &rows, false).contains("NO STORE READ"),
        "a store that WAS read must say nothing — the line is a finding, not a header"
    );
}

/// ⚠ The empty `--blocked` view must say NOTHING IS BLOCKED, not print an empty table. An empty
/// table is indistinguishable from a broken command, and this is the view an operator opens
/// when they already suspect something is wrong.
#[test]
fn an_empty_blocked_view_says_nothing_is_blocked() {
    let out = render_table(None, true, &[], true);
    assert!(out.contains("nothing is blocked"), "{out}");
}

/// The tally always states all three tiers, so `live` being zero is SAID rather than left to be
/// inferred from an absence — on this screen that is the number being looked for.
#[test]
fn the_tally_always_names_all_three_tiers() {
    assert_eq!(tally([VenueMode::Paper, VenueMode::Paper].into_iter()), "0 live, 0 demo, 2 paper");
    assert_eq!(tally(std::iter::empty()), "0 live, 0 demo, 0 paper");
}

/// The unlabelled account renders `-`, NEVER the word `default`: it has no label, and printing
/// one invents a name the operator cannot write into any file.
#[test]
fn the_unlabelled_account_is_a_dash_and_never_the_word_default() {
    let r = row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed);
    assert_eq!(account_cell(&r), "-");
    let out = render_table(None, true, &[r], false);
    assert!(!out.to_lowercase().contains("default"), "{out}");
}

#[test]
fn a_venue_summary_line_appears_only_where_a_venue_has_more_than_one_account() {
    let one = [row("okx", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed)];
    assert!(!render_table(None, true, &one, false).contains("okx: "), "one account, no summary");
    let two = [
        row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
        row("binance", Some("ALT"), VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
    ];
    assert!(render_table(None, true, &two, false).contains("binance: 2 accounts"));
}
