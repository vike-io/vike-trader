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
//! consults: when this was written `VIKE_TRADEHUB_CONTROL_KEY` was `Binary` in `vike-app` (reading
//! the credential map) and `Library` in `vike-cli` (reading process env) — the exact inverse of any
//! layer-based rule. (Every row of it is an `Injected` map lookup now.)
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
//! was measured to do nothing, which was CORRECT behaviour (it was vike-app's strategy-state
//! SIDECAR, not the `settings/state/` root the README names) — reported as though it had worked.
//! And `state_dir` is the obvious name for the directory an operator is looking for, so it was
//! precisely the row they landed on. (`config.state_dir` is deleted since: its one reader went with
//! the desktop's local core, and `consumed.rs` records the key's removal where its row stood.)
//!
//! The cell therefore names the BINARY (`desktop`, `tradehub`), DERIVED by
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
  check   validate this box's settings store + credential store; the EXIT CODE is the product
  set     write ONE row into the settings database, and record it in the change journal

  migrate-store  apply the settings store's pending migrations (decision 0095) and say what they did
  retired-env    read KEY=VALUE lines on stdin and print the startup refusal for any RETIRED
                 variable among them — the deploy pre-flight's judge

  mirror  store a run/daemon/recorder PROFILE document as rows (there are no settings files any
          more — 0086 — so this no longer touches policy/config/preferences/flags)

  recorder  print the recorder profile ROWS, leading with the store that answered

  activate    make a STORED profile (run|daemon|recorder) the one this box READS from its next
              restart; --proves <file> is required and the rows must render back to it
  deactivate  clear that kind's active row and fall back to --config / VIKE_RUN_PROFILE; the
              stored bodies are untouched, so it is the rollback and needs no redeploy

  bootstrap-daemon  build a daemon profile FROM ARGUMENTS (no file, ever) and activate it — the
                    one act that gets a box with no profile at all to a running paper mount

  bootstrap-recorder  the recorder twin of bootstrap-daemon: build a ONE-subscription recorder
                      profile FROM ARGUMENTS and activate it — the one act that gets a box with no
                      recorder profile at all to a running recording data daemon

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
                state_dir: ctx.state_dir,
                now_ms: ctx.now_ms,
            },
        ),

        // The REPAIR verb a refusing root names (decision 0095) — see `crate::cmd::config_migrate`.
        "migrate-store" => crate::cmd::config_migrate::run(
            args,
            crate::cmd::config_migrate::Ctx { settings_dir: ctx.settings_dir, now_ms: ctx.now_ms },
        ),
        // The deploy helper's judge — see `crate::cmd::config_retired_env`.
        "retired-env" => crate::cmd::config_retired_env::run(args),

        // The PROFILE mirror (0086's *Phasing* leaves this as a going concern: a run/daemon/
        // recorder profile is still a document, unlike the four settings files this verb used to
        // ALSO carry). It takes the dispatcher's own settings directory like every verb above, and
        // deliberately takes neither `state_dir` nor `now_ms`: a mirror states no new value — it
        // copies a document an operator already filed — so it journals nothing. See
        // `crate::cmd::config_mirror`'s module doc.
        "mirror" => crate::cmd::config_mirror::run(args, ctx.settings_dir),

        // The recorder profile, READ back out of the store. It is the replacement for the
        // `grep '^store' <root>/settings/recorder.toml` an operator ran (and which two shipped
        // units embedded in their troubleshooting comments) — see
        // `crate::cmd::config_recorder`'s module doc for why a read verb was unavoidable rather
        // than a convenience.
        "recorder" => crate::cmd::config_recorder::run(args, ctx.settings_dir),

        // WHICH stored profile this box reads — the owner's *the ROW wins* ruling (0057 Question 3)
        // as an operator act. `crate::cmd::config_activate` carries why it is a SEPARATE verb from
        // `mirror` and why `--proves <file>` is required: a mirror stores a body and cannot see
        // what is in force, so `plan_active_row` could only ever withhold here — the crossing needs
        // a PROOF rather than a guess. Neither journals: they state no VALUE, only which ARTIFACT
        // this box reads.
        "activate" => crate::cmd::config_activate::run_activate(args, ctx.settings_dir),
        "deactivate" => crate::cmd::config_activate::run_deactivate(args, ctx.settings_dir),

        // The bootstrap rung `config activate` cannot serve — see
        // `crate::cmd::config_profile_bootstrap`'s module doc for why a box with no profile at all
        // needs a writer that takes no file to prove against.
        "bootstrap-daemon" => crate::cmd::config_profile_bootstrap::run(args, ctx.settings_dir),

        // The recorder twin — see `crate::cmd::config_recorder_bootstrap`'s module doc. Same
        // rung, same reason: `config activate recorder <name> --proves <file>` cannot help a box
        // with no recorder profile at all, because there is no file to prove against.
        "bootstrap-recorder" => crate::cmd::config_recorder_bootstrap::run(args, ctx.settings_dir),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            if let Some((_, msg)) = RETIRED_CONFIG_VERBS.iter().find(|(name, _)| *name == other) {
                eprintln!("{msg}");
                return ExitCode::FAILURE;
            }
            eprintln!(
                "vike-cli config: unknown verb '{other}' (expected `show`, `check`, `set`, \
                 `migrate-store`, `retired-env`, `mirror`, `recorder`, `activate`, `deactivate`, \
                 `bootstrap-daemon` or `bootstrap-recorder`)\n{USAGE}"
            );
            ExitCode::FAILURE
        }
    }
}

/// **`config` sub-verbs retired by `docs/decisions/0086`** (settings live only in the database —
/// no files, no crossing, no file-vs-row comparison). Same idiom as the top-level
/// `RETIRED_COMMANDS` in `crate::lib`: naming the old spelling here fails naming what replaced it,
/// rather than falling through to the unknown-verb catch-all as if it were a typo.
const RETIRED_CONFIG_VERBS: &[(&str, &str)] = &[
    (
        "compare",
        "vike-cli config compare: retired (docs/decisions/0086) — there are no settings files any \
         more to compare the database against. `vike-cli config show` discloses every key's \
         resolved value and where it came from.",
    ),
    (
        "adopt",
        "vike-cli config adopt: retired (docs/decisions/0086) — the settings database is the only \
         settings store now: there is no crossing to perform and nothing to undo. A key is written \
         one row at a time with `vike-cli config set <key> <value>`. An unsound store is restored \
         from this box's nightly backup; no command repairs it.",
    ),
];

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
            None => format!("{}/{}", vike_model::state_path::PROJECT_SETTINGS_DIR, self.label()),
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

    // ...and the RUN PROFILE's `[risk]` values, and the answer to the question `print_ceilings`
    // below could previously only pose: what ARE my live pre-trade ceilings?
    //
    // ⚠ **THEY COME OFF THE PROFILE BODY PLANE NOW, NOT OFF `profile_risk`.** That table was
    // 0057's Phase 2 — a DISCLOSURE mirror of one `[risk]` table, keyed by file NAME, with no
    // `active` column and a reader forbidden by name. The run profile's whole body is stored on the
    // Phase-3 plane instead (`vike_secrets::profile_store`), the daemon READS it, and so the
    // sentence this block used to print — *"READABLE, NOT ENFORCEABLE"* — is no longer universally
    // true and the block has to say WHICH rung wins. A read failure still degrades to a warning,
    // for the same reason the settings rows' does: this is the command an operator reaches for when
    // a box will not start.
    let profile_risk = run_profile_rows(settings_dir);

    // The `venue_setting` rows (decision 0095), read once through the same snapshot every root uses.
    let venue_settings = match settings_dir.map(vike_secrets::venue_setting::load_venue_settings) {
        None => std::collections::BTreeMap::new(),
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            eprintln!(
                "warning: the venue_setting rows could not be read ({e}); the venue block below \
                 shows defaults only"
            );
            std::collections::BTreeMap::new()
        }
    };
    let mut venue = if args.section.files() {
        crate::cmd::config_venue::venue_rows(&venue_settings, args.filter.as_deref())
    } else {
        Vec::new()
    };
    if args.changed_only {
        venue.retain(|r| r.origin != "default");
    }

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
        print_json(&described, &secrets, &files, &envs, &unknown, &profile_risk, &venue)
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
            &venue,
        );
        Ok(())
    }
}

/// **The run profile's `[risk]` values, read off the PROFILE BODY plane, plus WHICH body the box
/// reads.**
///
/// ⚠ The carrier is still `vike_secrets::ProfileRiskSource` / `StoredProfileRisk`, deliberately:
/// those are pure DATA types with no store behind them, every renderer and every JSON field in this
/// file is already built on them, and rebuilding all of that to change where the rows come FROM
/// would be churn with no reader-visible benefit. What is new is [`Self::active`], and it is the
/// half that matters — on this plane a row can BIND.
struct RunProfileRows {
    /// The `[risk]` rows of every stored `run` profile, or the store's own account of why there
    /// are none.
    source: vike_secrets::ProfileRiskSource,
    /// The name of the ACTIVE `run` profile, when one is selected. `Some` means the daemon builds
    /// its pre-trade ceilings from that body rather than from `--profile`/`VIKE_RUN_PROFILE`, so
    /// the rows below are ENFORCEABLE and the block must say so.
    active: Option<String>,
}

/// Project the stored `run` profiles into [`RunProfileRows`].
///
/// The three noes are kept apart, because they name three different next commands: no database at
/// all, a database written before the profile tables existed, and profile tables holding no `run`
/// body. `vike_secrets::profile_store::Profiles` collapses the first two on purpose (the
/// arming-preservation property), so this reports the pair it CAN tell apart and never invents a
/// distinction the read path has thrown away.
fn run_profile_rows(settings_dir: Option<&Path>) -> RunProfileRows {
    let Some(dir) = settings_dir else {
        return RunProfileRows {
            source: vike_secrets::ProfileRiskSource::NoDatabase { path: PathBuf::new() },
            active: None,
        };
    };
    let db = vike_secrets::db_path_in(dir);
    let profiles = match vike_secrets::profile_store::read_profiles(&db) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "warning: the stored run-profile rows could not be read ({e}). The ceilings block \
                 below names the keys and cannot print their values, exactly as it did before this \
                 box was mirrored."
            );
            return RunProfileRows {
                source: vike_secrets::ProfileRiskSource::NoDatabase { path: db },
                active: None,
            };
        }
    };
    if !profiles.tables_present() {
        return RunProfileRows {
            source: vike_secrets::ProfileRiskSource::TableAbsent { path: db },
            active: None,
        };
    }
    let active =
        profiles.active(vike_secrets::profile_store::ProfileKind::Run).map(|p| p.row.name.clone());
    let rows = profiles
        .all()
        .iter()
        .filter(|p| p.row.kind == vike_secrets::profile_store::ProfileKind::Run)
        .map(|p| vike_secrets::StoredProfileRisk {
            profile: p.row.name.clone(),
            rows: p
                .settings
                .iter()
                .filter_map(|(path, value)| {
                    path.strip_prefix("risk.").map(|key| vike_secrets::ProfileRiskRow {
                        key: key.to_string(),
                        value: value.clone(),
                    })
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    RunProfileRows { source: vike_secrets::ProfileRiskSource::Rows(rows), active }
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
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config_venue::VenueRow],
) -> serde_json::Value {
    serde_json::json!({
        "settings_dir": d.settings_dir.as_ref().map(|p| p.display().to_string()),
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
        // THE STORED RUN-PROFILE `[risk]` ROWS, and the VALUES the `ceilings` array above still
        // cannot carry from the file. Unfiltered, like `ceilings`, and for the same reason.
        //
        // ⚠ `enforced` and `read_by` were the literal `false` and `null` on every row, on the
        // ground that NOTHING on the mount path read them. **That stopped being true when the run
        // profile's body moved onto the profile plane and the daemon learned to read it**, so they
        // are now computed per profile: `active` names the body the daemon builds its ceilings
        // from, and only THAT profile's rows are enforced. A machine that treated every row as a
        // ceiling in force would be wrong about the inactive ones; one that treated none as in
        // force would be wrong about the active one, which is the more dangerous half.
        // `state` is how a consumer tells "stored and empty" from "never stored".
        "profile_risk": {
            "state": match profile_risk.source {
                vike_secrets::ProfileRiskSource::Rows(_) => "rows",
                vike_secrets::ProfileRiskSource::NoDatabase { .. } => "no-database",
                vike_secrets::ProfileRiskSource::TableAbsent { .. } => "table-absent",
            },
            // Which stored `run` body this box READS, or `null` when the file rungs still decide.
            "active": profile_risk.active,
            "enforced": profile_risk.active.is_some(),
            "read_by": profile_risk.active.as_ref().map(|_| "vike-tradehub (the active run row)"),
            "profiles": profile_risk.source.profiles().unwrap_or_default().iter().map(|p| serde_json::json!({
                "profile": p.profile,
                // Per profile, so a consumer never has to join two fields to answer the one
                // question that matters about a pre-trade ceiling.
                "active": profile_risk.active.as_deref() == Some(p.profile.as_str()),
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
        // THE `venue_setting` ROWS (decision 0095): every declared field with its value and origin,
        // and every undeclared stored row as `origin: "undeclared"`. A secret value is `<set>`.
        "venue_settings": venue.iter().map(|r| serde_json::json!({
            "key": r.key,
            "value": r.value,
            "origin": r.origin,
            "secret": r.secret,
            "doc": r.doc,
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
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config_venue::VenueRow],
) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&settings_json(
        d,
        secrets,
        files,
        envs,
        unknown,
        profile_risk,
        venue,
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
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config_venue::VenueRow],
) {
    print_header(d, secrets);
    if section.files() {
        println!();
        print_file_table(files, &d.settings.warnings, d.store_refusal.as_deref());

        // ⚠ NOT gated on `--changed-only`, and not on the settings table being non-empty. The
        // whole finding is that a pre-trade ceiling can be in force from a file this command does
        // not read, so "nothing matched above" is precisely when an operator most needs to be told
        // the other file exists.
        print_ceilings(filter, profile_risk.source.profiles().unwrap_or_default());
        // ...and, since decision 0057's Phase 2, the VALUES — when somebody has mirrored them.
        print_profile_risk(filter, profile_risk);
        // ...and, since decision 0095, the declared venue-settings catalog.
        crate::cmd::config_venue::print_venue_table(venue);
    }
    if section.env() {
        println!();
        print_env_table(envs, secrets, unknown);
    }
}

/// **Which project am I reading?** — the question behind every other question here, and the one
/// nothing used to answer. Prints the resolved settings directory, then the credential store's
/// presence, so "my write did nothing" resolves to a stated fact on a line rather than to a guess.
///
/// ⚠ **This used to also print each of the four settings files' presence and key count.**
/// `docs/decisions/0086` deletes the files outright — there is nothing left on disk for this header
/// to describe, so the settings half of this question is answered entirely by `print_file_table`'s
/// row table below, keyed off the settings DATABASE alone.
fn print_header(d: &Description, secrets: &StoreStatus) {
    let Some(dir) = &d.settings_dir else {
        println!(
            "settings directory: NONE — no project above the working directory, and no explicit \
             override."
        );
        println!("  NO settings row could be read: every setting below is a compiled-in default.");
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
                vike_model::state_path::PROJECT_SETTINGS_DIR
            );
        }
        return;
    };
    println!("settings directory: {}", dir.display());

    let width = secrets.label().len();
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

/// The settings half: one row per typed setting, its effective value, and the layer that set it.
fn print_file_table(rows: &[FileRow], warnings: &[String], store_refusal: Option<&str>) {
    println!("-- settings -----------------------------------------------------------------");
    // RENDERED from `vike_config::PRECEDENCE`, never typed here. This line named a per-project
    // override file for two months while every composition root passed `None` for the project
    // directory, so the layer was implemented, tested, advertised — and read by nothing. That layer
    // is now REMOVED, which is the second way a hand-written header goes false: it would still be
    // naming the file today. A header derived from the loader's own layer list can only name layers
    // that exist, in both directions, and the two reachability gates
    // (`crates/vike-config/tests/layers_are_reachable.rs`,
    // `crates/vike-cli/tests/settings_layers_reachable.rs`) hold each of them to a proof of effect.
    //
    // ⚠ **This used to lead with a `source: files|db` line that named WHICH of two sources
    // answered.** `docs/decisions/0086` deletes the second source outright — every key resolves
    // from the settings database or from its compiled-in default, unconditionally — so there is no
    // longer a fact for that line to state that `precedence_line` does not already carry.
    println!("{}", vike_config::precedence_line());
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
/// `preferences.chart_style` all said `yes` while their only reader was `vike-app` — measured:
/// setting `config.state_dir` on a daemon box did nothing, which was correct behaviour (it was
/// vike-app's strategy-state SIDECAR, not the `settings/state/` root the README names, and it is
/// deleted since) reported as though it had worked. A column that says "something reads this" is
/// not much use to somebody deciding whether to set it HERE.
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
/// What CAN be printed is a value an operator has STORED in the settings database —
/// `vike-cli config mirror --profile <file>`. That is why this function takes the stored profiles:
/// the `VALUE SHOWN` cell says *yes, below* for a key some stored body carries and names the repair
/// for one nothing does, instead of the flat "this command does not read that file" that was the
/// only true answer before. The values themselves are printed by [`print_profile_risk`], which is
/// also where the sentence that matters lives — and ⚠ that sentence is now CONDITIONAL: a stored
/// body is readable and inert until `config activate` selects it, at which point the daemon builds
/// its `vike_exec::ProfileRisk` from it and the numbers judge every order.
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

/// **The stored run-profile `[risk]` values** — the answer to the one question [`print_ceilings`]
/// above could previously only pose.
///
/// # Why this block is worth a screen
///
/// The ceilings block names two keys that REFUSE a live mount when absent and judge every order the
/// core admits — and it could not print their numbers, because they live in a run profile and this
/// command links neither of the crates that parse one. So an operator could read *what can refuse
/// my order* and never *at what size*. Storing the profile's body in the settings database
/// (`config mirror --profile`) puts the values somewhere a light binary CAN read, **with the daemon
/// down and with no `sqlite3` on the box** — which is 0054's read-back requirement, reaching a file
/// 0054 had not opened.
///
/// # ⚠ THE SENTENCE THIS BLOCK USED TO PRINT IS NOW CONDITIONAL, AND THAT IS THE WHOLE CHANGE
///
/// It said, flatly: *"A mirrored row is READABLE, not ENFORCEABLE. Nothing on the mount path reads
/// one."* That was true of `profile_risk`, the Phase-2 disclosure mirror, and it is FALSE of the
/// profile BODY plane these rows come off now: `crates/vike-tradehub/src/tradehub_cli.rs` reads the
/// ACTIVE `run` body and builds its `vike_exec::ProfileRisk` from it, ahead of `VIKE_RUN_PROFILE`.
///
/// So the block names WHICH RUNG WINS instead of asserting one half as if universal. An ACTIVE row
/// is marked, and the paragraph says outright that the marked body is what judges an order. A
/// stored-but-inactive body keeps the old sentence, which is still exactly right for it. Printing
/// the old sentence over an active row would be positive confirmation of something false about a
/// live pre-trade ceiling — the failure this whole disclosure exists to prevent.
///
/// # …and the one it must print when it has nothing
///
/// A box with no stored bodies, a store that predates the profile tables, and a stored profile that
/// genuinely sets no ceiling are three different facts, and the middle one is not an absence of
/// ceilings — the FILE may hold every one of them. So the empty case prints the store's own account
/// of itself rather than nothing at all; a silent block would read as "no ceilings", which is the
/// worst thing this command could say about a live box.
fn print_profile_risk(filter: Option<&str>, view: &RunProfileRows) {
    let matches = |hay: &str| match filter {
        None => true,
        Some(f) => hay.to_lowercase().contains(&f.to_lowercase()),
    };
    // The block's own words are filterable like every other, so `--filter risk` reaches it.
    let block_matched = filter.is_none_or(|f| {
        let f = f.to_lowercase();
        "run profile [risk]".contains(f.as_str()) || "ceiling".contains(f.as_str())
    });

    let source = &view.source;
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
    println!("-- run profile [risk], as stored ------------------------------------------");
    for line in wrap(
        "The live pre-trade ceilings, read from the settings database rather than from the profile \
         file — so this box can answer with the daemon down.",
        92,
    ) {
        println!("{line}");
    }
    match &view.active {
        Some(name) => {
            for line in wrap(
                &format!(
                    "⚠ ENFORCEABLE: `{name}` is the ACTIVE run profile, so the daemon builds its \
                     `vike_exec::ProfileRisk` from THAT body — the row wins over `--profile` and \
                     `VIKE_RUN_PROFILE`, which the daemon names as shadowed at every boot. Any \
                     OTHER profile below is stored and inert. `vike-cli config deactivate run` \
                     hands the decision back to the file rungs; a running daemon holds what it \
                     BOOTED with either way.",
                ),
                92,
            ) {
                println!("{line}");
            }
        }
        None => {
            for line in wrap(
                "READABLE, NOT ENFORCEABLE on this box: no `run` profile row is ACTIVE, so the \
                 daemon still resolves its ceilings from the FILE named by `--profile` or \
                 `VIKE_RUN_PROFILE`, and a row that disagrees with that file means the stored body \
                 is STALE rather than that the ceiling has changed. `vike-cli config activate run \
                 <name> --proves <file>` is what makes a body bind.",
                92,
            ) {
                println!("{line}");
            }
        }
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
        // ⚠ The ACTIVE marker rides the profile's own heading rather than a separate legend,
        // because a legend is what a reader skips: the one fact that decides whether the numbers
        // below judge an order has to be on the same line as the name.
        if view.active.as_deref() == Some(p.profile.as_str()) {
            println!("{}  ⚠ ACTIVE — the daemon builds its ceilings from THIS body", p.profile);
        } else {
            println!("{}  (stored, not selected)", p.profile);
        }
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

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
