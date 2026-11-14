//! `vike-cli config ibkr-cash-settled-roots list|add|remove|seed` — **the futures roots on which IBKR admits a dated
//! future** (`ibkr_cash_settled_root`, `vike_secrets::ibkr_cash_settled_root`; decision 0234, amended 2026-11-13).
//!
//! IBKR's contract details state no settlement, so which roots settle in cash is the operator's rows. A future on any
//! other root is refused by name; a box whose store has no table admits no future at all.
//!
//! - `list` prints the roots, in the order they were added.
//! - `add ROOT` / `remove ROOT` write ONE row (a root is letters and digits, read upper-case). Before the seed there is
//!   no table, and both refuse, writing nothing.
//! - `seed` creates the table holding the roots the release ships, `vike_secrets::ibkr_cash_settled_root::RELEASE_SEED`,
//!   when it does not exist, and leaves an existing one exactly as it is. The installers run it at every start
//!   (`deploy/docker/entrypoint.sh`'s `start_server`).
//!
//! A change applies when the gatewayhub next reads its settings: at the next tradehub restart. Nothing here is
//! journalled (`vike_secrets::ibkr_cash_settled_root`'s module doc says why).

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::ibkr_cash_settled_root::{
    RELEASE_SEED, add_ibkr_cash_settled_root, read_ibkr_cash_settled_roots,
    remove_ibkr_cash_settled_root, seed_ibkr_cash_settled_roots,
};

use crate::exit::{CliError, CommandResult};

/// The program name the verbs are read under.
const BIN: &str = "vike-cli config ibkr-cash-settled-roots";

/// When a written change is read.
const APPLIES: &str =
    "  it applies when the gatewayhub next reads its settings: the next tradehub restart";

/// The four verbs.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum RootsVerb {
    #[command(about = "Every cash-settled root, in the order it was added")]
    List,
    #[command(about = "Add ONE root (letters and digits); refused before the seed")]
    Add(RootArg),
    #[command(about = "Remove ONE root; refused before the seed")]
    Remove(RootArg),
    #[command(
        about = "Create the table with the release's roots when it does not exist; an existing one is left as it is"
    )]
    Seed,
}

/// `add` and `remove`'s one argument.
#[derive(clap::Args, Debug)]
pub(crate) struct RootArg {
    #[arg(help = "the futures root, as IBKR names it in a contract (`ES`, `MES`)")]
    root: String,
}

/// Entry point `config ibkr-cash-settled-roots` routes to.
pub(crate) fn run(verb: RootsVerb, settings_dir: Option<&Path>) -> ExitCode {
    let (name, result) = match &verb {
        RootsVerb::List => ("list", list(settings_dir)),
        RootsVerb::Add(arg) => ("add", add(&arg.root, settings_dir)),
        RootsVerb::Remove(arg) => ("remove", remove(&arg.root, settings_dir)),
        RootsVerb::Seed => ("seed", seed(settings_dir)),
    };
    match result {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN} {name}: {}", e.message);
            e.exit.into()
        }
    }
}

fn dir_of(settings_dir: Option<&Path>) -> CommandResult<&Path> {
    settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — `cd` into the project, or name one with $VIKE_SETTINGS_DIR",
        )
    })
}

/// The root an operator typed, upper-case, or the refusal: letters and digits only, as a contract name spells it.
fn root_of(typed: &str) -> CommandResult<String> {
    let root = typed.trim().to_ascii_uppercase();
    if root.is_empty() || !root.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(CliError::usage(format!(
            "`{typed}` is not a futures root: a root is letters and digits (`ES`, `M2K`); nothing was written"
        )));
    }
    Ok(root)
}

/// The refusal of a write before the seed.
fn not_seeded() -> CliError {
    CliError::failed(format!(
        "the settings database holds no `ibkr_cash_settled_root` table yet, so nothing was written: `{BIN} seed` \
         creates it with the release's roots ({})",
        RELEASE_SEED.join(", ")
    ))
}

fn list(settings_dir: Option<&Path>) -> CommandResult<Vec<String>> {
    let dir = dir_of(settings_dir)?;
    match read_ibkr_cash_settled_roots(dir).map_err(|e| CliError::failed(e.to_string()))? {
        None => Ok(vec![format!(
            "no `ibkr_cash_settled_root` table yet: no IBKR future is admitted until `{BIN} seed` runs"
        )]),
        Some(roots) => {
            let mut out = vec![format!(
                "{} cash-settled root(s); an IBKR future on any other root is refused",
                roots.len()
            )];
            out.extend(roots.into_iter().map(|r| format!("  {r}")));
            Ok(out)
        }
    }
}

fn add(typed: &str, settings_dir: Option<&Path>) -> CommandResult<Vec<String>> {
    let root = root_of(typed)?;
    let dir = dir_of(settings_dir)?;
    match add_ibkr_cash_settled_root(dir, &root).map_err(|e| CliError::failed(e.to_string()))? {
        None => Err(not_seeded()),
        Some(false) => {
            Ok(vec![format!("{root} is a cash-settled root already; nothing was written")])
        }
        Some(true) => Ok(vec![format!("added {root}"), APPLIES.to_string()]),
    }
}

fn remove(typed: &str, settings_dir: Option<&Path>) -> CommandResult<Vec<String>> {
    let root = root_of(typed)?;
    let dir = dir_of(settings_dir)?;
    match remove_ibkr_cash_settled_root(dir, &root).map_err(|e| CliError::failed(e.to_string()))? {
        None => Err(not_seeded()),
        Some(false) => Ok(vec![format!("{root} is not a cash-settled root; nothing was written")]),
        Some(true) => Ok(vec![format!("removed {root}"), APPLIES.to_string()]),
    }
}

fn seed(settings_dir: Option<&Path>) -> CommandResult<Vec<String>> {
    let dir = dir_of(settings_dir)?;
    if seed_ibkr_cash_settled_roots(dir).map_err(|e| CliError::failed(e.to_string()))? {
        Ok(vec![
            format!(
                "created `ibkr_cash_settled_root` with the release's roots: {}",
                RELEASE_SEED.join(", ")
            ),
            APPLIES.to_string(),
        ])
    } else {
        Ok(vec!["`ibkr_cash_settled_root` is there already and was left as it is".to_string()])
    }
}

#[cfg(test)]
#[path = "ibkr_cash_settled_roots_tests.rs"]
mod tests;
