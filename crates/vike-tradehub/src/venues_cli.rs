//! `vike-backend venues` — **which venues and accounts this box actually trades on, and why the
//! rest do not.**
//!
//! # The question it answers
//!
//! An operator writes `live` into `policy.venues.<venue>` (or `policy.accounts.<venue>.<LABEL>`),
//! restarts, and the venue is still on paper. Until now the only answer was one line in the startup
//! log — `policy.venues = live=none demo=[bybit] paper=<13>` — which says WHAT happened and nothing
//! about WHY, and which is gone from the terminal the moment anything else logs.
//!
//! The cause is never mysterious once you can see it: no credentials for that tier, a
//! `{VENUE}_MAINNET` switch nobody set, a cargo feature this binary lacks, an account with no line
//! of its own, or the one JForex sidecar already given to another dukascopy account.
//! [`vike_config::ArmingBlock`] has one variant per distinct cause precisely because each has a
//! different fix, and [`vike_config::VenueArming::why`] renders each as a sentence. This command is
//! where an operator reads them.
//!
//! # ⚠ Why it lives in THIS binary and not in `vike-cli`
//!
//! The `EFFECTIVE` column — the one that earns the screen — cannot be computed without the venue
//! bridges. Whether `binance` reaches `live` is a question about `BINANCE_MAINNET` plus a specific
//! key set; `aster` has no flag at all and picks its tier by WHICH keys exist; `dukascopy` gives its
//! single Java sidecar to one account and drops the other to paper. `vike_mount::venue_account_arming`
//! reaches THIRTEEN venue crates to answer, and it must, because that knowledge is the exchanges'
//! rules.
//!
//! `vike-cli` deliberately does not link those crates — it is kept light, and
//! `scripts/ci_feature_suite.sh`'s `light-consumers` lane exists to keep it that way. This binary
//! already links every one of them for the daemon beside this verb, so the screen costs no new
//! dependency anywhere. That is the whole reason for the split, and it is the same reason
//! `catalog` sits here rather than in the CLI.
//!
//! # What it is NOT
//!
//! It **changes nothing**. It mounts no venue, opens no socket, signs nothing and can place no
//! order — it is a projection over the settings and the credential store, which is also why it
//! answers with the daemon DOWN. Arming a venue is `vike-cli config set` (and, for a loosening, the
//! typed-confirm ceremony); this verb only reports.
//!
//! ⚠ **It reads credential PRESENCE, never a value.** `venue_account_arming` asks each bridge's own
//! loader whether a key set resolves and drops the credentials on the floor; nothing here holds,
//! formats or prints one. The columns are modes and cause names.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use vike_config::{VenueArming, VenueMode};

/// The operator-facing help. Spelled once; `run` prints it for `-h`, `--help`, `help` and no args.
const USAGE: &str = "\
usage: vike-backend venues [--venue <id>] [--blocked] [--json]

  --venue <id>  only this venue's rows (a `vike_model::VENUES` id, exactly)
  --blocked     only rows whose EFFECTIVE tier is below the CEILING somebody asked for —
                the \"why is this still on paper\" view
  --json        one JSON object per row on stdout, for a script
  -h, --help    print this and exit 0

Reports what this box WOULD mount and why. Changes nothing, opens no socket, places no order,
and answers with the daemon down.";

/// What the flags resolved to. A struct rather than three locals so [`parse_args`] can be a PURE
/// function a test can drive — `run` itself boots, so nothing in it is reachable from a unit test.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    venue: Option<String>,
    blocked_only: bool,
    json: bool,
    /// `-h`/`--help`/`help` was given: print the usage and stop, whatever else was on the line.
    help: bool,
}

/// The whole flag grammar, PURE. `Err` is the operator-facing reason.
fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0usize;
    while i < args.len() {
        // Both spellings, the pair every other verb in this tree accepts.
        let (flag, inline) = match args[i].split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (args[i].as_str(), None),
        };
        match flag {
            "-h" | "--help" | "help" => {
                o.help = true;
                return Ok(o);
            }
            "--blocked" => {
                o.blocked_only = true;
                i += 1;
            }
            "--json" => {
                o.json = true;
                i += 1;
            }
            "--venue" => match inline.or_else(|| args.get(i + 1).cloned()) {
                // A value that is itself a flag means the operator's value went missing and the
                // NEXT flag was eaten as one — the failure `submit --coid has space` taught.
                Some(v) if !v.is_empty() && !v.starts_with("--") => {
                    o.venue = Some(v);
                    i += if args[i].contains('=') { 1 } else { 2 };
                }
                _ => return Err("`--venue` needs a venue id".to_string()),
            },
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(o)
}

/// Entry point — the multicall's `venues` tool.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    let Opts { venue, blocked_only, json, help } = match parse_args(args) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    if help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-backend venues: {e}");
            return ExitCode::from(1);
        }
    };
    let rows = collect(env, &booted);
    let rows: Vec<VenueArming> = rows
        .into_iter()
        .filter(|r| venue.as_deref().is_none_or(|v| r.venue == v))
        .filter(|r| !blocked_only || r.effective < r.ceiling)
        .collect();

    // A `--venue` naming nothing is an ERROR, not an empty table: an empty table reads as "this
    // venue is fine", which is the opposite of what a typo means.
    if rows.is_empty()
        && let Some(v) = &venue
        && !vike_model::VENUES.contains(&v.as_str())
    {
        eprintln!(
            "vike-backend venues: '{v}' is not a venue this build knows. The roster is: {}",
            vike_model::VENUES.join(", ")
        );
        return ExitCode::from(2);
    }

    if json {
        print_json(&rows);
    } else {
        print!(
            "{}",
            render_table(
                booted.settings_dir.as_deref(),
                booted.credentials.is_some(),
                &rows,
                blocked_only
            )
        );
    }
    ExitCode::SUCCESS
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("vike-backend venues: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}

/// The startup this verb needs and no more.
///
/// ⚠ `SettingsLoad::Load` — unlike [`crate::catalog_cli`]'s sibling boot, this verb's whole subject
/// IS the resolved policy, so it must load settings exactly the way the daemon does or it would be
/// reporting on a different tree than the one that mounts. `RemovedEnv::Ignore` for the catalog
/// verb's reason: this command resolves a ceiling only to PRINT it, mounts nothing and can place no
/// order, so a stale risk variable cannot have affected anything it does — and refusing to start
/// over one would stop an operator diagnosing the very box that refuses.
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    let load = || vike_bridge_core::credentials::load_workspace_secrets_from_env(env);
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this command resolves a ceiling only to PRINT it — it mounts no venue and can place \
             no order — so a stale risk variable cannot have affected anything it does, and \
             refusing to start over one would stop an operator diagnosing this box.",
        ),
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::LoadWith(&load),
        log_home: vike_boot::LogHome::Elsewhere(
            "a one-shot command builds no subscriber: it prints its result and exits, so there is \
             no rolling file to place.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "the whole output IS the disclosure, in a form built to be read rather than a banner \
             scrolled past.",
        ),
    })
}

/// Every `(venue, account)` row this box would resolve, through the SAME projection the mount
/// selects accounts with.
///
/// ⚠ **`vike_run::venue_arming`, never a local re-derivation.** The GUI's Venues tab already reads
/// this exact function, and the reason its own doc gives applies verbatim here: a column computed
/// from the ceiling alone would generate the complaint this screen exists to answer — *"I set live
/// and it is still paper"* — on its very first use, because the ceiling is a `min` and can only
/// ever refuse an arming, never create one.
///
/// ⚠ The `account` TABLE is read here for the reason the daemon's own composition root states: the
/// projection must describe exactly what the mount will do, and dukascopy resolves WHICH LEGAL
/// ENTITY an order reaches out of those rows. One snapshot, on the policy object the projection
/// already takes.
fn collect(env: &HashMap<String, String>, booted: &vike_boot::Booted) -> Vec<VenueArming> {
    let policy = vike_run::MountPolicy {
        accounts: vike_run::AccountDirectory::read(
            vike_bridge_core::credentials::load_workspace_accounts_from_env(env),
            vike_bridge_core::credentials::load_workspace_account_keys_from_env(env),
        ),
        ..vike_run::MountPolicy::from(&booted.settings.policy)
    };
    // ⚠ **An ABSENT store and an EMPTY one are different facts and must not collapse here.** No
    // store is the ordinary unconfigured state — every venue stays paper, correctly — while a store
    // that exists and could not be opened is a permissions bug wearing the same answer. The map is
    // what the projection needs either way; WHICH of the two happened is said by the caller, on its
    // own line, so a reader never has to infer it from a table of `NoCredentials`.
    let no_creds = HashMap::new();
    let mut rows =
        vike_run::venue_arming(booted.credentials.as_ref().unwrap_or(&no_creds), &policy);
    // Venue, then label — §7.3's order, and the one that makes a venue's accounts adjacent.
    rows.sort_by(|a, b| a.venue.cmp(b.venue).then_with(|| a.label.cmp(&b.label)));
    rows
}

/// `-` for the unlabelled account, the label otherwise. ⚠ Never the word `default`: the unlabelled
/// account has no label, and printing one invents a name the operator cannot write anywhere.
fn account_cell(row: &VenueArming) -> String {
    row.label.text().map_or_else(|| "-".to_string(), str::to_string)
}

/// The `(venue, label)` pairs that actually get an ENGINE, answered by
/// [`vike_run::accounts_to_mount`] — the mount's own function, called rather than restated.
///
/// ⚠ **Not "effective is not paper".** The venue's DEFAULT account is mounted even on paper, and a
/// screen that used the obvious predicate would print `ENGINE no` beside every paper venue on a box
/// where those engines exist. The rule is fed a venue's WHOLE row set because that is the shape it
/// decides over, so this walks venue by venue rather than row by row.
fn engines_of(rows: &[VenueArming]) -> std::collections::HashSet<(&'static str, String)> {
    let mut out = std::collections::HashSet::new();
    for venue in dedup_venues(rows) {
        let of_venue: Vec<VenueArming> =
            rows.iter().filter(|r| r.venue == venue).cloned().collect();
        for label in vike_run::accounts_to_mount(&of_venue) {
            out.insert((venue, label.to_string()));
        }
    }
    out
}

/// The whole screen as TEXT.
///
/// ⚠ Returns a `String` rather than printing, so the renderer is reachable from a unit test.
/// [`run`] boots — it resolves a project, opens a credential store and loads settings — so nothing
/// inside it can be driven from one, and a screen whose job is to be READ is exactly the kind that
/// should not be verified by looking at it once.
fn render_table(
    settings_dir: Option<&Path>,
    creds_read: bool,
    rows: &[VenueArming],
    blocked_only: bool,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let engines = engines_of(rows);
    match settings_dir {
        Some(d) => writeln!(s, "settings: {}", d.display()).ok(),
        None => {
            writeln!(s, "settings: NONE FOUND — every venue stays paper, whatever a file says").ok()
        }
    };
    // The credential store gets its own line for the reason the root `CLAUDE.md` insists on: an
    // ABSENT store is the ordinary unconfigured state, and a table full of `NoCredentials` with no
    // line above it reads as fourteen separate venue problems rather than one missing file.
    if !creds_read {
        writeln!(
            s,
            "credentials: NO STORE READ — every row below reads `NoCredentials` for that one \
             reason, not fourteen. `vike-cli secrets path` prints where one would go."
        )
        .ok();
    }
    s.push('\n');

    if rows.is_empty() {
        writeln!(
            s,
            "{}",
            match blocked_only {
                true => "nothing is blocked: every account reaches the tier its ceiling asks for.",
                false => "no venue rows — this build knows no venues, which should be impossible.",
            }
        )
        .ok();
        return s;
    }

    let w_venue = rows.iter().map(|r| r.venue.len()).max().unwrap_or(5).max(5);
    let w_acct = rows.iter().map(|r| account_cell(r).len()).max().unwrap_or(7).max(7);
    writeln!(
        s,
        "{:<w_venue$}  {:<w_acct$}  {:<7}  {:<9}  {:<6}  BLOCK",
        "VENUE",
        "ACCOUNT",
        "CEILING",
        "EFFECTIVE",
        "ENGINE",
        w_venue = w_venue,
        w_acct = w_acct
    )
    .ok();
    for r in rows {
        writeln!(
            s,
            "{:<w_venue$}  {:<w_acct$}  {:<7}  {:<9}  {:<6}  {:?}",
            r.venue,
            account_cell(r),
            r.ceiling.as_str(),
            r.effective.as_str(),
            if engines.contains(&(r.venue, r.label.to_string())) { "yes" } else { "no" },
            r.block,
            w_venue = w_venue,
            w_acct = w_acct
        )
        .ok();
    }

    // ⚠ The SENTENCES, and only for the rows that lost something. `why()` is a paragraph per row —
    // printing one for all fifty would bury the two that matter, and printing none would make the
    // BLOCK column a name the operator has to go look up. So: the table always, the sentence where
    // a ceiling was actually refused.
    //
    // ⚠ **The KEY leads and the sentence follows, which is the opposite of the first draft.**
    // `VenueArming::why` names the VENUE, not the account, so two accounts of one venue blocked for
    // one reason render the SAME sentence twice — measured on a two-account binance, where it read
    // as a rendering bug. The key is unique per row (`policy.venues.binance` against
    // `policy.accounts.binance.ALT`) AND it is the line the operator edits, so leading with it both
    // disambiguates and puts the action first.
    let capped: Vec<&VenueArming> = rows.iter().filter(|r| r.effective < r.ceiling).collect();
    if !capped.is_empty() {
        writeln!(s, "\n-- why these are below the tier somebody asked for ---------------------")
            .ok();
        for r in &capped {
            writeln!(s, "  {}", r.key()).ok();
            writeln!(s, "     {}", r.why()).ok();
        }
    }

    // The tally, and a per-venue line wherever a venue carries more than one account — §7.3's
    // navigability rule, which only bites at the scale this design is for.
    s.push('\n');
    for venue in dedup_venues(rows) {
        let of_venue: Vec<&VenueArming> = rows.iter().filter(|r| r.venue == venue).collect();
        if of_venue.len() > 1 {
            writeln!(
                s,
                "{venue}: {} accounts — {}",
                of_venue.len(),
                tally(of_venue.iter().map(|r| r.effective))
            )
            .ok();
        }
    }
    writeln!(
        s,
        "{} row(s) over {} venue(s) — {}",
        rows.len(),
        dedup_venues(rows).len(),
        tally(rows.iter().map(|r| r.effective))
    )
    .ok();
    s
}

/// The venues present in `rows`, in the order they appear (the rows are already venue-sorted).
fn dedup_venues(rows: &[VenueArming]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for r in rows {
        if out.last() != Some(&r.venue) {
            out.push(r.venue);
        }
    }
    out
}

/// `0 live, 1 demo, 13 paper` — always all three, in descending order of authority, so the shape of
/// the line never changes and `live=0` is stated rather than left to be inferred from an absence.
fn tally(modes: impl Iterator<Item = VenueMode>) -> String {
    let (mut live, mut demo, mut paper) = (0usize, 0usize, 0usize);
    for m in modes {
        match m {
            VenueMode::Live => live += 1,
            VenueMode::Demo => demo += 1,
            VenueMode::Paper => paper += 1,
        }
    }
    format!("{live} live, {demo} demo, {paper} paper")
}

/// One JSON object per line (JSONL), so a script can `jq` a stream without the whole table being
/// one document it must hold in memory — and so a row added later cannot shift anything before it.
fn print_json(rows: &[VenueArming]) {
    let engines = engines_of(rows);
    for r in rows {
        let v = serde_json::json!({
            "venue": r.venue,
            // `null`, never "default": the unlabelled account HAS no label, and a string here would
            // be a name the operator cannot write into policy.toml.
            "account": r.label.text(),
            "route_key": vike_model::account_keys::route_key_of(r.venue, &r.label),
            "ceiling": r.ceiling.as_str(),
            "effective": r.effective.as_str(),
            "engine": engines.contains(&(r.venue, r.label.to_string())),
            "block": format!("{:?}", r.block),
            "why": r.why(),
            "key": r.key(),
        });
        println!("{v}");
    }
}

#[cfg(test)]
mod tests {
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
            row(
                "binance",
                Some("ALT"),
                VenueMode::Live,
                VenueMode::Paper,
                ArmingBlock::NoCredentials,
            ),
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
            row(
                "binance",
                Some("ALT"),
                VenueMode::Live,
                VenueMode::Paper,
                ArmingBlock::NoCredentials,
            ),
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
        assert_eq!(
            tally([VenueMode::Paper, VenueMode::Paper].into_iter()),
            "0 live, 0 demo, 2 paper"
        );
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
        assert!(
            !render_table(None, true, &one, false).contains("okx: "),
            "one account, no summary"
        );
        let two = [
            row("binance", None, VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
            row("binance", Some("ALT"), VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
        ];
        assert!(render_table(None, true, &two, false).contains("binance: 2 accounts"));
    }
}
