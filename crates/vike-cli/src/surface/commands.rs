//! `cli.json`'s `commands` array: EVERY verb of `vike-cli`, walked off its `clap` tree, so a flag
//! cannot be declared and not published. No row here is written by hand.
//!
//! A verb reads its own argv tail as its own `clap` struct (decision 0155 keeps "first token = verb"
//! in the router), so the binary has no one command to walk. [`whole_command`] assembles it: the root
//! command with each verb's unit row replaced by that verb's own command.
//!
//! ⚠ A row lists what `clap` READS. A plane with ONE flag set shared by its sub-verbs (`backtest`'s
//! reading verbs, `data history`, `secrets`, `backend`) refuses a foreign flag by name after the parse, so
//! the row of such a sub-verb also lists flags it then refuses; for `backtest`, `flags[].applies_to`
//! says which sub-verb takes which.
//!
//! # `evidence`
//!
//! Every argument's citation is generated as `file's Struct::field`. `clap` knows the field (the
//! argument's id) and the struct (the id of the `ArgGroup` the derive adds, [`owner_of`]) but not the
//! file, so [`HOMES`] carries ONE row per struct, never one per flag. A flag added to a struct is
//! cited with no edit here; a struct or a verb added gets its row, and `rows` panics until it has
//! one. `surface_tests.rs`'s `every_generated_citation_resolves` reads each file: a struct moved or
//! renamed fails there, and a renamed field cannot go stale, since the id IS the field's name.

use serde_json::{Value, json};
use vike_utility::cli::{FlagRow, commands, describe};

use crate::Root;
use crate::cmd::args;

/// Where each `clap` struct lives: the command words it sits under, the file, the symbol. A name may
/// repeat across planes (`Cli`, `Flags`, `LsCommand`), so a row is found by the words AND the struct.
///
/// `Enum::Variant` is a variant carrying its own fields; `::augment_args` is the one visible argument
/// set built by hand, from a key table, so it has no field to cite.
pub(super) const HOMES: &[(&str, &str, &str)] = &[
    ("access", "crates/vike-cli/src/cmd/access.rs", "Args::Add"),
    ("access", "crates/vike-cli/src/cmd/access.rs", "Args::Role"),
    ("access", "crates/vike-cli/src/cmd/access.rs", "Args::Revoke"),
    ("access", "crates/vike-cli/src/cmd/access.rs", "Args::Invite"),
    ("access", "crates/vike-cli/src/cmd/access.rs", "Args::Swap"),
    ("account", "crates/vike-cli/src/cmd/secrets/grammar.rs", "Flags"),
    ("backend", "crates/vike-cli/src/cmd/backend.rs", "Flags"),
    ("backtest", "crates/vike-cli/src/cmd/backtest/read.rs", "ReadCommand"),
    ("backtest", "crates/vike-cli/src/cmd/backtest/run_args.rs", "RunCommand"),
    ("completions", "crates/vike-cli/src/cmd/completions.rs", "Args"),
    ("config", "crates/vike-cli/src/cmd/config.rs", "ShowCommand"),
    ("config", "crates/vike-cli/src/cmd/config/profile/activate.rs", "ActivateCommand"),
    ("config", "crates/vike-cli/src/cmd/config/profile/activate.rs", "DeactivateCommand"),
    ("config", "crates/vike-cli/src/cmd/config/profile/create/daemon.rs", "BootstrapDaemonCommand"),
    (
        "config",
        "crates/vike-cli/src/cmd/config/profile/create/recorder.rs",
        "BootstrapRecorderCommand",
    ),
    (
        "config profile create run",
        "crates/vike-cli/src/cmd/config/profile/create/run.rs",
        "BootstrapRunCommand::augment_args",
    ),
    ("config", "crates/vike-cli/src/cmd/config/check.rs", "CheckCommand"),
    (
        "config",
        "crates/vike-cli/src/cmd/config/delete_operator_instrument.rs",
        "DeleteOperatorInstrumentCommand",
    ),
    ("config", "crates/vike-cli/src/cmd/config/profile/rm.rs", "DeleteProfileCommand"),
    (
        "config ibkr-cash-settled-roots",
        "crates/vike-cli/src/cmd/config/ibkr_cash_settled_roots.rs",
        "RootArg",
    ),
    ("config", "crates/vike-cli/src/cmd/config/profile/ls.rs", "RecorderCommand"),
    ("config", "crates/vike-cli/src/cmd/config/rename_symbol.rs", "RenameSymbolCommand"),
    ("config", "crates/vike-cli/src/cmd/config/set.rs", "SetCommand"),
    (
        "config",
        "crates/vike-cli/src/cmd/config/profile/set_mount_max_leverage.rs",
        "SetMountMaxLeverageCommand",
    ),
    ("config", "crates/vike-cli/src/cmd/config/profile/set_mount_param.rs", "SetMountParamCommand"),
    ("config", "crates/vike-cli/src/cmd/config/unset.rs", "UnsetCommand"),
    ("config url", "crates/vike-cli/src/cmd/config/url.rs", "LsCommand"),
    ("config url", "crates/vike-cli/src/cmd/config/url.rs", "ProbesCommand"),
    ("config url", "crates/vike-cli/src/cmd/config/url.rs", "SetCommand"),
    ("config url", "crates/vike-cli/src/cmd/config/url.rs", "UnsetCommand"),
    ("data", "crates/vike-cli/src/cmd/data/catalog.rs", "CatalogFlags"),
    ("data", "crates/vike-cli/src/cmd/data/hist.rs", "HistFlags"),
    ("data", "crates/vike-cli/src/cmd/data/instruments.rs", "AddFlags"),
    ("data", "crates/vike-cli/src/cmd/data/instruments.rs", "ChangeFlags"),
    ("data", "crates/vike-cli/src/cmd/data/instruments.rs", "LsFlags"),
    ("data", "crates/vike-cli/src/cmd/data/realtime.rs", "RealtimeFlags"),
    ("data", "crates/vike-cli/src/cmd/data/realtime/live_tokens.rs", "LiveTokensFlags"),
    ("data", "crates/vike-cli/src/cmd/data/record.rs", "RecordFlags"),
    ("data", "crates/vike-cli/src/cmd/data/source.rs", "SourceFlags"),
    ("data", "crates/vike-cli/src/cmd/data/splits.rs", "SplitsAddFlags"),
    ("data", "crates/vike-cli/src/cmd/data/splits.rs", "SplitsLsFlags"),
    ("data", "crates/vike-cli/src/cmd/data/splits.rs", "SplitsRenameFlags"),
    ("data", "crates/vike-cli/src/cmd/data/splits.rs", "SplitsSeriesFlags"),
    ("data", "crates/vike-cli/src/cmd/data/splits.rs", "SplitsWarningsFlags"),
    ("gatewayhub", "crates/vike-cli/src/cmd/gatewayhub.rs", "Args::Setup"),
    ("indicators", "crates/vike-cli/src/cmd/indicators.rs", "Args"),
    ("init", "crates/vike-cli/src/cmd/init.rs", "Args"),
    ("report", "crates/vike-cli/src/cmd/report.rs", "Cli"),
    ("research", "crates/vike-cli/src/cmd/research/study.rs", "StudyCommand"),
    ("secrets", "crates/vike-cli/src/cmd/secrets/grammar.rs", "Flags"),
    ("strategy", "crates/vike-cli/src/cmd/strategy/migrate.rs", "MigrateCommand"),
    ("strategy", "crates/vike-cli/src/cmd/strategy/sdk_diff.rs", "SdkDiffCommand"),
    ("surface", "crates/vike-cli/src/cmd/surface.rs", "Args"),
    ("trade", "crates/vike-cli/src/cmd/trade.rs", "Cli"),
    ("trade account", "crates/vike-cli/src/cmd/trade/account.rs", "RemoveSymbolCommand"),
    ("trade account", "crates/vike-cli/src/cmd/trade/account.rs", "StateCommand"),
    ("trade", "crates/vike-cli/src/cmd/trade/health.rs", "HealthCommand"),
    ("trade", "crates/vike-cli/src/cmd/trade/links.rs", "LinksCommand"),
    ("trade", "crates/vike-cli/src/cmd/trade/oneshot.rs", "WrapperArgs"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order.rs", "BracketCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/combo.rs", "ComboCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/decode.rs", "DecodeCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/list.rs", "OcoCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/list.rs", "OtoCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/list.rs", "LadderCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order.rs", "CancelCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order.rs", "MassCancelCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order.rs", "ModifyCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order.rs", "SubmitCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/selector.rs", "LsCommand"),
    ("trade order", "crates/vike-cli/src/cmd/trade/order/stops.rs", "StopsCommand"),
    ("trade position", "crates/vike-cli/src/cmd/trade/position.rs", "CloseAllCommand"),
    ("trade position", "crates/vike-cli/src/cmd/trade/position.rs", "FlattenCommand"),
    ("trade position", "crates/vike-cli/src/cmd/trade/selector.rs", "LsCommand"),
    ("trade reconcile", "crates/vike-cli/src/cmd/trade/recon.rs", "CancelCommand"),
    ("trade reconcile", "crates/vike-cli/src/cmd/trade/recon.rs", "ConfirmCommand"),
    ("trade reconcile", "crates/vike-cli/src/cmd/trade/recon.rs", "LsCommand"),
    ("trade", "crates/vike-cli/src/cmd/trade/status.rs", "StatusCommand"),
    ("trade strategy", "crates/vike-cli/src/cmd/trade/strategy.rs", "LsCommand"),
    ("trade strategy", "crates/vike-cli/src/cmd/trade/strategy.rs", "MountCommand"),
    ("trade strategy", "crates/vike-cli/src/cmd/trade/strategy.rs", "ResetBudgetCommand"),
    ("trade strategy", "crates/vike-cli/src/cmd/trade/strategy.rs", "UnmountCommand"),
    ("trade venue", "crates/vike-cli/src/cmd/trade/venue.rs", "ReadCommand"),
    ("trade venue", "crates/vike-cli/src/cmd/trade/venue.rs", "ResumeCommand"),
    ("trade", "crates/vike-cli/src/cmd/trade/write.rs", "ModeCommand"),
];

/// The [`HOMES`] row of one argument.
///
/// # Panics
/// When no row names its struct: a struct added without a row, which `rows` then cannot cite.
pub(super) fn citation(
    tree: &clap::Command,
    row: &FlagRow,
) -> (&'static str, &'static str, Option<String>) {
    let (command, arg) = declaration(tree, row);
    let owner = owner_of(command, arg);
    let words = row.command.join(" ");
    let under = |scope: &str| words == scope || words.starts_with(&format!("{scope} "));
    // The most specific scope wins: `config url set`'s `SetCommand` is `config url`'s, not `config`'s.
    let home = HOMES
        .iter()
        .filter(|(scope, _, symbol)| match &owner {
            Some(owner) => under(scope) && symbol.rsplit("::").next() == Some(owner.as_str()),
            None => under(scope) && symbol.ends_with("::augment_args"),
        })
        .max_by_key(|(scope, _, _)| scope.len());
    let Some((_, file, symbol)) = home else {
        panic!(
            "`vike-cli {words}` declares `{}` in `{owner:?}`, which no row of HOMES names: add one",
            arg.get_id()
        )
    };
    (file, symbol, owner.map(|_| arg.get_id().to_string()))
}

/// `file's Struct::field` for a derived argument, `file's Struct::augment_args` for a hand-built one.
fn evidence(tree: &clap::Command, row: &FlagRow) -> String {
    match citation(tree, row) {
        (file, symbol, Some(field)) => format!("{file}'s `{symbol}::{field}`"),
        (file, symbol, None) => format!("{file}'s `{symbol}`"),
    }
}

/// The derive struct or enum variant that declares `arg`: the id of the `ArgGroup` the derive adds to a
/// command for it, `None` for an argument built by hand with `clap::Arg::new`.
///
/// ⚠ A struct holding a `#[command(flatten)]` field gets a group with NO members (clap_derive: nested
/// groups are not validated yet), so an argument in no group's list belongs to that memberless group.
fn owner_of(command: &clap::Command, arg: &clap::Arg) -> Option<String> {
    let member = command.get_groups().find(|group| group.get_args().any(|id| id == arg.get_id()));
    let group =
        member.or_else(|| command.get_groups().find(|group| group.get_args().next().is_none()));
    group.map(|group| group.get_id().to_string())
}

/// The whole `vike-cli` command line as one `clap` tree: what the published reference and
/// `examples_tests.rs` walk. Nothing parses a real argv through it.
///
/// The verbs are `crate::dispatch`'s `VERB_TREES`, whose doc says what holds that list whole.
pub(crate) fn whole_command() -> clap::Command {
    crate::dispatch::VERB_TREES
        .into_iter()
        .fold(args::command::<Root>(args::PROGRAM), |root, (verb, tree)| {
            root.mut_subcommand(verb, |_| tree().name(verb))
        })
}

/// One row per command, depth first, the root first. An ARRAY carrying its own path: order-bearing
/// data never relies on a JSON map's key order (`crate::surface`'s module doc).
pub(super) fn rows() -> Vec<Value> {
    let tree = whole_command();
    let every_arg = describe(&tree);
    commands(&tree)
        .into_iter()
        .map(|command| {
            let own: Vec<Value> = every_arg
                .iter()
                .filter(|arg| arg.command == command.path)
                .map(|arg| arg_json(&tree, arg))
                .collect();
            json!({ "path": command.path, "about": command.about, "args": own })
        })
        .collect()
}

/// The `clap` declaration a `describe` row came from: what the published row reads and `describe`, in
/// a crate the latency gate measures, does not carry.
fn declaration<'a>(tree: &'a clap::Command, row: &FlagRow) -> (&'a clap::Command, &'a clap::Arg) {
    let mut command = tree;
    for word in &row.command {
        command = command.find_subcommand(word).expect("`describe` walked this path");
    }
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_long().unwrap_or(arg.get_id().as_str()) == row.name)
        .expect("`describe` read this argument");
    (command, arg)
}

fn arg_json(tree: &clap::Command, arg: &FlagRow) -> Value {
    let (_, declared) = declaration(tree, arg);
    // The derive names a metavar and a `true|false` value set on a bare switch too.
    let value_name =
        declared.get_value_names().filter(|_| arg.takes_value).and_then(|names| names.first());
    let possible_values = if arg.takes_value { arg.possible_values.as_slice() } else { &[] };
    json!({
        "name": arg.name,
        "long": (!arg.positional).then(|| format!("--{}", arg.name)),
        "short": arg.short.map(String::from),
        "positional": arg.positional,
        "value": if arg.takes_value { "required" } else { "none" },
        "value_name": value_name.map(ToString::to_string),
        "required": arg.required,
        "repeatable": arg.repeatable,
        "default": arg.default,
        "possible_values": possible_values,
        "help": arg.help,
        "evidence": evidence(tree, arg),
    })
}
