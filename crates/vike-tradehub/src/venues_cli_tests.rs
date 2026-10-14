use super::*;
use vike_config::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;

fn arg(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn row(
    venue: &'static str,
    label: Option<&str>,
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
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
/// A DEPLOYMENT name the store is read for by name — the read probe: an account's credential has
/// no name to probe by (decision 0156 item 6). Taken from its table, so the settings registry's
/// literal sweep reads no variable here.
const CREDENTIAL: &str = vike_venues::credential_keys::DATA_PROVIDER_KEYS[0];

/// A project whose credential store holds `body`'s `KEY=value` lines, and the environment that
/// points the verb at it.
/// Callers compose a venue-setting name with `concat!` so the settings registry's literal sweep
/// does not read a variable out of this file.
fn project_with(body: &str) -> (tempfile::TempDir, HashMap<String, String>) {
    let dir = tempfile::tempdir().expect("a temp project");
    // The settings DATABASE is the only credential store: create it the one way a store comes into
    // being (`vike-cli secrets init`'s library half) and seed `body`'s `KEY=value` lines
    // through the writer's record-less test door, so the boot reads these rows.
    vike_secrets::create_store(dir.path().to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> = body
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        dir.path(),
        vike_secrets::Table::Credential,
        &rows,
        vike_secrets::UpsertIntent::Add,
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
    assert!(booted.credentials.as_ref().is_some_and(|c| c.secrets.keys().any(|k| k == CREDENTIAL)));
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
    assert!(booted.credentials.as_ref().is_some_and(|c| c.secrets.keys().any(|k| k == CREDENTIAL)));
}

// ------------------------------------------------------------------------------------------
// The flag grammar
// ------------------------------------------------------------------------------------------

#[test]
fn no_flags_is_the_whole_table() {
    assert_eq!(read_opts(&[]).unwrap(), Opts::default());
}

/// ⚠ `--venue` takes its value in BOTH spellings — the pair every other verb here accepts, so
/// an operator who learned one at `submit` does not have to learn the other at this prompt.
#[test]
fn the_venue_flag_takes_both_spellings() {
    assert_eq!(read_opts(&arg("--venue binance")).unwrap().venue.as_deref(), Some("binance"));
    assert_eq!(read_opts(&arg("--venue=binance")).unwrap().venue.as_deref(), Some("binance"));
}

/// ⚠ **A flag where a VALUE should be is a refusal, not a value.** `--venue --json` means the
/// operator's venue went missing and the next flag was eaten as one; accepting it would filter
/// on a venue called `--json` and print an empty table, which reads as "nothing to see".
#[test]
fn a_flag_in_the_value_slot_is_refused() {
    let e = read_opts(&arg("--venue --json")).expect_err("the value went missing");
    assert_eq!(e.code, 2, "a usage error exits 2: {e:?}");
    assert!(e.text.contains("--venue"), "{e:?}");
    assert!(read_opts(&arg("--venue")).is_err(), "a trailing --venue has no value at all");
}

/// Every command line `venues` ACCEPTS today, pinned as POSITIVE cases (decision 0155). The case
/// table is the pin; only `accepted` (`Some` = accepted) adapts when the reader changes.
#[test]
fn every_accepted_command_line_stays_accepted() {
    let accepted = |line: &str| match read_opts(&arg(line)) {
        Ok(opts) => Some(opts),
        // `-h`/`--help` are answered by the reader (exit 0, stdout), not run.
        Err(exit) if exit.code == 0 => Some(Opts { help: true, ..Opts::default() }),
        Err(_) => None,
    };
    let opts = |venue: Option<&str>, blocked_only, json, help| {
        Some(Opts { venue: venue.map(str::to_string), blocked_only, json, help })
    };
    let cases = [
        // docs/ops/fxcm-forexconnect.md: `vike-backend venues` and `venues --venue fxcm`.
        ("the whole table", "", opts(None, false, false, false)),
        ("one venue", "--venue fxcm", opts(Some("fxcm"), false, false, false)),
        ("one venue, inline", "--venue=fxcm", opts(Some("fxcm"), false, false, false)),
        ("the blocked view", "--blocked", opts(None, true, false, false)),
        ("for a script", "--json", opts(None, false, true, false)),
        (
            "everything at once",
            "--blocked --json --venue binance",
            opts(Some("binance"), true, true, false),
        ),
        (
            "flags after the value",
            "--venue binance --json",
            opts(Some("binance"), false, true, false),
        ),
        ("a venue called help", "--venue help", opts(Some("help"), false, false, false)),
        ("-h", "-h", opts(None, false, false, true)),
        ("--help", "--help", opts(None, false, false, true)),
        ("the word help", "help", opts(None, false, false, true)),
        ("help after a flag", "--json --help", opts(None, false, false, true)),
        ("help ahead of a dangling flag", "--help --venue", opts(None, false, false, true)),
        ("the word help after a flag", "--json help", opts(None, false, false, true)),
    ];
    for (what, line, want) in cases {
        let got = accepted(line);
        // A help request stops the parse, so whatever else was on the line is not part of the
        // answer: compare the help bit alone.
        match (&got, &want) {
            (Some(g), Some(w)) if w.help => assert!(g.help, "{what}: `{line}`"),
            _ => assert_eq!(got, want, "{what}: `{line}`"),
        }
    }
}

#[test]
fn help_wins_over_everything_else_on_the_line() {
    // `-h`/`--help` stop the parse where they stand and come back as the reader's answer: exit 0,
    // on stdout, whatever else (even a dangling `--venue`) follows.
    for line in ["--json --help", "--help --venue", "-h"] {
        let exit = read_opts(&arg(line)).expect_err("a help request is answered, not run");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{line}: {exit:?}");
        assert!(exit.text.contains("Usage:"), "{line}: {}", exit.text);
    }
    // The bare word is the same request, and it IS a parse result.
    assert!(read_opts(&arg("help")).unwrap().help);
    assert!(read_opts(&arg("--json help")).unwrap().help);
}

#[test]
fn an_unknown_flag_is_refused_by_name() {
    let e = read_opts(&arg("--verbose")).expect_err("not a flag this verb has");
    assert_eq!(e.code, 2, "a usage error exits 2: {e:?}");
    assert!(e.text.contains("--verbose"), "the refusal must NAME it: {e:?}");
    // A stray WORD is refused too (only `help` is one), and is never echoed.
    let word = read_opts(&arg("binance")).expect_err("not a word this verb has");
    assert_eq!(word.code, 2, "{word:?}");
    assert!(!word.text.contains("binance"), "{word:?}");
}

// ------------------------------------------------------------------------------------------
// The rendering
// ------------------------------------------------------------------------------------------

/// ⚠ **THE COLUMN THAT EARNS THE SCREEN**: an account whose row says `live` and that does NOT RUN
/// (decision 0156: never `paper` in its place) must render its tier, no effective tier, no engine,
/// and the cause, side by side. A table that showed the account row's tier alone would generate
/// the complaint this command exists to answer.
#[test]
fn a_row_that_does_not_run_shows_its_tier_no_effective_tier_and_the_cause() {
    let rows = [row("binance", None, Some(AccountTier::Live), None, ArmingBlock::NoCredentials)];
    let out = render_table(None, true, &rows, false);
    assert!(out.contains("TIER"), "the column is the account's TIER: {out}");
    assert!(!out.contains("CEILING"), "there is no venue ceiling any more: {out}");
    let line = out.lines().find(|l| l.starts_with("binance")).expect("a binance row");
    let cells: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        cells,
        ["binance", "-", "live", "-", "no", "NoCredentials"],
        "venue, account, the TIER its row states, no effective tier, no engine, the cause"
    );
    assert!(!line.contains("paper"), "an account that does not run is never drawn paper: {line}");
    assert!(out.contains("0 live, 0 demo, 0 paper, 1 not running"), "the tally counts it: {out}");
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
            row("binance", None, Some(AccountTier::Live), None, ArmingBlock::NoCredentials),
            &[3],
        ),
        with_ids(
            row("binance", Some("ALT"), Some(AccountTier::Live), None, ArmingBlock::NoCredentials),
            &[7],
        ),
    ];
    let out = render_table(None, true, &rows, false);
    assert!(out.contains("  binance  (account row id 3)"), "the default account's lead: {out}");
    assert!(out.contains("  binance#ALT  (account row id 7)"), "…and ALT's: {out}");
}

/// **A `TierConflict` is SAID, though no single tier is stated for it.** Two ACTIVE non-paper rows
/// for one account leave it NOT RUNNING (decisions 0119, 0156): it is the one table cause the
/// operator never chose. A deliberate `paper` account stays in `--blocked` (it carries a block)
/// but gets no sentence, so it cannot bury it.
#[test]
fn a_tier_conflict_is_blocked_and_said_while_a_paper_tier_is_only_blocked() {
    let conflict = row("binance", None, None, None, ArmingBlock::TierConflict);
    let paper = row(
        "okx",
        None,
        Some(AccountTier::Paper),
        Some(AccountTier::Paper),
        ArmingBlock::PaperTier,
    );
    let armed =
        row("bybit", None, Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
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
        row("binance", Some("ALT"), Some(AccountTier::Live), None, ArmingBlock::NoCredentials),
        &[7, 9],
    );
    let src = include_str!("venues_cli.rs");
    let start = src.find("\nfn print_json(").expect("venues_cli.rs defines `print_json`");
    let body = &src[start..];
    let body = &body[..body.find("\n}\n").expect("`print_json`'s closing brace")];
    for key in ["\"tier\": r.tier.map(AccountTier::as_str)", "\"account_ids\": r.account_ids"] {
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

/// ⚠ **`ENGINE` is "the account runs", not "effective is not paper".** A `paper` account runs the
/// simulator, so it has an engine; an account that does not run has none, whatever its row states
/// (decision 0156). The obvious predicate would print `no` beside every paper account.
#[test]
fn a_paper_account_has_an_engine_and_one_that_does_not_run_has_none() {
    let rows = [
        row(
            "binance",
            None,
            Some(AccountTier::Paper),
            Some(AccountTier::Paper),
            ArmingBlock::PaperTier,
        ),
        row("binance", Some("ALT"), Some(AccountTier::Live), None, ArmingBlock::NoCredentials),
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
    let rows = [row("binance", None, Some(AccountTier::Live), None, ArmingBlock::NoCredentials)];
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

/// The tally always states all three tiers and the accounts that do not run, so `live` being zero
/// is SAID rather than left to be inferred from an absence — on this screen that is the number
/// being looked for — and an account that does not run is never counted as paper (decision 0156).
#[test]
fn the_tally_always_names_all_three_tiers() {
    let paper = Some(AccountTier::Paper);
    assert_eq!(tally([paper, paper].into_iter()), "0 live, 0 demo, 2 paper, 0 not running");
    assert_eq!(tally(std::iter::empty()), "0 live, 0 demo, 0 paper, 0 not running");
    assert_eq!(
        tally([None, Some(AccountTier::Live)].into_iter()),
        "1 live, 0 demo, 0 paper, 1 not running"
    );
}

/// The unlabelled account renders `-`, NEVER the word `default`: it has no label, and printing
/// one invents a name the operator cannot write into any file.
#[test]
fn the_unlabelled_account_is_a_dash_and_never_the_word_default() {
    let r = row(
        "binance",
        None,
        Some(AccountTier::Paper),
        Some(AccountTier::Paper),
        ArmingBlock::PaperTier,
    );
    assert_eq!(account_cell(&r), "-");
    let out = render_table(None, true, &[r], false);
    assert!(!out.to_lowercase().contains("default"), "{out}");
}

#[test]
fn a_venue_summary_line_appears_only_where_a_venue_has_more_than_one_account() {
    let one = [row(
        "okx",
        None,
        Some(AccountTier::Paper),
        Some(AccountTier::Paper),
        ArmingBlock::PaperTier,
    )];
    assert!(!render_table(None, true, &one, false).contains("okx: "), "one account, no summary");
    let two = [
        row(
            "binance",
            None,
            Some(AccountTier::Paper),
            Some(AccountTier::Paper),
            ArmingBlock::PaperTier,
        ),
        row(
            "binance",
            Some("ALT"),
            Some(AccountTier::Paper),
            Some(AccountTier::Paper),
            ArmingBlock::PaperTier,
        ),
    ];
    assert!(render_table(None, true, &two, false).contains("binance: 2 accounts"));
}

/// The full-scope handle over the store in `dir` (none: a handle over no store) — what
/// `Booted::store` is for a test project.
fn full_store(dir: Option<&Path>) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        dir.map(vike_secrets::StoreLocation::new),
        vike_secrets::CredentialScope::Full,
    )
}

/// **The projection resolves against the venue settings the daemon reads.** A bridge's `resolve`
/// reads them — IBKR's turns an unparseable stored backend into paper
/// (`crates/bridges/vike-ibkr/src/adapter/tests/demo.rs`'s `resolve_is_the_arms_own_gate`) — so a
/// projection over an EMPTY settings view would print that account armed while the daemon mounts
/// it paper. The stored row must reach the policy the projection hands `vike_tradehub_core::venue_arming`.
#[test]
fn the_projection_reads_the_venue_settings_the_daemon_reads() {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a fresh settings store");
    vike_secrets::set_venue_setting_in_unjournalled(
        dir.path(),
        "ibkr",
        Some(vike_model::AccountTier::Demo),
        "BACKEND",
        "cpap",
    )
    .expect("plant a stored backend the loader refuses");

    let policy = projection_policy(
        &full_store(Some(dir.path())),
        &vike_config::Policy::default(),
        Some(dir.path()),
    );
    let ibkr = policy.venue_settings.get("ibkr").expect("the ibkr rows reach the projection");
    assert_eq!(
        ibkr.get_exact(
            vike_secrets::venue_setting::SettingTier::Tier(AccountTier::Demo),
            "backend"
        ),
        Some("cpap")
    );
    // …and no settings directory is an empty view, as it is for the daemon.
    assert!(
        projection_policy(&full_store(None), &vike_config::Policy::default(), None)
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
/// whole boot — builds no `AdapterPolicy` of its own and hands the boot's settings directory to the
/// function [`the_projection_reads_the_venue_settings_the_daemon_reads`] tests.
#[test]
fn collect_takes_its_policy_from_projection_policy() {
    let src = include_str!("venues_cli.rs");
    let start = src.find("\nfn collect(").expect("venues_cli.rs defines `collect`");
    let rest = &src[start..];
    let body = &rest[..rest.find("\n}\n").expect("`collect`'s closing brace")];
    assert!(
        body.contains(
            "projection_policy(&booted.store, &booted.settings.policy, booted.settings_dir.as_deref())"
        ),
        "`collect` must resolve against `projection_policy` over the boot's settings dir:\n{body}"
    );
    assert!(
        !body.contains("AdapterPolicy {"),
        "`collect` builds a `AdapterPolicy` of its own again, beside `projection_policy`:\n{body}"
    );
}

/// **`vike-backend venues` says a live-tier key set exists when one does** — end to end, over the REAL
/// registry and the real projection, rendered by the real table. The account below is a `live`
/// oanda account holding its complete pair: the venue's arm mounts only its practice tier, so the
/// account does NOT RUN (a `live` account never trades the practice account in silence) and its mount
/// logs that at `error!` — and this screen used to print `NoCredentials` beside such a row. It now
/// prints the cause the log line is about, and the sentence under the table says which tier was found
/// and which way out there is. The credentials are FIELDS of the account row (decision 0156 item 6).
#[test]
fn a_live_tier_key_set_is_named_not_called_missing() {
    let planted = vike_tradehub_core::AdapterPolicy::default().with_account(
        "oanda",
        &AccountLabel::Default,
        AccountTier::Live,
    );
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(1, "API_KEY", "live-token");
    book.insert_account(1, "ACCOUNT_ID", "001-001-1-001");
    let policy = vike_tradehub_core::AdapterPolicy {
        accounts: planted.accounts.clone().with_credentials(book),
        ..planted
    };
    let rows: Vec<VenueArming> =
        vike_tradehub_core::venue_arming(crate::registry::REGISTRY, &HashMap::new(), &policy)
            .into_iter()
            .filter(|r| r.venue == "oanda")
            .collect();
    assert_eq!(rows.len(), 1, "the default account's row");
    let out = render_table(None, true, &rows, false);
    let line = out.lines().find(|l| l.starts_with("oanda")).expect("an oanda row");
    assert!(
        line.contains("live") && !line.contains("paper"),
        "its tier, and it does not run: {line}"
    );
    assert!(line.contains("LiveTierNotWired"), "the cause is the live tier: {line}");
    assert!(!line.contains("NoCredentials"), "…and not 'no credentials': {line}");
    let ids = &rows[0].account_ids;
    assert_eq!(ids.len(), 1, "the one planted `(oanda, live)` row states the tier: {ids:?}");
    assert!(
        out.contains(&format!("  oanda  (account row id {})", ids[0])),
        "the account and its row id lead the sentence: {out}"
    );
    assert!(
        out.contains("LIVE-tier") && out.contains("does NOT RUN"),
        "the sentence under the table: {out}"
    );
    // The same row through `--json`'s own two fields.
    let r = &rows[0];
    assert_eq!(format!("{:?}", r.block), "LiveTierNotWired");
    assert!(r.why().contains("oanda"), "{}", r.why());
    // …and no key or token value appears anywhere on the screen.
    for secret in ["live-token", "001-001-1-001"] {
        assert!(!out.contains(secret), "{secret} leaked onto the screen");
    }
}
