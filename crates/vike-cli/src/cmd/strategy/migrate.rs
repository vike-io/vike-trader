//! `vike-cli strategy migrate FILE [--to N] [--dry-run]` — carry a Rust strategy file's JSON state hooks
//! onto `StrategyState` (step 0), move one that pins `[strategy] sdk = N` to a newer SDK, or carry a
//! Rhai script across the names this vike renamed.
//! User-run and opt-in, it MAY rewrite the file
//! (`docs/decisions/0296-a-rust-strategy-file-builds-against-a-numbered-frozen-sdk-it-may-pin.md`,
//! verdict 4, which superseded the rulings that the tooling only hints; the `.rhai` half is the
//! owner's word of 2026-11-19, which replaced refusing a `.rhai` file).
//!
//! # A `.rs` file, in order
//!
//! 0. **The state signature of `ABI_VERSION` 15** (`vike_model::host_build::state_signature`): a
//!    `save_state`/`load_state` still on `serde_json::Value` is carried onto `vike_model::StrategyState`
//!    first, for every `.rs` file, since no SDK this vike builds has the JSON one; a body of no shape
//!    that rule rewrites is left as it is, with a hint naming its line. A file that pins NO SDK takes
//!    this step alone: its result is built on the builder's newest SDK and written by step 4 when it
//!    builds (no installed SDK is needed for that); with nothing to carry, or a hint, it is refused.
//!    **And the rows' symbol type of the SDK of 2026-11-20** (`vike_model::host_build::row_symbol`): a
//!    bar's or a tick's `symbol` read or written as a `String` is carried onto `vike_marketdata::Symbol`,
//!    for a file that pins no SDK or pins one whose rows held `String`s ([`rows_hold_symbols`]); a read
//!    that rule cannot type is left as it is, with a hint naming its line (it may build as it stands).
//! 1. **Refuses before anything is dialled**: a file that is neither `.rs` nor `.rhai`, a header that
//!    does not parse, a file that pins no SDK and has no state to carry (step 0), a target below
//!    the pin, and a target this vike does not build (`vike_rust_strategy::sdk`'s `usable`).
//! 2. **Builds the file as it stands** (after step 0) on its own SDK, through the backtesthub's Build
//!    (the builder takes the SDK from the header's pin), and refuses one that does not build: migrate
//!    moves a file that builds, so every error after that is the migration's. ⚠ A pin this vike no
//!    longer builds (retired, or cut with another boundary) cannot be checked, and moving such a file
//!    is what migrate is FOR (that refusal names it): the walk then starts unchecked, and says so.
//! 3. **Steps ONE SDK at a time** (Pine's model), N to N + 1 to … the target: applies the step's
//!    rename table (`vike_model::host_build::source_edits`), sets the pin to the step's SDK
//!    ([`with_sdk_pin`]) and builds. A step is kept only when it builds; the first that does not
//!    ends the walk, with rustc's words and the step's hints. A step whose SDK this vike does not
//!    build is applied unchecked and checked by the next build, which the target always gets.
//! 4. **Writes the last step that built**, after a numbered backup of the original ([`keep_backup`]),
//!    or with `--dry-run` prints the diff and writes nothing. A walk that stopped early still writes
//!    what built, and exits on the failed rung. ⚠ The file is read AGAIN first: a save made while
//!    the walk was building (Studio's, an editor's) is refused in words and nothing is written,
//!    since writing would lose it and the backup would hold the older text.
//!
//! # A `.rhai` file
//!
//! A script pins no SDK (a `.rhai` header naming `sdk` stays refused) and runs on this vike's own
//! code, so there is no build to gate on and no record of the tables it has had. So: the script
//! must PARSE as it stands (`vike_rhai::parse_check`, nothing run); the `rhai` rows of every
//! installed SDK's table up to `--to` (absent: all) are applied in order, each edit only where the
//! old name still resolves to NOTHING on this vike (`vike_model::host_build::source_edits`'s
//! `rhai_edits`, with `vike_rhai::binds_name`), which is what makes a second run change nothing;
//! a property or type row is never an edit (nothing can tell vike's from the script's own field of
//! that name), only a hint naming the file and the line; the result must parse again, or nothing is
//! written; then step 4, the same backup and dry run.
//! A table whose SDK this box no longer keeps is not read (its snapshot went with it).
//!
//! # Where the SDKs and their tables come from
//!
//! The installed SDKs, whether each builds here and each step's rename table are read from THIS
//! machine's project (`vike_rust_strategy::sdk`'s `SDK_SOURCE_DIR`, `installed`, `usable`;
//! `super::rename_table`), and each build is the backtesthub's at `--addr` → `config.backtest_addr`
//! → the default. So it runs on the box that holds the SDKs, and a project with none installed is
//! refused in words.
//!
//! The pre-SDK names (`vike_model::host_build::source_edits::builtin_renames`) are NOT applied: a
//! file that builds on an SDK reaches none of them, and fed them, a word a later SDK gave to a new
//! item would be renamed away.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use vike_backtesthub_client::BacktesthubClient;
use vike_hub_protocol::auth::Scope;
use vike_model::host_build::row_symbol::symbol_edits;
use vike_model::host_build::source_edits::{
    RhaiRename, SdkRename, SourceEdits, apply_edits, rhai_edits, source_edits,
};
use vike_model::host_build::state_signature::state_edits;
use vike_strategy::file::StrategyFileHeader;
use vike_strategy::file::scan::{StrategyFileLanguage, language_of};

use super::{Context, rename_table};
use crate::exit::{CliError, CommandResult};

/// `migrate`'s command line.
#[derive(clap::Args, Debug)]
pub(crate) struct MigrateCommand {
    #[arg(value_name = "FILE", help = "The Rust strategy file to move, a path on this machine")]
    pub(super) file: PathBuf,
    #[arg(
        long,
        value_name = "N",
        help = "The SDK to move it to. Absent: the newest installed under \
                <project>/vendor/vike-strategy-source"
    )]
    pub(super) to: Option<u32>,
    #[arg(long, help = "Print the diff of what would be written, and write nothing")]
    pub(super) dry_run: bool,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "The backtesthub that builds each step. Absent: config.backtest_addr, then the \
                default address"
    )]
    pub(super) addr: Option<String>,
}

/// What the walk decided, before a byte is written.
#[derive(Debug, PartialEq, Eq)]
struct Migration {
    /// The last SDK the text builds on (the pin it started at, when no step built).
    reached: u32,
    /// The text at [`Self::reached`].
    text: String,
    /// Why the step after [`Self::reached`] was not kept, when the walk ended before the target.
    stopped: Option<String>,
}

/// The verb: the module doc's steps, for a `.rs` or a `.rhai` file.
pub(super) fn run(cmd: &MigrateCommand, ctx: &Context<'_>) -> CommandResult<()> {
    let file = cmd.file.as_path();
    let name = file.display().to_string();
    let Some(language) = language_of(file) else {
        return Err(CliError::usage(format!(
            "{name} is not a strategy file: migrate moves a Rust (`.rs`) or Rhai (`.rhai`) one"
        )));
    };
    let original = std::fs::read_to_string(file)
        .map_err(|e| CliError::failed(format!("cannot read {name}: {e}")))?;
    // A `.rs` file's header, and the state rule of ABI 15 and the symbol rule applied to it first (step 0).
    let rust = match language {
        StrategyFileLanguage::Rhai => None,
        StrategyFileLanguage::Rust => {
            let header = StrategyFileHeader::parse_file(&original, StrategyFileLanguage::Rust)
                .map_err(|e| CliError::refused(format!("{name}: {e}")))?;
            let state = state_edits(&original);
            for hint in &state.hints {
                println!("  hint: {name}: {hint}");
            }
            let symbol = symbol_edits(&original);
            let Some(from) = header.strategy.sdk else {
                // An unpinned file builds on the newest SDK, whose rows hold `Symbol`s: both rules.
                print_symbol_hints(&name, &symbol);
                let start = unpinned(&name, &original, &step_zero(&state, Some(&symbol)), cmd.to)?;
                let mut client = connect(cmd, ctx)?;
                client.build_strategy_file(&built_as(file), &start, None).map_err(|e| {
                    CliError::refused(format!(
                        "the step-0 rewrite does not build, so nothing is written:\n{e}"
                    ))
                })?;
                // `reached` is not read for an unpinned file: it builds on the builder's newest SDK.
                let migration = Migration { reached: 0, text: start, stopped: None };
                let reached = "the state of ABI 15 and the `Symbol` rows";
                return finish(file, &original, &migration, reached, cmd.dry_run);
            };
            Some((from, state, symbol))
        }
    };
    let project = ctx.project_root.ok_or_else(|| {
        CliError::refused(
            "no project above the working directory, and migrate reads the installed SDKs from \
             <project>/vendor/vike-strategy-source",
        )
    })?;
    let sdk_root = project.join(vike_rust_strategy::sdk::SDK_SOURCE_DIR);
    let installed = vike_rust_strategy::sdk::installed(&sdk_root).map_err(CliError::failed)?;
    let Some(&newest) = installed.last() else {
        return Err(CliError::refused(format!(
            "no SDK is installed under {}: migrate runs on the box that holds the SDKs",
            sdk_root.display()
        )));
    };
    let to = cmd.to.unwrap_or(newest);
    let Some((from, state, symbol)) = rust else {
        let tables = installed
            .iter()
            .filter(|sdk| **sdk <= to)
            .map(|&sdk| rename_table::step_into(&sdk_root, sdk).map(|table| (sdk, table.rhai)))
            .collect::<Result<Vec<_>, String>>()
            .map_err(CliError::failed)?;
        let parses = |text: &str| vike_rhai::parse_check(text).map_err(|e| e.to_string());
        let text = rhai_walk(&name, &original, &tables, &vike_rhai::binds_name, &parses)?;
        let migration = Migration { reached: to, text, stopped: None };
        return finish(
            file,
            &original,
            &migration,
            &format!("the renames through SDK {to}"),
            cmd.dry_run,
        );
    };
    if to < from {
        return Err(CliError::refused(format!(
            "{name} pins SDK {from}, and migrate moves a file forward only: --to {to} is older"
        )));
    }
    // The symbol rule carries a file written against `String` rows; one whose pin already lends `Symbol`
    // rows was written against them, and the rule would only undo its reads.
    let symbol = (!rows_hold_symbols(&sdk_root, from)).then_some(&symbol);
    if let Some(symbol) = symbol {
        print_symbol_hints(&name, symbol);
    }
    let start = apply_edits(&original, &step_zero(&state, symbol).edits);
    if to == from && start == original {
        println!("{name} is on SDK {from} already: nothing to move");
        return Ok(());
    }
    // Whether this vike builds SDK `sdk`: installed here, and usable by core's one rule.
    let builds_here = |sdk: u32| -> Result<(), String> {
        if !installed.contains(&sdk) {
            return Err(format!("SDK {sdk} is not installed under {}", sdk_root.display()));
        }
        let snapshot = vike_rust_strategy::sdk::snapshot_dir(&sdk_root, sdk);
        let marker = vike_rust_strategy::sdk::SdkMarker::read(&snapshot)?;
        // A malformed adapter is no adapter: it refuses only the SDK it was for, never another.
        let adapters = vike_rust_strategy::sdk::host_adapters(&sdk_root).adapters;
        vike_rust_strategy::sdk::usable(&marker, &snapshot, newest, &adapters)
            .map(drop)
            .map_err(|e| e.to_string())
    };
    builds_here(to).map_err(|e| CliError::refused(format!("--to {to}: {e}")))?;

    let built_as = built_as(file);
    let mut client = connect(cmd, ctx)?;
    let mut migration = walk(
        &start,
        (from, to),
        &builds_here,
        &mut |sdk| rename_table::step_into(&sdk_root, sdk).map(|table| table.rust),
        &mut |text, sdk| client.build_strategy_file(&built_as, text, Some(sdk)).map(drop),
    )?;
    if migration.reached == from && builds_here(from).is_err() {
        // No step built and the pin was never checked: step 0's edits built nowhere, so none is kept.
        migration.text = original.clone();
    }
    finish(file, &original, &migration, &format!("SDK {}", migration.reached), cmd.dry_run)
}

/// Step 0's edits (the module doc): the state rule's, and the symbol rule's when `symbol` applies, sorted
/// and never overlapping (the state rule edits `save_state`/`load_state`, the symbol rule a row's reads and
/// literals; were two to meet, the first wins). Only the state rule's hints: a symbol hint names a read
/// migrate leaves, which may build as it stands, so it refuses nothing ([`print_symbol_hints`]).
fn step_zero(state: &SourceEdits, symbol: Option<&SourceEdits>) -> SourceEdits {
    let mut edits: Vec<_> =
        state.edits.iter().chain(symbol.into_iter().flat_map(|s| &s.edits)).cloned().collect();
    edits.sort_by_key(|e| (e.at.start, e.at.end));
    let mut carried = SourceEdits { edits: Vec::new(), hints: state.hints.clone() };
    for e in edits {
        if carried.edits.last().is_none_or(|last| last.at.end <= e.at.start) {
            carried.edits.push(e);
        }
    }
    carried
}

/// The symbol rule's hints, each a read it left as it is, printed beside the file's name.
fn print_symbol_hints(name: &str, symbol: &SourceEdits) {
    for hint in &symbol.hints {
        println!("  hint: {name}: {hint}");
    }
}

/// Whether SDK `sdk` installed under `sdk_root` lends its bars' and ticks' symbols as this vike does
/// (`vike_marketdata::Symbol`): its snapshot's lent-field golden has this vike's `Bar.symbol` line
/// (`vike_rust_strategy::sdk`'s `LENT_FIELDS`). `false` for one not installed here, so the symbol rule
/// applies to it, build-gated. shortcut: an SDK cut with `Symbol` rows and since dropped from the box reads
/// as `false` too, and the rule's edits on its reads then fail the walk's build in words; reading the
/// rows off the SDK's tag is the upgrade the day such a file turns up.
fn rows_hold_symbols(sdk_root: &Path, sdk: u32) -> bool {
    use vike_rust_strategy::sdk::{LENT_FIELDS, LENT_FIELDS_FILE, RUST_STRATEGY_CRATE_DIR};
    let golden = vike_rust_strategy::sdk::snapshot_dir(sdk_root, sdk)
        .join(RUST_STRATEGY_CRATE_DIR)
        .join(LENT_FIELDS_FILE);
    let Some(this) = LENT_FIELDS.lines().find(|line| line.starts_with("Bar.symbol:")) else {
        return false;
    };
    std::fs::read_to_string(golden).is_ok_and(|text| text.lines().any(|line| line == this))
}

/// The name the builder is handed: it takes a path only to check its shape (the artifact is named by
/// the text), so the file's own name serves wherever it lives.
fn built_as(file: &Path) -> String {
    file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The backtesthub that builds each text: `--addr`, then `config.backtest_addr`, then the default.
fn connect(cmd: &MigrateCommand, ctx: &Context<'_>) -> CommandResult<BacktesthubClient> {
    let addr = vike_client::resolved::backtesthub_dial_address(
        cmd.addr.as_deref(),
        ctx.configured_address,
    );
    // `Scope::Write`: a Build hands source to a compiler, a Control verb.
    Ok(BacktesthubClient::dial(&addr, ctx.keys, Scope::Write)?)
}

/// An unpinned `.rs` file (the file `name`, read as `original`) builds against the newest SDK, so the
/// state rule of ABI 15 (`state`, its hints printed already) is all migrate carries for it: the text with
/// the rule's edits applied, or a refusal saying why there is none. A hint refuses it whole: a hook left
/// on the JSON signature would fail the build anyway.
fn unpinned(
    name: &str,
    original: &str,
    state: &SourceEdits,
    to: Option<u32>,
) -> CommandResult<String> {
    let why = match (state.edits.is_empty(), state.hints.is_empty(), to) {
        (false, true, None) => return Ok(apply_edits(original, &state.edits)),
        (_, false, _) => {
            "and a state hook is no shape migrate rewrites: change the lines the hints above name \
             by hand, then run it again"
        }
        (false, true, Some(_)) => {
            "and --to moves a pinned file: run migrate without it to carry its state signature"
        }
        (true, true, _) => {
            "so it already builds against the newest one on the box: there is nothing to move it \
             from. Pin the SDK it was written for (`sdk = N` under `[strategy]`), then migrate it."
        }
    };
    Err(CliError::refused(format!("{name} pins no SDK, {why}")))
}

/// The `.rhai` half of the module doc: `original` (the file `name`) must parse, each `(sdk, rows)` of
/// `tables` is applied in order where `binds` says the old name resolves to nothing here, and the
/// result must parse again. `parses` is the parse check (`vike_rhai::parse_check`); a row this walk
/// will not edit (a property or a type) is printed as a hint naming the file and the line. Nothing
/// is written here.
fn rhai_walk(
    name: &str,
    original: &str,
    tables: &[(u32, Vec<RhaiRename>)],
    binds: &dyn Fn(&str) -> bool,
    parses: &dyn Fn(&str) -> Result<(), String>,
) -> CommandResult<String> {
    parses(original).map_err(|e| {
        CliError::refused(format!(
            "it does not parse as it stands, and migrate moves only a script that parses (so that \
             a parse error after it is the migration's): fix it first.\n{e}"
        ))
    })?;
    let mut text = original.to_string();
    for (sdk, rows) in tables {
        let found = rhai_edits(&text, rows, binds);
        if found.edits.is_empty() && found.hints.is_empty() {
            continue;
        }
        println!("SDK {sdk}'s Rhai renames: {} edit(s)", found.edits.len());
        for hint in &found.hints {
            println!("  hint: {name}: {hint}");
        }
        text = apply_edits(&text, &found.edits);
    }
    parses(&text).map_err(|e| {
        CliError::refused(format!(
            "the renames leave a script that does not parse, so nothing is written:\n{e}"
        ))
    })?;
    Ok(text)
}

/// Steps 2 and 3 of the module doc, from SDK `from` to `to`: each step's table from
/// `table_of(step)`, each text judged by `build(text, step)` (built against that step's SDK) when
/// `builds_here(step)` says this vike builds it.
fn walk(
    original: &str,
    (from, to): (u32, u32),
    builds_here: &dyn Fn(u32) -> Result<(), String>,
    table_of: &mut dyn FnMut(u32) -> Result<Vec<SdkRename>, String>,
    build: &mut dyn FnMut(&str, u32) -> Result<(), String>,
) -> CommandResult<Migration> {
    match builds_here(from) {
        Ok(()) => build(original, from).map_err(|e| {
            CliError::refused(format!(
                "it does not build on SDK {from} as it stands, and migrate moves only a file that \
                 builds (so that every error after it is the migration's): fix it first.\n{e}"
            ))
        })?,
        Err(why) => {
            println!("SDK {from}, the pin, does not build here ({why}): the walk starts unchecked")
        }
    }
    let mut migration = Migration { reached: from, text: original.to_string(), stopped: None };
    // The walk's text: ahead of `migration.text` while a step's SDK is one this vike does not build.
    let mut text = original.to_string();
    for sdk in from + 1..=to {
        let step = table_of(sdk).and_then(|table| {
            let found = source_edits(&text, &table);
            with_sdk_pin(&apply_edits(&text, &found.edits), sdk).map(|pinned| (found, pinned))
        });
        let (found, pinned) = match step {
            Ok(step) => step,
            Err(e) => {
                migration.stopped = Some(format!(
                    "the step to SDK {sdk} cannot be made, so the file stays at SDK {}: {e}",
                    migration.reached
                ));
                break;
            }
        };
        println!("SDK {} to {sdk}: {} edit(s)", sdk - 1, found.edits.len());
        for hint in &found.hints {
            println!("  hint: {hint}");
        }
        text = pinned;
        if let Err(why) = builds_here(sdk) {
            println!("  not built: SDK {sdk} does not build here ({why}); the next step checks it");
            continue;
        }
        match build(&text, sdk) {
            Ok(()) => {
                println!("  builds on SDK {sdk}");
                migration = Migration { reached: sdk, text: text.clone(), stopped: None };
            }
            Err(e) => {
                migration.stopped = Some(format!(
                    "the step to SDK {sdk} does not build, so the file stays at SDK {}:\n{e}",
                    migration.reached
                ));
                break;
            }
        }
    }
    if migration.stopped.is_none() && migration.reached != to {
        migration.stopped = Some(format!(
            "SDK {to} does not build here, so nothing past SDK {} was checked",
            migration.reached
        ));
    }
    Ok(migration)
}

/// Step 4 of the module doc: the diff, then (unless `dry_run`) the backup and the write; then the
/// failed rung when the walk stopped early. `reached` says what the written text is ("SDK 5").
fn finish(
    file: &Path,
    original: &str,
    migration: &Migration,
    reached: &str,
    dry_run: bool,
) -> CommandResult<()> {
    let name = file.display().to_string();
    if migration.text == original && migration.stopped.is_none() {
        println!("{name}: nothing to change for {reached}");
    }
    if migration.text != original {
        print!("--- {name}\n+++ {name} ({reached})\n{}", line_diff(original, &migration.text));
        if dry_run {
            println!("--dry-run: nothing written ({name} would move to {reached})");
        } else {
            // The walk took several remote builds: a save made meanwhile (Studio's, an editor's) would
            // be overwritten here and kept nowhere, the backup holding the older text. So the file
            // must still be what was read. shortcut: a save landing between this read and the
            // rename below still loses; a lock the editors honour would close it.
            let now = std::fs::read_to_string(file)
                .map_err(|e| CliError::failed(format!("cannot read {name} again: {e}")))?;
            if now != original {
                return Err(CliError::refused(format!(
                    "{name} changed while migrating; nothing was written. Run migrate again on \
                     the file as it is now."
                )));
            }
            let backup = keep_backup(file, original)?;
            vike_utility::atomic_file::write_atomic(file, migration.text.as_bytes()).map_err(
                |e| {
                    CliError::failed(format!(
                        "writing {name}: {e} (the original is kept at {})",
                        backup.display()
                    ))
                },
            )?;
            println!("{name}: moved to {reached} (the original is kept at {})", backup.display());
        }
    }
    match &migration.stopped {
        Some(why) => Err(CliError::failed(why.clone())),
        None => Ok(()),
    }
}

/// The lines `new` changed from `old`, each change under `@@ line N @@` (N in `new`): `-` the old
/// line, `+` the new one, a `+` alone for a line only `new` has.
///
/// shortcut: a greedy line walk, exact for what a migration changes (a rename keeps every line, the
/// pin adds at most one); an LCS diff is the upgrade the day an edit adds or removes lines elsewhere.
fn line_diff(old: &str, new: &str) -> String {
    let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let (mut i, mut j, mut in_change) = (0, 0, false);
    let mut out = String::new();
    while i < old.len() || j < new.len() {
        if i < old.len() && old.get(i) == new.get(j) {
            (i, j, in_change) = (i + 1, j + 1, false);
            continue;
        }
        if !in_change {
            out.push_str(&format!("@@ line {} @@\n", j + 1));
            in_change = true;
        }
        let only_new = new.len() - j > old.len() - i && new.get(j + 1) == old.get(i);
        if !only_new && let Some(line) = old.get(i) {
            out.push_str(&format!("-{line}\n"));
            i += 1;
        }
        if let Some(line) = new.get(j) {
            out.push_str(&format!("+{line}\n"));
            j += 1;
        }
    }
    out
}

/// Keep `text` (the file as it was) beside `file` as `FILE.bak.K`, K the first number free. Created
/// with `create_new`, so a backup is NEVER overwritten, by a second run or by another process racing
/// this one. No scan loads it: `vike_strategy::file::scan`'s `language_of` reads `.rs` and `.rhai`.
fn keep_backup(file: &Path, text: &str) -> CommandResult<PathBuf> {
    for number in 1u32.. {
        let mut name = file.as_os_str().to_owned();
        name.push(format!(".bak.{number}"));
        let path = PathBuf::from(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut backup) => {
                backup.write_all(text.as_bytes()).and_then(|()| backup.sync_all()).map_err(
                    |e| CliError::failed(format!("writing the backup {}: {e}", path.display())),
                )?;
                return Ok(path);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(CliError::failed(format!(
                    "creating the backup {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Err(CliError::failed(format!("no backup name is free beside {}", file.display())))
}

/// `text` with its header's `[strategy] sdk` set to `sdk`: the number of an `sdk = N` line under
/// `[strategy]` replaced (its trailing comment kept), or `sdk = N` added on the line after
/// `[strategy]`. The header's lines are found as `vike_strategy::file::StrategyFileHeader::parse`
/// finds them, and the result is parsed back: an `Err` names a header this edit cannot place the pin
/// in, which is then the user's to write.
fn with_sdk_pin(text: &str, sdk: u32) -> Result<String, String> {
    let mut edit: Option<(std::ops::Range<usize>, String)> = None;
    let (mut start, mut in_block, mut in_strategy) = (0, false, false);
    for line in text.split_inclusive('\n') {
        let at = start;
        start += line.len();
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        let Some(marker) = ["//!", "///", "//"].into_iter().find(|m| trimmed.starts_with(m)) else {
            if trimmed.is_empty() {
                continue;
            }
            break;
        };
        let after_marker = &trimmed[marker.len()..];
        let content = after_marker.strip_prefix(' ').unwrap_or(after_marker);
        let word = content.trim();
        if !in_block {
            in_block = word == "```toml";
            continue;
        }
        if word == "```" {
            break;
        }
        if word.starts_with('[') {
            in_strategy = word == "[strategy]";
            if in_strategy {
                let prefix = &body[..body.len() - content.len()];
                let eol = &line[body.len()..];
                edit =
                    Some((at + line.len()..at + line.len(), format!("{prefix}sdk = {sdk}{eol}")));
            }
            continue;
        }
        let value = content
            .trim_start()
            .strip_prefix("sdk")
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix('='))
            .map(str::trim_start);
        if let Some(value) = value.filter(|_| in_strategy) {
            let digits = value.len() - value.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let from = at + body.len() - value.len();
            edit = Some((from..from + digits, sdk.to_string()));
            break;
        }
    }
    let unplaced = || {
        format!(
            "the pin cannot be placed in this header: write `sdk = {sdk}` under `[strategy]` by hand"
        )
    };
    let (range, new) = edit.ok_or_else(unplaced)?;
    let pinned = format!("{}{new}{}", &text[..range.start], &text[range.end..]);
    match StrategyFileHeader::parse(&pinned) {
        Ok(header) if header.strategy.sdk == Some(sdk) => Ok(pinned),
        Ok(_) => Err(unplaced()),
        Err(e) => Err(format!("{}: {e}", unplaced())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file pinned to SDK `sdk` that reaches `vike_indicators::OldMean`.
    fn file_on(sdk: u32) -> String {
        format!(
            "//! A strategy.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! sdk = {sdk}   # pinned\n//! ```\n\nuse vike_indicators::OldMean;\n\npub fn rule(m: OldMean) {{}}\n"
        )
    }

    /// SDK 4 renamed `OldMean` to `NewMean`; SDK 5 renamed nothing.
    fn tables(sdk: u32) -> Result<Vec<SdkRename>, String> {
        Ok(match sdk {
            4 => vec![SdkRename {
                old: "vike_indicators::OldMean".into(),
                new: Some("vike_indicators::NewMean".into()),
                hint: None,
            }],
            _ => Vec::new(),
        })
    }

    #[test]
    fn the_pin_is_replaced_in_place_or_added_under_strategy() {
        let pinned = with_sdk_pin(&file_on(3), 4).expect("placed");
        assert!(pinned.contains("//! sdk = 4   # pinned\n"), "{pinned}");
        let unpinned = file_on(3).replace("//! sdk = 3   # pinned\n", "");
        let pinned = with_sdk_pin(&unpinned, 7).expect("placed");
        assert!(pinned.contains("//! [strategy]\n//! sdk = 7\n//! runs_on"), "{pinned}");
        assert!(with_sdk_pin("pub fn rule() {}\n", 4).is_err(), "no header, no pin");
    }

    #[test]
    fn the_diff_shows_each_changed_line_and_the_inserted_pin() {
        let old = file_on(3).replace("//! sdk = 3   # pinned\n", "");
        let new = with_sdk_pin(
            &apply_edits(&old, &source_edits(&old, &tables(4).expect("table")).edits),
            4,
        )
        .expect("placed");
        assert_eq!(
            line_diff(&old, &new),
            "@@ line 5 @@\n+//! sdk = 4\n\
             @@ line 9 @@\n-use vike_indicators::OldMean;\n+use vike_indicators::NewMean;\n\
             @@ line 11 @@\n-pub fn rule(m: OldMean) {}\n+pub fn rule(m: NewMean) {}\n"
        );
        assert_eq!(line_diff(&old, &old), "");
    }

    /// Every SDK builds here.
    fn every(_: u32) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn a_file_that_does_not_build_as_it_stands_is_refused_before_any_step() {
        let mut built = Vec::new();
        let refused = walk(&file_on(3), (3, 5), &every, &mut tables, &mut |text, _| {
            built.push(text.to_string());
            Err("error[E0425]: cannot find value `x`".to_string())
        })
        .expect_err("refused");
        assert_eq!(refused.exit, crate::exit::Exit::Refused);
        assert!(
            refused.message.contains("does not build on SDK 3 as it stands"),
            "{}",
            refused.message
        );
        assert!(refused.message.contains("cannot find value"), "rustc's words ride along");
        assert_eq!(built.len(), 1, "no step is tried");
    }

    #[test]
    fn each_step_applies_its_table_and_its_pin_and_the_walk_stops_at_the_first_that_does_not_build()
    {
        let mut builds = Vec::new();
        let migration = walk(&file_on(3), (3, 6), &every, &mut tables, &mut |text, sdk| {
            builds.push(sdk);
            if text.contains("sdk = 6") { Err("error: SDK 6 refused".to_string()) } else { Ok(()) }
        })
        .expect("walked");
        assert_eq!(migration.reached, 5);
        let expected = file_on(5).replace("OldMean", "NewMean");
        assert_eq!(migration.text, expected);
        assert!(migration.stopped.as_deref().is_some_and(|why| why.contains("SDK 6 refused")));
        assert_eq!(
            builds,
            [3, 4, 5, 6],
            "as it stands, then SDK 4, 5 and 6, each against its own SDK"
        );
    }

    /// **A pin this vike retired is what migrate is for**: the file cannot be built as it stands, so
    /// the walk starts unchecked, a retired step is applied unchecked, and the first SDK that builds
    /// here checks them all.
    #[test]
    fn a_retired_pin_and_a_retired_step_are_carried_to_the_first_sdk_that_builds() {
        let retired_below_five =
            |sdk: u32| if sdk < 5 { Err(format!("SDK {sdk} is retired")) } else { Ok(()) };
        let mut built = Vec::new();
        let migration =
            walk(&file_on(3), (3, 6), &retired_below_five, &mut tables, &mut |text, _| {
                built.push(text.to_string());
                Ok(())
            })
            .expect("walked");
        assert_eq!(migration.reached, 6);
        assert_eq!(migration.text, file_on(6).replace("OldMean", "NewMean"));
        assert_eq!(migration.stopped, None);
        assert_eq!(built.len(), 2, "SDK 5 and 6 only: 3 and 4 do not build here");
        assert!(built[0].contains("NewMean"), "SDK 4's rename reached the first build");

        // ...and when the first SDK that builds refuses the text, nothing was checked: no step kept.
        let migration = walk(&file_on(3), (3, 5), &retired_below_five, &mut tables, &mut |_, _| {
            Err("error: no".to_string())
        })
        .expect("walked");
        assert_eq!((migration.reached, migration.text), (3, file_on(3)));
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.rs");
        std::fs::write(&file, file_on(3)).expect("plant");
        let migration =
            walk(&file_on(3), (3, 4), &every, &mut tables, &mut |_, _| Ok(())).expect("walked");
        finish(&file, &file_on(3), &migration, "SDK 4", true).expect("finished");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), file_on(3));
        let names: Vec<_> = std::fs::read_dir(dir.path()).expect("list").flatten().collect();
        assert_eq!(names.len(), 1, "no backup, no temporary file: {names:?}");
    }

    #[test]
    fn a_second_run_never_overwrites_a_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.rs");
        std::fs::write(&file, file_on(3)).expect("plant");
        for (from, to) in [(3, 4), (4, 5)] {
            let text = std::fs::read_to_string(&file).expect("read");
            let migration =
                walk(&text, (from, to), &every, &mut tables, &mut |_, _| Ok(())).expect("walked");
            finish(&file, &text, &migration, "SDK 5", false).expect("finished");
        }
        let backup = |n: u32| std::fs::read_to_string(dir.path().join(format!("s.rs.bak.{n}")));
        assert_eq!(backup(1).expect("first backup"), file_on(3), "the first run's original");
        assert_eq!(
            backup(2).expect("second backup"),
            file_on(4).replace("OldMean", "NewMean"),
            "the second run's original, beside the first and not over it"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            file_on(5).replace("OldMean", "NewMean")
        );
        // A backup made by hand under the next free name is skipped, never replaced.
        std::fs::write(dir.path().join("s.rs.bak.3"), "mine").expect("plant");
        assert_eq!(keep_backup(&file, "x").expect("kept"), dir.path().join("s.rs.bak.4"));
        assert_eq!(backup(3).expect("kept"), "mine");
    }

    /// A project holding SDK 4 whose rename table (the step from SDK 3) carries `rhai_rows`, and a
    /// script `s.rhai` holding `script`. The marker only has to exist for the SDK to be installed.
    fn rhai_project(rhai_rows: &str, script: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = dir.path().join(vike_rust_strategy::sdk::SDK_SOURCE_DIR).join("sdk-4");
        std::fs::create_dir_all(&snapshot).expect("snapshot");
        std::fs::write(snapshot.join(vike_rust_strategy::sdk::SDK_MARKER_FILE), "")
            .expect("marker");
        let table = format!("vike-sdk-rename-table\t1\nfrom\tsdk-3\nto\tsdk-4\n{rhai_rows}");
        let table_file = vike_rust_strategy::sdk::rename_table::RENAME_TABLE_FILE;
        std::fs::write(snapshot.join(table_file), table).expect("table");
        let file = dir.path().join("s.rhai");
        std::fs::write(&file, script).expect("script");
        (dir, file)
    }

    /// `migrate` of the `.rhai` file `file` in the project at `root`, no flags.
    fn migrate_rhai(root: &Path, file: &Path) -> CommandResult<()> {
        let ctx = Context { project_root: Some(root), configured_address: None, keys: None };
        let cmd = MigrateCommand { file: file.to_path_buf(), to: None, dry_run: false, addr: None };
        run(&cmd, &ctx)
    }

    /// The script the Rhai cases start from: a renamed function, a renamed indicator (bare and per
    /// line), and `close`, which this vike binds today.
    const SCRIPT: &str = "fn on_bar(strategy) {\n    let s = old_signal(1);\n    let k = oldosc_k(14) + oldosc(14);\n    if close() > 0.0 { }\n}\n";

    /// **A Rhai rename and a removed item with a replacement, then a second run that changes
    /// nothing**: the idempotence rule (an old name is rewritten only while it resolves to nothing)
    /// is what lets every table be applied again; a row whose old name this vike binds is left alone.
    #[test]
    fn a_rhai_script_takes_its_renames_once_and_a_second_run_changes_nothing() {
        let rows = "rhai\tfn\told_signal\tnew_signal\nrhai\tindicator\toldosc\tnew_osc\nrhai\tfn\tclose\tflatten\n";
        let (dir, file) = rhai_project(rows, SCRIPT);
        migrate_rhai(dir.path(), &file).expect("migrated");
        let migrated = SCRIPT
            .replace("old_signal", "new_signal")
            .replace("oldosc_k", "new_osc_k")
            .replace("oldosc(", "new_osc(");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), migrated);
        assert!(migrated.contains("close()"), "a name this vike binds is never renamed away");
        let backup = dir.path().join("s.rhai.bak.1");
        assert_eq!(std::fs::read_to_string(&backup).expect("backup"), SCRIPT);

        migrate_rhai(dir.path(), &file).expect("migrated again");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), migrated, "the second run");
        assert!(!dir.path().join("s.rhai.bak.2").exists(), "nothing changed, so nothing backed up");
    }

    /// **The edits leave a script that does not parse**: refused in words, the file and the folder
    /// untouched.
    #[test]
    fn a_rhai_script_the_renames_would_break_is_refused_and_left_untouched() {
        let (dir, file) = rhai_project("rhai\tfn\told_signal\tlet\n", SCRIPT);
        let refused = migrate_rhai(dir.path(), &file).expect_err("refused");
        assert_eq!(refused.exit, crate::exit::Exit::Refused);
        assert!(refused.message.contains("does not parse"), "{}", refused.message);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), SCRIPT);
        assert!(!dir.path().join("s.rhai.bak.1").exists(), "no backup either");
    }

    /// **A property row never rewrites the script's own field**: `get size` renamed to `quantity`
    /// leaves the user's map field `state.size` exactly as it was (the walk prints a hint naming the
    /// file and the line instead), while a function row in the same table is still applied.
    #[test]
    fn a_user_map_field_named_like_a_renamed_property_stays_untouched() {
        let script = "fn on_bar(strategy) {\n    let state = #{ size: 1 };\n    let n = state.size + old_signal(1);\n}\n";
        let rows = "rhai\tget\tsize\tquantity\nrhai\tfn\told_signal\tnew_signal\n";
        let (dir, file) = rhai_project(rows, script);
        migrate_rhai(dir.path(), &file).expect("migrated");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            script.replace("old_signal", "new_signal"),
            "the field stays `size`; only the function moved"
        );
        let hints = rhai_edits(
            script,
            &[RhaiRename { kind: "get".into(), old: "size".into(), new: Some("quantity".into()) }],
            &|_| false,
        )
        .hints;
        assert!(hints.len() == 1 && hints[0].starts_with("line 3: `size`"), "{hints:?}");
    }

    /// An unpinned file whose state hooks are `save` and `load`, in the JSON signature of before
    /// `ABI_VERSION` 15.
    fn unpinned_with(save: &str, load: &str) -> String {
        format!(
            "//! A strategy.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! ```\n\nimpl<B: OrderBroker> Strategy<B> for S {{\n    fn save_state(&self) -> Option<serde_json::Value> {{\n        {save}\n    }}\n    fn load_state(&mut self, state: &serde_json::Value) {{\n        {load}\n    }}\n}}\n"
        )
    }

    /// The shipped makers' shapes.
    const SAVE: &str = "serde_json::to_value(&self.n).ok()";
    const LOAD: &str = "if let Ok(n) = serde_json::from_value(state.clone()) { self.n = n; }";

    /// `migrate` of the unpinned `.rs` file `file` with `--to` `to`: no project, no daemon (a refusal
    /// comes before either is reached).
    fn migrate_unpinned(file: &Path, to: Option<u32>) -> CommandResult<()> {
        let ctx = Context { project_root: None, configured_address: None, keys: None };
        let cmd = MigrateCommand { file: file.to_path_buf(), to, dry_run: false, addr: None };
        run(&cmd, &ctx)
    }

    /// **An unpinned file on the JSON state signature is carried onto `StrategyState`** (its build is
    /// `crates/vike-strategy-builder/tests/build_errors.rs`'s lane test), and the result, run again,
    /// is refused with nothing to carry: the second run changes nothing.
    #[test]
    fn an_unpinned_files_json_state_is_carried_once_and_a_second_run_changes_nothing() {
        let old = unpinned_with(SAVE, LOAD);
        let carried = unpinned("s.rs", &old, &state_edits(&old), None).expect("carried");
        let expected = old
            .replace("Option<serde_json::Value>", "Option<vike_model::StrategyState>")
            .replace(SAVE, "vike_model::StrategyState::of(&self.n).ok()")
            .replace("&serde_json::Value", "&vike_model::StrategyState")
            .replace("serde_json::from_value(state.clone())", "state.read()");
        assert_eq!(carried, expected);
        let again = unpinned("s.rs", &carried, &state_edits(&carried), None).expect_err("refused");
        assert!(
            again.message.contains("already builds against the newest one"),
            "{}",
            again.message
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.rs");
        std::fs::write(&file, &carried).expect("plant");
        let refused = migrate_unpinned(&file, None).expect_err("nothing to carry");
        assert_eq!(refused.exit, crate::exit::Exit::Refused);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), carried);
        let refused = unpinned("s.rs", &old, &state_edits(&old), Some(4)).expect_err("refused");
        assert!(refused.message.contains("run migrate without it"), "{}", refused.message);
    }

    /// **A body of no shape the state rule rewrites is left unchanged**, the refusal naming the hints
    /// (which name its line): no backup, nothing written.
    #[test]
    fn an_unpinned_file_whose_state_body_is_unrecognized_is_left_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.rs");
        let old = unpinned_with(SAVE, "self.n = state[\"n\"].as_u64().unwrap_or(0);");
        std::fs::write(&file, &old).expect("plant");
        let hints = state_edits(&old).hints;
        assert!(hints.len() == 1 && hints[0].starts_with("line 12: `load_state`"), "{hints:?}");
        let refused = migrate_unpinned(&file, None).expect_err("refused");
        assert_eq!(refused.exit, crate::exit::Exit::Refused);
        assert!(refused.message.contains("no shape migrate rewrites"), "{}", refused.message);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), old, "unchanged");
        assert!(!dir.path().join("s.rs.bak.1").exists(), "no backup");
    }

    /// **The symbol rule follows the pin's rows**: an SDK whose lent-field golden is this host's (`Symbol`
    /// rows) does not take it; one whose golden says `String`, or that is not installed here, does.
    #[test]
    fn the_symbol_rule_applies_to_a_pin_whose_rows_held_strings() {
        use vike_rust_strategy::sdk::{
            LENT_FIELDS, LENT_FIELDS_FILE, RUST_STRATEGY_CRATE_DIR, snapshot_dir,
        };
        let root = tempfile::tempdir().expect("tempdir");
        let plant = |sdk: u32, golden: &str| {
            let path =
                snapshot_dir(root.path(), sdk).join(RUST_STRATEGY_CRATE_DIR).join(LENT_FIELDS_FILE);
            std::fs::create_dir_all(path.parent().expect("its folder")).expect("the folder");
            std::fs::write(path, golden).expect("plant");
        };
        plant(2, LENT_FIELDS);
        plant(1, &LENT_FIELDS.replace("vike_marketdata::symbol::Symbol", "alloc::string::String"));
        assert!(rows_hold_symbols(root.path(), 2), "this host's rows");
        assert!(!rows_hold_symbols(root.path(), 1), "String rows");
        assert!(!rows_hold_symbols(root.path(), 7), "not installed here");
    }

    /// **A save made while the walk was building is never lost**: the file no longer holds what
    /// was read, so nothing is written and no backup is made; the save stands.
    #[test]
    fn a_file_changed_during_the_walk_is_refused_and_keeps_the_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.rs");
        std::fs::write(&file, file_on(3)).expect("plant");
        let saved = format!("{}// saved from Studio during the build\n", file_on(3));
        let migration = walk(&file_on(3), (3, 4), &every, &mut tables, &mut |_, _| {
            std::fs::write(&file, &saved).map_err(|e| e.to_string())
        })
        .expect("walked");
        let refused = finish(&file, &file_on(3), &migration, "SDK 4", false).expect_err("refused");
        assert_eq!(refused.exit, crate::exit::Exit::Refused);
        assert!(refused.message.contains("changed while migrating; nothing was written"));
        assert_eq!(std::fs::read_to_string(&file).expect("read"), saved, "the save stands");
        assert!(!dir.path().join("s.rs.bak.1").exists(), "no backup of the stale text");
    }
}
