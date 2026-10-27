//! `vike-backend venues` — **which venues and accounts this box actually trades on, and why the
//! rest do not.**
//!
//! # The question it answers
//!
//! An operator gives an account the tier `live` (`vike-cli secrets account set-tier --id <N> --tier
//! live`, decision 0119: the account's own `tier` + `active` decide), restarts, and the account
//! does not run (decision 0156). The startup log says WHAT happened and nothing about WHY, and it is gone from the
//! terminal the moment anything else logs.
//!
//! The cause is never mysterious once you can see it: no credentials for that tier, an account row
//! that is inactive or absent, one of two ACTIVE non-paper tiers for one account
//! (which stops it until one is deactivated), a cargo feature this binary lacks, or the one
//! JForex sidecar already given to another dukascopy account.
//! [`vike_config::ArmingBlock`] has one variant per distinct cause precisely because each has a
//! different fix, and [`vike_config::VenueArming::why`] renders each as a sentence. This command is
//! where an operator reads them.
//!
//! # ⚠ Where the answer is computed (wave 2, W11; decision 0164)
//!
//! The `EFFECTIVE` column — the one that earns the screen — cannot be computed without the venue
//! bridges. Whether `binance` reaches `live` is a question about the account's `live` tier plus
//! that account's complete key set; a `live` aster account with an incomplete set does not run,
//! never testnet (a live account never trades demo); `dukascopy` gives its single Java sidecar to
//! one account and stops the other. That knowledge is the exchanges' rules, and the process that
//! holds the bridges and the credentials is the gatewayhub: this command ASKS it
//! (`ArmingQuery`, the table the live mount fetches) and renders the answer with the fold's own
//! `venue_arming`, so the screen and the mount are two readers of one snapshot. It used to compute
//! the projection in-process over this binary's own registry; that registry (and the credential
//! read behind it) is the gatewayhub's now.
//!
//! It lives in THIS binary and not in `vike-cli` for the reason it always did: `vike-cli` is kept
//! light and transport-free (`crates/vike-boot/tests/dependency_floor.rs`), and this binary already
//! holds the Trade key, the gatewayhub client and the fold's vocabulary for the daemon beside this
//! verb. It is `catalog`'s reason too.
//!
//! # What it is NOT
//!
//! It **changes nothing**. It mounts no venue, opens no venue socket, signs no order and can place
//! none — it asks one read-only question of the gatewayhub and prints the answer. It answers with the
//! tradehub daemon DOWN, but NOT with the gatewayhub down: that is an error naming the address, never
//! an empty table (an empty screen reads as "no account runs"). Arming an account is the account
//! verbs (`vike-cli secrets accounts` lists the rows and their ids; `vike-cli secrets account
//! activate|deactivate|set-tier --id <N>` changes one, effective at the next restart); this verb
//! only reports.
//!
//! ⚠ **It reads no credential at all** — not even presence: the gatewayhub does, and the answer
//! carries words (modes and cause names), never a value. Nothing here holds, formats or prints one.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use vike_config::VenueArming;
use vike_model::AccountTier;
use vike_util::cli::{CliExit, Program};

/// The program name every message of this verb carries: `vike-backend venues: <message>`, and the
/// `Usage:` line.
const BIN: &str = "vike-backend venues";

/// The command line, as `clap` reads it (decision 0155). The doc comments on the FIELDS are the
/// `--help` text, so notes sit in plain comments.
//
// The bare word `help` is also a help request (as it was before `clap`): it is the one positional,
// restricted to that word, so `venues help` and `venues --json help` print the page.
#[derive(clap::Parser, Debug)]
#[command(
    about = "Which venues and accounts this box would mount, and why the rest do not run.",
    long_about = None,
    after_help = "\
Reports what this box WOULD mount and why. Changes nothing, opens no socket, places no order,
and answers with the daemon down.",
    disable_help_subcommand = true
)]
struct VenuesArgs {
    /// Only this venue's rows (a `vike_model::VENUES` id, exactly).
    #[arg(long, value_name = "ID")]
    venue: Option<String>,
    /// Only rows that do not run at the TIER their account row states, or that carry any block --
    /// the "why does this account not run" view.
    #[arg(long)]
    blocked: bool,
    /// One JSON object per row on stdout, for a script.
    #[arg(long)]
    json: bool,
    /// The word `help`: print this page and exit 0.
    #[arg(value_name = "help", value_parser = ["help"])]
    word: Option<String>,
}

/// What the flags resolved to. A struct apart from [`VenuesArgs`] so [`read_opts`] can be a PURE
/// function a test can drive — `run` itself boots, so nothing in it is reachable from a unit test.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    venue: Option<String>,
    blocked_only: bool,
    json: bool,
    /// The word `help` was given: print the usage and stop, whatever else was on the line.
    help: bool,
}

/// The whole flag grammar, PURE. `-h`/`--help` and `-V`/`--version` come back as the [`CliExit`]
/// (code 0, stdout), like every refusal (code 2, stderr).
fn read_opts(args: &[String]) -> Result<Opts, CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let parsed: VenuesArgs = Program::new(BIN, &version).try_parse_from(args.iter().cloned())?;
    Ok(Opts {
        venue: parsed.venue,
        blocked_only: parsed.blocked,
        json: parsed.json,
        help: parsed.word.is_some(),
    })
}

/// The help page, as `--help` prints it.
fn help_text() -> String {
    Program::new(BIN, "").command::<VenuesArgs>().render_help().to_string()
}

/// Entry point — the multicall's `venues` tool.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    // `--help`, `--version` and every refusal are printed and the process exits (a refusal is 2).
    let Opts { venue, blocked_only, json, help } = match read_opts(args) {
        Ok(o) => o,
        Err(exit) => exit.emit(),
    };
    if help {
        println!("{}", help_text());
        return ExitCode::SUCCESS;
    }

    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-backend venues: {e}");
            return ExitCode::from(1);
        }
    };
    let rows = match collect(&booted) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("vike-backend venues: {e}");
            return ExitCode::from(1);
        }
    };
    let rows: Vec<VenueArming> = rows
        .into_iter()
        .filter(|r| venue.as_deref().is_none_or(|v| r.venue == v))
        .filter(|r| !blocked_only || is_blocked(r))
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
                // The gatewayhub reads its OWN credential store; this process reads none, and the
                // "NO STORE READ" line describes a store THIS process failed to find.
                true,
                &rows,
                blocked_only
            )
        );
    }
    ExitCode::SUCCESS
}

/// The `--blocked` predicate: the account does NOT RUN at the tier its row states
/// ([`VenueArming::runs`], decision 0156), OR the row carries any block at all (a `paper` account
/// on the simulator says `PaperTier`, an account whose orders are held says `OrdersHeld`).
fn is_blocked(r: &VenueArming) -> bool {
    !r.runs() || !r.block.is_clear()
}

/// The rows that get a SENTENCE under the table: an account whose row states `demo`/`live` and that
/// does not run at it, and a `TierConflict` (two ACTIVE non-paper tiers for one account), the one
/// table cause that is never the operator's deliberate choice and that stops a configured account.
/// The other table causes (`AccountInactive`, `NoAccountRow`) and a `paper` account are the ordinary
/// state of every venue a box does not trade, and a sentence for each would bury the ones that
/// matter; their rows still say the cause in the BLOCK column.
fn needs_a_sentence(r: &VenueArming) -> bool {
    (!r.runs() && r.tier.is_some_and(|t| t != AccountTier::Paper))
        || r.block == vike_config::ArmingBlock::TierConflict
}

/// A tier cell: the tier's own word, or `-` where there is none (no row states one; the account
/// does not run).
fn tier_cell(tier: Option<AccountTier>) -> &'static str {
    tier.map_or("-", AccountTier::as_str)
}

/// The `account.id`s the remedy's `--id` takes, as the lead of a row's sentence. A row that states
/// no id (a `TierConflict` names no single tier) points at the listing instead of inventing one.
fn ids_text(r: &VenueArming) -> String {
    match r.account_ids.as_slice() {
        [] => "account rows: `vike-cli secrets accounts`".to_string(),
        ids => {
            let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!("account row id {}", ids.join(", "))
        }
    }
}

/// The startup this verb needs and no more.
///
/// ⚠ `SettingsLoad::Load` — this verb's subject IS what the box would mount, so it resolves the
/// settings the way the daemon does (and it needs `config.gatewayhub_addr` from them).
/// `RemovedEnv::Ignore` for the catalog verb's reason: this command only PRINTS what the gatewayhub
/// computed, mounts nothing and can place no order, so a retired variable cannot have affected
/// anything it does, and refusing to start over one would stop an operator diagnosing the very box
/// that refuses.
///
/// ⚠ **`Credentials::DeferredNoVenueKeys`: this verb reads no venue credential at all.** The table it
/// prints is computed by the gatewayhub, which holds the credentials; this process holds only the
/// Trade key it asks with (read from the hub-key store through `crate::hub_server`'s one door).
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this command prints what the gatewayhub computed — it mounts no venue and can place \
             no order — so a retired variable (a removed risk ceiling or a venue setting that \
             now lives on a database row) cannot have affected anything it does, and refusing \
             to start over one would stop an operator diagnosing this box.",
        ),
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::DeferredNoVenueKeys(
            "the arming table is computed by the gatewayhub from its own account rows and \
             credentials; this command asks for it with the Trade key and reads no venue \
             credential.",
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

/// Every `(venue, account)` row this box would resolve, **as the gatewayhub computed it**: one
/// `ArmingQuery` (the same table the live mount fetches), turned back into the fold's own
/// [`vike_tradehub_core::ArmingTable`] and read through the fold's own `venue_arming`, so the screen
/// and the mount are two readers of one snapshot.
///
/// ⚠ **A gatewayhub that does not answer is an ERROR, never an empty table**: an empty screen
/// reads as "no account runs", which is the opposite of "I could not ask". One try, no wait: the
/// operator is at the terminal and can run it again, unlike a daemon starting before the gatewayhub.
fn collect(booted: &vike_boot::Booted) -> Result<Vec<VenueArming>, String> {
    let dial =
        crate::tradehub_cli::GatewayDial::from_config(&booted.settings.config, &booted.store);
    let table = vike_gatewayhub_client::trade::rpc::arming_table(&dial.link).map_err(|e| {
        format!(
            "the gatewayhub at {} did not answer ({e}). This table is computed THERE, from the \
             account rows and the credentials it holds; start the gatewayhub (`vike-backend \
             gatewayhub`, or its unit) and run this again",
            dial.link.addr()
        )
    })?;
    Ok(rows_in_order(&crate::remote_backend::arming_table_of_wire(&table)))
}

/// The rows of `table`, sorted venue then label — §7.3's order, the one that makes a venue's accounts
/// adjacent. The startup preflight's stop is a fact of one start, so none is laid over them.
fn rows_in_order(table: &vike_tradehub_core::ArmingTable) -> Vec<VenueArming> {
    let mut rows =
        vike_tradehub_core::venue_arming(table, &vike_tradehub_core::AdapterPolicy::default());
    rows.sort_by(|a, b| a.venue.cmp(b.venue).then_with(|| a.label.cmp(&b.label)));
    rows
}

/// Every account row of every roster venue, through the gatewayhub's own lane (a dev dependency) — the SAME
/// projection the screen printed before the venues moved behind the gatewayhub, kept for the parity golden (`venues_cli_parity_tests.rs`), which records the printed
/// screen over it and holds [`render_table`] byte-identical across the move.
#[cfg(test)]
fn arming_rows(
    vars: &HashMap<String, String>,
    policy: &vike_tradehub_core::AdapterPolicy,
) -> Vec<VenueArming> {
    // The REAL venue half now lives in the gatewayhub: the rows come from its lane (`BridgeTrade`, a dev
    // dependency of this crate) through the wire's mirror and the remote backend's own conversion, so the
    // printed-screen golden still holds the renderer byte-identical over real arming.
    let lane = vike_gatewayhub::trade::adapters::bridge_trade::BridgeTrade::new(
        vike_gatewayhub::registry::trade::REGISTRY,
        vars.clone(),
        &policy.accounts,
        &policy.venue_settings,
    );
    let table = crate::remote_backend::wire::table_of_wire(&lane.arming_table_quiet());
    let mut rows = vike_tradehub_core::venue_arming(&table, policy);
    rows.sort_by(|a, b| a.venue.cmp(b.venue).then_with(|| a.label.cmp(&b.label)));
    rows
}

/// `-` for the unlabelled account, the label otherwise. ⚠ Never the word `default`: the unlabelled
/// account has no label, and printing one invents a name the operator cannot write anywhere.
fn account_cell(row: &VenueArming) -> String {
    row.label.text().map_or_else(|| "-".to_string(), str::to_string)
}

/// The `(venue, label)` pairs that actually get an ENGINE, answered by
/// [`vike_tradehub_core::accounts_to_run`] — the mount's own function, called rather than restated.
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
        for label in vike_tradehub_core::accounts_to_run(&of_venue) {
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
    // Every `writeln!(s, ..).ok()` below discards a `fmt::Result` that is infallible into a `String`.
    let mut s = String::new();
    let engines = engines_of(rows);
    match settings_dir {
        Some(d) => writeln!(s, "settings: {}", d.display()).ok(),
        None => writeln!(s, "settings: NONE FOUND — no account runs, whatever a file says").ok(),
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
                true => {
                    "nothing is blocked: every account reaches the tier its account row states."
                }
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
        "TIER",
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
            tier_cell(r.tier),
            tier_cell(r.effective),
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
    // an account's own tier was actually refused (`needs_a_sentence`).
    //
    // ⚠ **The ACCOUNT leads and the sentence follows, which is the opposite of the first draft.**
    // `VenueArming::why` names the VENUE, not the account, so two accounts of one venue blocked for
    // one reason render the SAME sentence twice — measured on a two-account binance, where it read
    // as a rendering bug. The route key is unique per row (`binance` against `binance#ALT`), and
    // the `account.id`s beside it are what the account verbs' `--id` takes, so leading with both
    // disambiguates and puts the action first.
    let capped: Vec<&VenueArming> = rows.iter().filter(|r| needs_a_sentence(r)).collect();
    if !capped.is_empty() {
        writeln!(s, "\n-- why these do not run at the tier their account row states ---------")
            .ok();
        for r in &capped {
            writeln!(s, "  {}  ({})", r.route_key(), ids_text(r)).ok();
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
                tally(of_venue.iter().map(|r| r.effective.filter(|_| r.runs())))
            )
            .ok();
        }
    }
    writeln!(
        s,
        "{} row(s) over {} venue(s) — {}",
        rows.len(),
        dedup_venues(rows).len(),
        tally(rows.iter().map(|r| r.effective.filter(|_| r.runs())))
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

/// `0 live, 1 demo, 1 paper, 12 not running` — always all four, in descending order of authority,
/// so the shape of the line never changes and `live=0` is stated rather than left to be inferred
/// from an absence. `None` is an account that does not run (decision 0156): it is counted as such,
/// never as paper.
fn tally(tiers: impl Iterator<Item = Option<AccountTier>>) -> String {
    let (mut live, mut demo, mut paper, mut off) = (0usize, 0usize, 0usize, 0usize);
    for t in tiers {
        match t {
            Some(AccountTier::Live) => live += 1,
            Some(AccountTier::Demo) => demo += 1,
            Some(AccountTier::Paper) => paper += 1,
            None => off += 1,
        }
    }
    format!("{live} live, {demo} demo, {paper} paper, {off} not running")
}

/// One JSON object per line (JSONL), so a script can `jq` a stream without the whole table being
/// one document it must hold in memory — and so a row added later cannot shift anything before it.
fn print_json(rows: &[VenueArming]) {
    let engines = engines_of(rows);
    for r in rows {
        let v = serde_json::json!({
            "venue": r.venue,
            // `null`, never "default": the unlabelled account HAS no label (its `account` rows carry
            // `label = NULL`), and a string here would be a name the operator cannot write anywhere.
            "account": r.label.text(),
            "route_key": vike_model::accounts::account_keys::route_key_of(r.venue, &r.label),
            "tier": r.tier.map(AccountTier::as_str),
            "effective": r.effective.map(AccountTier::as_str),
            "engine": engines.contains(&(r.venue, r.label.to_string())),
            "block": format!("{:?}", r.block),
            "why": r.why(),
            "account_ids": r.account_ids,
        });
        println!("{v}");
    }
}

#[path = "venues_cli_tests.rs"]
#[cfg(test)]
mod venues_cli_tests;

#[path = "venues_cli_parity_tests.rs"]
#[cfg(test)]
mod venues_cli_parity_tests;
