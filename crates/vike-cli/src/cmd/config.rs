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
//! ⚠ **"The credential store" is TWO artifacts since `docs/decisions/0054`'s credential half, and
//! WHICH one is a per-RUN fact this command now asks about rather than assuming.** A project
//! holding `<project>/settings/db/vike.db` is answered WHOLLY by that database and its
//! `secrets.env` is not read at all. Every sentence here that used to name that file
//! unconditionally — the header's store row, the precedence line, the stranded-row diagnosis, the
//! unmatched-key complement, `--json`'s `secrets` object — named it on a migrated box too, beside
//! a key count read out of the database: the operator got positive confirmation of something false,
//! which is the failure this whole command exists to remove. The choice is
//! `vike_secrets::backend_in`'s, asked ONCE in [`store_status`] and carried down as data; a
//! per-KEY fallback (the database, then the file) is the ladder
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids and is not written anywhere
//! here. `crates/vike-cli/src/cmd/config_check.rs`'s `store_findings` is the sibling verb's twin of
//! the same repair.
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
use vike_config::redact::{REDACTED, SET, UNSET, is_secret};
// The FILES-half row builder — MOVED to `vike_config::show` (one authority; the tradehub
// `SettingsShow` wire verb consumes the same builder), re-consumed here by the printers.
use vike_config::show::{FileRow, file_rows};
use vike_ops::settings::{Naming, SETTINGS, Setting};

use crate::cmd::args::{self, Flags};
use crate::cmd::config_check::DirOrigin;

/// The `config` verb list. Printed by `config --help` and by the unknown-verb arm, so a verb cannot
/// be added without showing up in help.
const USAGE: &str = "\
usage: vike-cli config <verb> [options]

verbs:
  show    every setting, its effective value, and WHERE that value came from
  check   validate this box's settings tree + credential store; the EXIT CODE is the product
  set     write ONE key into this box's settings files, and record it in the change journal
  mirror  copy this box's settings files into the settings database
  compare resolve this box twice — files alone, then rows alone — and diff the two (exit 1 if
          they differ); the pre-flight `adopt` refuses without
  adopt   make the settings DATABASE answer for every settings key on this box (`--undo` hands
          it back to the files; neither needs a redeploy)

  recorder  print the recorder profile ROWS, leading with the store that answered

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
    /// The credential store, when the FILE answers — `<project>/settings/secrets.env`.
    Dotenv,
    /// The credential store, when the settings DATABASE answers instead —
    /// `<project>/settings/db/vike.db`, `docs/decisions/0054-settings-move-into-one-database.md`.
    ///
    /// ⚠ **A fourth word in a consumer-visible column, added rather than folded into
    /// [`Source::Dotenv`]** — the same call `vike_secrets::Source::Database`'s own doc makes, for
    /// the same reason. Folding would keep the three pinned words at the cost of printing `dotenv`
    /// beside a value that came out of a database on every migrated box, which is the positive
    /// confirmation of something false that 0054's constraint 2 exists to prevent. A tool that
    /// matched on the three old words sees a new one the day a box migrates, and that is the
    /// cheaper failure: it is visible.
    Database,
    /// The real process environment.
    Env,
}

impl Source {
    /// The stable wire/table spelling. Pinned by test — later phases may move stores, but a tool
    /// parsing `--json` must keep reading the same words for the same stores.
    fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Dotenv => "dotenv",
            // The SAME word `vike-cli secrets list --json`'s `kind` prints for the same store, so
            // an operator comparing the two surfaces is reading one fact.
            Source::Database => "database",
            Source::Env => "env",
        }
    }

    /// The word for a value that came from **the credential store** — which store that is being
    /// `vike_secrets::backend_in`'s per-RUN answer, taken here as a PARAMETER.
    ///
    /// ⚠ **Nothing in this file probes.** The choice is made once, in [`execute`], and travels
    /// down as data. A per-KEY fallback — look in the database, then in the file — is the ladder
    /// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids and the shape
    /// `vike_secrets::Backend`'s own doc argues against; a resolver that could ask twice would be
    /// able to answer twice.
    fn of_store(backend: &vike_secrets::Backend) -> Self {
        match backend {
            vike_secrets::Backend::Files => Source::Dotenv,
            vike_secrets::Backend::Database(_) => Source::Database,
        }
    }

    /// Did this value come from the credential store at all — either of its two shapes?
    fn is_store(self) -> bool {
        matches!(self, Source::Dotenv | Source::Database)
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
        // Either shape of the store — the diagnosis is about the value not being EXPORTED, which
        // is equally true of a row the settings database answered.
        self.source.is_store() && self.reads == Reads::ProcessEnv
    }
}

/// Resolve ONE registry row against two caller-supplied maps. Pure: no process env, no filesystem,
/// no globals — which is what makes the precedence and redaction rules testable without the
/// process-global `set_var` races that `SRC_TEST_MODULE_OVERRIDES` exists to work around.
///
/// Precedence: `env` > the credential store > `row.default`. PRESENCE wins, not non-emptiness (see
/// the module doc): a key mapped to `""` is a real override and is reported as one.
///
/// `dotenv` is the map the credential loader returned and `backend` is WHICH STORE produced it —
/// [`Source::of_store`]'s parameter. The two travel together because a map alone cannot say where
/// it came from, which is exactly how this command came to print `dotenv` for a database.
pub(crate) fn resolve(
    row: &Setting,
    env: &HashMap<String, String>,
    dotenv: &HashMap<String, String>,
    backend: &vike_secrets::Backend,
) -> Resolved {
    let (raw, source) = match env.get(row.name) {
        Some(v) => (v.as_str(), Source::Env),
        None => match dotenv.get(row.name) {
            Some(v) => (v.as_str(), Source::of_store(backend)),
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
    backend: &vike_secrets::Backend,
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
        .map(|row| resolve(row, env, dotenv, backend))
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

/// Everything the `config` family needs from the dispatcher's ONE boot walk and its ONE clock read.
///
/// It was three positional parameters until `set` landed — a WRITER needs two facts a reader does
/// not (the ledger's home and the instant to stamp a record with), and five positional `Option`s in
/// a row is the signature nobody can read a call site of. The same argument
/// `crate::cmd::secrets`' `Ctx` makes, and the same rule behind it: a `src/cmd/` file reads no
/// environment and performs no second walk of its own.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// THE settings directory the dispatcher already resolved (`VIKE_SETTINGS_DIR`, else the
    /// project walk) — the same value `secrets` and `trade` receive. A parameter rather than a
    /// re-derivation, so this command can never report or WRITE a different directory than the one
    /// every other surface reads.
    pub settings_dir: Option<&'a Path>,
    /// Which of those two rungs answered — an answer only the dispatcher has, and one
    /// [`crate::cmd::config_check`] needs: a NAMED directory that is not there is a refusal, while
    /// a WALKED one that is not there is an unconfigured checkout.
    pub origin: DirOrigin,
    /// That directory's `state` child, off the SAME walk: the change journal's home for
    /// [`crate::cmd::config_set`]. `None` journals nothing and still writes.
    pub state_dir: Option<&'a Path>,
    /// The instant a journal record is stamped with, read once by the dispatcher because
    /// `vike_model::change_journal` reads no clock.
    pub now_ms: i64,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `config` subcommand — the
/// first of which is the verb (`show`, `check` or `set`).
pub fn run(mut args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let verb = match args.next() {
        Some(v) => v,
        None => {
            eprintln!("vike-cli config: missing verb\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match verb.as_str() {
        "show" => run_show(args, ctx.settings_dir),
        "check" => crate::cmd::config_check::run(args, ctx.settings_dir, ctx.origin),
        "set" => crate::cmd::config_set::run(
            args,
            crate::cmd::config_set::Ctx {
                settings_dir: ctx.settings_dir,
                // ⚠ The WRITE verb takes `origin` for the reason `check` does and then some: its
                // confirmation must name the DIRECTORY it wrote, and which rung resolved it.
                origin: ctx.origin,
                state_dir: ctx.state_dir,
                now_ms: ctx.now_ms,
            },
        ),
        // The MIRROR write (decision 0057 Phase 1). It takes the dispatcher's own settings
        // directory like every verb above, and deliberately takes neither `state_dir` nor `now_ms`:
        // a mirror states no new value — it copies values an operator already filed — so it
        // journals nothing. See `crate::cmd::config_mirror`'s module doc.
        "mirror" => crate::cmd::config_mirror::run(args, ctx.settings_dir),
        // The CROSSING, as two operator acts — `crates/vike-cli/src/cmd/config_adopt.rs` carries
        // why it is an operator act and not a probe the binary evaluates on its own. Neither
        // journals: `compare` writes nothing at all, and `adopt` states no VALUE — it records which
        // ARTIFACT this box reads, having first refused unless the two resolve identically.
        "compare" => crate::cmd::config_adopt::run_compare(args, ctx.settings_dir),
        "adopt" => crate::cmd::config_adopt::run_adopt(args, ctx.settings_dir),

        // The recorder profile, READ back out of the store. It is the replacement for the
        // `grep '^store' <root>/settings/recorder.toml` an operator ran (and which two shipped
        // units embedded in their troubleshooting comments) — see
        // `crate::cmd::config_recorder`'s module doc for why a read verb was unavoidable rather
        // than a convenience.
        "recorder" => crate::cmd::config_recorder::run(args, ctx.settings_dir),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "vike-cli config: unknown verb '{other}' (expected `show`, `check`, `set`, \
                 `mirror`, `compare`, `adopt` or `recorder`)\n{USAGE}"
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

/// The credential store's own status — **the store that ANSWERED**, not the file this command
/// could always name. Key COUNT only: never a name, never a value.
///
/// ⚠ **Every field describes ONE store.** Before `docs/decisions/0054`'s credential half this
/// struct was a file path, an `is_file` on that path, and a key count taken from the LOADER — three
/// facts about one store while there was only one. On a migrated box they came apart: the path and
/// the presence bit described `secrets.env` while the count described the database that had
/// replaced it, and the two wrong halves read as one confident answer. That is the same defect
/// `crates/vike-cli/src/cmd/config_check.rs`'s `store_findings` fixed for the sibling verb, and
/// this is its `config show` twin.
struct StoreStatus {
    /// Which store answered — `vike_secrets::backend_in`'s per-RUN answer, asked ONCE in
    /// [`execute`] and carried here as DATA. Nothing below re-probes, so no printed cell can
    /// disagree with any other.
    backend: vike_secrets::Backend,
    /// The store that answered, as a path: the settings database when one exists, else the
    /// credential FILE. `None` only when no settings directory resolved at all.
    path: Option<PathBuf>,
    /// Whether that store is on disk. A `Backend::Database` is present by construction — the probe
    /// that produced it IS an `is_file` — so this only ever reports on the file arm.
    present: bool,
    /// How many keys the loader returned, i.e. how many the ANSWERING store holds.
    keys: usize,
    /// Set when the database answered and the credential file it replaced is still on disk. The
    /// sentence is `vike_secrets::ShadowedStore`'s own `Display`, never a second spelling of it:
    /// `config check` and `secrets list` print the same words for the same finding.
    shadowed: Option<vike_secrets::ShadowedStore>,
}

impl StoreStatus {
    /// The FILE NAME the header's store row is labelled with — `vike.db` on a migrated box,
    /// `secrets.env` otherwise. Derived from [`Self::backend`], never typed at a call site.
    fn label(&self) -> &'static str {
        match self.backend {
            vike_secrets::Backend::Files => vike_secrets::SECRETS_FILE,
            vike_secrets::Backend::Database(_) => vike_secrets::DB_FILE,
        }
    }

    /// The machine word for WHICH KIND of store answered — the same vocabulary
    /// `vike-cli secrets list --json`'s `kind` field prints, so a tool reading both surfaces is
    /// reading one fact.
    fn kind(&self) -> &'static str {
        match self.backend {
            vike_secrets::Backend::Files => "file",
            vike_secrets::Backend::Database(_) => "database",
        }
    }

    /// How the answering store is NAMED in a sentence: its resolved path when there is one, else
    /// the conventional `settings/<file>` shorthand the no-directory arm falls back to.
    fn named(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => format!("{}/{}", vike_secrets::SETTINGS_DIR, self.label()),
        }
    }
}

/// **WHICH CREDENTIAL STORE ANSWERED THIS RUN — asked ONCE, and the only probe in this file.**
///
/// `vike_secrets::backend_in` is the question `vike_secrets::resolve_store_in` asks on the reader's
/// side and `vike_secrets::save_credentials_to_store` asks on the writer's, so the store this
/// command REPORTS on is the store a daemon on this box READS. `keys` is the loader's own count,
/// passed in rather than recomputed: the map came out of the same backend and counting it again
/// from a path would be the second answer this function exists to prevent.
///
/// ⚠ **It OPENS NOTHING.** `backend_in` is one `is_file` on one path; the credentials are already
/// in hand. So this adds no credential-store read — the thing
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down — and takes
/// its directory as a PARAMETER, so it cannot walk for a store from a working directory either.
///
/// With NO settings directory there is nothing to probe: the loader's own last resort is a
/// working-directory-relative `settings/`, and resolving one here would be a SECOND resolution this
/// command has no business performing (the repo-wide *one walk DECIDES* rule). [`print_header`]'s
/// no-directory arm says that outright instead of naming a store it never established.
fn store_status(settings_dir: Option<&Path>, keys: usize) -> StoreStatus {
    let Some(dir) = settings_dir else {
        return StoreStatus {
            backend: vike_secrets::Backend::Files,
            path: None,
            present: false,
            keys,
            shadowed: None,
        };
    };
    let backend = vike_secrets::backend_in(dir);
    let file = dir.join(vike_secrets::SECRETS_FILE);
    let (path, present, shadowed) = match &backend {
        vike_secrets::Backend::Files => (Some(file.clone()), file.is_file(), None),
        vike_secrets::Backend::Database(db) => (
            Some(db.clone()),
            // A `Database` backend IS an `is_file` that succeeded.
            true,
            // The file the database now shadows, when it is still sitting there. A FINDING and
            // never a refusal — the posture `vike_secrets::ShadowedStore` argues for — and the
            // operator-facing half of the whole move: every runbook in this tree says *edit
            // `<project>/settings/secrets.env`*, and after a migration that edit changes nothing
            // while looking exactly like it worked.
            file.is_file()
                .then(|| vike_secrets::ShadowedStore { file: file.clone(), db: db.clone() }),
        ),
    };
    StoreStatus { backend, path, present, keys, shadowed }
}

/// Read the stores and print. The process-env read is a `std::env::vars()` SWEEP — it names no
/// variable, so it needs no `SETTINGS` row of its own (see that table's design note); this command
/// is a reader OF the registry, never a new entry in it. The credential half goes through the
/// workspace's own loader, never a local re-parse (see the module doc).
fn execute(args: &Args, settings_dir: Option<&Path>) -> Result<(), String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let dotenv = load_workspace_secrets_from_env(&env);
    let secrets = store_status(settings_dir, dotenv.len());

    // The settings DATABASE's rows, if this box has been mirrored — decision 0057's Phase 1, which
    // is the READ-BACK path 0054 requires to land before any file retires: an operator with the
    // daemon down and no `sqlite3` binary reads their settings HERE.
    //
    // ⚠ Opened by the BINARY and handed to the loader as DATA. `vike-config` never opens the store
    // and must not — under one database a handle that reaches the settings rows reaches the
    // `credential` table too (`vike_config::mirror`'s module doc carries the argument).
    //
    // ⚠ A read FAILURE is carried into the DESCRIPTION rather than rewritten into "there is no
    // database" here. That rewrite stood at four other call sites as well and is deleted from all
    // of them: it threw away the one distinction `vike_config::StoreLayer` exists to carry, and on
    // an adopted box it would turn *the source of every ceiling could not be opened* into *this box
    // has never been mirrored* — two states that will resolve to opposite things.
    //
    // This command does not REFUSE on it (see `Description::store_refusal`, which the renderer
    // leads with): `config show` is the disclosure verb an operator reaches for precisely when a
    // box will not start, and a refusal here is the brick one door over from the one the JSON
    // incident actually produced.
    let store = settings_dir.map(vike_secrets::read_settings_in);
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal);

    // ...and the RUN PROFILE `[risk]` rows — decision 0057's Phase 2, and the answer to the
    // question `print_ceilings` below could previously only pose: what ARE my live pre-trade
    // ceilings? They are a DISCLOSURE copy (`vike_config::profile_risk`'s module doc), so a read
    // failure degrades to a warning for the same reason the settings rows' does, only more
    // strongly: nothing anywhere acts on them.
    let profile_risk = match settings_dir.map(vike_secrets::read_profile_risk_in) {
        Some(Ok(found)) => found,
        Some(Err(e)) => {
            eprintln!(
                "warning: the mirrored run-profile `[risk]` rows could not be read ({e}). The \
                 ceilings block below names the keys and cannot print their values, exactly as it \
                 did before this box was mirrored."
            );
            vike_secrets::ProfileRiskSource::NoDatabase { path: e.path }
        }
        None => vike_secrets::ProfileRiskSource::NoDatabase { path: PathBuf::new() },
    };

    // The files half. A load error is FATAL here on purpose: a broken `policy.toml` reported as
    // "defaults" would be the single most misleading thing this command could print.
    // A disclosure command resolves the SAME layers the daemons do — one loader over one settings
    // directory, and the same refusal of a settings file that has been retired.
    let described = vike_config::describe_with_source(settings_dir, source, &env)
        .map_err(|e| format!("settings could not be loaded: {e}"))?;

    let files = if args.section.files() {
        file_rows(&described, args.filter.as_deref(), args.changed_only)
    } else {
        Vec::new()
    };
    let envs = if args.section.env() {
        resolve_all(&env, &dotenv, &secrets.backend, args.filter.as_deref(), args.changed_only)
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
        print_json(&described, &secrets, &files, &envs, &unknown, &profile_risk)
    } else {
        print_human(
            args.section,
            args.filter.as_deref(),
            &described,
            &secrets,
            &files,
            &envs,
            &unknown,
            &profile_risk,
        );
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
    profile_risk: &vike_secrets::ProfileRiskSource,
) -> serde_json::Value {
    serde_json::json!({
        "settings_dir": d.settings_dir.as_ref().map(|p| p.display().to_string()),
        "files": d.files.iter().map(|f| serde_json::json!({
            "name": f.name,
            "path": f.path.display().to_string(),
            "present": f.present,
            "keys": f.keys,
        })).collect::<Vec<_>>(),
        // THE STORE THAT ANSWERED, never the file this command could always name. `path` is a path
        // either way, so a consumer reading it as a location is unchanged by the database landing;
        // what it can no longer learn from that field alone is that the location is a TEXT FILE,
        // which is what `kind` — the same word `vike-cli secrets list --json` prints — says
        // outright rather than smuggling into an extension.
        "secrets": {
            "path": secrets.path.as_ref().map(|p| p.display().to_string()),
            "kind": secrets.kind(),
            "present": secrets.present,
            "keys": secrets.keys,
            // The credential FILE the database has replaced and left on disk, `null` on every box
            // where that is not the situation. A machine reading this document is the other
            // consumer of the finding `shadowed` exists for.
            "shadowed": secrets.shadowed.as_ref().map(|s| serde_json::json!({
                "file": s.file.display().to_string(),
                "read": false,
            })),
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
            // The machine half of the same correction: a consumer that only reads `why_unread`
            // gets the paragraph that was misleading on its own.
            "unread_verdict": r.unread_verdict,
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
            // The machine half of the same correction the human table below carries: `reads` says
            // WHERE the read is, never whether anything runs it. `None` for every variable this
            // workspace has nothing gated to say about.
            "reader_verdict": vike_config::env_verdict(r.name),
        })).collect::<Vec<_>>(),
        // THE PRE-TRADE CEILINGS, from `vike_config::PRE_TRADE_CEILINGS`. Unfiltered and always
        // present: a machine asking "what can refuse this order" must not have its answer depend
        // on the human filter, and `value_shown` is the field that says whether `settings` above
        // carries the number (`false` = it lives in the run profile `VIKE_RUN_PROFILE` names, which
        // this command does not read — see `print_ceilings`). A `false` row is NOT an unset
        // ceiling: `max_total_exposure` is mandatory for a live mount.
        "ceilings": vike_config::PRE_TRADE_CEILINGS.iter().map(|c| serde_json::json!({
            "name": c.name,
            "home": c.home.label(),
            "operator_doc": c.home.operator_doc(),
            "value_shown": c.home.value_shown_by_config_show(),
            "guards": c.guards,
            "enforced": c.is_enforced(),
            "enforced_at": c.enforced_at.iter().map(|s| serde_json::json!({
                "file": s.file,
                "what": s.what,
            })).collect::<Vec<_>>(),
            "absent_means": c.absent_means,
            // DERIVED, never typed: true when another home carries this same key and the two judge
            // different acts. The `# mirrors settings/policy.toml` comment this replaced.
            "shares_its_name": vike_config::shared_names().contains(&c.name),
            // ⚠ `false` here does NOT mean "absent is uncapped" — read `absent_means`, which is
            // the prose column that distinguishes the two answers. `true` means a LIVE mount
            // REFUSES TO START without this key, and it is gated against the source of the only
            // function that performs that refusal, so a machine can rely on it: see
            // `vike_config::Ceiling::refuses_live_mount_when_absent`.
            "refuses_live_mount_when_absent": c.refuses_live_mount_when_absent,
        })).collect::<Vec<_>>(),
        // THE MIRRORED RUN-PROFILE `[risk]` ROWS — decision 0057 Phase 2, and the VALUES the
        // `ceilings` array above still cannot carry from the file. Unfiltered, like `ceilings`, and
        // for the same reason.
        //
        // ⚠ `read_by` is deliberately the literal `null` and `enforced` the literal `false` on
        // every row: NOTHING on the mount path reads these, so a machine must not treat a present
        // row as a ceiling in force. The profile FILE is what judges an order. `state` is how a
        // consumer tells "mirrored and empty" from "never mirrored" — the distinction the
        // `ProfileRiskSource` enum exists for.
        "profile_risk": {
            "state": match profile_risk {
                vike_secrets::ProfileRiskSource::Rows(_) => "rows",
                vike_secrets::ProfileRiskSource::NoDatabase { .. } => "no-database",
                vike_secrets::ProfileRiskSource::TableAbsent { .. } => "table-absent",
            },
            "enforced": false,
            "read_by": serde_json::Value::Null,
            "profiles": profile_risk.profiles().unwrap_or_default().iter().map(|p| serde_json::json!({
                "profile": p.profile,
                "rows": p.rows.iter().map(|r| serde_json::json!({
                    "key": r.key,
                    "value": r.value,
                    // `null` for a row whose key is in no `[risk]` schema — see
                    // `vike_config::unknown_rows`, and note that no verb in this tree can write one.
                    "shape": vike_config::profile_risk_key(&r.key).map(|k| k.kind.as_str()),
                    "known": vike_config::profile_risk_key(&r.key).is_some(),
                })).collect::<Vec<_>>(),
                // The keys this profile does NOT set, so a machine need not diff against the
                // roster itself. Two of them REFUSE a live mount when absent — the `ceilings`
                // array above is the authority for which.
                "unset": vike_config::missing_keys(p).iter().map(|k| k.name).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        },
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
    profile_risk: &vike_secrets::ProfileRiskSource,
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&settings_json(
        d,
        secrets,
        files,
        envs,
        unknown,
        profile_risk,
    ))
    .map_err(|e| format!("cannot serialize settings JSON: {e}"))?;
    println!("{text}");
    Ok(())
}

/// An empty cell renders as `-` in the human tables, so an empty override is visibly a value rather
/// than a formatting gap. The `--json` view keeps the raw `""` — a tool must not have to un-invent
/// this placeholder.
fn dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

/// The whole human view: the header (which project am I reading?), then the requested halves.
///
/// ⚠ One parameter per THING RENDERED, past clippy's threshold since decision 0057's Phase 2 added
/// the mirrored `[risk]` rows. Bundling them into a struct is the obvious cure and is deliberately
/// not taken: every one of these is resolved from a DIFFERENT store by `execute` above — the files,
/// the process environment, the credential store, the settings database — and a struct would put
/// four independently-failing reads behind one name, which is the thing this command exists to
/// stop doing. The cost is a long signature at four call sites, all of them in this file.
#[allow(clippy::too_many_arguments)]
fn print_human(
    section: Section,
    filter: Option<&str>,
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
    profile_risk: &vike_secrets::ProfileRiskSource,
) {
    print_header(d, secrets);
    if section.files() {
        println!();
        print_file_table(files, &d.settings.warnings, d.authority, d.store_refusal.as_deref());

        // ⚠ NOT gated on `--changed-only`, and not on the settings table being non-empty. The
        // whole finding is that a pre-trade ceiling can be in force from a file this command does
        // not read, so "nothing matched above" is precisely when an operator most needs to be told
        // the other file exists.
        print_ceilings(filter, profile_risk.profiles().unwrap_or_default());
        // ...and, since decision 0057's Phase 2, the VALUES — when somebody has mirrored them.
        print_profile_risk(filter, profile_risk);
    }
    if section.env() {
        println!();
        print_env_table(envs, secrets, unknown);
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
                 working-directory-relative `{}/` store, which this command did not resolve and \
                 so cannot name.",
                secrets.keys,
                vike_secrets::SETTINGS_DIR
            );
        }
        return;
    };
    println!("settings directory: {}", dir.display());

    let width = d
        .files
        .iter()
        .map(|f| f.name.len())
        .chain(std::iter::once(secrets.label().len()))
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
    // ⚠ **The STORE THAT ANSWERED, labelled with ITS OWN file name** — `vike.db` on a migrated box.
    // This row named `secrets.env` unconditionally and counted keys the loader had read out of the
    // database, so a migrated box got the dead store's name beside the live store's count. Key
    // COUNT only either way — names are `vike-cli secrets list`'s job, values are nobody's.
    let state = if secrets.present {
        format!("present, {} key(s) — names: `vike-cli secrets list`", secrets.keys)
    } else {
        "absent".to_string()
    };
    println!("  {:<width$}  {state}", secrets.label());
    // Says DATABASE, deliberately, and for the reason `config check`'s twin row states: 0054's
    // constraint 2 is that an operator reads the store with `cat` and `sqlite3` is not installed on
    // the live box, so a `.db` path in a sentence that says "file" hands them something they cannot
    // open and no hint why.
    if matches!(secrets.backend, vike_secrets::Backend::Database(_)) {
        println!("  {:<width$}  (the settings DATABASE, not a text file)", "");
    }
    // The file the database replaced and left on disk. `ShadowedStore`'s own sentence, so this
    // surface, `config check` and `secrets list` cannot word one finding three ways.
    if let Some(s) = &secrets.shadowed {
        println!("  ⚠ {s}");
    }
}

/// The files half: one row per typed setting, its effective value, and the layer that set it.
fn print_file_table(
    rows: &[FileRow],
    warnings: &[String],
    authority: vike_config::Authority,
    store_refusal: Option<&str>,
) {
    println!("-- settings files ---------------------------------------------------------");
    // RENDERED from `vike_config::PRECEDENCE`, never typed here. This line named a per-project
    // override file for two months while every composition root passed `None` for the project
    // directory, so the layer was implemented, tested, advertised — and read by nothing. That layer
    // is now REMOVED, which is the second way a hand-written header goes false: it would still be
    // naming the file today. A header derived from the loader's own layer list can only name layers
    // that exist, in both directions, and the two reachability gates
    // (`crates/vike-config/tests/layers_are_reachable.rs`,
    // `crates/vike-cli/tests/settings_layers_reachable.rs`) hold each of them to a proof of effect.
    // ⚠ **The SOURCE line leads, and it is not decoration.** It is the `vike-cli secrets list`
    // idiom applied to settings: *which artifact answered* used to be inferable only from a path,
    // and a reader who inferred it wrongly would read every ORIGIN cell below backwards. On an
    // ADOPTED box the four files are not opened for resolution at all, so a `present` file listed
    // further down is a stale DRAFT — this line is what says so before the table is read.
    println!("source: {authority}");
    // RENDERED from `vike_config::precedence`, never typed here, and now PARAMETERISED by which
    // source answered. This line named a per-project override file for two months while every
    // composition root passed `None` for the project directory, so the layer was implemented,
    // tested, advertised — and read by nothing. That layer is now REMOVED, which is the second way
    // a hand-written header goes false: it would still be naming the file today. A header derived
    // from the loader's own layer list can only name layers that exist, in both directions, and the
    // two reachability gates (`crates/vike-config/tests/layers_are_reachable.rs`,
    // `crates/vike-cli/tests/settings_layers_reachable.rs`) hold each of them to a proof of effect.
    println!("{}", vike_config::precedence_line(authority));
    // ⚠ And the refusal LEADS the values rather than trailing them. A table printed under a store
    // nobody could open is not what a daemon on this box would resolve, and printing it without
    // saying so is the "positive confirmation of something false" this whole command exists to
    // prevent.
    if let Some(why) = store_refusal {
        println!();
        println!("⚠ SOURCE UNREADABLE — these values are NOT what a daemon would resolve.");
        println!("  {why}");
    }
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
            // ⚠ The VERDICT first, then the paragraph — and the order is the fix rather than
            // formatting. The paragraph alone named a library that reads the variable without
            // saying whether anything calls that library, so six keys' worth of it read as "export
            // the variable instead" while exporting it did nothing. An operator who stops after
            // one line must still get the true answer.
            if let Some(verdict) = r.unread_verdict {
                for line in wrap(verdict, 92) {
                    println!("      {line}");
                }
            }
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

/// **The pre-trade ceilings, and the ACT each one judges** — rendered from
/// [`vike_config::PRE_TRADE_CEILINGS`], which is the authority.
///
/// # Why this block exists at all
///
/// The table above covers the four settings TOMLs. A run profile is not one of them, and pre-trade
/// ceilings live in one: MEASURED on a live box, `[risk] max_total_exposure = 500.0` was enforced,
/// mandatory for the mount to start live, and present in **no** payload this command printed —
/// while `Policy::max_total_exposure` had been DELETED from `policy.toml` for the opposite defect
/// (declared, displayed, read by nothing). The same box carried `max_notional_per_order` in BOTH
/// files, kept in step by a `# mirrors settings/policy.toml` comment, and the two do not judge the
/// same act: the policy key guards three EDGE surfaces and never reaches `vike_exec::RiskLimits`,
/// while the profile key judges every order the core admits.
///
/// So this block answers the question the settings table structurally cannot: *what else can refuse
/// my order, and where do I write it?*
///
/// # What it deliberately does not print, and the half decision 0057 Phase 2 changed
///
/// VALUES for the run-profile rows, **from the FILE**. Resolving them there means parsing a
/// `RunProfile`, and `vike-cli` links neither `vike-core` nor `vike-exec` on purpose — the
/// `light-consumers` CI lane exists to hold it out of that closure. Inventing a second `[risk]`
/// parser here is exactly the drift this workspace forbids. Naming the ceiling, its act, its
/// absence semantics and the file that holds it is the honest shape; the ORIGIN line says which
/// rows this command read and which it did not, so a blank is never mistaken for an unset ceiling.
///
/// What CAN be printed, since Phase 2, is a value an operator has MIRRORED into the settings
/// database — `vike-cli config mirror --profile <file>`. That is why this function takes the
/// mirrored profiles: the `VALUE SHOWN` cell says *yes, below* for a key some mirrored profile
/// carries and names the repair for one nothing does, instead of the flat "this command does not
/// read that file" that was the only true answer before. The values themselves are printed by
/// [`print_profile_risk`], which is also where the sentence that matters lives: a mirrored ceiling
/// is READABLE, never ENFORCEABLE — nothing on the mount path reads a row.
fn print_ceilings(filter: Option<&str>, profiles: &[vike_secrets::StoredProfileRisk]) {
    let rows: Vec<&vike_config::Ceiling> = vike_config::PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| match filter {
            None => true,
            Some(f) => {
                let f = f.to_lowercase();
                c.name.to_lowercase().contains(&f)
                    || c.home.label().to_lowercase().contains(&f)
                    || "ceiling".contains(f.as_str())
            }
        })
        .collect();
    if rows.is_empty() {
        return;
    }

    println!();
    println!("-- pre-trade ceilings -----------------------------------------------------");
    for line in wrap(
        "Every ceiling an operator of this deployment can write, and the ACT each judges. Two \
         ceilings can share a NAME and judge different acts; nothing compares them.",
        92,
    ) {
        println!("{line}");
    }
    println!();

    let wn = rows.iter().map(|c| c.name.len()).max().unwrap_or(0).max("CEILING".len());
    let wh = rows.iter().map(|c| c.home.label().len()).max().unwrap_or(0).max("LIVES IN".len());
    println!("{:<wn$}  {:<wh$}  VALUE SHOWN", "CEILING", "LIVES IN");
    for c in &rows {
        // Three answers, not two. The third is the one Phase 2 added: a run-profile ceiling whose
        // value this command CAN print, because an operator mirrored the profile it lives in.
        let mirrored: Vec<&str> = profiles
            .iter()
            .filter(|p| p.rows.iter().any(|r| r.key == c.name))
            .map(|p| p.profile.as_str())
            .collect();
        let shown = if c.home.value_shown_by_config_show() {
            "yes — in the settings table above".to_string()
        } else if !mirrored.is_empty() {
            format!("yes — from the mirrored {} below", mirrored.join(" / "))
        } else {
            "no — `config mirror --profile <file>` is what makes it readable here".to_string()
        };
        println!("{:<wn$}  {:<wh$}  {shown}", c.name, c.home.label());
    }

    // The label rides the FIRST line of each wrapped paragraph and the rest hangs under it. The
    // obvious alternative — repeating the label on every line — was tried and reads as several
    // separate claims rather than one sentence, which is the opposite of what this block is for.
    let para = |label: &str, text: &str| {
        for (i, line) in wrap(text, 84).into_iter().enumerate() {
            if i == 0 {
                println!("    {label:<9} {line}");
            } else {
                println!("    {:<9} {line}", "");
            }
        }
    };
    for c in &rows {
        println!();
        println!("{} — {}", c.name, c.home.label());
        para("guards:", c.guards);
        if c.enforced_at.is_empty() {
            para("refuses:", "NOTHING refuses on this key from this file.");
        } else {
            for s in c.enforced_at {
                para("refuses:", &format!("{} — {}", s.what, s.file));
            }
        }
        para("if unset:", c.absent_means);
    }

    // The relationship itself, DERIVED from the table rather than typed here — add a second home
    // for a key and this paragraph appears with no edit. It is the last thing printed because it is
    // the thing an operator most needs and least expects.
    let shared: Vec<&str> = vike_config::shared_names()
        .into_iter()
        .filter(|n| rows.iter().any(|c| c.name == *n))
        .collect();
    for name in shared {
        let homes: Vec<&str> = vike_config::ceilings_named(name).map(|c| c.home.label()).collect();
        println!();
        for line in wrap(
            &format!(
                "⚠ `{name}` is written in {} places ({}) and they are NOT one ceiling. Nothing \
                 compares the two numbers — not at load, not at mount, not at submit — and neither \
                 refusal mentions the other. Read each row's `guards` above before assuming the \
                 stricter one is in force for the act you care about.",
                homes.len(),
                homes.join(" and "),
            ),
            92,
        ) {
            println!("  {line}");
        }
    }
}

/// **The mirrored run-profile `[risk]` values** —
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 2, and the
/// answer to the one question [`print_ceilings`] above could previously only pose.
///
/// # Why this block is worth a screen
///
/// The ceilings block names two keys that REFUSE a live mount when absent and judge every order the
/// core admits — and it could not print their numbers, because they live in a run profile and this
/// command links neither of the crates that parse one. So an operator could read *what can refuse
/// my order* and never *at what size*. Mirroring the profile into the settings database
/// (`config mirror --profile`) puts the values somewhere a light binary CAN read, **with the daemon
/// down and with no `sqlite3` on the box** — which is 0054's read-back requirement, reaching a file
/// 0054 had not opened.
///
/// # ⚠ The sentence this block must never stop printing
///
/// A mirrored row is READABLE, not ENFORCEABLE. Nothing on the mount path reads one
/// (`crates/vike-ops/tests/profile_risk_readers_gate.rs` is the gate), so the profile FILE is still
/// the only thing that builds a `vike_exec::ProfileRisk`, judges an order, or satisfies
/// `vike_mount::require_live_risk_budget`. A table an operator can read is one step from a table an
/// operator believes is in force, and the distance is one unchallenged sentence.
///
/// # …and the one it must print when it has nothing
///
/// An unmirrored box, a store that predates the table, and a mirrored profile that genuinely sets
/// no ceiling are three different facts, and the middle one is not an absence of ceilings — the
/// FILE may hold every one of them. So the empty case prints the store's own account of itself
/// rather than nothing at all; a silent block would read as "no ceilings", which is the worst thing
/// this command could say about a live box.
fn print_profile_risk(filter: Option<&str>, source: &vike_secrets::ProfileRiskSource) {
    let matches = |hay: &str| match filter {
        None => true,
        Some(f) => hay.to_lowercase().contains(&f.to_lowercase()),
    };
    // The block's own words are filterable like every other, so `--filter risk` reaches it.
    let block_matched = filter.is_none_or(|f| {
        let f = f.to_lowercase();
        "run profile [risk]".contains(f.as_str()) || "ceiling".contains(f.as_str())
    });

    let profiles: Vec<&vike_secrets::StoredProfileRisk> = source
        .profiles()
        .unwrap_or_default()
        .iter()
        .filter(|p| block_matched || matches(&p.profile) || p.rows.iter().any(|r| matches(&r.key)))
        .collect();
    if profiles.is_empty() && !block_matched {
        return;
    }

    println!();
    println!("-- run profile [risk], as mirrored ----------------------------------------");
    for line in wrap(
        "The live pre-trade ceilings, read from the settings database rather than from the profile \
         file — so this box can answer with the daemon down. ⚠ READABLE, NOT ENFORCEABLE: nothing \
         on the mount path reads these rows. The profile FILE is still what judges an order and \
         what a live mount's risk-budget refusal consults, so a row that disagrees with the file \
         means the mirror is STALE, never that the ceiling has changed.",
        92,
    ) {
        println!("{line}");
    }
    for line in wrap(
        "Which profile is LIVE is decided by VIKE_RUN_PROFILE (or --profile) on the daemon, never \
         by this table: a row is keyed by a file NAME and selects nothing.",
        92,
    ) {
        println!("{line}");
    }
    println!();

    if profiles.is_empty() {
        // Never silence. See this function's doc: "nothing mirrored" and "no ceilings" are
        // different facts and only one of them is safe to read as an absence.
        println!("  {source}");
        println!(
            "  `vike-cli config mirror --profile <file>` is what puts a profile's ceilings here."
        );
        return;
    }

    for p in profiles {
        println!("{}", p.profile);
        let rows: Vec<&vike_secrets::ProfileRiskRow> = p
            .rows
            .iter()
            .filter(|r| block_matched || matches(&r.key) || matches(&p.profile))
            .collect();
        if rows.is_empty() {
            println!("  (this profile sets no [risk] key that matches the filter)");
        } else {
            let wk = rows.iter().map(|r| r.key.len()).max().unwrap_or(0).max("KEY".len());
            let wv = rows.iter().map(|r| r.value.len()).max().unwrap_or(0).max("VALUE".len());
            println!("  {:<wk$}  {:<wv$}  BOUNDS", "KEY", "VALUE");
            for r in &rows {
                match vike_config::profile_risk_key(&r.key) {
                    Some(k) => {
                        for (i, line) in wrap(k.what, 60).into_iter().enumerate() {
                            if i == 0 {
                                println!("  {:<wk$}  {:<wv$}  {line}", r.key, r.value);
                            } else {
                                println!("  {:<wk$}  {:<wv$}  {line}", "", "");
                            }
                        }
                    }
                    // A row whose key is in no `[risk]` schema. No verb in this tree can write
                    // one, so it arrived by a hand `INSERT`, a restored backup or a migration
                    // written elsewhere — and it configures NOTHING. Saying so is the read half of
                    // the roster's refusal; rendering it as a ceiling would be the defect a
                    // database introduces that a file does not.
                    None => println!(
                        "  {:<wk$}  {:<wv$}  ⚠ UNKNOWN KEY — no `[risk]` field goes by this name, \
                         so it bounds nothing",
                        r.key, r.value
                    ),
                }
            }
        }

        // The unset half, and the two that are not merely unset.
        let missing = vike_config::missing_keys(p);
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|k| k.name).collect();
            for line in wrap(&format!("not set here: {}", names.join(", ")), 88) {
                println!("  {line}");
            }
        }
        for k in &missing {
            // The flag comes from the ceilings table, which is its one authority — see
            // `vike_config::ceiling_for`, which is the join rather than a second copy.
            if vike_config::ceiling_for(k.name).is_some_and(|c| c.refuses_live_mount_when_absent) {
                for line in wrap(
                    &format!(
                        "⚠ `{}` is UNSET in this profile, and a LIVE venue mount REFUSES TO START \
                         without it. On a paper or backtest mount it is simply no ceiling.",
                        k.name
                    ),
                    88,
                ) {
                    println!("  {line}");
                }
            }
        }
        println!();
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
fn print_env_table(rows: &[Resolved], secrets: &StoreStatus, unknown: &UnknownKeys) {
    println!("-- environment variables --------------------------------------------------");
    // The middle rung is NAMED from the store that answered, never from a constant: this line read
    // `env > secrets.env > default` on every box, including the ones where that file had stopped
    // being read — a precedence claim about a store nothing consults.
    println!(
        "precedence: env > {} > default   (READS = what the reader consults)",
        secrets.label()
    );
    println!();
    if rows.is_empty() {
        println!("(no settings matched)");
        // …but a store key matching NO row is exactly the case a filter with no matching row can
        // still have found, so the complement is printed on this path too.
        print_unknown_store_keys(secrets, unknown);
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
    // `database` is the longest source word; `caller-map` the longest reads word.
    let ws = "database".len().max("SOURCE".len());
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
            "! {} row(s) take their value from {}, but that crate reads the variable \
             with a direct `env::var`:",
            stranded.len(),
            secrets.label()
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

    // ⚠ THE OTHER DIAGNOSIS THE `READS` COLUMN CANNOT CARRY, and this command used to contradict
    // itself over it. `READS` says WHERE the read is; it says nothing about whether anything runs
    // that code. For a handful of flags nothing does — the read sits inside a poller no
    // composition root constructs — and the FILE half of this very command already prints
    // "NEITHER SPELLING DOES ANYTHING" for them. Printing those variables here as ordinary rows
    // under a header promising "READS = what the reader consults" was the same false positive
    // confirmation one section up, in the same invocation. The verdict is DERIVED
    // (`vike_config::env_verdict` over FLAG_REGISTRY x CONSUMPTION) so the two halves cannot drift.
    let dead: Vec<(&Resolved, &str)> =
        rows.iter().filter_map(|r| vike_config::env_verdict(r.name).map(|v| (r, v))).collect();
    if !dead.is_empty() {
        println!();
        println!(
            "! {} row(s) are read by code no shipped binary runs, so EXPORTING THEM CHANGES \
             NOTHING:",
            dead.len()
        );
        for (r, _) in &dead {
            println!("    {} ({})", r.name, r.krate);
        }
        println!(
            "  the feature is unmounted, not removed. `vike-cli config show --section file` \
             prints the"
        );
        println!("  per-key verdict and the reason; `--json` carries it as `reader_verdict`.");
    }

    print_unknown_store_keys(secrets, unknown);
}

/// The env table's COMPLEMENT: keys the store holds that no registry row covers. Silent when there
/// are none, so a block here always means something is genuinely unaccounted for.
///
/// See [`UnknownKeys`] for why the named/counted split is what it is.
fn print_unknown_store_keys(secrets: &StoreStatus, unknown: &UnknownKeys) {
    if unknown.is_empty() {
        return;
    }
    println!();
    // Named from the store the keys were actually READ OUT OF. The old spelling was a hardcoded
    // `settings/secrets.env`, which on a migrated box pointed the operator at a file that does not
    // hold the key it is complaining about.
    println!(
        "! {} holds key(s) that match NO row above — nothing else in this tool would",
        secrets.named()
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

    /// The UNMIGRATED box — the backend every case below resolves against unless it is about the
    /// database. `vike_secrets::Backend::Files` is a unit variant, so this is a `const` and every
    /// call site reads as the fact it asserts: *this box has no settings database*.
    const FILES: vike_secrets::Backend = vike_secrets::Backend::Files;

    /// The MIGRATED box. A path, never a probe: `resolve` is pure and the backend is its
    /// parameter, so nothing here has to put a file on disk to test the word it prints.
    fn database() -> vike_secrets::Backend {
        vike_secrets::Backend::Database(PathBuf::from("/p/settings/db/vike.db"))
    }

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
        let r = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[]), &FILES);
        assert_eq!(r.source, Source::Default);
        assert_eq!(r.value, "127.0.0.1");
        assert_eq!(r.default, "127.0.0.1");
        assert!(!r.secret);
    }

    #[test]
    fn the_dotenv_beats_the_default() {
        let r = resolve(
            &row("ACME_HOST", "127.0.0.1"),
            &map(&[]),
            &map(&[("ACME_HOST", "the CI box")]),
            &FILES,
        );
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
            &FILES,
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
            &FILES,
        );
        assert_eq!(r.source, Source::Env);
        assert_eq!(r.value, "");
    }

    #[test]
    fn the_source_words_are_pinned() {
        assert_eq!(Source::Default.as_str(), "default");
        assert_eq!(Source::Dotenv.as_str(), "dotenv");
        assert_eq!(Source::Env.as_str(), "env");
        // The fourth word. It is `vike-cli secrets list --json`'s spelling for the same store, so
        // the two disclosure surfaces name one artifact one way.
        assert_eq!(Source::Database.as_str(), "database");
    }

    /// **THE FIX.** The same store map, the same registry row, the same value — and the SOURCE
    /// word follows the store that actually answered, because it is a parameter rather than an
    /// assumption.
    ///
    /// Before this, a migrated box printed `dotenv` for a value read out of
    /// `<project>/settings/db/vike.db` and pointed the operator at a file that had stopped being
    /// read: positive confirmation of something false, which is the exact failure
    /// `docs/decisions/0054-settings-move-into-one-database.md`'s constraint 2 names.
    #[test]
    fn the_source_word_names_the_store_that_answered() {
        let (row, store) = (row("ACME_HOST", "127.0.0.1"), map(&[("ACME_HOST", "the CI box")]));

        let unmigrated = resolve(&row, &map(&[]), &store, &FILES);
        assert_eq!(unmigrated.source, Source::Dotenv, "an unmigrated box is unchanged");

        let migrated = resolve(&row, &map(&[]), &store, &database());
        assert_eq!(migrated.source, Source::Database);
        // The VALUE is the same either way: only the provenance was ever wrong.
        assert_eq!(unmigrated.value, migrated.value);
        assert_eq!(unmigrated.value, "the CI box");
    }

    /// The precedence is untouched by which store answers: the process environment still wins, and
    /// an absent key still falls to its documented default rather than to the other store. That
    /// second half is the per-KEY ladder `docs/decisions/0051` forbids, and this is the assertion
    /// that it was not smuggled in with the fourth word.
    #[test]
    fn the_backend_moves_the_word_and_never_the_precedence() {
        let migrated = database();
        let r = resolve(
            &row("ACME_HOST", "127.0.0.1"),
            &map(&[("ACME_HOST", "from-env")]),
            &map(&[("ACME_HOST", "from-the-database")]),
            &migrated,
        );
        assert_eq!(r.source, Source::Env, "the process env still outranks the store");
        assert_eq!(r.value, "from-env");

        let unset = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[]), &migrated);
        assert_eq!(unset.source, Source::Default);
        assert_eq!(unset.value, "127.0.0.1");
    }

    /// The stranded-row diagnosis is about a value not being EXPORTED, so it holds for both stores.
    /// It was keyed on `Source::Dotenv` alone, which would have silently stopped firing on every
    /// migrated box — a diagnosis that goes quiet exactly where the store moved under the reader.
    #[test]
    fn the_stranded_flag_survives_the_database() {
        let r = resolve(
            &row("ACME_CONTROL_KEY", ""),
            &map(&[]),
            &map(&[("ACME_CONTROL_KEY", "k")]),
            &database(),
        );
        assert_eq!(r.source, Source::Database);
        assert_eq!(r.reads, Reads::ProcessEnv);
        assert!(r.store_may_not_reach_reader());
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
        let r = resolve(
            &row("ACME_CONTROL_KEY", ""),
            &map(&[]),
            &map(&[("ACME_CONTROL_KEY", "k")]),
            &FILES,
        );
        assert_eq!(r.source, Source::Dotenv, "the store DOES hold it — that stays true");
        assert_eq!(r.reads, Reads::ProcessEnv);
        assert!(r.store_may_not_reach_reader());
    }

    /// ...and the three shapes that are NOT that failure stay unflagged: a map-reading row (the
    /// caller may well hand it the store), an exported value, and an unset one.
    #[test]
    fn the_flag_is_narrow() {
        let mapped =
            resolve(&injected_row("ACME_HOST", ""), &map(&[]), &map(&[("ACME_HOST", "x")]), &FILES);
        assert_eq!(mapped.reads, Reads::CallerMap);
        assert!(!mapped.store_may_not_reach_reader());

        let exported =
            resolve(&row("ACME_HOST", ""), &map(&[("ACME_HOST", "x")]), &map(&[]), &FILES);
        assert!(!exported.store_may_not_reach_reader());

        let unset = resolve(&row("ACME_HOST", ""), &map(&[]), &map(&[]), &FILES);
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
        let r = resolve(store_only[0], &map(&[]), &map(&[(name, "x")]), &FILES);
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
            let r = resolve(&row("ACME_API_KEY", ""), &env, &dotenv, &FILES);
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
        assert_eq!(
            resolve(&secret, &map(&[("ACME_API_KEY", "k")]), &map(&[]), &FILES).source,
            Source::Env
        );
        assert_eq!(
            resolve(&secret, &map(&[]), &map(&[("ACME_API_KEY", "k")]), &FILES).source,
            Source::Dotenv
        );
        assert_eq!(resolve(&secret, &map(&[]), &map(&[]), &FILES).source, Source::Default);
    }

    #[test]
    fn an_unconfigured_or_empty_secret_prints_unset() {
        // nothing configured it
        assert_eq!(resolve(&row("ACME_API_KEY", ""), &map(&[]), &map(&[]), &FILES).value, UNSET);
        // present but empty — configured, but there is no key there to call `<set>`
        let r = resolve(&row("ACME_API_KEY", ""), &map(&[("ACME_API_KEY", "")]), &map(&[]), &FILES);
        assert_eq!(r.value, UNSET);
        assert_eq!(r.source, Source::Env, "still reports that env held the key");
    }

    /// No credential row today declares a non-empty default; if one ever did, the DEFAULT column
    /// must not become the leak the VALUE column is not.
    #[test]
    fn a_nonempty_secret_default_is_redacted_too() {
        let r = resolve(&row("ACME_API_KEY", "hardcoded"), &map(&[]), &map(&[]), &FILES);
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
        let all = resolve_all(&env, &dotenv, &FILES, None, false);
        assert_eq!(all.len(), SETTINGS.len(), "one row per (name, krate) pair");
        // With both stores empty every row falls through to its default, so the "what have I
        // configured?" view is empty.
        assert!(resolve_all(&env, &dotenv, &FILES, None, true).is_empty());
    }

    #[test]
    fn changed_only_keeps_the_rows_a_store_actually_set() {
        let name = SETTINGS[0].name;
        let env = map(&[(name, "x")]);
        let rows = resolve_all(&env, &map(&[]), &FILES, None, true);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.source == Source::Env));
        assert!(rows.iter().all(|r| r.name == name), "only the configured name survives");
    }

    #[test]
    fn the_filter_matches_name_or_crate_case_insensitively() {
        let (env, dotenv) = (map(&[]), map(&[]));
        let by_crate = resolve_all(&env, &dotenv, &FILES, Some("VIKE-CLI"), false);
        assert!(by_crate.iter().all(|r| r.krate.contains("vike-cli")));

        let name = SETTINGS[0].name;
        let by_name = resolve_all(&env, &dotenv, &FILES, Some(&name.to_ascii_lowercase()), false);
        assert!(by_name.iter().any(|r| r.name == name));

        assert!(
            resolve_all(&env, &dotenv, &FILES, Some("no-such-setting-anywhere"), false).is_empty()
        );
    }

    #[test]
    fn rows_are_sorted_by_name_then_crate() {
        let rows = resolve_all(&map(&[]), &map(&[]), &FILES, None, false);
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
        assert!(
            parse_args(["--filter".to_string()].into_iter())
                .unwrap_err()
                .contains("requires a value")
        );
        assert!(
            parse_args(["--json=1".to_string()].into_iter())
                .unwrap_err()
                .contains("takes no value")
        );
        assert!(
            parse_args(["--section=nope".to_string()].into_iter())
                .unwrap_err()
                .contains("files|env|all")
        );
        assert_eq!(parse_args(["-h".to_string()].into_iter()).unwrap_err(), "help requested");
    }

    // -- the printers --------------------------------------------------------------------------

    fn empty_store() -> StoreStatus {
        StoreStatus { backend: FILES, path: None, present: false, keys: 0, shadowed: None }
    }

    /// A box that has never mirrored a run profile — what every deployment looks like the day this
    /// lands, and the case where the ceilings block must still name the keys it cannot value.
    fn no_profiles() -> vike_secrets::ProfileRiskSource {
        vike_secrets::ProfileRiskSource::NoDatabase {
            path: PathBuf::from("/p/settings/db/vike.db"),
        }
    }

    /// A box migrated BEFORE 0057 Phase 2 — the third state, which must not read as "no ceilings".
    fn table_absent() -> vike_secrets::ProfileRiskSource {
        vike_secrets::ProfileRiskSource::TableAbsent {
            path: PathBuf::from("/p/settings/db/vike.db"),
        }
    }

    /// A mirrored `run-live.toml`: one of the two mount-refusing ceilings set, the other NOT (so
    /// the unset warning is exercised), plus a row whose key is in no `[risk]` schema — the one a
    /// hand `INSERT` produces and no verb in this tree can write.
    fn mirrored_profiles() -> vike_secrets::ProfileRiskSource {
        vike_secrets::ProfileRiskSource::Rows(vec![vike_secrets::StoredProfileRisk {
            profile: "run-live.toml".to_string(),
            rows: vec![
                vike_secrets::ProfileRiskRow {
                    key: "max_levrage".to_string(),
                    value: "9.0".to_string(),
                },
                vike_secrets::ProfileRiskRow {
                    key: "max_notional_per_order".to_string(),
                    value: "250.0".to_string(),
                },
            ],
        }])
    }

    /// The MIGRATED box's store status, with the credential file still sitting there unread — the
    /// one configuration every sentence this change touches used to describe wrongly.
    fn migrated_store() -> StoreStatus {
        let db = PathBuf::from("/p/settings/db/vike.db");
        let file = PathBuf::from("/p/settings/secrets.env");
        StoreStatus {
            backend: vike_secrets::Backend::Database(db.clone()),
            path: Some(db.clone()),
            present: true,
            keys: 3,
            shadowed: Some(vike_secrets::ShadowedStore { file, db }),
        }
    }

    #[test]
    fn both_printers_render_everything_without_panicking() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let files = file_rows(&d, None, false);
        let envs = resolve_all(&map(&[]), &map(&[]), &FILES, None, false);
        let unknown = unknown_store_keys(&map(&[("ACME_TYPOD", "1"), ("ACME_API_KEY", "k")]), None);
        print_human(
            Section::All,
            None,
            &d,
            &empty_store(),
            &files,
            &envs,
            &unknown,
            &no_profiles(),
        );
        print_json(&d, &empty_store(), &files, &envs, &unknown, &mirrored_profiles()).unwrap();
        // and the empty views
        let none = UnknownKeys::default();
        print_human(
            Section::Files,
            None,
            &d,
            &empty_store(),
            &[],
            &[],
            &none,
            &mirrored_profiles(),
        );
        print_human(Section::Env, None, &d, &empty_store(), &[], &[], &none, &no_profiles());
        // …and the filtered-to-nothing env table WITH a complement, the one path that would
        // otherwise return before printing it.
        print_human(Section::Env, None, &d, &empty_store(), &[], &[], &unknown, &no_profiles());
        print_json(&d, &empty_store(), &[], &[], &none, &no_profiles()).unwrap();
        // …and the MIGRATED box, whose header grows two extra lines (the DATABASE qualifier and
        // the shadowed-file finding) that no other case reaches.
        print_human(
            Section::All,
            None,
            &d,
            &migrated_store(),
            &files,
            &envs,
            &unknown,
            &table_absent(),
        );
        print_json(&d, &migrated_store(), &files, &envs, &unknown, &table_absent()).unwrap();
    }

    /// **The store the header, the precedence line and the complement all NAME** — one answer,
    /// derived from the backend, so the three sentences cannot disagree with each other or with the
    /// SOURCE column beside them.
    #[test]
    fn the_store_labels_follow_the_backend() {
        let unmigrated = empty_store();
        assert_eq!(unmigrated.label(), vike_secrets::SECRETS_FILE);
        assert_eq!(unmigrated.kind(), "file");
        // With no settings directory there is no path to print, so the sentence falls back to the
        // conventional shorthand rather than to nothing.
        assert_eq!(
            unmigrated.named(),
            format!("{}/{}", vike_secrets::SETTINGS_DIR, vike_secrets::SECRETS_FILE)
        );

        let migrated = migrated_store();
        assert_eq!(migrated.label(), vike_secrets::DB_FILE);
        assert_eq!(migrated.kind(), "database");
        assert!(migrated.named().ends_with("vike.db"), "{}", migrated.named());
    }

    /// **[`store_status`] is the one probe, and it describes ONE store.** The defect it replaced
    /// was a struct whose path and presence bit described `secrets.env` while its key count came
    /// out of the database that had shadowed it.
    #[test]
    fn the_store_status_describes_the_store_that_answers() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(vike_secrets::SECRETS_FILE), "A=1\n").expect("the file");

        // No database on disk ⇒ the file answers, exactly as it did before 0054.
        let files = store_status(Some(dir.path()), 1);
        assert_eq!(files.backend, vike_secrets::Backend::Files);
        assert_eq!(files.path.as_deref(), Some(dir.path().join("secrets.env").as_path()));
        assert!(files.present);
        assert!(files.shadowed.is_none(), "nothing shadows anything on an unmigrated box");

        // A database beside it ⇒ IT answers, and the file it left behind is a FINDING.
        let db = dir.path().join(vike_secrets::DB_DIR).join(vike_secrets::DB_FILE);
        std::fs::create_dir_all(db.parent().expect("db dir")).expect("db dir");
        std::fs::write(&db, b"").expect("the database");
        let migrated = store_status(Some(dir.path()), 3);
        assert_eq!(migrated.backend, vike_secrets::Backend::Database(db.clone()));
        assert_eq!(
            migrated.path.as_deref(),
            Some(db.as_path()),
            "the DATABASE's path, not the file's"
        );
        assert!(migrated.present);
        let shadowed = migrated.shadowed.expect("the file is still on disk and is not read");
        assert_eq!(shadowed.file, dir.path().join(vike_secrets::SECRETS_FILE));
        assert!(
            shadowed.to_string().contains("NO LONGER READ"),
            "the finding must carry `vike_secrets::ShadowedStore`'s own words, not a second \
             spelling of them: {shadowed}"
        );

        // …and a migrated project whose file was retired reports no finding to make.
        std::fs::remove_file(dir.path().join(vike_secrets::SECRETS_FILE)).expect("retire it");
        assert!(store_status(Some(dir.path()), 3).shadowed.is_none());
    }

    #[test]
    fn the_json_document_carries_the_documented_fields() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let files = file_rows(&d, Some("policy"), false);
        let envs = resolve_all(&map(&[]), &map(&[]), &FILES, Some("vike-cli"), false);
        assert!(!files.is_empty() && !envs.is_empty());
        let doc = settings_json(
            &d,
            &empty_store(),
            &files,
            &envs,
            &UnknownKeys::default(),
            &mirrored_profiles(),
        );

        for field in [
            "settings_dir",
            "files",
            "secrets",
            "warnings",
            "settings",
            "env",
            "ceilings",
            "profile_risk",
            "unknown_env_keys",
        ] {
            assert!(doc.get(field).is_some(), "missing {field}");
        }
        for field in ["named", "credential_shaped"] {
            assert!(
                doc["unknown_env_keys"].get(field).is_some(),
                "missing unknown_env_keys.{field}"
            );
        }
        // The store object names WHICH store answered, not just where it is: a `path` alone cannot
        // tell a consumer whether the thing at the end of it is a text file it may `cat`.
        for field in ["path", "kind", "present", "keys", "shadowed"] {
            assert!(doc["secrets"].get(field).is_some(), "missing secrets.{field}");
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

    /// **The `--json` document names the store that answered too**, because the machine surface
    /// lied in exactly the same way the human one did: `secrets.path` was the credential FILE's,
    /// beside a `keys` count read out of the database.
    #[test]
    fn the_json_store_object_names_the_database_when_it_answers() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let envs = resolve_all(&map(&[]), &map(&[]), &database(), Some("vike-cli"), false);
        let doc = settings_json(
            &d,
            &migrated_store(),
            &[],
            &envs,
            &UnknownKeys::default(),
            &no_profiles(),
        );

        assert_eq!(doc["secrets"]["kind"], "database");
        assert!(
            doc["secrets"]["path"].as_str().expect("a path").ends_with("vike.db"),
            "{}",
            doc["secrets"]["path"]
        );
        assert!(
            doc["secrets"]["shadowed"]["file"]
                .as_str()
                .expect("the shadowed file")
                .ends_with("secrets.env"),
            "the file the database replaced must be disclosed, not silently dropped: {}",
            doc["secrets"]["shadowed"]
        );

        // …and the unmigrated box's document is the one it always was.
        let plain =
            settings_json(&d, &empty_store(), &[], &envs, &UnknownKeys::default(), &no_profiles());
        assert_eq!(plain["secrets"]["kind"], "file");
        assert_eq!(plain["secrets"]["shadowed"], serde_json::Value::Null);
    }

    /// **The disclosure half of the two-ceilings finding**, asserted on the SERIALIZED document.
    ///
    /// `vike_config::PRE_TRADE_CEILINGS` can be a perfectly correct table and still change nothing
    /// for an operator if this command does not render it — which is exactly the state
    /// `[risk] max_total_exposure` was in: enforced, mandatory for a live mount, MEASURED at 500 on
    /// a live box, and present in no payload `config show` printed. So the properties pinned here
    /// are the ones an operator's question depends on, not the table's own shape (that is
    /// `crates/vike-config/tests/ceilings_are_distinct.rs`'s job):
    ///
    /// 1. the array exists and carries EVERY row, unfiltered — a machine asking "what can refuse
    ///    this order" must not get an answer that depends on a human's `--filter`;
    /// 2. `max_total_exposure` is in it, with `value_shown: false` — the row whose invisibility
    ///    started this, saying out loud that the number lives in a file this command did not read;
    /// 3. the same-named pair is marked `shares_its_name` on BOTH rows with DIFFERENT `guards`, so
    ///    a reader of the JSON alone cannot conclude they are one ceiling.
    #[test]
    fn the_json_document_discloses_every_pre_trade_ceiling() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        // Deliberately the FILTERED-to-one-key view: the ceilings array must be complete anyway.
        let files = file_rows(&d, Some("halt_admit"), false);
        let doc =
            settings_json(&d, &empty_store(), &files, &[], &UnknownKeys::default(), &no_profiles());

        let rows = doc["ceilings"].as_array().expect("`ceilings` missing from the JSON document");
        assert_eq!(
            rows.len(),
            vike_config::PRE_TRADE_CEILINGS.len(),
            "the ceilings array must carry every row regardless of --filter"
        );
        for field in ["name", "home", "value_shown", "guards", "enforced", "absent_means"] {
            assert!(rows[0].get(field).is_some(), "missing {field} in {}", rows[0]);
        }

        let exposure: Vec<&serde_json::Value> =
            rows.iter().filter(|r| r["name"] == "max_total_exposure").collect();
        assert_eq!(exposure.len(), 1, "`max_total_exposure` must be disclosed exactly once");
        assert_eq!(
            exposure[0]["value_shown"], false,
            "this command reads no run profile; claiming it showed the value would be the \
             positive-confirmation-of-something-false defect the READ column exists for"
        );
        assert_eq!(exposure[0]["enforced"], true, "it IS enforced — that is the whole point");

        let notional: Vec<&serde_json::Value> =
            rows.iter().filter(|r| r["name"] == "max_notional_per_order").collect();
        assert_eq!(notional.len(), 2, "the finding is that TWO files carry this key");
        assert_ne!(
            notional[0]["guards"], notional[1]["guards"],
            "two ceilings sharing a name must disclose different acts, or the JSON reads as one"
        );
        assert_ne!(notional[0]["home"], notional[1]["home"]);
        for r in &notional {
            assert_eq!(r["shares_its_name"], true, "{r} must be flagged as sharing its name");
        }
    }

    /// The human view of the same, driven through the real printer so a panic or a missing block
    /// is caught: the filtered-to-nothing table is exactly the view where an operator most needs
    /// to be told a ceiling lives in a file this command does not read.
    #[test]
    fn the_ceilings_block_renders_filtered_and_unfiltered() {
        print_ceilings(None, &[]);
        print_ceilings(Some("max_notional_per_order"), &[]);
        print_ceilings(Some("run profile"), mirrored_profiles().profiles().unwrap());
        // A filter that matches no ceiling must print nothing rather than an empty table.
        print_ceilings(Some("zzz-no-such-ceiling"), &[]);
    }

    /// The Phase-2 block, driven through the real printer in all four states it can be in — the
    /// three `ProfileRiskSource` arms plus the filtered views — so a panic or an unreachable branch
    /// is caught. What it ASSERTS is the one sentence that must never disappear, and it asserts it
    /// on captured output rather than on the source: see the test below.
    #[test]
    fn the_profile_risk_block_renders_in_every_state() {
        print_profile_risk(None, &no_profiles());
        print_profile_risk(None, &table_absent());
        print_profile_risk(None, &mirrored_profiles());
        print_profile_risk(Some("max_notional"), &mirrored_profiles());
        print_profile_risk(Some("run-live"), &mirrored_profiles());
        // A filter matching neither the block's own words nor any row: nothing is printed.
        print_profile_risk(Some("zzz-no-such-key"), &mirrored_profiles());
        // ...and a mirrored profile carrying no rows at all, which is a legitimate mirror of a
        // profile that sets no ceiling and must not read as "never mirrored".
        print_profile_risk(
            None,
            &vike_secrets::ProfileRiskSource::Rows(vec![vike_secrets::StoredProfileRisk {
                profile: "run-paper.toml".to_string(),
                rows: Vec::new(),
            }]),
        );
    }

    /// **The disclosure this block exists for, asserted on the SERIALIZED document** — the same
    /// shape the ceilings half is asserted in, and for the same reason: a correct table that
    /// nothing renders changes nothing.
    ///
    /// Three claims, and the middle one is the load-bearing one:
    ///
    /// 1. a mirrored VALUE is present, which is the whole of what Phase 2 buys;
    /// 2. it is marked NOT enforced and read by NOTHING, so a machine cannot read a mirrored
    ///    ceiling as a ceiling in force — the file is still what judges an order;
    /// 3. a row whose key is in no `[risk]` schema is marked `known: false` rather than rendered
    ///    as a ceiling.
    #[test]
    fn the_json_document_carries_the_mirrored_ceilings_and_calls_them_unenforced() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let doc = settings_json(
            &d,
            &empty_store(),
            &[],
            &[],
            &UnknownKeys::default(),
            &mirrored_profiles(),
        );
        let block = &doc["profile_risk"];
        assert_eq!(block["state"], "rows");
        assert_eq!(block["enforced"], false, "a mirrored ceiling is READABLE, never ENFORCEABLE");
        assert!(block["read_by"].is_null(), "nothing on the mount path reads these rows");

        let rows = block["profiles"][0]["rows"].as_array().expect("rows missing");
        let notional = rows
            .iter()
            .find(|r| r["key"] == "max_notional_per_order")
            .expect("the mirrored ceiling must be present — this is what Phase 2 buys");
        assert_eq!(notional["value"], "250.0");
        assert_eq!(notional["known"], true);
        assert_eq!(notional["shape"], "float");

        let bogus = rows.iter().find(|r| r["key"] == "max_levrage").expect("row missing");
        assert_eq!(bogus["known"], false, "a key in no `[risk]` schema bounds nothing");
        assert!(bogus["shape"].is_null());

        // The complement: the ceiling this profile does NOT set is named as unset, which is how a
        // machine learns that a live mount would refuse to start.
        let unset = block["profiles"][0]["unset"].as_array().expect("unset missing");
        assert!(
            unset.iter().any(|k| k.as_str() == Some("max_total_exposure")),
            "an unset mount-refusing ceiling must be named: {unset:?}"
        );
    }

    /// The THREE arms are three different facts, and the JSON must not collapse them. An
    /// unmirrored box and a box whose store predates the table both have no rows, and neither of
    /// them means "this profile sets no ceilings".
    #[test]
    fn an_unmirrored_box_and_a_pre_phase_two_store_are_distinguishable_in_the_json() {
        let d = vike_config::describe(None, &map(&[])).unwrap();
        let of = |src: &vike_secrets::ProfileRiskSource| {
            settings_json(&d, &empty_store(), &[], &[], &UnknownKeys::default(), src)["profile_risk"]
                ["state"]
                .clone()
        };
        assert_eq!(of(&no_profiles()), "no-database");
        assert_eq!(of(&table_absent()), "table-absent");
        assert_eq!(of(&mirrored_profiles()), "rows");
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
        let envs = vec![resolve(&row("ACME_API_KEY", ""), &env, &map(&[]), &FILES)];
        let store = StoreStatus { present: true, keys: 42, ..empty_store() };
        let unknown = unknown_store_keys(
            &map(&[("ACME_API_KEY", LEAK), ("SOMEVENUE_LIVE_API_SECRET", LEAK)]),
            None,
        );
        let text =
            serde_json::to_string(&settings_json(&d, &store, &[], &envs, &unknown, &no_profiles()))
                .unwrap();
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
