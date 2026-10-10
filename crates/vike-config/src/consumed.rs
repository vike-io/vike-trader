//! **Which settings are actually READ** — one row per [`Config`](crate::Config) /
//! [`Preferences`](crate::Preferences) / [`Flags`](crate::Flags) key, naming the code that consumes
//! it or admitting, in writing, that nothing does.
//!
//! # Why this table exists
//!
//! A settings file that validates, is accepted by `deny_unknown_fields`, and is then reported by
//! `vike-cli config show` as the ORIGIN of an effective value gives the operator **positive
//! confirmation of something false** when nothing reads the value. That is strictly worse than an
//! unimplemented feature: an unimplemented feature has no output claiming it works.
//!
//! `Policy::max_total_exposure` was that shape and was DELETED for it;
//! `crates/vike-config/tests/policy_is_consumed.rs` is the gate on the policy side. **This is that
//! gate's twin for the other three types**, and it goes one step further, because a test alone
//! would leave `config show` still lying: the table is `pub` DATA, so the disclosure command can
//! name the unread keys instead of confirming them.
//!
//! # The rule a row must satisfy
//!
//! [`Consumer::At`] names a repo-relative FILE and a NEEDLE that must appear in it, and
//! `crates/vike-config/tests/settings_are_consumed.rs` opens the file and looks. The needle is the
//! READ ITSELF (`dir: settings.config.log_dir.clone()`), never the field name — a `tracing` line
//! that logs a resolved value is not consumption, and neither is a doc comment describing one.
//!
//! ⚠ **A needle inside `crates/vike-config/` does not count and the gate rejects it.** This crate
//! parses, validates, clamps and serializes every field; if that counted, every field would be
//! "consumed" and the table would assert nothing. Consumption means something OUTSIDE the settings
//! system acts on the value.
//!
//! [`Consumer::Not`] is the honest alternative, and its `why` must name the READER that owns the
//! variable today, so the row doubles as the work list for moving it. `why` is length-checked and
//! TODO-checked by the gate, because "nothing reads it" waved through as a one-word excuse is the
//! exact shape being prevented.
//!
//! # Why so many `Not` rows, and why that is the honest answer
//!
//! The rows are genuinely file-settable settings whose reads have not moved out of a library yet:
//! moving one is not a line change but threading a value from a composition root down through
//! `vike_mount::NodeConfig` / `vike_mount::make_engine` into the adapter, per flag, which is Phase 6
//! of the settings-unification design and is deliberately done a flag at a time, together with the
//! env read it replaces. A flag living in two places that DISAGREE would be worse than the state
//! this program is fixing.
//!
//! ⚠ **A `Not` row does NOT mean the environment spelling works.** For `flags.{hl_outcome,
//! pm_resolve, poly_auto_redeem, poly_heartbeat, poly_redeem_halt, record_chains}` the library read
//! is reached from NOTHING a shipped binary runs: each sits inside a poller (or a recorder
//! constructor) that no composition root ever builds, and setting either spelling changes nothing
//! (decision 0095 retired the first five's variables — a set one refuses startup). A row saying
//! *export the variable instead* is `Policy::max_total_exposure`'s defect one level down: positive
//! confirmation of something false, printed by the very command added to stop it.
//!
//! So the claim is DATA, not prose. [`Reader`] says which of the three states a
//! `Not` row is in, `crates/vike-config/tests/settings_are_consumed.rs` CHECKS it, and `config
//! show` renders [`Reader::verdict`] above the paragraph so the operator is told whether the
//! variable is worth exporting before they read why the file key is not.
//!
//! # Why none of the six is DELETED instead
//!
//! Deleting a key is the dangerous half — `deny_unknown_fields` is on every patch type, so a key
//! an operator already wrote becomes a hard startup refusal naming only "unknown field". A deletion
//! must therefore ship a tombstone, and for these six **both available tombstones would print a
//! second lie in place of the first**:
//!
//! * [`crate::flags::REMOVED_FLAG_KEYS`] refused the key saying *"Delete the key and export
//!   `<VAR>=1` instead"*, justified then by every variable in that table still being read by the
//!   venue adapter that owned it — which is exactly what was not true here. (Decision 0095 has since
//!   retired every variable in that table too; its refusal now names each one's new home, or says
//!   nothing reads it.)
//! * [`crate::REMOVED_ENV`] would refuse the VARIABLE at startup, i.e. stop a daemon dead over a
//!   spelling that changes nothing, while the adapter's live `std::env::var` call site stands and
//!   still needs its own `vike_ops::settings::SETTINGS` row — advertising a removal that has not
//!   happened.
//!
//! ⚠ **Decision 0095 retired the VARIABLE of five of the six, and kept the ROW**: the settlement
//! pollers' entry points take `enabled`/`halted` as PARAMETERS (D4), `REMOVED_ENV` refuses the five
//! variables, and the five rows say "INERT, and its environment spelling is RETIRED". The ROW is
//! still not deleted, for the reason below. `record_chains` is the sixth, and its variable is still
//! looked up by the unbuilt recorder constructor.
//!
//! And the feature behind each key is intact, not gone: what is missing is a composition root that
//! MOUNTS it, and every one of those is its own decision (unattended on-chain money movement for
//! `poly_auto_redeem`; a settlement poller writing synthetic terminal fills into the core for
//! `pm_resolve`/`hl_outcome`; a guard that must outlive the thing it guards for
//! `poly_redeem_halt`). Deleting the field would also delete its `FLAG_REGISTRY` stewardship row
//! (owner + review date + disposition), which is the mechanism that gets the mount decided. So the
//! rows stay, and say what is true.

use crate::provenance::setting_keys;

mod table;
pub use table::CONSUMPTION;

/// What reads one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumer {
    /// A real consumer. `file` is repo-root-relative and must EXIST; `needle` must appear in it and
    /// must be the read itself. See the module doc for why a needle inside `crates/vike-config/`
    /// is rejected.
    At {
        /// Repo-root-relative path of the file that reads it.
        file: &'static str,
        /// The read, verbatim enough to be found by substring search.
        needle: &'static str,
    },
    /// Declared, validated, displayed — and read by NOTHING that acts on it. `why` must name the
    /// reader that owns the variable today, so this doubles as the work list; `reader` states, as
    /// CHECKED data rather than prose, whether the environment spelling still works.
    Not {
        /// Which code reads the environment variable instead, and what wiring it would take.
        why: &'static str,
        /// What the environment spelling actually does — see [`Reader`] for why this is not left
        /// to `why` alone.
        reader: Reader,
    },
}

/// **What the ENVIRONMENT spelling of an unread key actually does.**
///
/// [`Consumer::Not`] carried prose alone, and the gate checked that prose for LENGTH and for the
/// absence of "todo" — never for truth. Six of eight rows said *the file key is inert, export the
/// variable instead*, and for six of them the variable was inert too, because the function reading
/// it is reached from nothing a shipped binary runs. See the module doc for what that cost.
///
/// Every variant except [`Reader::Nothing`] carries a claim
/// `crates/vike-config/tests/settings_are_consumed.rs` can open a file and check, and the claims
/// are each other's opposites — so the day somebody WIRES one of these, the row that says nothing
/// reaches it goes red and has to be promoted rather than quietly going on lying.
///
/// ⚠ **The residual, declared rather than implied:** none of this is call-graph analysis. A
/// substring gate proves that a named entry point has, or has not, a call site outside the test
/// modules; it cannot see a call through a trait object, an alias or a macro, and one hop of
/// evidence is not a proof of reachability from `main`. What it does pin is the exact link that
/// was missing in all six bad rows — an entry point nothing outside its own file ever calls.
///
/// ⚠⚠ **A `needle` here NAMES THE READING FUNCTION'S SIGNATURE — never the read expression — and
/// that is a hard constraint from ANOTHER gate, not a style preference.** (Or, for a value the
/// unstarted code now takes as a PARAMETER — the five settlement-poller flags, decision 0095's D4 —
/// the gate on that parameter: see `Uncalled`'s `needle`. Either way, never a read expression.)
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` scans every `src/` file for env-read-shaped TEXT,
/// resolving `env::var(SOME_CONST)` through the declaring crate's consts. It has no call syntax to
/// anchor on, so a string LITERAL that merely looks like a read scores as one. Writing the obvious
/// needle here — a string spelling out the read of `RECORD_CHAINS_ENV`, which this sentence
/// deliberately does not quote — made that gate believe **`vike-config`
/// itself** reads `VIKE_RECORD_CHAINS` and `VIKE_SWEEP_THREADS`, and it failed three ways at once:
/// `every_read_variable_is_declared` (an undeclared `(name, krate)` pair),
/// `library_rows_do_not_grow` (the `Layer::Library` ratchet, which may shrink and never grow), and
/// `dynamic_sites_are_allowlisted` (a const this crate cannot resolve, e.g. polymarket's
/// `HEARTBEAT_ENV`). Measured, not theorised — it is how this note came to be written. A signature
/// is also the stabler anchor: a read expression is refactored far more often than the `pub fn`
/// line above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// **The environment variable still works**; only the FILE key is inert. `file`/`needle` locate
    /// the read, and `caller`/`call` are the proof that something outside `file` reaches it — the
    /// half whose absence made six rows false.
    Live {
        /// Repo-root-relative file holding the environment read.
        file: &'static str,
        /// The SIGNATURE of the function that performs the read, verbatim enough for a substring
        /// search — never the read expression itself. See the enum's ⚠ note on why.
        needle: &'static str,
        /// A repo-root-relative file, OUTSIDE `file`, that calls into the read.
        caller: &'static str,
        /// The call in `caller`, verbatim enough for a substring search.
        call: &'static str,
    },
    /// **Neither spelling configures anything.** The read exists (`file`/`needle`) and is reached
    /// only from `entry` — one or more `Type::method`-qualified entry points that NOTHING outside
    /// test code constructs or calls. Re-arming the key needs a composition root that mounts the
    /// feature, not a flag-threading change.
    ///
    /// The read is either an environment read (`record_chains`) or, since decision 0095 retired a
    /// variable and made its value a PARAMETER of the unstarted code (D4), the gate on that
    /// parameter (the five settlement-poller flags).
    Uncalled {
        /// Repo-root-relative file holding the read.
        file: &'static str,
        /// Where the read is, verbatim enough for a substring search: the SIGNATURE of the
        /// function performing an environment read — never the read expression itself, see the
        /// enum's ⚠ note on why — or, for a value taken as a parameter, the gate on it.
        needle: &'static str,
        /// Every public entry point that reaches the read, each `Type::method`-qualified so its
        /// own definition (`pub fn method(`) cannot satisfy the search for a CALL. ALL of them
        /// must be uncalled — naming one of two doors would leave the other free to open.
        entry: &'static [&'static str],
    },
    /// **There is no reader at all** — not in a library, not anywhere. The variable is resolved by
    /// this crate's own `Flags::apply_env` into a field nothing consumes.
    ///
    /// The one state with no obligation here, and deliberately so: there is no file to open and no
    /// symbol to search for. It is covered from the other side instead — by
    /// `the_unconsumed_list_is_exactly_the_not_rows` below, which pins the row on the unread list,
    /// and by `crates/vike-ops/tests/settings_secrets/settings_registry/directions.rs`'s `every_read_variable_is_declared`,
    /// which fails the moment any crate starts reading the variable without declaring a row.
    Nothing,
}

impl Reader {
    /// `true` only for [`Reader::Live`] — the one state in which telling an operator to export the
    /// variable instead is honest advice.
    #[must_use]
    pub fn env_still_works(&self) -> bool {
        matches!(self, Reader::Live { .. })
    }

    /// The one-line verdict `vike-cli config show` prints ABOVE the `why` paragraph, so the
    /// operator learns whether the variable is worth exporting before reading why the file key is
    /// not. DERIVED from the variant, never a hand-written column that could disagree with it.
    #[must_use]
    pub fn verdict(&self) -> &'static str {
        match self {
            Reader::Live { .. } => {
                "The ENVIRONMENT VARIABLE still works — only the file key is inert. Export it \
                 instead; the paragraph below names the read."
            }
            Reader::Uncalled { .. } => {
                "NEITHER SPELLING CONFIGURES ANYTHING: the code this setting is for is reached \
                 from nothing any shipped binary runs."
            }
            Reader::Nothing => {
                "NEITHER SPELLING DOES ANYTHING: nothing in the tree reads the variable at all."
            }
        }
    }
}

impl Consumer {
    /// `true` for [`Consumer::At`]. The question `config show` asks per row.
    #[must_use]
    pub fn is_consumed(&self) -> bool {
        matches!(self, Consumer::At { .. })
    }

    /// The consuming file, or `None` when nothing consumes it.
    #[must_use]
    pub fn file(&self) -> Option<&'static str> {
        match self {
            Consumer::At { file, .. } => Some(*file),
            Consumer::Not { .. } => None,
        }
    }

    /// The written admission, or `None` when the setting IS consumed.
    #[must_use]
    pub fn why_not(&self) -> Option<&'static str> {
        match self {
            Consumer::At { .. } => None,
            Consumer::Not { why, .. } => Some(*why),
        }
    }

    /// What the environment spelling of an unread key does, or `None` when the setting IS consumed.
    #[must_use]
    pub fn reader(&self) -> Option<Reader> {
        match self {
            Consumer::At { .. } => None,
            Consumer::Not { reader, .. } => Some(*reader),
        }
    }

    /// The one-line verdict for an unread key ([`Reader::verdict`]), or `None` when it IS consumed.
    ///
    /// Exists so `config show` renders the verdict without matching on [`Reader`] itself: the
    /// rendering rule belongs beside the data, not in the CLI, and a second `match` there is a
    /// second thing to forget when a variant is added.
    #[must_use]
    pub fn unread_verdict(&self) -> Option<&'static str> {
        self.reader().map(|r| r.verdict())
    }

    /// **WHICH BINARY reads it** — the short program name, or `None` when the consumer is a library
    /// (or nothing consumes it at all).
    ///
    /// `is_consumed` answers a yes/no question, and a clean install found that "yes" is not enough:
    /// on a headless tradehub or recorder box, `config.state_dir`, `config.store_root` and
    /// `preferences.chart_style` all reported `READ: yes` while their ONLY reader is `vike-desktop`
    /// (then `vike-app`), the GUI, which will never execute there. Setting one on a daemon box does
    /// nothing, and the output confirmed that it would work — positive confirmation of something
    /// false, which is the failure the `READ` column was added to remove in the first place.
    ///
    /// DERIVED from [`Consumer::At::file`], never a second hand-written column: the file path is
    /// already gated (`crates/vike-config/tests/settings_are_consumed.rs` opens it and looks for the
    /// needle), so a rule over it inherits that gate instead of adding a copy that can rot.
    ///
    /// The rule, and what each answer means to an operator:
    ///
    /// | `file` | answer | reading |
    /// |---|---|---|
    /// | `crates/<krate>/src/main.rs` | `<krate>` minus its `vike-` prefix | ONE program reads this |
    /// | `crates/<krate>/src/bin/<bin>.rs` | `<bin>` minus its `vike-` prefix | ditto |
    /// | anything else (a library file) | `None` | read wherever that library is linked |
    ///
    /// `None` is deliberately not "unknown": a library read genuinely belongs to every binary that
    /// links it, so there is no single honest name to print and `config show` keeps saying `yes`.
    #[must_use]
    pub fn binary(&self) -> Option<&'static str> {
        let file = self.file()?;
        let rest = file.strip_prefix("crates/")?;
        // `bridges/<venue>/…` and any other nesting is handled by taking what precedes `/src/`.
        let (krate, tail) = rest.split_once("/src/")?;
        // ⚠ THREE shapes now, and the third is the multicall convention. A binary's BODY lives in
        // `src/<name>_cli.rs` so the `vike-backend` dispatcher can reach it without a second static
        // copy of its closure, leaving `src/main.rs` (or `src/bin/<name>.rs`) a shim. The reader is
        // that file, so that is what a `Consumption` row names — and without this arm every such row
        // silently reclassifies from "a binary reads this" to "nothing does", which is the exact
        // false answer `vike-cli config show`'s READ column exists to prevent.
        //
        // ⚠ The name comes from the FILE, never the crate: `tearsheet_cli.rs` lives in `vike-report`
        // and `study_cli.rs` in `vike-studio-core`, so a crate-derived name would be wrong for both
        // while being right for the four whose binary matches their crate.
        let name = match tail {
            "main.rs" => krate.rsplit('/').next()?,
            t if t.ends_with("_cli.rs") => t.strip_suffix("_cli.rs")?,
            _ => tail.strip_prefix("bin/")?.strip_suffix(".rs")?,
        };
        Some(name.strip_prefix("vike-").unwrap_or(name))
    }
}

/// One row: a dotted setting key and what reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consumption {
    /// The dotted key exactly as [`setting_keys`] spells it — `config.log_dir`, `flags.poly_exec`.
    pub key: &'static str,
    /// What reads it.
    pub by: Consumer,
}

/// What reads `key`, or `None` for a key this table does not carry (every `policy.*` key, and any
/// misspelling).
#[must_use]
pub fn consumer_of(key: &str) -> Option<&'static Consumer> {
    CONSUMPTION.iter().find(|c| c.key == key).map(|c| &c.by)
}

/// `true` when something outside the settings system reads `key` and acts on it.
///
/// A key this table does not carry answers `true`: the only such keys are `policy.*`, which have
/// their own gate, and a caller asking about one must not be told it is unread.
#[must_use]
pub fn is_consumed(key: &str) -> bool {
    consumer_of(key).is_none_or(Consumer::is_consumed)
}

/// Every key this table admits nothing reads, in table order — what `config show` warns about and
/// what Phase 6 works through.
#[must_use]
pub fn unconsumed_keys() -> Vec<&'static str> {
    CONSUMPTION.iter().filter(|c| !c.by.is_consumed()).map(|c| c.key).collect()
}

/// The verdict on an ENVIRONMENT VARIABLE — `Some` only when this table gates a flag whose
/// variable spelling does nothing, `None` for every other name.
///
/// ⚠ **This exists because `vike-cli config show` contradicted itself inside one invocation.** Its
/// FILE half printed that neither spelling does anything for six flags ([`Reader::verdict`]),
/// while its ENVIRONMENT half listed those same variables in a table headed by an env-over-store
/// precedence — one command, two answers, and the
/// more authoritative-looking half was the wrong one. The env table now names the dead rows
/// underneath itself, from HERE, so the two halves cannot disagree.
///
/// DERIVED, never a second list: [`FLAG_REGISTRY`](crate::FLAG_REGISTRY) already maps each flag
/// field to its variable, and [`CONSUMPTION`] already carries the [`Reader`]. A row added to either
/// changes this answer with no edit here. The filter is `!`[`Reader::env_still_works`], so a
/// [`Reader::Live`] key — where exporting the variable IS the honest advice — deliberately says
/// nothing.
///
/// ⚠ **A RETIRED variable gets no verdict either** ([`crate::FlagMeta::reads_env`] is `false`):
/// exporting one refuses startup ([`crate::REMOVED_ENV`]), so listing it among the variables whose
/// export "changes nothing" would be the false claim — the registry row's own default already says
/// it is refused.
#[must_use]
pub fn env_verdict(var: &str) -> Option<&'static str> {
    let meta = crate::FLAG_REGISTRY.iter().find(|m| m.env == var)?;
    if !meta.reads_env() {
        return None;
    }
    consumer_of(&format!("flags.{}", meta.field))?
        .reader()
        .filter(|r| !r.env_still_works())
        .map(|r| r.verdict())
}

/// The keys this table is REQUIRED to carry: every [`setting_keys`] row that is not `policy.*`.
///
/// Derived from the same function `config show` renders — whose flags half is itself derived from
/// [`FLAG_REGISTRY`](crate::FLAG_REGISTRY) — so the table cannot silently fall behind a new field or
/// a new flag. Exposed rather than duplicated inside the test because that derivation is owned data,
/// not test scaffolding.
#[must_use]
pub fn keys_requiring_a_row() -> Vec<String> {
    setting_keys().into_iter().map(|k| k.key).filter(|k| !k.starts_with("policy.")).collect()
}

#[path = "consumed_tests.rs"]
#[cfg(test)]
mod consumed_tests;
