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
//! The cause is never mysterious once you can see it: no credentials for that tier, a ceiling
//! (`policy.venues.<venue>`) capped below what the operator believes it is, a cargo feature this
//! binary lacks, an account with no line of its own, or the one JForex sidecar already given to
//! another dukascopy account.
//! [`vike_config::ArmingBlock`] has one variant per distinct cause precisely because each has a
//! different fix, and [`vike_config::VenueArming::why`] renders each as a sentence. This command is
//! where an operator reads them.
//!
//! # ⚠ Why it lives in THIS binary and not in `vike-cli`
//!
//! The `EFFECTIVE` column — the one that earns the screen — cannot be computed without the venue
//! bridges. Whether `binance` reaches `live` is a question about the ceiling
//! `policy.venues.binance` plus a specific (LIVE-tier) key set; `aster`'s ceiling caps it like any
//! venue but does not CHOOSE its network — it picks its tier by WHICH keys exist; `dukascopy` gives
//! its single Java sidecar to one account and drops the other to paper. `vike_mount::venue_account_arming`
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
//! answers with the daemon DOWN. Arming a venue is `vike-cli config set`, or the Data Manager's
//! one-click switch (no typed confirm in either direction since `docs/decisions/0086` point 7);
//! this verb only reports.
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
    if let Some(notice) = stranded_notice(&booted) {
        eprintln!("vike-backend venues: {notice}");
    }
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
/// order, so a retired variable — a removed risk ceiling or one of the venue settings decision 0095
/// moved onto database rows — cannot have affected anything it does, and refusing to start over
/// one would stop an operator diagnosing the very box that refuses.
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
             no order — so a retired variable (a removed risk ceiling or a venue setting that \
             now lives on a database row) cannot have affected anything it does, and refusing \
             to start over one would stop an operator diagnosing this box.",
        ),
        settings: vike_boot::SettingsLoad::Load,
        // this verb's whole subject is what the ceilings mean.
        ceilings: vike_boot::Ceilings::Interpret { now_ms: vike_model::now_ms() },
        // ⚠ REPORTING, not refusing, for a venue setting still filed as a credential row (decision
        // 0095's Task 7) — see [`stranded_notice`]. The row-arming refusal on the same load is kept.
        credentials: vike_boot::Credentials::LoadReportingStrandedSettings(
            &load,
            "this verb mounts no venue and reads none of the rows the daemons refuse over; its \
             subject is WHY a venue is or is not armed, so stopping on the very store that makes \
             the daemon refuse would take the diagnosis away — it prints the finding on stderr \
             instead (`stranded_notice`)",
        ),
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

/// **What a verb that REPORTS a stranded store says, once, on stderr** — `None` for a clean one.
///
/// Decision 0095's Task 7 made a credential row under a venue setting's old name a startup
/// REFUSAL for every root that mounts a venue, because nothing reads it now. This binary's one-shot
/// verbs ([`run`] and `crate::catalog_cli`) mount nothing and read none of those rows, so refusing
/// over them stops an operator diagnosing the very box that refuses — the stance their boots
/// already take toward a retired risk variable — and they boot with
/// `vike_boot::Credentials::LoadReportingStrandedSettings`, the arm the desktop uses. That arm
/// still refuses a row that ARMS real money. This is the finding it would otherwise have raised,
/// for the operator to read: `Booted::stranded_venue_settings`, the SAME
/// `vike_config::stranded_venue_settings_report` the daemons' refusal is built from — computed by
/// `vike_boot::boot` itself, because `crates/vike-boot/tests/one_owner.rs` refuses a root that
/// calls a step of the sequence for itself — so it names the rows the daemons name.
///
/// Names and the settings they stand for, never a value. Stderr, so `--json` and a table on stdout
/// are byte-identical to before for a clean store, and a pipe reads only what it asked for.
pub(crate) fn stranded_notice(booted: &vike_boot::Booted) -> Option<String> {
    let report = booted.stranded_venue_settings.as_deref()?;
    Some(format!(
        "⚠ the trading daemon will REFUSE to start on this store (the data daemon does too, for \
         the unlabelled names), and this command reads none of these rows: {report}"
    ))
}

/// The credential map the arming projection reads, with the resolved flags folded in EXACTLY as the
/// daemon folds them (`crate::tradehub_cli`'s `fold_flags_into_vars`). Decision 0095: Polymarket's
/// gates are `flags.poly_exec` / `flags.poly_reconcile`, read by the arm from this map, so a
/// projection that skipped the fold would report `ExecFlagUnset` for a box whose daemon arms
/// Polymarket.
fn arming_vars(
    credentials: Option<&HashMap<String, String>>,
    flags: vike_config::Flags,
) -> HashMap<String, String> {
    let mut vars = credentials.cloned().unwrap_or_default();
    crate::tradehub_cli::fold_flags_into_vars(flags, &mut vars);
    vars
}

/// Every `(venue, account)` row this box would resolve, through the SAME projection the mount
/// selects accounts with.
///
/// ⚠ **`vike_mount::venue_arming`, never a local re-derivation** — over this daemon's own
/// `REGISTRY`, the one the live mount passes. The GUI's Venues tab already reads this exact
/// function, and the reason its own doc gives applies verbatim here: a column computed from the
/// ceiling alone would generate the complaint this screen exists to answer — *"I set live and it
/// is still paper"* — on its very first use, because the ceiling is a `min` and can only ever
/// refuse an arming, never create one.
///
/// ⚠ The `account` TABLE is read here for the reason the daemon's own composition root states: the
/// projection must describe exactly what the mount will do, and dukascopy resolves WHICH LEGAL
/// ENTITY an order reaches out of those rows. One snapshot, on the policy object the projection
/// already takes.
fn collect(env: &HashMap<String, String>, booted: &vike_boot::Booted) -> Vec<VenueArming> {
    let policy = projection_policy(env, &booted.settings.policy, booted.settings_dir.as_deref());
    // ⚠ **An ABSENT store and an EMPTY one are different facts and must not collapse here.** No
    // store is the ordinary unconfigured state — every venue stays paper, correctly — while a store
    // that exists and could not be opened is a permissions bug wearing the same answer. The map is
    // what the projection needs either way; WHICH of the two happened is said by the caller, on its
    // own line, so a reader never has to infer it from a table of `NoCredentials`.
    let vars = arming_vars(booted.credentials.as_ref(), booted.settings.flags);
    let mut rows = vike_mount::venue_arming(crate::registry::REGISTRY, &vars, &policy);
    // Venue, then label — §7.3's order, and the one that makes a venue's accounts adjacent.
    rows.sort_by(|a, b| a.venue.cmp(b.venue).then_with(|| a.label.cmp(&b.label)));
    rows
}

/// The [`vike_mount::MountPolicy`] the projection resolves against: the ceilings, the `account`
/// table, and the `venue_setting` rows — each read the way the live mount reads it.
///
/// ⚠ **The venue settings are the daemon's own read** (`crate::tradehub_cli`'s
/// `load_venue_settings_for`), because a bridge's `resolve` reads them: since decision 0095's
/// Task 7 IBKR's `resolve` reads the tier's gateway rows, and an unparseable backend or port makes
/// that mount paper. This field was left EMPTY here until then (a declared residual, harmless while
/// no `resolve` read a setting), so the screen could print an IBKR account armed that the daemon
/// mounts paper.
fn projection_policy(
    env: &HashMap<String, String>,
    policy: &vike_config::Policy,
    settings_dir: Option<&Path>,
) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        accounts: vike_bridge_core::account_directory::AccountDirectory::read(
            vike_bridge_core::credentials::load_workspace_accounts_from_env(env),
            vike_bridge_core::credentials::load_workspace_account_keys_from_env(env),
        ),
        venue_settings: crate::tradehub_cli::load_venue_settings_for(settings_dir),
        ..vike_mount::MountPolicy::from(policy)
    }
}

/// `-` for the unlabelled account, the label otherwise. ⚠ Never the word `default`: the unlabelled
/// account has no label, and printing one invents a name the operator cannot write anywhere.
fn account_cell(row: &VenueArming) -> String {
    row.label.text().map_or_else(|| "-".to_string(), str::to_string)
}

/// The `(venue, label)` pairs that actually get an ENGINE, answered by
/// [`vike_mount::accounts_to_mount`] — the mount's own function, called rather than restated.
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
        for label in vike_mount::accounts_to_mount(&of_venue) {
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
            // be a name the operator cannot write into a `policy.accounts.<venue>.<LABEL>` row.
            "account": r.label.text(),
            "route_key": vike_model::accounts::account_keys::route_key_of(r.venue, &r.label),
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

#[path = "venues_cli_tests.rs"]
#[cfg(test)]
mod venues_cli_tests;
