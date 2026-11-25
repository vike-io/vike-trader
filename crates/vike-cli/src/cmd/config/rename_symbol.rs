//! `vike-cli config rename-symbol (--id <ID> | --mount <PROFILE>:<ORD>) --from OLD --to NEW [--dry-run]
//! [--note WORD]` — **ONE row's symbol, renamed from exactly OLD to NEW**: decision 0290's rename of the rows,
//! written by a release's row steps (`deploy/row-steps`, `docs/ops/release.md`'s "The release's row steps").
//!
//! # Why a verb of its own, and why NEW is held to its shape alone
//!
//! The row steps run on each pre-flight probe's COPY and then on the box, with no hub running and BEFORE the
//! post-deploy `vike-cli data catalog refresh`, so the kept lists they find predate the user names.
//! `account set-symbol` reads a name off that list and would refuse every new one, and no verb renamed a
//! stored mount. The hubs resolve a stored name against the refreshed list at their start: they refuse it in words
//! while the list is missing and heal by themselves once it is written
//! (`crates/vike-tradehub/src/server/tests/kept_list_door.rs`). So NEW passes the account column's floor
//! (`vike_secrets::normalized_account_symbol`) and the door's SHAPE (`vike_catalog::door_symbol` of the row's venue
//! spells it unchanged), and no kept list is read.
//!
//! # The rules
//!
//! - **The row must hold OLD exactly.** A row already holding NEW answers `unchanged` and exits 0, so a re-run, and
//!   the undo of a step that wrote nothing, are harmless; any other symbol is refused naming what the row holds,
//!   and nothing is written.
//! - **An account** (`--id ID`) is written through `vike_secrets::edit_account_in` (`AccountEdit::SetSymbol`),
//!   which records it in the change journal, as `account set-symbol` does.
//! - **A mount** (`--mount PROFILE:ORD`, the stored DAEMON profile's mount of that ordinal) is written by storing the
//!   profile's body again with that one symbol changed (`vike_secrets::profile_store::update_profile`, which keeps the
//!   `active` bit and renames on the body read under the write's own lock, so a concurrent edit is kept), and recorded as `config profile rm` records a profile write (`crate::cmd::settings_write`).
//!   A running daemon keeps the mount it started on: the rename binds at its next start.
//! - **`--note WORD`** is printed as one loud line, each `_` shown as a blank: a row step's value holds no blank,
//!   and a step may owe the operator a sentence (IG's `EURUSD` is another lot than the epic it replaces).
//! - **`--dry-run`** says what would change and writes nothing.
//! - Not on the MCP surface, as no `config` writer is.

use std::path::Path;
use std::process::ExitCode;

use vike_change_journal::{Change, ChangeActor, ChangeOutcome, ChangeProcess};
use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, read_profiles, update_profile,
};

use crate::cmd::args;
use crate::cmd::settings_write;
use crate::exit::{CliError, CommandResult};

/// The program name this verb is read and reported under.
const BIN: &str = "vike-cli config rename-symbol";

/// `config rename-symbol`'s arguments, as `clap` reads them.
#[derive(clap::Args, Debug)]
#[command(
    about = "Rename ONE account's or ONE stored mount's symbol from exactly OLD to NEW",
    long_about = "Rename ONE account's symbol (--id ID) or ONE stored daemon profile's mount symbol \
                  (--mount PROFILE:ORD)\nfrom exactly --from OLD to --to NEW: decision 0290's rename, run \
                  by a release's row steps. A row\nalready holding NEW is left unchanged (exit 0); a row \
                  holding anything else is refused and\nnothing is written. NEW is held to its shape on \
                  the row's venue, never to a kept list (the\ndeploy writes before the lists are \
                  refreshed). --note prints one loud line, each `_` a blank."
)]
pub(crate) struct RenameSymbolCommand {
    #[arg(long, value_name = "ID", conflicts_with = "mount", help = "the account row, by its id")]
    id: Option<i64>,
    #[arg(
        long,
        value_name = "PROFILE:ORD",
        help = "the stored daemon profile's mount of that ordinal (`tradehub:0`)"
    )]
    mount: Option<String>,
    #[arg(long, value_name = "OLD", help = "the symbol the row must hold now")]
    from: String,
    #[arg(long, value_name = "NEW", help = "the symbol it holds after")]
    to: String,
    #[arg(long, help = "say what would change and write nothing")]
    dry_run: bool,
    #[arg(long, value_name = "WORD", help = "a line to print loudly, each `_` shown as a blank")]
    note: Option<String>,
}

/// The one row a rename names.
#[derive(Debug, PartialEq, Eq)]
enum Row {
    Account(i64),
    Mount { profile: String, ord: i64 },
}

#[derive(Debug)]
struct Args {
    row: Row,
    from: String,
    to: String,
    dry_run: bool,
    note: Option<String>,
}

impl RenameSymbolCommand {
    fn into_args(self) -> Result<Args, String> {
        let row = match (self.id, self.mount) {
            (Some(id), None) => Row::Account(id),
            (None, Some(mount)) => {
                let (profile, ord) = parse_mount(&mount)?;
                Row::Mount { profile, ord }
            }
            _ => {
                return Err("name the ONE row to rename: --id ID or --mount PROFILE:ORD. \
                            Nothing was written."
                    .to_string());
            }
        };
        Ok(Args { row, from: self.from, to: self.to, dry_run: self.dry_run, note: self.note })
    }
}

/// `--mount PROFILE:ORD` read as the stored daemon profile's name and its mount's ordinal (also
/// `config profile mount set-param`'s address).
pub(super) fn parse_mount(mount: &str) -> Result<(String, i64), String> {
    mount
        .rsplit_once(':')
        .and_then(|(profile, ord)| Some((profile, ord.parse::<i64>().ok()?)))
        .filter(|(profile, ord)| !profile.is_empty() && *ord >= 0)
        .map(|(profile, ord)| (profile.to_string(), ord))
        .ok_or_else(|| {
            format!(
                "--mount `{mount}` is not PROFILE:ORD (a stored daemon profile's name and its \
                 mount's ordinal, `tradehub:0`). Nothing was written."
            )
        })
}

/// Read `config rename-symbol`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    use super::Sub;
    parse_config_verb!(argv, "rename-symbol", cmd in Sub::RenameSymbol(cmd), BIN)
}

/// Entry point: the note, then the rename, then its report.
pub(crate) fn run(cmd: RenameSymbolCommand, ctx: settings_write::Context<'_>) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    if let Some(word) = parsed.note.as_deref() {
        println!("{}", note_line(word));
    }
    match execute(&parsed, &ctx) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

/// `--note`'s loud line: a row step's value holds no blank, so `_` stands for one.
fn note_line(word: &str) -> String {
    format!("⚠⚠ NOTE: {}", word.replace('_', " "))
}

fn execute(args: &Args, ctx: &settings_write::Context<'_>) -> CommandResult<String> {
    let dir = ctx.settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — there is no store to rename a symbol in. Set \
             VIKE_SETTINGS_DIR, or run from a project that has one.",
        )
    })?;
    match &args.row {
        Row::Account(id) => rename_account(args, *id, dir, ctx.now_ms),
        Row::Mount { profile, ord } => rename_mount(args, profile, *ord, dir, ctx),
    }
}

/// Why NEW is not a symbol of `venue`'s shape, `None` when it is: the account column's floor, then the door's own
/// spelling of it, unchanged. No kept list is read (the module doc says why).
fn shape_refusal(venue: &str, new: &str) -> Option<String> {
    if vike_secrets::normalized_account_symbol(new).as_deref() != Some(new) {
        return Some(format!(
            "it is not one token of printable ASCII of at most {} bytes",
            vike_secrets::ACCOUNT_SYMBOL_MAX_BYTES
        ));
    }
    match vike_catalog::door_symbol(venue, new) {
        Some(Ok(spelled)) if spelled == new => None,
        Some(Ok(spelled)) => Some(format!("the {venue} door spells it `{spelled}`: write that")),
        Some(Err(why)) => Some(why),
        None => Some(format!("{venue} names no instrument by a symbol")),
    }
}

/// Whether the row `what`, holding `held`, is renamed (`true`) or already holds NEW (`false`); any other symbol is
/// refused, naming it.
fn renames(what: &str, held: Option<&str>, args: &Args) -> CommandResult<bool> {
    match held {
        Some(symbol) if symbol == args.to => Ok(false),
        Some(symbol) if symbol == args.from => Ok(true),
        _ => Err(CliError::usage(format!(
            "{what} holds {}, not `{}`, so it is not renamed. Nothing was written.",
            held.map_or_else(|| "no symbol".to_string(), |symbol| format!("`{symbol}`")),
            args.from
        ))),
    }
}

/// The checks both rows share, in order: NEW's shape, then what the row holds. `Ok(None)` is a rename to make,
/// `Ok(Some(report))` the answer without a write (unchanged, or a dry run).
fn verdict(
    what: &str,
    venue: &str,
    held: Option<&str>,
    args: &Args,
) -> CommandResult<Option<String>> {
    if let Some(why) = shape_refusal(venue, &args.to) {
        return Err(CliError::usage(format!(
            "{what}: `{}` is refused: {why}. Nothing was written.",
            args.to
        )));
    }
    if !renames(what, held, args)? {
        return Ok(Some(format!(
            "unchanged — {what} already holds `{}`. Nothing was written.",
            args.to
        )));
    }
    Ok(args.dry_run.then(|| {
        format!(
            "would rename {what} from `{}` to `{}` — NOTHING WAS WRITTEN (--dry-run).",
            args.from, args.to
        )
    }))
}

fn rename_account(args: &Args, id: i64, dir: &Path, now_ms: i64) -> CommandResult<String> {
    // The account table only: no venue key value is selected.
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::NoVenueKeys,
    );
    let rows = match store
        .accounts()
        .map_err(|e| CliError::failed(format!("{e}. Nothing was written.")))?
    {
        vike_secrets::AccountRows::Known(rows) => rows,
        vike_secrets::AccountRows::Unanswerable(why) => {
            return Err(CliError::failed(format!("{why}. Nothing was written.")));
        }
    };
    let row = rows.iter().find(|row| row.id == id).ok_or_else(|| {
        CliError::usage(format!("no account with id {id} in this store. Nothing was written."))
    })?;
    let what = format!("account {id} ({})", row.venue);
    if let Some(report) = verdict(&what, &row.venue, row.symbol.as_deref(), args)? {
        return Ok(report);
    }
    let stamp = vike_secrets::ChangeStamp {
        actor: ChangeActor::cli("vike-cli"),
        process: ChangeProcess::current(env!("CARGO_PKG_VERSION")),
        now_ms,
    };
    let (_, journal_error) = vike_secrets::edit_account_in(
        dir,
        vike_secrets::AccountEdit::SetSymbol { id, symbol: Some(&args.to) },
        stamp,
    )
    .map_err(|e| CliError::failed(e.to_string()))?;
    crate::cmd::secrets::report_unrecorded_write(
        &format!("{BIN}: ⚠ account {id} was written"),
        "it",
        journal_error,
    );
    Ok(format!(
        "renamed {what} from `{}` to `{}`; its engine mounts it from its next start.",
        args.from, args.to
    ))
}

fn rename_mount(
    args: &Args,
    profile: &str,
    ord: i64,
    dir: &Path,
    ctx: &settings_write::Context<'_>,
) -> CommandResult<String> {
    let db = vike_secrets::db_path_in(dir);
    let profiles = read_profiles(&db).map_err(|e| CliError::failed(e.to_string()))?;
    let body = match profiles.by_name(profile) {
        Some(stored) if stored.row.kind == ProfileKind::Daemon => stored.clone(),
        Some(stored) => {
            return Err(CliError::usage(format!(
                "`{profile}` is stored as a `{}` profile, and only a daemon profile mounts a symbol. \
                 Nothing was written.",
                stored.row.kind.sql_word()
            )));
        }
        None => {
            return Err(CliError::usage(format!(
                "no stored profile called `{profile}` in {}. Nothing was written.",
                db.display()
            )));
        }
    };
    let Some(mount) = body.mounts.iter().find(|mount| mount.ord == ord) else {
        return Err(CliError::usage(format!(
            "daemon profile `{profile}` has no mount {ord}. Nothing was written."
        )));
    };
    let what = format!("mount {profile}:{ord} ({})", mount.venue);
    if let Some(report) = verdict(&what, &mount.venue, mount.symbol.as_deref(), args)? {
        return Ok(report);
    }
    let key = format!("profile.daemon.{profile}.mount.{ord}.symbol");
    let record = |outcome: ChangeOutcome, reason: Option<&str>| {
        let change = Change::set_setting(
            outcome,
            ChangeActor::cli("vike-cli"),
            "profile",
            &key,
            Some(&args.from),
            &args.to,
        )
        .with_reason(reason);
        settings_write::record_change(ctx, &key, "renamed", outcome, &change);
    };
    // The rename is made on the body read under the write's own lock (`update_profile`), so an
    // edit of another part of this body landing since the read above is kept, not lost (decision
    // 0297); a mount that no longer holds OLD there is refused.
    match update_profile(
        &db,
        profile,
        &ProfileOperatorWrite::claim(BIN),
        ctx.now_ms / 1000,
        vike_model::AssetClass::SQL_WORDS,
        |held| {
            let mut held = held.filter(|p| p.row.kind == ProfileKind::Daemon);
            let mount = held
                .as_mut()
                .and_then(|p| p.mounts.iter_mut().find(|m| m.ord == ord))
                .filter(|m| m.symbol.as_deref() == Some(args.from.as_str()))
                .ok_or_else(|| {
                    format!(
                        "{what} no longer holds `{}`: it changed while this ran. Nothing was \
                         written.",
                        args.from
                    )
                })?;
            mount.symbol = Some(args.to.clone());
            Ok(held)
        },
    ) {
        Ok(_) => {
            record(ChangeOutcome::AppliedPendingRestart, None);
            Ok(format!(
                "renamed {what} from `{}` to `{}`; a running daemon keeps the mount it started on, so \
                 it binds at the daemon's next start.",
                args.from, args.to
            ))
        }
        Err(e) => {
            record(ChangeOutcome::Refused, Some(&e.to_string()));
            Err(CliError::failed(e.to_string()))
        }
    }
}

/// Its store fixtures are shared with `config profile mount set-param`'s and `config profile mount set-max-leverage`'s tests.
#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeMap;

    use vike_model::{AccountTier, AssetClass, Contract, ContractPerp, Instrument, Margining};
    use vike_secrets::profile_store::{
        MountRow, ProfileRecord, ProfileRow, set_active, store_profile,
    };

    use super::*;
    use crate::exit::Exit;

    fn parsed(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    pub(in crate::cmd::config) fn ctx<'a>(
        dir: &'a Path,
        state: &'a Path,
    ) -> settings_write::Context<'a> {
        settings_write::Context {
            settings_dir: Some(dir),
            state_dir: Some(state),
            now_ms: 1_794_658_380_000,
            surface: BIN,
        }
    }

    /// An empty store in a temp directory.
    pub(in crate::cmd::config) fn store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path())).unwrap();
        dir
    }

    /// A store holding one demo account of `venue` labelled DEMO and stating `symbol`, written as `account
    /// add` and `set-symbol` write them (record-less); its id.
    fn store_with_account(venue: &str, symbol: &str) -> (tempfile::TempDir, i64) {
        let dir = store();
        let create = vike_secrets::AccountEdit::Create {
            venue,
            tier: AccountTier::Demo,
            label: Some("DEMO"),
        };
        let id = vike_secrets::edit_account_in_unjournalled(dir.path(), create)
            .unwrap()
            .after
            .unwrap()
            .id;
        let set = vike_secrets::AccountEdit::SetSymbol { id, symbol: Some(symbol) };
        vike_secrets::edit_account_in_unjournalled(dir.path(), set).unwrap();
        (dir, id)
    }

    fn account_symbol(dir: &Path, id: i64) -> Option<String> {
        let store = vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(dir)),
            vike_secrets::CredentialScope::NoVenueKeys,
        );
        match store.accounts().unwrap() {
            vike_secrets::AccountRows::Known(rows) => {
                rows.into_iter().find(|row| row.id == id).unwrap().symbol
            }
            vike_secrets::AccountRows::Unanswerable(why) => panic!("{why}"),
        }
    }

    /// The daemon profile the CI box stores (read 2026-11-16): `tradehub`, ACTIVE, its mount 0 the spread maker on bybit
    /// `CryptoPerp` `BTCUSDT` for account DEMO, its knobs under `[strategy.params]`.
    pub(in crate::cmd::config) fn store_prod2_tradehub(dir: &Path) {
        let db = vike_secrets::db_path_in(dir);
        let mut mount = MountRow::new(0, "bybit", "CryptoPerp");
        mount.symbol = Some("BTCUSDT".into());
        mount.interval = Some("1m".into());
        mount.tick_size = Some(0.1);
        mount.seed_cash = Some(1000.0);
        mount.account = Some("DEMO".into());
        mount.strategy_file = Some("market_making/spread_maker.rs".into());
        let stored = ProfileRecord {
            row: ProfileRow {
                name: "tradehub".into(),
                kind: ProfileKind::Daemon,
                active: false,
                note: None,
            },
            mounts: vec![mount],
            params: BTreeMap::from([
                ((0, "half_spread".into()), "0.0005".into()),
                ((0, "qty".into()), "0.001".into()),
            ]),
            settings: BTreeMap::from([
                ("daemon.shutdown_deadline_ms".into(), "10000".into()),
                ("daemon.summary_ms".into(), "60000".into()),
            ]),
            recorder: None,
        };
        let w = ProfileOperatorWrite::claim("test");
        store_profile(&db, &stored, &w, 1, AssetClass::SQL_WORDS).unwrap();
        set_active(&db, ProfileKind::Daemon, "tradehub", &w, 1, AssetClass::SQL_WORDS).unwrap();
    }

    pub(in crate::cmd::config) fn tradehub(dir: &Path) -> ProfileRecord {
        let profiles = read_profiles(&vike_secrets::db_path_in(dir)).unwrap();
        profiles.by_name("tradehub").unwrap().clone()
    }

    /// bybit's kept rows of BTC/USDT under decision 0290: the SPOT pair `BTCUSDT` and the LINEAR perpetual
    /// `BTCUSDT.P` (the row keeps the lane's `.P`; its wire name is `BTCUSDT`), named as the datahub's write stores
    /// them (`vike_catalog::spelled`).
    fn bybit_rows() -> Vec<Instrument> {
        let linear = Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        vike_catalog::spelled(vec![
            Instrument::new("bybit", "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot),
            Instrument::new("bybit", "BTCUSDT.P", "BTC", "USDT", linear),
        ])
    }

    /// Every file under the journal's state directory, concatenated.
    pub(in crate::cmd::config) fn journal_text(state: &Path) -> String {
        let mut out = String::new();
        let mut stack = vec![state.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push_str(&t);
                }
            }
        }
        out
    }

    #[test]
    fn the_parser_takes_one_row_an_old_and_a_new_name() {
        let a = parsed(&["--id", "4", "--from", "BTCUSDT", "--to", "BTCUSDT.P"]).unwrap();
        assert_eq!(
            (a.row, a.from.as_str(), a.to.as_str()),
            (Row::Account(4), "BTCUSDT", "BTCUSDT.P")
        );
        let m =
            parsed(&["--mount", "tradehub:0", "--from", "A", "--to", "B", "--dry-run"]).unwrap();
        assert_eq!(m.row, Row::Mount { profile: "tradehub".into(), ord: 0 });
        assert!(m.dry_run);
        let note = parsed(&["--id", "13", "--from", "A", "--to", "B", "--note", "x_y"]).unwrap();
        assert_eq!(note.note.as_deref(), Some("x_y"));
        for bad in [
            &["--from", "A", "--to", "B"][..],
            &["--id", "4", "--mount", "tradehub:0", "--from", "A", "--to", "B"],
            &["--mount", "tradehub", "--from", "A", "--to", "B"],
            &["--mount", ":0", "--from", "A", "--to", "B"],
            &["--mount", "tradehub:-1", "--from", "A", "--to", "B"],
            &["--id", "4", "--to", "B"],
        ] {
            assert!(parsed(bad).is_err(), "{bad:?}");
        }
        assert!(parsed(&["--help"]).unwrap_err().is_help());
    }

    /// `--account ID` is retired with no alias: it refuses by its id, naming `--id`, in either spelling.
    #[test]
    fn the_retired_account_flag_names_id() {
        use crate::refusals::RetiredConfigSpelling;
        let id = RetiredConfigSpelling::RenameSymbolAccount.id().word();
        for flag in [&["--account", "4"][..], &["--account=4"]] {
            let said = parsed(&[flag, &["--from", "A", "--to", "B"][..]].concat()).unwrap_err();
            assert!(said.contains(&format!("error [{id}]")), "{}", &*said);
            assert!(said.contains("is now `config rename-symbol --id ID`"), "{}", &*said);
        }
    }

    #[test]
    fn the_note_is_one_loud_line_with_each_underscore_a_blank() {
        assert_eq!(
            note_line("EURUSD_on_IG_is_now_the_STANDARD_lot"),
            "⚠⚠ NOTE: EURUSD on IG is now the STANDARD lot"
        );
    }

    /// **An account is renamed and journalled; the same step again is `unchanged`**, so a re-run and an undo of a
    /// step that wrote nothing are harmless.
    #[test]
    fn an_account_is_renamed_and_a_rerun_is_unchanged() {
        let (d, id) = store_with_account("bybit", "BTCUSDT");
        let state = d.path().join("state");
        let args =
            parsed(&["--id", &id.to_string(), "--from", "BTCUSDT", "--to", "BTCUSDT.P"]).unwrap();
        let report = execute(&args, &ctx(d.path(), &state)).unwrap();
        let renamed = format!("renamed account {id} (bybit) from `BTCUSDT` to `BTCUSDT.P`");
        assert!(report.contains(&renamed), "{report}");
        assert_eq!(account_symbol(d.path(), id).as_deref(), Some("BTCUSDT.P"));
        assert!(journal_text(&state).contains("set-symbol"), "the account write is journalled");

        let again = execute(&args, &ctx(d.path(), &state)).unwrap();
        let unchanged = format!("unchanged — account {id} (bybit) already holds `BTCUSDT.P`");
        assert!(again.starts_with(&unchanged), "{again}");
        assert_eq!(account_symbol(d.path(), id).as_deref(), Some("BTCUSDT.P"));
    }

    /// **A row holding a third name is refused**, naming it, and nothing is written.
    #[test]
    fn a_row_holding_a_third_name_is_refused_and_nothing_is_written() {
        let (d, id) = store_with_account("bybit", "ETHUSDT");
        let state = d.path().join("state");
        let args =
            parsed(&["--id", &id.to_string(), "--from", "BTCUSDT", "--to", "BTCUSDT.P"]).unwrap();
        let e = execute(&args, &ctx(d.path(), &state)).unwrap_err();
        assert_eq!(e.exit, Exit::Usage);
        assert!(e.message.contains("holds `ETHUSDT`, not `BTCUSDT`"), "{}", e.message);
        assert_eq!(account_symbol(d.path(), id).as_deref(), Some("ETHUSDT"));
        assert!(!state.exists(), "a refusal writes no ledger row");
    }

    /// **NEW is held to the door's shape on the row's venue**, never to a list: a name the door spells another way
    /// (`btcusdt.p`) and hyperliquid's wire name (`@107`) are refused; a name no list holds yet passes.
    #[test]
    fn a_new_name_of_the_wrong_shape_is_refused() {
        for (venue, old, new, says) in [
            ("bybit", "BTCUSDT", "btcusdt.p", "spells it `BTCUSDT.P`"),
            ("hyperliquid", "BTC", "@107", "wire name"),
        ] {
            let (d, id) = store_with_account(venue, old);
            let args = parsed(&["--id", &id.to_string(), "--from", old, "--to", new]).unwrap();
            let e = execute(&args, &ctx(d.path(), &d.path().join("state"))).unwrap_err();
            assert_eq!(e.exit, Exit::Usage);
            assert!(
                e.message.contains(says) && e.message.contains("Nothing was written"),
                "{}",
                e.message
            );
            assert_eq!(account_symbol(d.path(), id).as_deref(), Some(old));
        }
        let (d, id) = store_with_account("deribit", "BTC-PERPETUAL");
        let args =
            parsed(&["--id", &id.to_string(), "--from", "BTC-PERPETUAL", "--to", "BTCUSD.P"])
                .unwrap();
        execute(&args, &ctx(d.path(), &d.path().join("state"))).unwrap();
        assert_eq!(
            account_symbol(d.path(), id).as_deref(),
            Some("BTCUSD.P"),
            "no kept list is read"
        );
    }

    /// `--dry-run` says what would change and writes nothing, on an account and on a mount.
    #[test]
    fn a_dry_run_writes_nothing() {
        let (d, id) = store_with_account("ig", "CS.D.EURUSD.MINI.IP");
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let account = parsed(&[
            "--id",
            &id.to_string(),
            "--from",
            "CS.D.EURUSD.MINI.IP",
            "--to",
            "EURUSD",
            "--dry-run",
        ])
        .unwrap();
        let report = execute(&account, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("NOTHING WAS WRITTEN"), "{report}");
        assert_eq!(account_symbol(d.path(), id).as_deref(), Some("CS.D.EURUSD.MINI.IP"));
        let mount = parsed(&[
            "--mount",
            "tradehub:0",
            "--from",
            "BTCUSDT",
            "--to",
            "BTCUSDT.P",
            "--dry-run",
        ])
        .unwrap();
        let report = execute(&mount, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("would rename mount tradehub:0 (bybit)"), "{report}");
        assert_eq!(tradehub(d.path()).mounts[0].symbol.as_deref(), Some("BTCUSDT"));
        assert!(!state.exists(), "a rehearsal writes no ledger row");
    }

    /// A mount the store does not hold is refused, and so is a profile that is not a daemon profile.
    #[test]
    fn a_mount_the_store_does_not_hold_is_refused() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        for (mount, says) in [("tradehub:1", "has no mount 1"), ("nonsuch:0", "no stored profile")]
        {
            let args =
                parsed(&["--mount", mount, "--from", "BTCUSDT", "--to", "BTCUSDT.P"]).unwrap();
            let e = execute(&args, &ctx(d.path(), &state)).unwrap_err();
            assert!(e.message.contains(says), "{mount}: {}", e.message);
        }
        assert_eq!(tradehub(d.path()).mounts[0].symbol.as_deref(), Some("BTCUSDT"));
    }

    /// **The spread maker's mount comes back on `BTCUSDT.P`, bybit's LINEAR perpetual** (deploy 2's mount step on
    /// the body the CI box stores): under decision 0290 the stored `BTCUSDT` is the SPOT pair; renamed, the body keeps its
    /// selection, knobs and settings, loads through the daemon's own boot loader with bybit's kept list held, and the
    /// door reads the mount as the linear perpetual whose wire name is `BTCUSDT` (the book bybit signs it in).
    #[test]
    fn the_spread_makers_mount_comes_back_on_the_linear_perpetual() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let rows = bybit_rows();
        let before = vike_catalog::kept_row_of_user_name(&rows, "bybit", "BTCUSDT").unwrap();
        assert_eq!(
            before.asset_class(),
            AssetClass::CryptoSpot,
            "the old name is the spot pair now"
        );

        let args =
            parsed(&["--mount", "tradehub:0", "--from", "BTCUSDT", "--to", "BTCUSDT.P"]).unwrap();
        let report = execute(&args, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("renamed mount tradehub:0 (bybit)"), "{report}");
        let stored = tradehub(d.path());
        assert!(stored.row.active, "the rename keeps the selection");
        assert_eq!(stored.mounts[0].symbol.as_deref(), Some("BTCUSDT.P"));
        assert_eq!((stored.params.len(), stored.settings.len()), (2, 2), "the body is kept whole");
        assert!(journal_text(&state).contains("profile.daemon.tradehub.mount.0.symbol"));

        vike_catalog::hold_kept_list("bybit", rows.clone());
        let daemon = vike_tradehub::profile_rows::rows_to_daemon_profile(&stored)
            .expect("the daemon's boot loader takes the renamed body");
        assert_eq!((daemon.venue(), daemon.mount_symbol()), ("bybit", "BTCUSDT.P"));
        let door = vike_catalog::door_user_symbol("bybit", daemon.mount_symbol(), &rows);
        assert_eq!(door.as_deref(), Ok("BTCUSDT.P"));
        let row =
            vike_catalog::kept_row_of_user_name(&rows, "bybit", daemon.mount_symbol()).unwrap();
        assert!(
            matches!(&row.contract, Contract::Perp(perp) if perp.margining == Margining::Linear),
            "{row:?}"
        );
        assert_eq!(vike_catalog::wire_name_of_row(row), "BTCUSDT");

        let again = execute(&args, &ctx(d.path(), &state)).unwrap();
        assert!(again.starts_with("unchanged"), "{again}");
    }
}
