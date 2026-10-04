use super::*;
use vike_config::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;

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
// A store its daemon refuses (decision 0095, Task 7)
// ------------------------------------------------------------------------------------------

const LEAK: &str = "sk-do-not-print-me";
/// A real credential's name, composed so the settings registry's literal sweep reads no variable.
const CREDENTIAL: &str = concat!("BINANCE", "_LIVE_API_KEY");

/// A project whose credential file holds `body`, and the environment that points the verb at it.
/// Callers compose a venue-setting name with `concat!` so the settings registry's literal sweep
/// does not read a variable out of this file.
fn project_with(body: &str) -> (tempfile::TempDir, HashMap<String, String>) {
    let dir = tempfile::tempdir().expect("a temp project");
    std::fs::write(dir.path().join("secrets.env"), body).expect("plant the credential file");
    let env = HashMap::from([(
        vike_secrets::SETTINGS_DIR_ENV.to_string(),
        dir.path().display().to_string(),
    )]);
    (dir, env)
}

/// …holding ONE venue setting under its old name beside one real credential.
fn stranded_project(stranded: &str) -> (tempfile::TempDir, HashMap<String, String>) {
    project_with(&format!("{stranded}={LEAK}\n{CREDENTIAL}={LEAK}\n"))
}

/// **`venues` exists to diagnose the box that refuses, so it starts on a store its daemon refuses
/// and says so.** A credential row under a venue setting's old name stops the trading and data
/// daemons at their boot; this verb mounts nothing and reads none of those rows, and its own boot
/// already declines to stop over a retired variable "because that would stop an operator
/// diagnosing this box". Refusing here, over the other half of the same decision, took the one
/// command that answers "why is this venue still paper" away from the operator holding exactly
/// that store.
///
/// The finding is named and valueless: the row, the setting it stands for and the verb that moves
/// it — never the planted value, never the credential beside it.
#[test]
fn a_stranded_store_is_reported_and_the_verb_still_starts() {
    let stranded = concat!("IBKR", "_DEMO_PORT");
    let (dir, env) = stranded_project(stranded);
    let booted = boot(&env, Some(dir.path())).unwrap_or_else(|why| {
        panic!("a diagnosing verb must start on a store its daemon refuses: {why}")
    });
    let note = stranded_notice(&booted).expect("…and say that the daemons will refuse it");
    assert!(note.contains(stranded) && note.contains("venue.ibkr.demo.port"), "{note}");
    assert!(note.contains("REFUSE"), "it must say what the daemons do: {note}");
    assert!(note.contains("vike-cli secrets move-venue-config"), "…and the repair: {note}");
    assert!(!note.contains(LEAK), "a value leaked: {note}");
    assert!(!note.contains(CREDENTIAL), "a credential name leaked: {note}");
}

/// …and a store with nothing stranded says nothing — the clean-box output is unchanged.
#[test]
fn a_clean_store_has_nothing_to_report() {
    let (dir, env) = project_with(concat!("BINANCE", "_DEMO_API_KEY=k\n"));
    let booted = boot(&env, Some(dir.path())).expect("a clean store boots");
    assert_eq!(stranded_notice(&booted), None);
}

/// Reporting the stranded rows must not soften the OTHER refusal the credential load carries: a
/// row that ARMS real money still stops this verb, exactly as before. (The arming refusal is the
/// half of that load this binary's boot calls "welcome"; only the stranded one is downgraded.)
#[test]
fn an_arming_row_still_refuses_the_boot() {
    let (dir, env) = project_with(concat!("BYBIT", "_MAINNET=1\n"));
    let why = boot(&env, Some(dir.path())).err().expect("an arming row refuses");
    assert!(why.contains("REFUSING TO START"), "{why}");
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
    use vike_config::VenuePolicy;
    let vars: HashMap<String, String> = [
        ("OANDA_DEMO_API_KEY", "demo-token"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001"),
        ("OANDA_LIVE_API_KEY", "live-token"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let policy = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("oanda", VenueMode::Demo),
        ..Default::default()
    };
    let rows: Vec<VenueArming> =
        vike_mount::venue_arming(crate::registry::REGISTRY, &vars, &policy)
            .into_iter()
            .filter(|r| r.venue == "oanda")
            .collect();
    assert_eq!(rows.len(), 1, "the default account's row");
    let out = render_table(None, true, &rows, false);
    let line = out.lines().find(|l| l.starts_with("oanda")).expect("an oanda row");
    assert!(line.contains("demo") && line.contains("paper"), "ceiling and effective: {line}");
    assert!(line.contains("LiveTierNotWired"), "the cause is the live tier: {line}");
    assert!(!line.contains("NoCredentials"), "…and not 'no credentials': {line}");
    assert!(out.contains("policy.venues.oanda"), "the key that leads the sentence: {out}");
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
