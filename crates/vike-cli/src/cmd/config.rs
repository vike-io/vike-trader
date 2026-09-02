//! `vike-cli config show` — **settings provenance**: every setting, its EFFECTIVE value, and
//! WHERE that value came from.
//!
//! Phase 0 of the settings-unification design
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`). Settings live in eight
//! mechanisms today with no root and no stated precedence, so "why is this 150 ms?" cannot be
//! answered without grepping source, the credential store, TOML profiles and process env. This
//! command is that answer — and it is deliberately the FIRST phase because it doubles as the
//! regression harness for every later one: as stores move underneath, **this output must stay
//! stable**.
//!
//! Nothing here changes behavior. It reads two catalogs and resolves each against the stores that
//! exist today.
//!
//! # Usage
//!
//! ```text
//! vike-cli config show [--json] [--filter <substr>] [--changed-only] [--section files|env|all]
//! ```
//!
//! - `--section`: which half to print. `files` = the settings TOMLs; `env` = the environment-
//!   variable registry; `all` (the default) = both. It exists because the env half is several
//!   hundred rows on its own, and the question that motivated the files half — *did my
//!   `policy.toml` get read?* — should not require scrolling past them. ⚠ The row COUNT is
//!   deliberately no longer written here: it was, and the credential grid moved it by more than
//!   half again in one PR. [`vike_ops::settings::SETTINGS`] is the authority.
//! - `--filter <substr>`: case-insensitive substring match on the setting NAME/KEY or the reading
//!   CRATE/file (so `--filter reconcile`, `--filter bridges/okx` and `--filter policy` all work).
//! - `--changed-only`: only rows something actually configured — the "what have I set?" view.
//! - `--json`: one OBJECT carrying the settings directory, the per-file status, both row sets and
//!   the loader's warnings. (It was a bare ARRAY of the env rows before the files half existed;
//!   nothing in the tree consumed it.)
//!
//! ⚠ **`show` DISCLOSES; it does not judge.** Its exit code says only whether the loader refused,
//! so nothing here can be put in front of a daemon. That is the sibling verb's job —
//! [`crate::cmd::config_check`] resolves the same directory through the same loader and makes the
//! EXIT CODE the product, which is what a systemd `ExecStartPre=` can act on.
//!
//! # Two halves, because there are two kinds of setting
//!
//! **The files half** — `<project>/settings/{policy,config,preferences,flags}.toml`, via
//! [`vike_config::describe`]. Its provenance is EXACT: an origin is decided by key PRESENCE in a
//! parsed layer, so a `policy.toml` that sets a key to the same number as the code default still
//! reports `policy.toml`. This half is why the command exists in its current shape.
//! [`vike_config::Policy`] implements neither `EnvOverride` nor `CliOverride` — both sealed — so a
//! risk ceiling can ONLY be set in a file, and until this half existed **nothing anywhere printed
//! whether that file had been read**. A typo'd filename or a settings directory resolved from the
//! wrong project produced a silently uncapped node.
//!
//! **The env half** — the [`vike_ops::settings::SETTINGS`] registry, the machine-gated catalog of
//! every environment variable the workspace reads, resolved against the two stores below.
//!
//! # Precedence, and the one thing this command cannot know
//!
//! For the env half: **process env > the credential store > documented default.** The printed
//! `source` is the store that WON under that order, never the highest store that merely holds a
//! key. Presence is what wins, not non-emptiness: an exported `NAME=` overrides the store with an
//! empty string, and this command reports that honestly (`source = env`, empty value) rather than
//! silently falling through.
//!
//! ⚠ **That order is what the STORES support. It is not a promise that any given reader honors
//! it** — most consumers read exactly ONE store, so a `source` word alone asserts a provenance the
//! tool cannot verify. Each row therefore also prints what its reader DOES:
//!
//! | `READS` | derived from | meaning |
//! |---|---|---|
//! | `env` | `Naming::Literal` / `Naming::Konst` | a direct `env::var` — the PROCESS environment |
//! | `caller-map` | `Naming::MapLookup` | a map the caller supplies; WHICH one is the caller's choice, and the registry does not record it |
//! | `unknown` | `Naming::Dynamic` | a computed name; the site is allowlisted, not resolved |
//!
//! and a row whose value came ONLY from the credential store while its reader calls `env::var` is
//! named in a warning under the table. That case is real, and it used to print a flat, confident
//! `dotenv`: `VIKE_TRADEHUB_CONTROL_KEY` sits in the store, `vike-cli`'s own `trade`/`mcp` read it
//! with `std::env::var`, and the command asserted a source that was never consulted.
//!
//! ⚠ [`vike_ops::settings::Setting::layer`] is NOT that signal and must not be read as one — this
//! doc used to say it was. `Layer` is computed from the FILE PATH of the read (`main.rs` ⇒
//! `Binary`, a `src/` file ⇒ `Library`, …), so it records where the read LIVES, not which store it
//! consults: `VIKE_TRADEHUB_CONTROL_KEY` is `Binary` in `vike-app` (reading the credential map) and
//! `Library` in `vike-cli` (reading process env) — the exact inverse of any layer-based rule.
//! `Naming` is the signal, because it is the column that distinguishes a direct `env::var` from a
//! map lookup, which is the actual question.
//!
//! # The `READ` column — the one thing this command must never get wrong
//!
//! Attributing a value to a file and printing its origin is an ASSERTION that the value is in
//! force. When nothing reads it, that assertion is positive confirmation of something false, which
//! is strictly worse than an unimplemented feature — an unimplemented feature has no output
//! claiming it works. A clean-install validation found five keys in exactly that state
//! (`flags.tradehub_control`, `config.tradehub_addr`, `config.log_dir`, `config.state_dir`,
//! `preferences.log_file_level`): all displayed as effective, none read by anything.
//!
//! So every row now carries `READ`, straight from [`vike_config::CONSUMPTION`] — the same table
//! `crates/vike-config/tests/settings_are_consumed.rs` gates by opening the claimed consumer's file
//! and looking for the read. A `NO` is followed, under the table, by what reads the key's
//! ENVIRONMENT VARIABLE instead, so the operator is pointed at the thing that works rather than
//! left with a value that does nothing. `policy.*` keys are exempt: they have their own gate
//! (`policy_is_consumed.rs`) and reporting one as unread would warn about an enforced ceiling.
//!
//! ## …and WHICH binary reads it
//!
//! `yes`/`NO` turned out not to be enough, for the same reason `READ` had to exist at all. A later
//! clean install, on a headless box, found `config.state_dir`, `config.store_root` and
//! `preferences.chart_style` all reporting `READ: yes` while their ONLY reader is `vike-app` — the
//! GUI, which will never execute on a tradehub or recorder host. Setting `config.state_dir` there
//! was measured to do nothing, which is CORRECT behaviour (it is vike-app's strategy-state
//! SIDECAR, not the `settings/state/` root the README names, and `consumed.rs` carries a ⚠ about
//! the `VIKE_STATE_DIR`/`VIKE_STATE_ROOT` collision) — reported as though it had worked. And
//! `state_dir` is the obvious name for the directory an operator is looking for, so it is precisely
//! the row they land on.
//!
//! The cell therefore names the BINARY (`app`, `tradehub`), DERIVED by
//! [`vike_config::Consumer::binary`] from the consuming file the table already records and the
//! gate above already opens — never a second hand-written column. `yes` survives for a LIBRARY
//! consumer, where the read genuinely belongs to every binary that links it and there is no single
//! honest name to print; `--json` carries the same answer as `read_by`, `null` for both `NO` and
//! the library case (which `consumed` disambiguates).
//!
//! # Redaction
//!
//! This output is meant to be pasted into issues, so **a secret's value never enters the program's
//! own data structures**: redaction happens inside `resolve`/`resolve_file_row`, not in either
//! printer, so the `--json` path cannot leak what the tables hide. A secret-shaped row prints
//! `<set>`/`<unset>` and still reports its true `source`. Store keys that match NO registry row —
//! [`unknown_store_keys`], the complement printed under the env table — are disclosed by KEY COUNT
//! only when they are credential-shaped: never a name, never a value (`vike-cli secrets list` is
//! the surface for names). And the files half is redacted by the SAME name shapes applied to each
//! key's leaf segment, so a credential-shaped field added to `Config` tomorrow is redacted by
//! construction rather than by anyone remembering.
//!
//! ⚠ Read that middle rule for what it is — a rule about the COMPLEMENT, not a promise that this
//! command names no credential key. It never was one (a declared credential row has always printed
//! its name with `<set>`/`<unset>`), and the ⚠ below is what that now amounts to in practice.
//!
//! ⚠ **What the default env table now discloses, stated plainly.** The registry declares the whole
//! `{VENUE}_{TIER}_API_*` grid since it became enumerable data (`vike_model::credential_keys`), so
//! this table NAMES every one of those keys and marks each `<set>` or `<unset>` — i.e. pasting the
//! default output into an issue reveals which venues and which tiers the box holds credentials for.
//! That is a real change and it is recorded here rather than left to be discovered. Three things
//! bound it, and they are why the answer was to state it rather than to hide the rows: no VALUE can
//! leak (every one of those names ends in `_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE`, which
//! [`is_secret`] matches, and this file's own `no_settings_row_leaks` is the standing gate, driven
//! off `SETTINGS` so it covers the grid rows automatically); the NAMES are not a new disclosure
//! class, because `vike-cli secrets list` names store keys outright and is documented as the
//! surface that does; and this table was ALREADY disclosing exactly this for whichever grid keys a
//! test fixture happened to spell, so what changed is that an accidental subset became a complete
//! one. **The narrow view for a paste is `--filter`, and it is the only one.** ⚠ `--changed-only`
//! is the WRONG instinct here and reads like the right one: it keeps precisely the rows something
//! actually set (`changed_only_keeps_the_rows_a_store_actually_set` and
//! `changed_only_drops_every_defaulted_row` pin that semantics), so on this table it strips the
//! `<unset>` noise and prints exactly the list of venues and tiers the box holds credentials for —
//! the sensitive half of the disclosure this paragraph exists to record, concentrated. `--filter`
//! narrows by substring and so requires knowing what to exclude, which is a real cost and still the
//! honest answer. (The attribution codes print in the clear, correctly: a builder address or broker
//! tag is public.)
//!
//! # The one thing the two tables cannot show: a key that is in the store and in no registry
//!
//! Both tables are driven by CATALOGS — the settings types and the `SETTINGS` registry — so a store
//! key that matches no row of either is not a `<unset>` row, it is **no row at all**. A typo'd
//! variable name therefore looked EXACTLY like one that was never set, which is the same
//! silent-by-construction failure this whole command exists to remove (found by a clean-install
//! validation: `VIKE_NODE_CONTROL_KEY`, a plausible mis-spelling of `VIKE_TRADEHUB_CONTROL_KEY`,
//! sat in the store and appeared nowhere). [`unknown_store_keys`] is the complement, printed under
//! the env table.
//!
//! ⚠ It is split by NAME SHAPE, and the split is the design, not a convenience. Naming every
//! unmatched key would make this command an enumerator of the credential store, which is the one
//! thing the redaction rule above forbids; counting all of them would be useless, because a real
//! store holds bespoke per-venue credentials (`FXCM_{TIER}_USER`, `DUKASCOPY_DEMO1_LOGIN`,
//! per-account ids) that no registry row matches, so on a real box that count is a large,
//! meaningless number. So: credential-SHAPED unmatched keys are counted and never named, everything
//! else is named outright. See [`unknown_store_keys`] for the full argument — including why this
//! count used to be larger still, back when the `{VENUE}_{TIER}_API_*` grid was undeclared.
//!
//! # The stores are read through the workspace's ONE loader
//!
//! `vike_bridge_core::credentials`' binary-facing secrets reader is what every composition root
//! uses, so this command calls it rather than parsing the file itself — a second parser would drift
//! from the first and nothing would catch it, which is precisely the failure this whole program
//! exists to remove. It also takes `VIKE_SETTINGS_DIR` out of the same env sweep, so this command's
//! store is the store every other surface reads; the previous CWD-walking entry point did not, and
//! could name a directory nothing else used. The DIRECTORY itself comes from the dispatcher, which
//! resolved it once for `secrets` and `trade`.
//!
//! Reaching it used to mean linking the venue transport stack (ureq+rustls, tungstenite, the
//! signers, vike-exec) into a CLI whose entire identity is being light. That was a packaging
//! problem, not a reason to copy code. vike-bridge-core carries a `full` feature (default ON, so
//! every bridge is unchanged) that gates exactly the transport/signing modules, and this crate
//! takes it with `default-features = false` — one definition, no transport stack.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_config::provenance::Description;
// The redaction SHAPES, and the words a redacted row prints — see the Redaction section below
// for why they live one crate down.
use vike_config::redact::{is_secret, REDACTED, SET, UNSET};
// The FILES-half row builder — MOVED to `vike_config::show` (one authority; the tradehub
// `SettingsShow` wire verb consumes the same builder), re-consumed here by the printers.
use vike_config::show::{file_rows, FileRow};
use vike_ops::settings::{Naming, Setting, SETTINGS};

use crate::cmd::args::{self, Flags};
use crate::cmd::config_check::DirOrigin;

/// The `config` verb list. Printed by `config --help` and by the unknown-verb arm, so a verb cannot
/// be added without showing up in help.
const USAGE: &str = "\
usage: vike-cli config <verb> [options]

verbs:
  show    every setting, its effective value, and WHERE that value came from
  check   validate this box's settings tree + credential store; the EXIT CODE is the product

run `vike-cli config <verb> --help` for a verb's own options";

const SHOW_USAGE: &str = "usage: vike-cli config show [--json] [--filter <substr>] \
                          [--changed-only] [--section files|env|all]";

// ---------------------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------------------
//
// ⚠ The SHAPES moved DOWN to `vike_config::redact`; the PROOF stayed here. This command was the
// only settings-disclosure surface when they were written, so they lived beside the printer. It is
// not any more — `vike_config::boot` renders the same rows into a daemon's startup log — and a
// second copy of a SECURITY table is the one duplication this tree cannot afford: a venue added to
// one list and not the other prints a live key into a log file.
//
// What could not move is `no_settings_row_leaks`, the test that runs the shapes over the REAL
// `vike_ops::settings::SETTINGS` registry: vike-config sits far below vike-ops and cannot see it.
// So the shapes have one home and the exhaustiveness gate has another, which is the correct split —
// the table is a definition, the registry sweep is evidence about this workspace.

// `SET` / `UNSET` — the two words a redacted row prints — moved DOWN with the shapes and are
// imported at the top of this file. Same argument: `vike_config::boot` prints the same rows into a
// daemon's startup log, and an operator comparing that block against this table is reading ONE
// fact, so two surfaces spelling the sentinel differently would make one value look like two.

// `REDACTED` — the DEFAULT-column sentinel for a secret row that somehow declares a non-empty
// default — moved down with them (`vike_config::redact::REDACTED`, imported at the top) the day
// the FILES-half row builder became `vike_config::show`; the env half below still uses it.

// ---------------------------------------------------------------------------------------------
// The pure resolver — the ENV half
// ---------------------------------------------------------------------------------------------

/// Which store the effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The documented default from the `SETTINGS` row — nothing configured it.
    Default,
    /// The credential store (`<project>/settings/secrets.env`).
    Dotenv,
    /// The real process environment.
    Env,
}

impl Source {
    /// The stable wire/table spelling. Pinned by test — later phases may move stores, but a tool
    /// parsing `--json` must keep reading the same three words.
    fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Dotenv => "dotenv",
            Source::Env => "env",
        }
    }
}

/// What a row's READER consults — the honest qualifier on [`Source`], derived from
/// [`vike_ops::settings::Setting::naming`] because that is the one column that distinguishes a
/// direct `env::var` from a lookup on a map somebody else supplied. See the module doc for why
/// `Setting::layer` cannot answer this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reads {
    /// A direct `env::var`/`env::var_os` — the PROCESS environment.
    ProcessEnv,
    /// `vars.get(..)` on a caller-supplied map. Which map is the CALLER's choice: the venue
    /// loaders are handed the credential map, `reconcile_config` and the store-root readers the
    /// process-env sweep. The registry does not record which, so neither does this.
    CallerMap,
    /// A computed/parameterised name (`Naming::Dynamic`) — allowlisted, not resolved.
    Unknown,
}

impl Reads {
    fn of(naming: Naming) -> Self {
        match naming {
            // ⚠ When ONE variable is named at BOTH kinds of site, `naming` records the DIRECT read
            // (`vike_ops::settings`' design note, tie-break 2) — so this says "at least one reader
            // calls env::var", never "no reader consults a map".
            Naming::Literal | Naming::Konst(_) => Reads::ProcessEnv,
            Naming::MapLookup => Reads::CallerMap,
            Naming::Dynamic => Reads::Unknown,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Reads::ProcessEnv => "env",
            Reads::CallerMap => "caller-map",
            Reads::Unknown => "unknown",
        }
    }
}

/// One resolved setting, ready to print.
///
/// INVARIANT: when `secret` is true, neither `value` nor `default` contains any byte of the
/// underlying stores — redaction happens here, in the resolver, so no printer can leak it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) name: &'static str,
    pub(crate) krate: &'static str,
    pub(crate) source: Source,
    pub(crate) reads: Reads,
    pub(crate) secret: bool,
    /// The effective value — ALREADY redacted when `secret`.
    pub(crate) value: String,
    /// The row's documented default — ALREADY redacted when `secret`.
    pub(crate) default: String,
}

impl Resolved {
    /// The row's value came ONLY from the credential store, but its reader calls `env::var` — so
    /// the process will not see it unless it is exported.
    ///
    /// REPORTED rather than corrected: a reader CAN have a store fallback (`poly_reconcile_enabled`
    /// is the documented one), and the registry does not record which do. "This may not reach its
    /// reader, here is why" is knowable; "this value is not in effect" is not.
    pub(crate) fn store_may_not_reach_reader(&self) -> bool {
        self.source == Source::Dotenv && self.reads == Reads::ProcessEnv
    }
}

/// Resolve ONE registry row against two caller-supplied maps. Pure: no process env, no filesystem,
/// no globals — which is what makes the precedence and redaction rules testable without the
/// process-global `set_var` races that `SRC_TEST_MODULE_OVERRIDES` exists to work around.
///
/// Precedence: `env` > `dotenv` > `row.default`. PRESENCE wins, not non-emptiness (see the module
/// doc): a key mapped to `""` is a real override and is reported as one.
pub(crate) fn resolve(
    row: &Setting,
    env: &HashMap<String, String>,
    dotenv: &HashMap<String, String>,
) -> Resolved {
    let (raw, source) = match env.get(row.name) {
        Some(v) => (v.as_str(), Source::Env),
        None => match dotenv.get(row.name) {
            Some(v) => (v.as_str(), Source::Dotenv),
            None => (row.default, Source::Default),
        },
    };

    let secret = is_secret(row.name);
    let (value, default) = if secret {
        let shown = if source != Source::Default && !raw.is_empty() { SET } else { UNSET };
        // A credential row's default is `""` today; anything else is redacted rather than printed.
        let default = if row.default.is_empty() { "" } else { REDACTED };
        (shown.to_string(), default.to_string())
    } else {
        (raw.to_string(), row.default.to_string())
    };

    Resolved {
        name: row.name,
        krate: row.krate,
        source,
        reads: Reads::of(row.naming),
        secret,
        value,
        default,
    }
}

/// Resolve the whole registry, apply the view filters, and sort.
///
/// Sorted by `(name, krate)` rather than left in `SETTINGS` array order on purpose: this output is
/// the later phases' regression baseline, and array order is an editing artifact that would churn
/// the diff every time a row is inserted. `SETTINGS` is keyed on `(name, krate)`, so one variable
/// read by several crates with different defaults yields several adjacent rows — that is the table
/// being honest, not a duplicate.
fn resolve_all(
    env: &HashMap<String, String>,
    dotenv: &HashMap<String, String>,
    filter: Option<&str>,
    changed_only: bool,
) -> Vec<Resolved> {
    let needle = filter.map(str::to_ascii_lowercase);
    let mut rows: Vec<Resolved> = SETTINGS
        .iter()
        .filter(|row| match needle.as_deref() {
            None => true,
            Some(n) => {
                row.name.to_ascii_lowercase().contains(n)
                    || row.krate.to_ascii_lowercase().contains(n)
            }
        })
        .map(|row| resolve(row, env, dotenv))
        .filter(|r| !changed_only || r.source != Source::Default)
        .collect();
    rows.sort_by(|a, b| a.name.cmp(b.name).then(a.krate.cmp(b.krate)));
    rows
}

/// The credential-store keys that match **no** `SETTINGS` row — the env table's complement, and the
/// answer to "I put it in the store and nothing anywhere shows it".
///
/// Both halves of this command are driven by catalogs, so an unmatched store key produces no row and
/// is INDISTINGUISHABLE from a key that was never set. That is the defect this type exists to close.
///
/// ⚠ **Split by name SHAPE.** Three options were weighed and only this one holds both properties:
///
/// * *name everything* — makes the command an enumerator of the credential store, exactly what the
///   module doc's redaction rule forbids for a surface meant to be pasted into an issue.
/// * *count everything* — safe, and useless: a real store holds bespoke per-venue credentials
///   (`FXCM_{TIER}_USER`, `DUKASCOPY_DEMO1_LOGIN`, per-account ids) beside its `{VENUE}_{TIER}_API_*`
///   keys, so on any box with live credentials the count is large, constant and buries the one key
///   that matters. ⚠ It used to be larger still, and for a worse reason: the registry declared
///   almost none of the `{VENUE}_{TIER}_API_*` grid at all, because those keys are COMPUTED and
///   appeared as no literal anywhere. That family is enumerable data now
///   (`vike_model::credential_keys`) and fully declared, so a store key of that shape MATCHES a row
///   and is reported in the table above with its true source instead of vanishing into this count.
/// * *split* — a credential-shaped name ([`is_secret`]) is COUNTED, never named; everything else is
///   NAMED. The counted half is where the expected noise lives, and its message says so; the named
///   half is high-signal, because a store key that is neither a known setting nor credential-shaped
///   is almost always a typo or a setting that has been removed.
///
/// The named half discloses strictly less than this command already prints: a non-secret registry
/// row's full VALUE is in the table above it. The counted half keeps the module doc's rule intact.
///
/// ⚠ A key whose typo lands on a credential SHAPE (`VIKE_NODE_CONTROL_KEY` — the case that prompted
/// this) is therefore counted, not named. That is the honest trade: its name shape is
/// indistinguishable from a real node key's, and `vike-cli secrets list` is the surface that names
/// store keys.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UnknownKeys {
    /// Unmatched keys safe to print by name, sorted for a stable diff.
    pub(crate) named: Vec<String>,
    /// Unmatched keys whose NAME is credential-shaped — disclosed by COUNT only.
    pub(crate) credential_shaped: usize,
}

impl UnknownKeys {
    fn is_empty(&self) -> bool {
        self.named.is_empty() && self.credential_shaped == 0
    }
}

/// Build [`UnknownKeys`] from the store map. Pure — no process env, no filesystem — and it reads
/// only the store's KEYS: a value never enters this path at all, so no redaction step can be
/// forgotten here.
///
/// `filter` is the same `--filter` needle the tables use, matched case-insensitively on the key.
fn unknown_store_keys(dotenv: &HashMap<String, String>, filter: Option<&str>) -> UnknownKeys {
    let declared: HashSet<&str> = SETTINGS.iter().map(|s| s.name).collect();
    let needle = filter.map(str::to_ascii_lowercase);
    let mut out = UnknownKeys::default();
    for key in dotenv.keys() {
        if declared.contains(key.as_str()) {
            continue;
        }
        if needle.as_deref().is_some_and(|n| !key.to_ascii_lowercase().contains(n)) {
            continue;
        }
        if is_secret(key) {
            out.credential_shaped += 1;
        } else {
            out.named.push(key.clone());
        }
    }
    out.named.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// The FILES half — MOVED to `vike_config::show` (`FileRow`/`resolve_file_row`/`file_rows`,
// imported at the top; its behaviour tests moved with it). The tradehub node's
// `Request::SettingsShow` arm serves the SAME rows, and a redaction rule with two copies is the
// one duplication a disclosure surface cannot afford — the printers below are all that stayed.
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------------------------

/// Which half to print.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Section {
    /// The settings TOMLs only.
    Files,
    /// The environment-variable registry only.
    Env,
    #[default]
    All,
}

impl Section {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "files" => Ok(Section::Files),
            "env" => Ok(Section::Env),
            "all" => Ok(Section::All),
            other => Err(format!("unknown --section '{other}' (expected files|env|all)")),
        }
    }
    fn files(self) -> bool {
        self != Section::Env
    }
    fn env(self) -> bool {
        self != Section::Files
    }
}

/// The parsed `config show` command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    filter: Option<String>,
    changed_only: bool,
    section: Section,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `config` subcommand — the
/// first of which is the verb (`show` or `check`).
///
/// `settings_dir` is THE settings directory the dispatcher already resolved (`VIKE_SETTINGS_DIR`,
/// else the project walk) — the same value `secrets` and `trade` receive. It arrives as a parameter
/// rather than being re-derived here so this command can never report a different store than the
/// one every other surface reads. `origin` is which of those two rungs answered, an answer only the
/// dispatcher has and one [`crate::cmd::config_check`] needs: a NAMED directory that is not there is
/// a refusal, while a WALKED one that is not there is an unconfigured checkout.
pub fn run(
    mut args: impl Iterator<Item = String>,
    settings_dir: Option<&Path>,
    origin: DirOrigin,
) -> ExitCode {
    let verb = match args.next() {
        Some(v) => v,
        None => {
            eprintln!("vike-cli config: missing verb\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match verb.as_str() {
        "show" => run_show(args, settings_dir),
        "check" => crate::cmd::config_check::run(args, settings_dir, origin),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "vike-cli config: unknown verb '{other}' (expected `show` or `check`)\n{USAGE}"
            );
            ExitCode::FAILURE
        }
    }
}

/// `config show`: parse flags, read the stores, print.
///
/// ⚠ The two help paths of this command are DIFFERENT code: the bare verb (`config --help`, in
/// [`run`] above) always printed its usage to stdout and exited 0, while `config show --help`
/// arrived here as the shared parser's short-circuit and hit the usage-error arm — exit 1, with the
/// internal token on stderr. That is the drift [`args::exit_for_parse_error`] exists to end.
fn run_show(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config show", SHOW_USAGE, &msg),
    };
    match execute(&args, settings_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vike-cli config show: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Hand-rolled arg parser over the shared [`crate::cmd::args`] glue (no `clap` — this crate adds no
/// dependency): `--flag value` and `--flag=value`.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--json" => {
                args::no_value(&flag, inline)?;
                out.json = true;
            }
            "--changed-only" => {
                args::no_value(&flag, inline)?;
                out.changed_only = true;
            }
            "--filter" => out.filter = Some(flags.value(&flag, inline)?),
            "--section" => out.section = Section::parse(&flags.value(&flag, inline)?)?,
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

/// The credential store's own status. Key COUNT only — never a name, never a value.
struct StoreStatus {
    path: Option<PathBuf>,
    present: bool,
    keys: usize,
}

/// Read the stores and print. The process-env read is a `std::env::vars()` SWEEP — it names no
/// variable, so it needs no `SETTINGS` row of its own (see that table's design note); this command
/// is a reader OF the registry, never a new entry in it. The credential half goes through the
/// workspace's own loader, never a local re-parse (see the module doc).
fn execute(args: &Args, settings_dir: Option<&Path>) -> Result<(), String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let dotenv = load_workspace_secrets_from_env(&env);
    let secrets_path = settings_dir.map(|d| d.join(vike_secrets::SECRETS_FILE));
    let secrets = StoreStatus {
        present: secrets_path.as_deref().is_some_and(Path::is_file),
        path: secrets_path,
        keys: dotenv.len(),
    };

    // The files half. A load error is FATAL here on purpose: a broken `policy.toml` reported as
    // "defaults" would be the single most misleading thing this command could print.
    // A disclosure command resolves the SAME layers the daemons do — one loader over one settings
    // directory, and the same refusal of a settings file that has been retired.
    let described = vike_config::describe(settings_dir, &env)
        .map_err(|e| format!("settings could not be loaded: {e}"))?;

    let files = if args.section.files() {
        file_rows(&described, args.filter.as_deref(), args.changed_only)
    } else {
        Vec::new()
    };
    let envs = if args.section.env() {
        resolve_all(&env, &dotenv, args.filter.as_deref(), args.changed_only)
    } else {
        Vec::new()
    };
    // Deliberately NOT gated on `--changed-only`: an unmatched store key is by definition something
    // the operator configured, so the "what have I set?" view is exactly where it belongs.
    let unknown = if args.section.env() {
        unknown_store_keys(&dotenv, args.filter.as_deref())
    } else {
        UnknownKeys::default()
    };

    if args.json {
        print_json(&described, &secrets, &files, &envs, &unknown)
    } else {
        print_human(args.section, &described, &secrets, &files, &envs, &unknown);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Printers
// ---------------------------------------------------------------------------------------------

/// The tooling view: ONE object, so the two halves and the header do not have to be re-derived by
/// whoever parses it.
///
/// `secret` rides along on every row because `value` is otherwise ambiguous to a machine: a tool
/// cannot tell a redaction marker from a knob whose literal value is the string `<set>`. The human
/// tables do not need it — there, `<set>` in a `_API_KEY` row reads as exactly what it is.
fn settings_json(
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
) -> serde_json::Value {
    serde_json::json!({
        "settings_dir": d.settings_dir.as_ref().map(|p| p.display().to_string()),
        "files": d.files.iter().map(|f| serde_json::json!({
            "name": f.name,
            "path": f.path.display().to_string(),
            "present": f.present,
            "keys": f.keys,
        })).collect::<Vec<_>>(),
        "secrets": {
            "path": secrets.path.as_ref().map(|p| p.display().to_string()),
            "present": secrets.present,
            "keys": secrets.keys,
        },
        "warnings": d.settings.warnings,
        "settings": files.iter().map(|r| serde_json::json!({
            "key": r.key,
            "value": r.value,
            "origin": r.origin_kind,
            "origin_detail": r.origin,
            "default": r.default,
            "adjusted": r.adjusted,
            "secret": r.secret,
            // `false` means the value is recorded and applied by NOTHING. A tool reading this
            // should treat an `origin != "default"` row with `consumed: false` as a
            // misconfiguration, not as a setting in force.
            "consumed": r.consumed,
            // WHICH binary reads it, short name. `null` with `consumed: true` means a LIBRARY
            // reads it, so it applies wherever that library is linked — NOT "unknown". An agent
            // deciding whether a setting does anything on THIS box wants this field, not
            // `consumed`: three GUI-only keys reported `consumed: true` on headless daemons.
            "read_by": r.read_by,
            "why_unread": r.why_unread,
        })).collect::<Vec<_>>(),
        "env": envs.iter().map(|r| serde_json::json!({
            "name": r.name,
            "value": r.value,
            "source": r.source.as_str(),
            "reads": r.reads.as_str(),
            "store_may_not_reach_reader": r.store_may_not_reach_reader(),
            "default": r.default,
            "krate": r.krate,
            "secret": r.secret,
        })).collect::<Vec<_>>(),
        // The env table's COMPLEMENT: store keys no registry row covers. `named` carries only the
        // keys `unknown_store_keys` cleared as non-credential-shaped; the rest are a count, so a
        // machine reading this document cannot enumerate the credential store either.
        "unknown_env_keys": {
            "named": unknown.named,
            "credential_shaped": unknown.credential_shaped,
        },
    })
}

/// Serialize [`settings_json`] and print it. Split from the builder so the redaction property can
/// be asserted on the DOCUMENT rather than on stdout.
fn print_json(
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&settings_json(d, secrets, files, envs, unknown))
        .map_err(|e| format!("cannot serialize settings JSON: {e}"))?;
    println!("{text}");
    Ok(())
}

/// An empty cell renders as `-` in the human tables, so an empty override is visibly a value rather
/// than a formatting gap. The `--json` view keeps the raw `""` — a tool must not have to un-invent
/// this placeholder.
fn dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

/// The whole human view: the header (which project am I reading?), then the requested halves.
fn print_human(
    section: Section,
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
) {
    print_header(d, secrets);
    if section.files() {
        println!();
        print_file_table(files, &d.settings.warnings);
    }
    if section.env() {
        println!();
        print_env_table(envs, unknown);
    }
}

/// **Which project am I reading?** — the question behind every other question here, and the one
/// nothing used to answer. Prints the resolved settings directory, then each file's presence and
/// key count, so "my `policy.toml` did nothing" resolves to the word `absent` on a line rather than
/// to a guess.
fn print_header(d: &Description, secrets: &StoreStatus) {
    let Some(dir) = &d.settings_dir else {
        println!(
            "settings directory: NONE — no project above the working directory, and no explicit \
             override."
        );
        println!("  NO settings file was read: every setting below is a compiled-in default.");
        // The credential loader has one more rung than the settings walk — a CWD-RELATIVE
        // `settings/secrets.env` when no project resolves. Saying nothing here would let this
        // command report "no settings directory" while the env half below shows `dotenv` rows,
        // which reads as a contradiction rather than as the two different fallbacks it is.
        if secrets.keys > 0 {
            println!(
                "  ...yet the credential loader found {} key(s): it also tries a \
                 working-directory-relative `{}/{}`.",
                secrets.keys,
                vike_secrets::SETTINGS_DIR,
                vike_secrets::SECRETS_FILE
            );
        }
        return;
    };
    println!("settings directory: {}", dir.display());

    let width = d
        .files
        .iter()
        .map(|f| f.name.len())
        .chain(std::iter::once(vike_secrets::SECRETS_FILE.len()))
        .max()
        .unwrap_or(0);
    for f in &d.files {
        let state = if f.present {
            format!("present, {} key(s) set", f.keys)
        } else {
            "absent".to_string()
        };
        println!("  {:<width$}  {state}", f.name);
    }
    // Key COUNT only — names are `vike-cli secrets list`'s job, values are nobody's.
    let state = if secrets.present {
        format!("present, {} key(s) — names: `vike-cli secrets list`", secrets.keys)
    } else {
        "absent".to_string()
    };
    println!("  {:<width$}  {state}", vike_secrets::SECRETS_FILE);
}

/// The files half: one row per typed setting, its effective value, and the layer that set it.
fn print_file_table(rows: &[FileRow], warnings: &[String]) {
    println!("-- settings files ---------------------------------------------------------");
    // RENDERED from `vike_config::PRECEDENCE`, never typed here. This line named a per-project
    // override file for two months while every composition root passed `None` for the project
    // directory, so the layer was implemented, tested, advertised — and read by nothing. That layer
    // is now REMOVED, which is the second way a hand-written header goes false: it would still be
    // naming the file today. A header derived from the loader's own layer list can only name layers
    // that exist, in both directions, and the two reachability gates
    // (`crates/vike-config/tests/layers_are_reachable.rs`,
    // `crates/vike-cli/tests/settings_layers_reachable.rs`) hold each of them to a proof of effect.
    println!("{}", vike_config::precedence_line());
    println!();
    if rows.is_empty() {
        println!("(no settings matched)");
        return;
    }

    let (mut wk, mut wv, mut wo, mut wd) =
        ("SETTING".len(), "VALUE".len(), "ORIGIN".len(), "DEFAULT".len());
    for r in rows {
        wk = wk.max(r.key.len());
        wv = wv.max(dash(&r.value).len());
        wo = wo.max(r.origin.len() + usize::from(r.adjusted));
        wd = wd.max(dash(&r.default).len());
    }

    println!("{:<wk$}  {:<wv$}  {:<wo$}  {:<wd$}  READ", "SETTING", "VALUE", "ORIGIN", "DEFAULT");
    let mut adjusted = false;
    for r in rows {
        let origin = if r.adjusted {
            adjusted = true;
            format!("{}*", r.origin)
        } else {
            r.origin.clone()
        };
        println!(
            "{:<wk$}  {:<wv$}  {:<wo$}  {:<wd$}  {}",
            r.key,
            dash(&r.value),
            origin,
            dash(&r.default),
            r.read_cell()
        );
    }
    println!();
    let configured = rows.iter().filter(|r| r.origin_kind != "default").count();
    println!("{} setting(s) shown, {configured} configured (origin != default)", rows.len());
    if adjusted {
        println!("* the effective value is not what that layer holds — a later rule moved it:");
    }
    // The loader's non-fatal resolutions — DATA it returns rather than logs, so somebody has to
    // print them, and a settings-disclosure command is exactly that somebody.
    for w in warnings {
        println!("  {w}");
    }
    print_read_scope(rows);
    print_unread(rows);
}

/// The `READ` column's legend — printed only when a shown row names a BINARY, so it costs nothing on
/// a filtered view that has none.
///
/// The column used to be binary in both senses: `yes` or `NO`, saying nothing about WHERE. On a
/// headless tradehub or recorder box, `config.state_dir`, `config.store_root` and
/// `preferences.chart_style` all said `yes` while their only reader is `vike-app` — measured:
/// setting `config.state_dir` on a daemon box did nothing, which is correct behaviour (it is
/// vike-app's strategy-state SIDECAR, not the `settings/state/` root the README names) reported as
/// though it had worked. A column that says "something reads this" is not much use to somebody
/// deciding whether to set it HERE.
fn print_read_scope(rows: &[FileRow]) {
    let mut binaries: Vec<&str> = rows.iter().filter_map(|r| r.read_by).collect();
    binaries.sort_unstable();
    binaries.dedup();
    if binaries.is_empty() {
        return;
    }
    println!();
    println!(
        "READ names the BINARY that reads each setting ({}) — that program and no other. A key \
         read only by `app` does nothing on a headless box, and vice versa; `yes` means a LIBRARY \
         reads it, so every binary linking it does.",
        binaries.join(", ")
    );
}

/// The `READ = NO` follow-up: which of the shown settings nothing reads, loudest first.
///
/// Split in two on purpose, because the two cases are not equally urgent. A key an operator
/// actually CONFIGURED and that nothing reads is the defect this column exists for — they wrote a
/// value, this command told them where it came from, and the program ignores it — so each one gets
/// its own paragraph naming what reads the variable instead. A key nobody configured is merely
/// declared-and-unwired; it gets a count and a pointer, because printing twenty-five paragraphs
/// nobody asked for would bury the one that matters.
fn print_unread(rows: &[FileRow]) {
    let unread: Vec<&FileRow> = rows.iter().filter(|r| !r.consumed).collect();
    if unread.is_empty() {
        return;
    }
    let (set, unset): (Vec<&&FileRow>, Vec<&&FileRow>) =
        unread.iter().partition(|r| r.origin_kind != "default");

    if !set.is_empty() {
        println!();
        println!(
            "⚠ {} setting(s) below are CONFIGURED and read by NOTHING — the value has no effect:",
            set.len()
        );
        for r in &set {
            println!("  {} = {}  (from {})", r.key, dash(&r.value), r.origin);
            if let Some(why) = r.why_unread {
                for line in wrap(why, 92) {
                    println!("      {line}");
                }
            }
        }
    }
    if !unset.is_empty() {
        println!();
        println!(
            "{} further setting(s) are declared but read by nothing yet (none of them configured \
             here); `--json` carries each one's reason.",
            unset.len()
        );
    }
}

/// Greedy word-wrap to `width` columns. Hand-rolled because this crate adds no dependency for a
/// paragraph, and the input is prose from a `&'static str` table, never user data.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The env half: the registry, resolved, plus the honest READS qualifier and its diagnosis.
fn print_env_table(rows: &[Resolved], unknown: &UnknownKeys) {
    println!("-- environment variables --------------------------------------------------");
    println!("precedence: env > secrets.env > default   (READS = what the reader consults)");
    println!();
    if rows.is_empty() {
        println!("(no settings matched)");
        // …but a store key matching NO row is exactly the case a filter with no matching row can
        // still have found, so the complement is printed on this path too.
        print_unknown_store_keys(unknown);
        return;
    }

    let (mut wn, mut wv, mut wd, mut wk) =
        ("NAME".len(), "VALUE".len(), "DEFAULT".len(), "CRATE".len());
    for r in rows {
        wn = wn.max(r.name.len());
        wv = wv.max(dash(&r.value).len());
        wd = wd.max(dash(&r.default).len());
        wk = wk.max(r.krate.len());
    }
    // `dotenv` is the longest source word; `caller-map` the longest reads word.
    let ws = "dotenv".len().max("SOURCE".len());
    let wr = "caller-map".len().max("READS".len());

    println!(
        "{:<wn$}  {:<wv$}  {:<ws$}  {:<wr$}  {:<wd$}  {:<wk$}",
        "NAME", "VALUE", "SOURCE", "READS", "DEFAULT", "CRATE"
    );
    for r in rows {
        println!(
            "{:<wn$}  {:<wv$}  {:<ws$}  {:<wr$}  {:<wd$}  {:<wk$}",
            r.name,
            dash(&r.value),
            r.source.as_str(),
            r.reads.as_str(),
            dash(&r.default),
            r.krate
        );
    }
    println!();
    let changed = rows.iter().filter(|r| r.source != Source::Default).count();
    println!("{} setting(s) shown, {changed} configured (source != default)", rows.len());

    // The diagnosis the flat SOURCE word used to hide. Named rows, not a footnote: this is the
    // failure that presents as "the daemon ignores my key".
    let stranded: Vec<&Resolved> = rows.iter().filter(|r| r.store_may_not_reach_reader()).collect();
    if !stranded.is_empty() {
        println!();
        println!(
            "! {} row(s) take their value from secrets.env, but that crate reads the variable \
             with a direct `env::var`:",
            stranded.len()
        );
        for r in &stranded {
            println!("    {} ({})", r.name, r.krate);
        }
        println!(
            "  export them, or confirm that reader also falls back to the store — when one \
             variable is read"
        );
        println!(
            "  both ways the registry records only the DIRECT read, so this cannot tell the two \
             apart."
        );
    }

    print_unknown_store_keys(unknown);
}

/// The env table's COMPLEMENT: keys the store holds that no registry row covers. Silent when there
/// are none, so a block here always means something is genuinely unaccounted for.
///
/// See [`UnknownKeys`] for why the named/counted split is what it is.
fn print_unknown_store_keys(unknown: &UnknownKeys) {
    if unknown.is_empty() {
        return;
    }
    println!();
    println!(
        "! {}/secrets.env holds key(s) that match NO row above — nothing else in this tool would",
        vike_secrets::SETTINGS_DIR
    );
    println!("  show them, so a MIS-SPELLED variable name looks exactly like one you never set:");
    for key in &unknown.named {
        println!("    {key}");
    }
    if unknown.credential_shaped > 0 {
        println!(
            "    (+{} credential-shaped name(s), counted not named — this output is meant to be \
             pasted",
            unknown.credential_shaped
        );
        println!(
            "     into an issue. Most venue credential names are read through COMPUTED keys and \
             have no"
        );
        println!(
            "     registry row at all, so a non-zero count here is NORMAL and does not by itself \
             mean a"
        );
        println!("     typo. `vike-cli secrets list` names them.)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_config::redact::is_secret_key;
    use vike_ops::settings::{Layer, Scope};

    // NOTE for anyone adding a fixture here: a REALISTIC name is fine, and preferred where the case
    // under test is about a specific real variable. This block used to demand invented ones
    // (`ACME_*`, `THING_*`): the settings-registry gate's literal sweep
    // (`vike_ops::scan::find_map_lookups`, driven by `crates/vike-ops/tests/settings_registry.rs`)
    // harvested EVERY env-shaped string literal carrying a known prefix and demanded a matching
    // `SETTINGS` row from the containing crate, so realistic fixture data failed that gate for a
    // variable this crate does not read. Its `read_evidence_literals` no longer counts a literal in
    // a test region as a read, and THIS file's trailing `#[cfg(test)]` block is a test region
    // because the file is listed in that gate's `SRC_TEST_MODULE_OVERRIDES`.
    //
    // Two things still bite, and neither is the gate being fussy:
    //   - a direct `std::env::var(..)` here IS a read, gate or no gate, and still needs a row;
    //   - a literal ABOVE this `#[cfg(test)]` attribute is library code and is swept normally.
    // Names with no prefix at all (`ACME_*`) are invisible to the sweep either way, and remain the
    // right choice for a fixture whose point is the NAME SHAPE rather than a particular variable.

    fn row(name: &'static str, default: &'static str) -> Setting {
        Setting {
            name,
            krate: "vike-cli",
            scope: Scope::Vike,
            layer: Layer::Binary,
            naming: Naming::Literal,
            default,
        }
    }

    /// The same fixture read through a caller-supplied MAP rather than `env::var`.
    fn injected_row(name: &'static str, default: &'static str) -> Setting {
        Setting { naming: Naming::MapLookup, ..row(name, default) }
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // -- precedence ----------------------------------------------------------------------------

    #[test]
    fn an_unset_setting_reports_its_documented_default() {
        let r = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[]));
        assert_eq!(r.source, Source::Default);
        assert_eq!(r.value, "127.0.0.1");
        assert_eq!(r.default, "127.0.0.1");
        assert!(!r.secret);
    }

    #[test]
    fn the_dotenv_beats_the_default() {
        let r = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[("ACME_HOST", "the CI box")]));
        assert_eq!(r.source, Source::Dotenv);
        assert_eq!(r.value, "the CI box");
        // the DEFAULT column keeps reporting the documented default, not the winning value
        assert_eq!(r.default, "127.0.0.1");
    }

    #[test]
    fn the_process_env_beats_both() {
        let r = resolve(
            &row("ACME_HOST", "127.0.0.1"),
            &map(&[("ACME_HOST", "from-env")]),
            &map(&[("ACME_HOST", "from-dotenv")]),
        );
        assert_eq!(r.source, Source::Env);
        assert_eq!(r.value, "from-env");
    }

    /// PRESENCE wins, not non-emptiness: an exported `NAME=` is a real override and must not
    /// silently fall through to the store or the default, because that is exactly the confusion
    /// this command exists to end.
    #[test]
    fn an_empty_override_is_still_an_override() {
        let r = resolve(
            &row("ACME_HOST", "127.0.0.1"),
            &map(&[("ACME_HOST", "")]),
            &map(&[("ACME_HOST", "from-dotenv")]),
        );
        assert_eq!(r.source, Source::Env);
        assert_eq!(r.value, "");
    }

    #[test]
    fn the_source_words_are_pinned() {
        assert_eq!(Source::Default.as_str(), "default");
        assert_eq!(Source::Dotenv.as_str(), "dotenv");
        assert_eq!(Source::Env.as_str(), "env");
    }

    // -- the READS qualifier -------------------------------------------------------------------

    #[test]
    fn the_reads_words_are_pinned_and_come_from_naming() {
        assert_eq!(Reads::of(Naming::Literal), Reads::ProcessEnv);
        assert_eq!(Reads::of(Naming::Konst("ACME_ENV")), Reads::ProcessEnv);
        assert_eq!(Reads::of(Naming::MapLookup), Reads::CallerMap);
        assert_eq!(Reads::of(Naming::Dynamic), Reads::Unknown);

        assert_eq!(Reads::ProcessEnv.as_str(), "env");
        assert_eq!(Reads::CallerMap.as_str(), "caller-map");
        assert_eq!(Reads::Unknown.as_str(), "unknown");
    }

    /// **The provenance fix.** A key that exists ONLY in the credential store, read by a crate that
    /// calls `env::var`, is flagged — the real `VIKE_TRADEHUB_CONTROL_KEY` / `vike-cli` shape,
    /// which used to print a flat, confident `dotenv` and assert a source never consulted.
    #[test]
    fn a_store_only_value_read_with_env_var_is_flagged() {
        let r =
            resolve(&row("ACME_CONTROL_KEY", ""), &map(&[]), &map(&[("ACME_CONTROL_KEY", "k")]));
        assert_eq!(r.source, Source::Dotenv, "the store DOES hold it — that stays true");
        assert_eq!(r.reads, Reads::ProcessEnv);
        assert!(r.store_may_not_reach_reader());
    }

    /// ...and the three shapes that are NOT that failure stay unflagged: a map-reading row (the
    /// caller may well hand it the store), an exported value, and an unset one.
    #[test]
    fn the_flag_is_narrow() {
        let mapped =
            resolve(&injected_row("ACME_HOST", ""), &map(&[]), &map(&[("ACME_HOST", "x")]));
        assert_eq!(mapped.reads, Reads::CallerMap);
        assert!(!mapped.store_may_not_reach_reader());

        let exported = resolve(&row("ACME_HOST", ""), &map(&[("ACME_HOST", "x")]), &map(&[]));
        assert!(!exported.store_may_not_reach_reader());

        let unset = resolve(&row("ACME_HOST", ""), &map(&[]), &map(&[]));
        assert!(!unset.store_may_not_reach_reader());
    }

    /// The REAL registry has rows of this shape — the finding that motivated the column. Keyed on
    /// the property rather than on a name, so it stays true as the registry moves.
    #[test]
    fn the_real_registry_has_direct_env_readers_a_store_value_would_not_reach() {
        let store_only: Vec<&Setting> =
            SETTINGS.iter().filter(|s| Reads::of(s.naming) == Reads::ProcessEnv).collect();
        assert!(
            !store_only.is_empty(),
            "no row reads with env::var — the READS column would be decorative"
        );
        let name = store_only[0].name;
        let r = resolve(store_only[0], &map(&[]), &map(&[(name, "x")]));
        assert!(r.store_may_not_reach_reader());
    }

    // -- redaction -----------------------------------------------------------------------------

    #[test]
    fn every_required_credential_shape_is_secret() {
        for name in [
            "ACME_API_KEY",
            "ACME_API_SECRET",
            "ACME_API_PASSPHRASE",
            "ACME_BOT_TOKEN",
            "ACME_PRIVATE_KEY",
            "ACME_DEMO1_LOGIN",
            "ACME_DEMO1_PASSWORD",
        ] {
            assert!(is_secret(name), "{name} must be treated as a secret");
        }
    }

    #[test]
    fn the_widened_shapes_are_secret_too() {
        for name in [
            "ACME_CONTROL_KEY",   // the HMAC node keys
            "ACME_CLIENT_SECRET", // OAuth2 secrets not spelled _API_SECRET
            "ACME_PASSPHRASE",
            "ACME_DEMO_USER",
            "ACME_SIGNATURE",
        ] {
            assert!(is_secret(name), "{name} must be treated as a secret");
        }
        // and a bare name spelled exactly like a shape
        assert!(is_secret("PASSWORD"));
        assert!(is_secret("TOKEN"));
    }

    #[test]
    fn ordinary_knobs_are_not_secret() {
        for name in [
            "ACME_HOST",
            "ACME_LOG_DIR",
            "ACME_HOLD_TOKENS",             // plural — not `_TOKEN`
            "ACME_ALLOW_WITHDRAW_KEYS",     // plural — not `_KEY`
            "ACME_SIGNATURE_TYPE",          // a mode, not the signature
            "ACME_RELAYER_API_KEY_ADDRESS", // an address, not the key
            "USERPROFILE",                  // OS path, not a `_USER` credential
        ] {
            assert!(!is_secret(name), "{name} must NOT be redacted");
        }
    }

    /// The files half is redacted by the same shapes, applied to a dotted key's LEAF — insurance
    /// against a credential-shaped settings field being added later. The last assertion states the
    /// property that makes it insurance rather than dead code TODAY.
    #[test]
    fn a_dotted_key_is_redacted_by_its_leaf_segment() {
        assert!(is_secret_key("config.bot_token"));
        assert!(is_secret_key("preferences.client_secret"));
        assert!(!is_secret_key("config.log_dir"));
        assert!(!is_secret_key("policy.max_notional_per_order"));

        let d = vike_config::describe(None, &map(&[])).unwrap();
        assert!(d.rows.iter().all(|r| !is_secret_key(&r.key)), "a settings field is secret-shaped");
    }

    /// The whole point: no store byte reaches [`Resolved`] for a secret row — so neither printer
    /// can leak it, whichever store won.
    #[test]
    fn a_secret_value_never_enters_the_resolved_row() {
        const LEAK: &str = "sk-do-not-print-me";
        for (env, dotenv) in
            [(map(&[("ACME_API_KEY", LEAK)]), map(&[])), (map(&[]), map(&[("ACME_API_KEY", LEAK)]))]
        {
            let r = resolve(&row("ACME_API_KEY", ""), &env, &dotenv);
            assert_eq!(r.value, SET);
            assert!(r.secret);
            assert!(!format!("{r:?}").contains(LEAK), "the secret leaked into {r:?}");
        }
    }

    // The FILES-half twin of the test above (`a_secret_settings_value_never_enters_the_file_row`)
    // MOVED to `vike_config::show` with the builder it pins.

    /// A redacted row must still report WHERE its value came from — the store vs a shell export is
    /// the provenance question, and the answer discloses nothing.
    #[test]
    fn a_redacted_row_still_reports_its_true_source() {
        let secret = row("ACME_API_KEY", "");
        assert_eq!(resolve(&secret, &map(&[("ACME_API_KEY", "k")]), &map(&[])).source, Source::Env);
        assert_eq!(
            resolve(&secret, &map(&[]), &map(&[("ACME_API_KEY", "k")])).source,
            Source::Dotenv
        );
        assert_eq!(resolve(&secret, &map(&[]), &map(&[])).source, Source::Default);
    }

    #[test]
    fn an_unconfigured_or_empty_secret_prints_unset() {
        // nothing configured it
        assert_eq!(resolve(&row("ACME_API_KEY", ""), &map(&[]), &map(&[])).value, UNSET);
        // present but empty — configured, but there is no key there to call `<set>`
        let r = resolve(&row("ACME_API_KEY", ""), &map(&[("ACME_API_KEY", "")]), &map(&[]));
        assert_eq!(r.value, UNSET);
        assert_eq!(r.source, Source::Env, "still reports that env held the key");
    }

    /// No credential row today declares a non-empty default; if one ever did, the DEFAULT column
    /// must not become the leak the VALUE column is not.
    #[test]
    fn a_nonempty_secret_default_is_redacted_too() {
        let r = resolve(&row("ACME_API_KEY", "hardcoded"), &map(&[]), &map(&[]));
        assert_eq!(r.default, REDACTED);
        assert!(!format!("{r:?}").contains("hardcoded"));
    }

    /// The redaction shapes are checked against the REAL registry, not only invented fixtures: a
    /// credential row added tomorrow whose name these shapes miss fails HERE instead of surfacing
    /// in a pasted issue. Keyed on the substrings that mark a credential rather than on the shape
    /// list itself, so the test cannot pass by agreeing with the code it is checking.
    #[test]
    fn no_settings_row_leaks() {
        const CREDENTIAL_MARKERS: &[&str] =
            &["API_KEY", "SECRET", "PASSWORD", "PASSPHRASE", "PRIVATE_KEY", "TOKEN", "LOGIN"];
        let missed: Vec<&str> = SETTINGS
            .iter()
            .map(|s| s.name)
            // A plural form is a collection of ids, not a credential (`..._TOKENS`), so the
            // markers are matched as a SUFFIX and those rows are correctly not flagged.
            .filter(|&name| {
                CREDENTIAL_MARKERS.iter().any(|&m| name.ends_with(m)) && !is_secret(name)
            })
            .collect();
        assert!(
            missed.is_empty(),
            "credential-shaped rows that would print in the clear: {missed:?}"
        );
    }

    // -- the view filters ----------------------------------------------------------------------

    #[test]
    fn changed_only_drops_every_defaulted_row() {
        let env = map(&[]);
        let dotenv = map(&[]);
        let all = resolve_all(&env, &dotenv, None, false);
        assert_eq!(all.len(), SETTINGS.len(), "one row per (name, krate) pair");
        // With both stores empty every row falls through to its default, so the "what have I
        // configured?" view is empty.
        assert!(resolve_all(&env, &dotenv, None, true).is_empty());
    }

    #[test]
    fn changed_only_keeps_the_rows_a_store_actually_set() {
        let name = SETTINGS[0].name;
        let env = map(&[(name, "x")]);
        let rows = resolve_all(&env, &map(&[]), None, true);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.source == Source::Env));
        assert!(rows.iter().all(|r| r.name == name), "only the configured name survives");
    }

    #[test]
    fn the_filter_matches_name_or_crate_case_insensitively() {
        let (env, dotenv) = (map(&[]), map(&[]));
        let by_crate = resolve_all(&env, &dotenv, Some("VIKE-CLI"), false);
        assert!(by_crate.iter().all(|r| r.krate.contains("vike-cli")));

        let name = SETTINGS[0].name;
        let by_name = resolve_all(&env, &dotenv, Some(&name.to_ascii_lowercase()), false);
        assert!(by_name.iter().any(|r| r.name == name));

        assert!(resolve_all(&env, &dotenv, Some("no-such-setting-anywhere"), false).is_empty());
    }

    #[test]
    fn rows_are_sorted_by_name_then_crate() {
        let rows = resolve_all(&map(&[]), &map(&[]), None, false);
        let keys: Vec<(&str, &str)> = rows.iter().map(|r| (r.name, r.krate)).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    // -- the FILES half ------------------------------------------------------------------------
    // MOVED to `vike_config::show` with the builder (`the_files_half_covers_every_typed_setting`,
    // `the_files_filter_matches_the_key_or_the_file`,
    // `an_environment_override_is_reported_with_its_variable`) — the tests travel with the code
    // they pin, and this module keeps only what still lives here: the env half + the printers'
    // glue.

    // -- arg parsing ---------------------------------------------------------------------------

    #[test]
    fn flags_parse_in_both_forms_and_default_to_the_full_human_output() {
        assert_eq!(parse_args(std::iter::empty()).unwrap(), Args::default());
        assert_eq!(Args::default().section, Section::All);

        let a = parse_args(
            ["--json", "--changed-only", "--filter", "recon", "--section", "files"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        assert!(a.json && a.changed_only);
        assert_eq!(a.filter.as_deref(), Some("recon"));
        assert_eq!(a.section, Section::Files);

        let a = parse_args(["--filter=poly".to_string(), "--section=env".to_string()].into_iter())
            .unwrap();
        assert_eq!(a.filter.as_deref(), Some("poly"));
        assert_eq!(a.section, Section::Env);
    }

    #[test]
    fn the_section_flag_picks_halves() {
        assert!(Section::All.files() && Section::All.env());
        assert!(Section::Files.files() && !Section::Files.env());
        assert!(!Section::Env.files() && Section::Env.env());
    }

    #[test]
    fn a_bad_flag_is_a_clean_error() {
        assert!(parse_args(["--nope".to_string()].into_iter()).unwrap_err().contains("--nope"));
        assert!(parse_args(["--filter".to_string()].into_iter())
            .unwrap_err()
            .contains("requires a value"));
        assert!(parse_args(["--json=1".to_string()].into_iter())
            .unwrap_err()
            .contains("takes no value"));
        assert!(parse_args(["--section=nope".to_string()].into_iter())
            .unwrap_err()
            .contains("files|env|all"));
        assert_eq!(parse_args(["-h".to_string()].into_iter()).unwrap_err(), "help requested");
    }

    // -- the printers --------------------------------------------------------------------------

    fn empty_store() -> StoreStatus {
        StoreStatus { path: None, present: false, keys: 0 }
    }

    #[test]
    fn both_printers_render_everything_without_panicking() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let files = file_rows(&d, None, false);
        let envs = resolve_all(&map(&[]), &map(&[]), None, false);
        let unknown = unknown_store_keys(&map(&[("ACME_TYPOD", "1"), ("ACME_API_KEY", "k")]), None);
        print_human(Section::All, &d, &empty_store(), &files, &envs, &unknown);
        print_json(&d, &empty_store(), &files, &envs, &unknown).unwrap();
        // and the empty views
        let none = UnknownKeys::default();
        print_human(Section::Files, &d, &empty_store(), &[], &[], &none);
        print_human(Section::Env, &d, &empty_store(), &[], &[], &none);
        // …and the filtered-to-nothing env table WITH a complement, the one path that would
        // otherwise return before printing it.
        print_human(Section::Env, &d, &empty_store(), &[], &[], &unknown);
        print_json(&d, &empty_store(), &[], &[], &none).unwrap();
    }

    #[test]
    fn the_json_document_carries_the_documented_fields() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let files = file_rows(&d, Some("policy"), false);
        let envs = resolve_all(&map(&[]), &map(&[]), Some("vike-cli"), false);
        assert!(!files.is_empty() && !envs.is_empty());
        let doc = settings_json(&d, &empty_store(), &files, &envs, &UnknownKeys::default());

        for field in
            ["settings_dir", "files", "secrets", "warnings", "settings", "env", "unknown_env_keys"]
        {
            assert!(doc.get(field).is_some(), "missing {field}");
        }
        for field in ["named", "credential_shaped"] {
            assert!(
                doc["unknown_env_keys"].get(field).is_some(),
                "missing unknown_env_keys.{field}"
            );
        }
        let first = &doc["settings"].as_array().unwrap()[0];
        for field in ["key", "value", "origin", "origin_detail", "default", "adjusted", "secret"] {
            assert!(first.get(field).is_some(), "missing {field} in {first}");
        }
        let first = &doc["env"].as_array().unwrap()[0];
        for field in [
            "name",
            "value",
            "source",
            "reads",
            "store_may_not_reach_reader",
            "default",
            "krate",
            "secret",
        ] {
            assert!(first.get(field).is_some(), "missing {field} in {first}");
        }
        // Every settings file is always listed, present or not — and EXACTLY the files the loader
        // consults: the four in the settings directory, no more. A row for a file nothing reads
        // would be the disclosure lying in the direction it exists to prevent.
        assert_eq!(doc["files"].as_array().unwrap().len(), 4);
    }

    /// The end-to-end redaction property, asserted on the SERIALIZED document: whatever the stores
    /// hold for a secret row, no byte of it reaches the `--json` output either — and the credential
    /// store is disclosed by COUNT, never by a key name.
    ///
    /// The UNMATCHED-key complement rides the same document, so it is driven here too, off a store
    /// whose keys are both credential-shaped and unmatched: neither name may appear, and neither
    /// value.
    #[test]
    fn the_json_document_cannot_leak_a_secret() {
        const LEAK: &str = "sk-do-not-print-me";
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let env = map(&[("ACME_API_KEY", LEAK)]);
        let envs = vec![resolve(&row("ACME_API_KEY", ""), &env, &map(&[]))];
        let store = StoreStatus { path: None, present: true, keys: 42 };
        let unknown = unknown_store_keys(
            &map(&[("ACME_API_KEY", LEAK), ("SOMEVENUE_LIVE_API_SECRET", LEAK)]),
            None,
        );
        let text = serde_json::to_string(&settings_json(&d, &store, &[], &envs, &unknown)).unwrap();
        assert!(!text.contains(LEAK), "{text}");
        assert!(text.contains(SET) && text.contains("\"secret\":true"), "{text}");
        assert!(text.contains("\"keys\":42"), "the store is disclosed by count only");
        assert!(!text.contains("SOMEVENUE_LIVE_API_SECRET"), "an unmatched credential NAME leaked");
        assert!(text.contains("\"credential_shaped\":2"), "…and is disclosed by count: {text}");
    }

    // -- the unmatched-store-key complement ------------------------------------------------------

    /// The defect: a store key that matches no registry row produced NO row, so a typo'd variable
    /// name was indistinguishable from one that was never set. It is now the complement — named
    /// when its name is not credential-shaped.
    #[test]
    fn a_store_key_no_registry_row_covers_is_surfaced() {
        let declared = SETTINGS[0].name;
        let u = unknown_store_keys(&map(&[(declared, "x"), ("ACME_NOT_A_SETTING", "1")]), None);
        assert_eq!(u.named, vec!["ACME_NOT_A_SETTING".to_string()]);
        assert_eq!(u.credential_shaped, 0, "a declared row is not unmatched, whatever its shape");
    }

    /// …and the half that keeps this from becoming a store dump: an unmatched key whose NAME is
    /// credential-shaped is COUNTED, never named. `VIKE_NODE_CONTROL_KEY` — the real mis-spelling
    /// of `VIKE_TRADEHUB_CONTROL_KEY` that prompted this — is deliberately in that bucket: its shape
    /// is indistinguishable from a genuine node key's, and the fixture now SPELLS it rather than
    /// standing in for it (until the settings-registry gate stopped harvesting test-region literals
    /// as reads, a `VIKE_NODE_CONTROL_KEY` literal anywhere under `crates/` failed that gate).
    #[test]
    fn an_unmatched_credential_shaped_key_is_counted_never_named() {
        let u = unknown_store_keys(
            &map(&[
                ("VIKE_NODE_CONTROL_KEY", "hmac"),
                ("SOMEVENUE_LIVE_API_KEY", "k"),
                ("ACME_NOT_A_SETTING", "1"),
            ]),
            None,
        );
        assert_eq!(u.credential_shaped, 2);
        assert_eq!(u.named, vec!["ACME_NOT_A_SETTING".to_string()], "only the safe shape is named");
        assert!(
            !format!("{u:?}").contains("CONTROL_KEY") && !format!("{u:?}").contains("API_KEY"),
            "a credential-shaped name reached the struct: {u:?}"
        );
    }

    /// Nothing unaccounted for ⇒ nothing to print, so a block on screen always means something.
    #[test]
    fn a_fully_declared_store_has_no_complement() {
        let declared = SETTINGS[0].name;
        assert!(unknown_store_keys(&map(&[(declared, "x")]), None).is_empty());
        assert!(unknown_store_keys(&map(&[]), None).is_empty());
    }

    /// The complement honours `--filter`, so `--filter node` narrows it the same way it narrows the
    /// table above it.
    #[test]
    fn the_complement_honours_the_filter() {
        let store = map(&[("ACME_NOT_A_SETTING", "1"), ("ACME_OTHER_THING", "2")]);
        assert_eq!(
            unknown_store_keys(&store, Some("not_a")).named,
            vec!["ACME_NOT_A_SETTING".to_string()]
        );
        assert!(unknown_store_keys(&store, Some("no-such-key-anywhere")).is_empty());
    }
}
