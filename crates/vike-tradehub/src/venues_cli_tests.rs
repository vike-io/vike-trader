use super::*;
use vike_config::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;

fn arg(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn row(
    venue: &'static str,
    label: Option<&str>,
    tier: VenueMode,
    effective: VenueMode,
    block: ArmingBlock,
) -> VenueArming {
    VenueArming {
        venue,
        label: label
            .map_or(AccountLabel::Default, |l| AccountLabel::parse(l).expect("a legal label")),
        tier,
        effective,
        block,
        account_ids: Vec::new(),
    }
}

/// The same row, stated by the `account` rows `ids`.
fn with_ids(row: VenueArming, ids: &[i64]) -> VenueArming {
    VenueArming { account_ids: ids.to_vec(), ..row }
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
// A store its daemon refuses (decision 0095, Task 7)
// ------------------------------------------------------------------------------------------

const LEAK: &str = "sk-do-not-print-me";
/// A real credential's name, composed so the settings registry's literal sweep reads no variable.
const CREDENTIAL: &str = concat!("BINANCE", "_LIVE_API_KEY");

/// A project whose credential store holds `body`'s `KEY=value` lines, and the environment that
/// points the verb at it.
/// Callers compose a venue-setting name with `concat!` so the settings registry's literal sweep
/// does not read a variable out of this file.
fn project_with(body: &str) -> (tempfile::TempDir, HashMap<String, String>) {
    let dir = tempfile::tempdir().expect("a temp project");
    // The settings DATABASE is the only credential store: create it the one way a store comes into
    // being (`vike-cli secrets init`'s library half) and seed `body`'s `KEY=value` lines
    // through the one sanctioned writer, so the boot reads these rows.
    vike_secrets::create_store(dir.path().to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> = body
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vike_secrets::save_credentials_to_store(
        dir.path(),
        vike_secrets::Table::Credential,
        &rows,
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .expect("seed the store");
    let env = HashMap::from([(
        vike_secrets::SETTINGS_DIR_ENV.to_string(),
        dir.path().display().to_string(),
    )]);
    (dir, env)
}

/// **A venue setting filed under its old credential name is read by nothing and stops nothing**
/// (`docs/decisions/0117-there-are-no-migrations.md`): `venues` starts on such a store, and the
/// credential beside it is still read.
#[test]
fn a_store_holding_an_old_setting_name_still_starts_the_verb() {
    let old_name = concat!("IBKR", "_DEMO_PORT");
    let (dir, env) = project_with(&format!("{old_name}={LEAK}\n{CREDENTIAL}={LEAK}\n"));
    let booted = boot(&env, Some(dir.path()))
        .unwrap_or_else(|why| panic!("a row nothing reads must not stop the verb: {why}"));
    assert!(booted.credentials.as_ref().is_some_and(|c| c.contains_key(CREDENTIAL)));
}

/// **A row under a retired arming switch's name is read by nothing and stops nothing** either: the
/// boot's credential-row arming refusal was deleted (`docs/decisions/0117-there-are-no-migrations.md`),
/// so `BYBIT_MAINNET` is an unknown credential name like any other.
#[test]
fn a_retired_arming_switch_row_no_longer_refuses_the_boot() {
    let retired = concat!("BYBIT", "_MAINNET");
    let (dir, env) = project_with(&format!("{retired}=1\n{CREDENTIAL}={LEAK}\n"));
    let booted = boot(&env, Some(dir.path()))
        .unwrap_or_else(|why| panic!("a row nothing reads must not stop the verb: {why}"));
    assert!(booted.credentials.as_ref().is_some_and(|c| c.contains_key(CREDENTIAL)));
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

/// ⚠ **THE COLUMN THAT EARNS THE SCREEN**: an account whose row says `live` and whose engine
/// reaches only `paper` must render BOTH, side by side, plus the cause. A table that showed the
/// account row's tier alone would generate the complaint this command exists to answer.
#[test]
fn a_capped_row_shows_the_tier_the_effective_tier_and_the_cause() {
    let rows =
        [row("binance", None, VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials)];
    let out = render_table(None, true, &rows, false);
    assert!(out.contains("TIER"), "the column is the account's TIER: {out}");
    assert!(!out.contains("CEILING"), "there is no venue ceiling any more: {out}");
    let line = out.lines().find(|l| l.starts_with("binance")).expect("a binance row");
    assert!(line.contains("live"), "the TIER the account row states: {line}");
    assert!(line.contains("paper"), "…the tier it actually reaches: {line}");
    assert!(line.contains("NoCredentials"), "…and the cause: {line}");
}

/// ⚠ **The account LEADS the sentence, and this test is the reason.** `VenueArming::why` names the
/// VENUE, not the account, so two accounts of one venue blocked for one reason render the same
/// sentence twice — which reads as a rendering bug. The route keys and the `account.id`s differ,
/// so leading with them is what keeps the two entries distinguishable, and the id is what the
/// account verbs' `--id` takes.
#[test]
fn two_accounts_blocked_for_one_reason_are_still_told_apart() {
    let rows = [
        with_ids(
            row("binance", None, VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials),
            &[3],
        ),
        with_ids(
            row(
                "binance",
                Some("ALT"),
                VenueMode::Live,
                VenueMode::Paper,
                ArmingBlock::NoCredentials,
            ),
            &[7],
        ),
    ];
    let out = render_table(None, true, &rows, false);
    assert!(out.contains("  binance  (account row id 3)"), "the default account's lead: {out}");
    assert!(out.contains("  binance#ALT  (account row id 7)"), "…and ALT's: {out}");
}

/// **A `TierConflict` is SAID, though its tier and its effective tier are both `paper`.** Two
/// ACTIVE non-paper rows for one account mount it PAPER (decision 0119): the comparison alone
/// would neither keep it in `--blocked` nor give it a sentence, and it is the one paper cause the
/// operator never chose. The ordinary paper causes stay in `--blocked` (they carry a block) but
/// get no sentence, so they cannot bury it.
#[test]
fn a_tier_conflict_is_blocked_and_said_while_a_paper_tier_is_only_blocked() {
    let conflict =
        row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::TierConflict);
    let paper = row("okx", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier);
    let armed = row("bybit", None, VenueMode::Demo, VenueMode::Demo, ArmingBlock::None);
    assert!(is_blocked(&conflict) && is_blocked(&paper), "every block is in the blocked view");
    assert!(!is_blocked(&armed), "an account at its own tier with no block is not blocked");
    let out = render_table(None, true, &[conflict, paper, armed], false);
    assert!(out.contains("  binance  (account rows: `vike-cli secrets accounts`)"), "{out}");
    assert!(!out.contains("  okx  ("), "a deliberate paper tier gets no sentence: {out}");
}

/// **`--json` carries `tier` and `account_ids`**, the keys a script reads — `ceiling` and `key`
/// named a per-venue line that decision 0119 deleted.
#[test]
fn the_json_row_names_the_tier_and_the_account_ids() {
    let r = with_ids(
        row("binance", Some("ALT"), VenueMode::Live, VenueMode::Paper, ArmingBlock::NoCredentials),
        &[7, 9],
    );
    let src = include_str!("venues_cli.rs");
    let start = src.find("\nfn print_json(").expect("venues_cli.rs defines `print_json`");
    let body = &src[start..];
    let body = &body[..body.find("\n}\n").expect("`print_json`'s closing brace")];
    for key in ["\"tier\": r.tier.as_str()", "\"account_ids\": r.account_ids"] {
        assert!(body.contains(key), "`--json` must carry {key}:\n{body}");
    }
    for gone in ["\"ceiling\"", "\"key\""] {
        assert!(!body.contains(gone), "`--json` still carries {gone}:\n{body}");
    }
    assert_eq!(
        serde_json::json!({ "account_ids": r.account_ids })["account_ids"],
        serde_json::json!([7, 9])
    );
}

/// ⚠ **`ENGINE` is not "effective is not paper".** The venue's DEFAULT account is mounted even
/// on paper; a labelled one capped to paper is not. Using the obvious predicate would print
/// `no` beside every paper venue on a box where those engines exist.
#[test]
fn the_default_account_has_an_engine_even_on_paper_and_a_capped_label_does_not() {
    let rows = [
        row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier),
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
    let r = row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier);
    assert_eq!(account_cell(&r), "-");
    let out = render_table(None, true, &[r], false);
    assert!(!out.to_lowercase().contains("default"), "{out}");
}

#[test]
fn a_venue_summary_line_appears_only_where_a_venue_has_more_than_one_account() {
    let one = [row("okx", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier)];
    assert!(!render_table(None, true, &one, false).contains("okx: "), "one account, no summary");
    let two = [
        row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier),
        row("binance", Some("ALT"), VenueMode::Paper, VenueMode::Paper, ArmingBlock::PaperTier),
    ];
    assert!(render_table(None, true, &two, false).contains("binance: 2 accounts"));
}

/// **The projection resolves against the venue settings the daemon reads.** A bridge's `resolve`
/// reads them — IBKR's turns an unparseable stored backend into paper
/// (`crates/bridges/vike-ibkr/src/mount_tests.rs`'s `resolve_is_the_arms_own_gate`) — so a
/// projection over an EMPTY settings view would print that account armed while the daemon mounts
/// it paper. The stored row must reach the policy the projection hands `vike_mount::venue_arming`.
#[test]
fn the_projection_reads_the_venue_settings_the_daemon_reads() {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a fresh settings store");
    vike_secrets::set_venue_setting_in(dir.path(), "ibkr", Some("demo"), "BACKEND", "cpap")
        .expect("plant a stored backend the loader refuses");

    let policy =
        projection_policy(&HashMap::new(), &vike_config::Policy::default(), Some(dir.path()));
    let ibkr = policy.venue_settings.get("ibkr").expect("the ibkr rows reach the projection");
    assert_eq!(
        ibkr.get_exact(vike_secrets::venue_setting::SettingTier::Demo, "backend"),
        Some("cpap")
    );
    // …and no settings directory is an empty view, as it is for the daemon.
    assert!(
        projection_policy(&HashMap::new(), &vike_config::Policy::default(), None)
            .venue_settings
            .is_empty()
    );
}

/// **`collect` takes its policy from [`projection_policy`], over the boot's settings directory.**
///
/// ⚠ A STRUCTURAL pin, and why it cannot be a behavioural one here: the only bridge whose `resolve`
/// reads a venue setting is IBKR, and its arm is compiled only under this crate's `ibkr` feature,
/// so in the build this test runs in no projection can be OBSERVED to change with the stored
/// settings. What can be held is the wiring: [`collect`] — the function [`run`] calls, which needs a
/// whole boot — builds no `MountPolicy` of its own and hands the boot's settings directory to the
/// function [`the_projection_reads_the_venue_settings_the_daemon_reads`] tests.
#[test]
fn collect_takes_its_policy_from_projection_policy() {
    let src = include_str!("venues_cli.rs");
    let start = src.find("\nfn collect(").expect("venues_cli.rs defines `collect`");
    let rest = &src[start..];
    let body = &rest[..rest.find("\n}\n").expect("`collect`'s closing brace")];
    assert!(
        body.contains(
            "projection_policy(env, &booted.settings.policy, booted.settings_dir.as_deref())"
        ),
        "`collect` must resolve against `projection_policy` over the boot's settings dir:\n{body}"
    );
    assert!(
        !body.contains("MountPolicy {"),
        "`collect` builds a `MountPolicy` of its own again, beside `projection_policy`:\n{body}"
    );
}

/// **`vike-backend venues` says a live-tier key set exists when one does** — end to end, over the REAL
/// registry and the real projection, rendered by the real table. The store below is the one the
/// final review found: oanda holding BOTH tiers. Its mount refuses to pick (a store that says `live`
/// must never trade the practice account in silence) and logs that at `error!`, and this screen used
/// to print `NoCredentials` beside it. It now prints the cause the log line is about, and the sentence
/// under the table says which tier was found and which way out there is.
#[test]
fn a_live_tier_key_set_is_named_not_called_missing() {
    let vars: HashMap<String, String> = [
        ("OANDA_DEMO_API_KEY", "demo-token"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001"),
        ("OANDA_LIVE_API_KEY", "live-token"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let policy = vike_mount::MountPolicy::default().with_account(
        "oanda",
        &AccountLabel::Default,
        VenueMode::Demo,
    );
    let rows: Vec<VenueArming> =
        vike_mount::venue_arming(crate::registry::REGISTRY, &vars, &policy)
            .into_iter()
            .filter(|r| r.venue == "oanda")
            .collect();
    assert_eq!(rows.len(), 1, "the default account's row");
    let out = render_table(None, true, &rows, false);
    let line = out.lines().find(|l| l.starts_with("oanda")).expect("an oanda row");
    assert!(line.contains("demo") && line.contains("paper"), "tier and effective: {line}");
    assert!(line.contains("LiveTierNotWired"), "the cause is the live tier: {line}");
    assert!(!line.contains("NoCredentials"), "…and not 'no credentials': {line}");
    let ids = &rows[0].account_ids;
    assert_eq!(ids.len(), 1, "the one planted `(oanda, demo)` row states the tier: {ids:?}");
    assert!(
        out.contains(&format!("  oanda  (account row id {})", ids[0])),
        "the account and its row id lead the sentence: {out}"
    );
    assert!(
        out.contains("LIVE-tier") && out.contains("stays PAPER"),
        "the sentence under the table: {out}"
    );
    // The same row through `--json`'s own two fields.
    let r = &rows[0];
    assert_eq!(format!("{:?}", r.block), "LiveTierNotWired");
    assert!(r.why().contains("oanda"), "{}", r.why());
    // …and no key or token value appears anywhere on the screen.
    for secret in ["demo-token", "live-token", "001-001-1-001"] {
        assert!(!out.contains(secret), "{secret} leaked onto the screen");
    }
}
