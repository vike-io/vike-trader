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
//! It is not hypothetical. A clean-install validation found `flags.tradehub_control`,
//! `config.tradehub_addr`, `config.log_dir`, `config.state_dir` and `preferences.log_file_level`
//! all displayed as effective while being read by nothing — no control server, nothing listening,
//! the log in the wrong directory, and a `trace` firehose the operator believed they had capped.
//! (`vike_log::file_level_directive`'s own doc records what that firehose once cost: 341 GB, on the
//! disk hosting a live trading node.) `Policy::max_total_exposure` was the same shape and was
//! DELETED for it; `crates/vike-config/tests/policy_is_consumed.rs` is the gate that stopped the
//! next one on the policy side. **This is that gate's twin for the other three types**, and it goes
//! one step further, because a test alone would have left `config show` still lying: the table is
//! `pub` DATA, so the disclosure command can name the unread keys instead of confirming them.
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
//! ⚠ **This section used to open "Every `Not` row below is a setting whose variable IS read today",
//! and that sentence was FALSE for six of the eight rows.** It is worth spelling out, because the
//! false version is the defect this file exists to prevent, wearing the costume of its own fix.
//!
//! The rows are genuinely file-settable settings whose reads have not moved out of a library yet:
//! moving one is not a line change but threading a value from a composition root down through
//! `vike_run::NodeConfig` / `vike_mount::make_engine` into the adapter, per flag, which is Phase 6
//! of the settings-unification design and is deliberately done a flag at a time, together with the
//! env read it replaces. A flag living in two places that DISAGREE would be worse than the state
//! this program is fixing. That much was always true.
//!
//! What was not true is the sentence's PREMISE — that the environment spelling therefore still
//! works. For `flags.{hl_outcome, pm_resolve, poly_auto_redeem, poly_heartbeat, poly_redeem_halt,
//! record_chains}` the library read exists and is reached from NOTHING a shipped binary runs: each
//! sits inside a poller (or a recorder constructor) that no composition root ever builds. The rows
//! said, in effect, *the file key is inert, export the variable instead* — and `vike-cli config
//! show` printed that sentence verbatim to an operator who had configured the key. Setting either
//! spelling changes nothing. That is `Policy::max_total_exposure`'s defect one level down: positive
//! confirmation of something false, printed by the very command added to stop it.
//!
//! So the claim stopped being prose and became DATA. [`Reader`] says which of the three states a
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
//! * [`crate::flags::REMOVED_FLAG_KEYS`] refuses the key and says *"Delete the key and export
//!   `<VAR>=1` instead"*. Its own doc justifies that message on "every variable below is still
//!   read, today, by the venue adapter that owns it" — which is exactly what is not true here.
//! * [`crate::REMOVED_ENV`] would refuse the VARIABLE at startup, i.e. stop a daemon dead over a
//!   spelling that changes nothing, while the adapter's live `std::env::var` call site stands and
//!   still needs its own `vike_ops::settings::SETTINGS` row — advertising a removal that has not
//!   happened.
//!
//! And the feature behind each key is intact, not gone: what is missing is a composition root that
//! MOUNTS it, and every one of those is its own decision (unattended on-chain money movement for
//! `poly_auto_redeem`; a settlement poller writing synthetic terminal fills into the core for
//! `pm_resolve`/`hl_outcome`; a guard that must outlive the thing it guards for
//! `poly_redeem_halt`). Deleting the field would also delete its `FLAG_REGISTRY` stewardship row
//! (owner + review date + disposition), which is the mechanism that gets the mount decided. So the
//! rows stay, and what changed is that they now say what is true.

use crate::provenance::setting_keys;

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
/// that is a hard constraint from ANOTHER gate, not a style preference.**
/// `crates/vike-ops/tests/settings_registry.rs` scans every `src/` file for env-read-shaped TEXT,
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
    /// **Neither spelling does anything.** The read exists (`file`/`needle`) and is reached only
    /// from `entry` — one or more `Type::method`-qualified entry points that NOTHING outside test
    /// code constructs or calls. Re-arming the key needs a composition root that mounts the
    /// feature, not a flag-threading change.
    Uncalled {
        /// Repo-root-relative file holding the environment read.
        file: &'static str,
        /// The SIGNATURE of the function that performs the read, verbatim enough for a substring
        /// search — never the read expression itself. See the enum's ⚠ note on why.
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
    /// and by `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared`,
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
                "NEITHER SPELLING DOES ANYTHING: the code that reads the variable is reached from \
                 nothing any shipped binary runs. Exporting it changes nothing either."
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
        // `src/<name>_cli.rs` so the `vike` dispatcher can reach it without a second static copy of
        // its closure, leaving `src/main.rs` (or `src/bin/<name>.rs`) a shim. The reader is that
        // file, so that is what a `Consumption` row names — and without this arm every such row
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

/// One row per `config.*` / `preferences.*` / `flags.*` key.
///
/// ⚠ `policy.*` is deliberately ABSENT: it has its own, older gate with its own table
/// (`crates/vike-config/tests/policy_is_consumed.rs`). Two tables rather than one because the policy
/// gate additionally proves the sealed no-env-layer property, and merging them would blur the one
/// distinction the whole taxonomy rests on.
///
/// Kept in `setting_keys()`'s own (sorted) order so a reader can diff the two by eye; the gate
/// compares them as sets, so a mis-ordered row fails nothing and a MISSING row fails loudly.
pub const CONSUMPTION: &[Consumption] = &[
    // ---------------------------------------------------------------------------------------
    // config.* — deployment
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "config.backtest_addr",
        by: Consumer::At {
            // The COMPUTE daemon's address (ruling 7 of
            // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), read by BOTH
            // sides of that wire out of ONE key — the daemon that BINDS it (`backtest --addr`, with
            // no value) and the `vike-cli backtest`/`walkforward`/`study` that DIAL it (it was four
            // verbs until ruling 13 deleted `sweep` — a parameter search is `backtest` on a profile
            // carrying a `[sweep]` table, over the same address). The
            // row names the DAEMON's read: it is the one that fails visibly if the key stops being
            // consumed (the server binds the wrong port), where a client's would fail as a refused
            // connection an operator could blame on anything.
            //
            // ⚠ **ONE row, and it has to be one.** The client half (ruling 16's `study`) arrived on
            // its own branch carrying a SECOND `config.backtest_addr` row naming
            // `crates/vike-cli/src/lib.rs`'s `booted.settings.config.backtest_addr`, because
            // neither branch could see the other's. Both reads are real and both still happen, but
            // `consumer_of` is a `find` — the first row wins and the second is unreachable — and
            // `every_setting_has_a_consumption_row` compares SETS, so the duplicate reddens
            // nothing and merely makes this table quietly stop being one-row-per-key. The client
            // read is recorded here instead: `vike-cli`'s ONE boot walk resolves the key and hands
            // it to the `backtest` arm as a parameter (a `src/cmd/` file reads no settings of its
            // own), where `crates/vike-cli/src/cmd/backtest.rs`'s `resolve_addr` folds the ladder
            // `--addr` → this → `vike_config::DEFAULT_BACKTEST_ADDR`.
            // ⚠ The fold MOVED onto that file and the `study` arm is now a CALLER of it — both
            // verbs dial the same compute daemon on this one key, so a second copy of the ladder
            // could answer differently about a blank rung.
            file: "crates/vike-backtest/src/backtest_cli.rs",
            needle: "settings.config.backtest_addr",
        },
    },
    Consumption {
        key: "config.datahub_addr",
        by: Consumer::At {
            // The Studio's remote-store branch (split-plane B12): set → the desktop's Studio dials
            // a `RemoteHistStore` at this address instead of opening the local DataFusion store;
            // unset → local, exactly as before. NB the key is the CLIENT dial address: the
            // `vike-datahub` SERVER bin still reads `VIKE_DATAHUB_ADDR` itself for its LISTEN
            // address (it loads no settings — it does not depend on vike-config).
            // ⚠ Re-keyed from `main.rs` when `App`'s non-constructor methods (this read lives in
            // `resolved_datahub_addr`) moved to `app_methods.rs`. Re-keyed a SECOND time when the
            // GUI shell was renamed `vike-app` → `vike-desktop`. The read itself is unchanged both
            // times — only the path moved, which is the whole reason this row names a path.
            file: "crates/vike-desktop/src/app_methods.rs",
            needle: "settings().config.datahub_addr",
        },
    },
    Consumption {
        key: "config.datahub_advertise_addr",
        by: Consumer::At {
            // The daemon's REQ-2 advertisement: set → the node server's `Welcome.features`
            // carries `datahub=<addr>` and a connected client with no explicit `datahub_addr`
            // of its own dials the datahub there. Read on the DAEMON's box (its client-facing
            // sibling `config.datahub_addr` above is read on the CLIENT's).
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.datahub_advertise_addr.as_deref()",
        },
    },
    Consumption {
        key: "config.tradehub_account_admin",
        by: Consumer::At {
            // The THREE-VALUED barrier DECLARATION (`docs/decisions/0065`): unset/`off` /
            // `loopback` / `contained`. `start_observe_server` hands it to `account_admin_source`,
            // which is the ONE site that decides whether this daemon builds an
            // account-administration capability at all — and, under `loopback`, CHECKS the
            // declaration against `server::bind_exposure` and refuses the capability when the bind
            // disagrees. Read on the DAEMON's box, at startup, once.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.tradehub_account_admin.as_deref()",
        },
    },
    Consumption {
        key: "config.tradehub_advertise_addr",
        by: Consumer::At {
            // The OVERRIDE half of the daemon's self-report: the daemon composes the
            // `WireNodeIdentity::advertise_addr` every published frame carries, and a configured
            // value WINS over the address it discovers from its own routing table
            // (`crate::self_address::advertise_addr`, whose first parameter this is). Read on the
            // DAEMON's box, at startup, once.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.tradehub_advertise_addr.as_deref()",
        },
    },
    Consumption {
        key: "config.instance_origin",
        by: Consumer::At {
            // The daemon threads it to `live_mount`, which puts it on the core's `CoreConfig`, so
            // every client order id this instance mints names the deployment that placed it.
            // ⚠ The GUI shell carried an identical read for its own live mount, and this comment
            // cited that file. It is GONE: the desktop mounts no venue and mints no client order
            // id, so the daemon is now the sole reader rather than one of two. Cited in prose
            // rather than as a path, because the path this sentence used to name no longer exists.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.instance_origin.clone()",
        },
    },
    Consumption {
        key: "config.journal_dir",
        by: Consumer::At {
            // The whole family DID move together, which is what the `Not` row here asked for:
            // `vike_core::journal_config_from` is `journal_config_from_env` over a caller-supplied
            // map, reading the same three variables in the same order, and the env-reading wrapper
            // survives for every caller that has no settings to offer it.
            //
            // The needle is the RESOLUTION, in the one binary that owns the sweep. `journal_vars`
            // starts from the real process env and inserts the file's value only where
            // `VIKE_JOURNAL_DIR` is ABSENT, so an `Environment=` line still wins — and it is called
            // ONCE, because the live core's `CoreConfig::journal` and the off-path materializer
            // both read it and two resolutions could name two directories.
            //
            // ⚠ It reaches the PAPER mount too, through `vike_run::PaperMountOpts::journal`. That
            // is not tidiness: a key that enabled the WAL on the live arm and silently did nothing
            // on the rehearsal would be this table's own defect wearing a smaller costume.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "journal_vars(settings.config.journal_dir.as_deref())",
        },
    },
    Consumption {
        key: "config.log_dir",
        by: Consumer::At {
            // …and the identical line in crates/vike-desktop/src/main.rs's `main`. `LogConfig::dir` is
            // the layer vike-log already had for exactly this and that nothing outside a vike-log
            // test ever set.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "dir: settings.config.log_dir.clone()",
        },
    },
    Consumption {
        key: "config.node_addr",
        by: Consumer::At {
            // The CLIENT half of `config.tradehub_addr`'s pair, and a different binary reads it:
            // this key is the default for `vike-cli`'s `--node`, resolved in that dispatcher's ONE
            // boot walk and handed to the `node` verbs as a parameter (a `src/cmd/` file reads no
            // settings of its own). `vike-tradehub` never reads it — it is not that daemon's
            // address, it is where a client looks for one.
            file: "crates/vike-cli/src/lib.rs",
            needle: "booted.settings.config.node_addr",
        },
    },
    // ⚠ `config.state_dir` stood here, as a written admission that its one reader — the desktop
    // shell's `state_dir_path` — went with the desktop cut's local core. That row named the honest
    // end state in its own text ("deletion plus a `REMOVED_ENV` refusal") and deferred it to a
    // change of its own; this is that change. The key, its `VIKE_STATE_DIR` env layer and its
    // `Config` field are gone, and BOTH spellings now refuse: `crate::REMOVED_ENV` for the
    // variable, `crate::config::ConfigPatch::state_dir` for the file key. A REMOVED key must not
    // keep a row here — a row is precisely what makes `config show` call a value effective.
    Consumption {
        key: "config.store_root",
        by: Consumer::At {
            // The Studio tool's bar store. The OTHER `VIKE_HIST_STORE` readers — vike-datahub's bin
            // and the map-taking `vike_backtest::binutil::store_root` /
            // `vike_backfill::cli::store_root` — are in binaries that load no settings.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().config.store_root",
        },
    },
    Consumption {
        key: "config.tradehub_addr",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.tradehub_addr.as_deref()",
        },
    },
    // ---------------------------------------------------------------------------------------
    // preferences.* — taste and tuning
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "preferences.chart_style",
        by: Consumer::At {
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.chart_style.clone()",
        },
    },
    Consumption {
        key: "preferences.log_file_level",
        by: Consumer::At {
            // THE 341-GB knob. Also set identically in crates/vike-desktop/src/main.rs's `main`.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "file_level: settings.preferences.log_file_level.clone()",
        },
    },
    Consumption {
        key: "preferences.log_level",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "console_level: settings.preferences.log_level.clone()",
        },
    },
    // ⚠ `preferences.rate_utilization` stood here as a `Consumer::Not`, and its own `why` recorded
    // the knock-on that finished it: `policy.rate.max_utilization` clamped this field and nothing
    // else, so a POLICY CEILING bounded a value no code consumed. Both halves are now tombstones
    // that refuse the key by name (`crate::preferences`' module doc carries the argument). Deleting
    // rather than keeping the honest `Not` row, because `config show` warns from `unconsumed_keys`
    // — which covers this table only. `policy.*` answers `is_consumed` = `true` by construction, so
    // the CEILING would have gone on displaying as effective with no warning at all: the
    // `max_total_exposure` defect exactly, and the reason that field was deleted rather than
    // annotated.
    Consumption {
        key: "preferences.sweep_threads",
        by: Consumer::At {
            // ⚠ **WIRED, and it was the table's last `Reader::Live` row** — the one key where the
            // ENVIRONMENT spelling worked and only the FILE key was inert. Its `Not` row argued the
            // obstacle honestly (through two corrections): this is a PROCESS-WIDE RESOURCE KNOB
            // read at rayon pool CONSTRUCTION, entered from three front doors in two crates
            // (`optimize.rs`'s fan-out, `walkforward.rs`, and the public `map_bounded`
            // vike-studio-core takes), none of which carries a settings value — so wiring it
            // needed "a caller-owned process-wide handle, an `init(spec)` + `OnceLock`".
            //
            // That is exactly what landed: `vike_backtest::harness::install_sweep_threads` is the
            // handle, `sweep_threads` reads it BEFORE the environment (the installed value IS the
            // resolved `env > file > default` answer, so consulting the variable again would be a
            // second authority), and two composition roots fill it.
            //
            // ⚠ **The needle names the `--addr` compute server and NOT the desktop, and the choice
            // is deliberate.** There are three consuming processes and they do not share a root:
            // the Studio inside `vike-desktop` (wired — `vike_studio::install_sweep_threads`, which
            // exists because that binary does not link vike-backtest), `backtest --addr`'s compute
            // server (wired — the needle below), and a ONE-SHOT `backtest` run, which is what
            // `crates/vike-cli/src/cmd/backtest.rs`'s `execute_local` SPAWNS as a child and which
            // loads NO settings at all. The needle names the root INSIDE the crate that owns the
            // pool, so the evidence stands whether or not the GUI is built — the same reason
            // `Reader::Live`'s `caller` field named `optimize.rs` rather than the Studio.
            //
            // ⚠ **The one-shot child is a declared RESIDUAL, not an oversight.** Giving it the file
            // key means making a non-`--addr` `backtest` invocation load settings, which is a
            // behaviour change to every scripted run (a malformed `config.toml` would start failing
            // one-shot backtests) and belongs to whoever wants it, not to this key's wiring. Until
            // then that process is capped by `VIKE_SWEEP_THREADS` and the compiled-in fallback,
            // exactly as it always was — and the environment is inherited by the child, so the CLI
            // path an operator drives is not left without a lever.
            file: "crates/vike-backtest/src/backtest_cli.rs",
            needle: "install_sweep_threads(settings.preferences.sweep_threads)",
        },
    },
    // ---------------------------------------------------------------------------------------
    // flags.* — operator toggles. Every `Not` row names the LIBRARY that owns the read today;
    // `FLAG_REGISTRY` carries each one's owner, review date and expected disposition.
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "flags.allow_withdraw_keys",
        by: Consumer::At {
            // ⚠ A SAFETY OVERRIDE, wired like any other setting by the owner's ruling that every
            // setting is editable from the UI, live gates included. What makes that safe is not the
            // ruling but the SHAPE, and the shape took a CORRECTION after review: `false` is the
            // guarded state, the value folded into the map is the RESOLVED flag, and the fold
            // writes it with `insert` rather than `or_insert` (`FoldTier::Resolved`).
            //
            // ⚠ That second half was missing when this row was first written, and the row asserted
            // the conclusion anyway. `binance_withdraw_gate` reads the process env FIRST and ORs
            // the map in SECOND, so "the file can never widen what an operator refused" holds only
            // if the map cannot carry a `1` the environment contradicted — and under `or_insert` a
            // `VIKE_ALLOW_WITHDRAW_KEYS=1` line already sitting in `<project>/settings/secrets.env`
            // survived the fold untouched and armed a live-money gate past an exported `=0`. The
            // credential store is not a tier for this key and never was; it is overwritten now.
            // `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `a_process_env_value_beats_a_file_value_for_every_wired_key` and
            // `crates/vike-mount/src/arming.rs`'s
            // `the_withdraw_override_is_refused_when_the_environment_says_zero` are the two halves
            // of the proof, meeting at the string `"0"`.
            //
            // ⚠ The seam widened exactly as the old row said it would have to, but not in the
            // direction that row imagined: `vike-mount` still accepts no `Flags`, and it does not
            // need to — `make_engine` already takes the `&HashMap` every venue fact travels on, so
            // the composition root FILLS that map and the library reads it. That is why nine keys
            // moved in one change rather than nine.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.allow_withdraw_keys, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.hl_outcome",
        by: Consumer::Not {
            // ⚠ CORRECTED. The old text ended "no composition root sees the decision", which reads
            // as "so export the variable instead" — and `config show` printed it to an operator who
            // had configured the key. The poller nothing mounts is also the only thing that reads
            // the variable, so the advice pointed at a second dead spelling.
            why: "BOTH SPELLINGS ARE INERT. `VIKE_HL_OUTCOME` is read by \
                  crates/bridges/hyperliquid/src/outcome_settlement.rs's `hl_outcome_enabled`, \
                  which is called from ONE place — that file's own `OutcomePoller::spawn` — and \
                  nothing outside the file's `#[cfg(test)]` module ever constructs that poller. So \
                  exporting the variable arms nothing either. What is missing is not a \
                  flag-threading change but a composition root that MOUNTS the settlement poller, \
                  and mounting one means a live venue writing synthetic terminal fills into the \
                  core — its own decision, taken with the FLAG_REGISTRY stewardship row this key \
                  would lose if the field were deleted.",
            reader: Reader::Uncalled {
                file: "crates/bridges/hyperliquid/src/outcome_settlement.rs",
                needle: "pub fn hl_outcome_enabled() -> bool",
                entry: &["OutcomePoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.hyperliquid_hip3",
        by: Consumer::At {
            // The cheapest of the nine, because the pure core already took the bool:
            // `HyperliquidInstruments::load_from(fetch, recorder, hip3)` has always been a
            // parameter, and only the impure wrapper above it reached for the process env. That
            // wrapper now has a map-taking twin (`load_from_vars`) and `vike-mount`'s hyperliquid
            // arm calls it with the `vars` it was already holding. The fold OVERWRITES this key
            // (`FoldTier::Resolved`): this reader took no credential-store tier before the twin
            // existed, so a `HYPERLIQUID_HIP3` line in `secrets.env` must not become one.
            //
            // ⚠ **A SECOND reader exists and this wiring does NOT reach it**, named here so the
            // row does not read as complete when it is not:
            // `crates/bridges/hyperliquid/src/catalog.rs`'s `HyperliquidCatalog` calls the
            // map-less `load`, so the picker's instrument universe would answer differently about
            // HIP-3 markets from the mount's symbology. It is INERT rather than shipping — no
            // binary constructs that provider since the desktop cut left `spawn_catalog_fetcher`
            // with `vike_deribit::DeribitCatalog` alone — and the call site carries the same note,
            // so a binary that re-links it is told to thread a map instead.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.hyperliquid_hip3, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.oco_cancel_sibling_on_dead_exit",
        by: Consumer::At {
            // …and the identical line in crates/vike-desktop/src/main.rs's `App::new`, plus the
            // daemon's PAPER arm via `vike_run::PaperMountOpts`.
            //
            // ⚠ The needle deliberately points at the DAEMON. vike-app was this flag's only reader
            // for its whole life, so an OCO safety behaviour existed in the GUI and could not be
            // turned on at all on the server that runs unattended — exactly the deployment where
            // "leave the book flat once protection dies" is most likely to be the wanted answer.
            // Anchoring the row here is what makes deleting the daemon's read fail this gate.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,",
        },
    },
    Consumption {
        key: "flags.cancel_orders_on_shutdown",
        by: Consumer::At {
            // The daemon's LIVE arm; the PAPER arm reads the same flag through
            // `vike_run::PaperMountOpts`, and both land on
            // `vike_core::CoreConfig::cancel_orders_on_shutdown`, which the core's teardown block
            // checks before it detaches the client.
            //
            // ⚠ WHAT THIS ROW DOES *NOT* CLAIM. Being consumed is not being reachable: under
            // `deploy/vike-tradehub.service` stdin is `/dev/null` and SIGTERM has no handler, so
            // `systemctl stop` never reaches the teardown this flag gates. The flag is honoured on
            // an interactive `shutdown`/`quit`/Ctrl-D stop. That gap is a property of the STOP
            // PATH, not of the wiring, and no consumption gate can see it —
            // `docs/ops/kill-switches.md` is where it is written down for the operator.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,",
        },
    },
    Consumption {
        key: "flags.pm_resolve",
        by: Consumer::Not {
            // ⚠ CORRECTED. "Same per-venue seam as the rest of the Polymarket family" was true and
            // was also the wrong half to state: the family's seam is not that the read sits in a
            // library, it is that NOTHING MOUNTS THE FAMILY.
            // `crates/bridges/polymarket/src/settlement/mod.rs`'s module doc says so outright —
            // "No composition root constructs anything here … the only drivers are this crate's
            // own tests and `#[ignore]`d live smokes" — and this row used to contradict it.
            why: "BOTH SPELLINGS ARE INERT. `VIKE_PM_RESOLVE` is read by \
                  `crates/bridges/polymarket/src/settlement/resolve.rs`'s `pm_resolve_enabled`, \
                  reached only from `ResolvePoller::spawn_with_deps`, whose two public doors \
                  (`ResolvePoller::spawn` and `ResolvePoller::spawn_with_chain`) are called from \
                  nothing outside that file's own tests. That file's module tree says the same: \
                  `crates/bridges/polymarket/src/settlement/mod.rs` records that no composition \
                  root constructs anything in the cluster. Re-arming this needs a root that MOUNTS \
                  the resolve poller — a settlement pass that writes synthetic terminal fills into \
                  the core — not a line of threading.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/resolve.rs",
                needle: "pub fn pm_resolve_enabled() -> bool",
                // BOTH public doors, not just the one the old text named: `spawn_with_deps` is
                // private, so naming it alone would leave `spawn_with_chain` free to acquire a
                // caller with this row still green.
                entry: &["ResolvePoller::spawn", "ResolvePoller::spawn_with_chain"],
            },
        },
    },
    Consumption {
        key: "flags.poly_auto_redeem",
        by: Consumer::Not {
            // ⚠ CORRECTED, and this one the tree already KNEW in another place while stating the
            // opposite here: `crates/vike-ops/tests/kill_switch_gate.rs` carries a row "RETIRED:
            // the auto-redeem poller is deliberately unspawned", whose text reads "nothing outside
            // tests constructs this poller". Two gated tables, one fact, and they disagreed.
            why: "BOTH SPELLINGS ARE INERT — and the flag is KEPT anyway, which is the unusual \
                  part. `POLY_AUTO_REDEEM` is read by crates/bridges/polymarket/src/settlement/\
                  auto_redeem.rs's `auto_redeem_enabled`, reached only from \
                  `AutoRedeemPoller::spawn`, which nothing outside that file's own tests \
                  constructs, so exporting the variable arms nothing. Spawning it is not a wiring \
                  task: it is NEW UNATTENDED ON-CHAIN MONEY MOVEMENT, refused deliberately — \
                  `crates/vike-ops/tests/kill_switch_gate.rs`'s RETIRED row is where that decision \
                  is recorded. The flag's own disposition stays KEEP-as-a-flag because an explicit \
                  per-run opt-in is what such a mount would need on the day it is taken.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/auto_redeem.rs",
                needle: "pub fn auto_redeem_enabled() -> bool",
                entry: &["AutoRedeemPoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.poly_exec",
        by: Consumer::At {
            // ⚠ The old row was right that a `Flags` field must not REPLACE one of the two tiers —
            // and that is not what happened. `poly_exec_enabled`'s hybrid read is untouched: the
            // process env is still consulted first and the credentials map second. The composition
            // root fills the SECOND tier, and only where nothing has filled it already
            // (`FoldTier::CredentialStoreFirst`), so a `POLY_EXEC=1` line in the credential store
            // still wins over the file and an exported one still wins over both.
            //
            // ⚠ **This is the ONLY family that keeps that tier, and it keeps it because a
            // credential-store line here cannot reach a RUNNING daemon.** `POLY_EXEC` is a
            // `crate::arming::CREDENTIAL_FILE_ARMING_REFUSED` row, so
            // `refuse_credential_file_arming` stops the process at `vike-boot` step 3 rather than
            // letting that line outrank the environment. Every other folded key is OVERWRITTEN by
            // the fold for want of that refusal, and the pairing is machine-checked —
            // `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `the_credential_store_tier_is_exactly_the_refused_arming_set` fails on a tier chosen
            // any other way.
            //
            // ⚠ The value folded in is the RESOLVED flag, which is what makes this safe for a
            // REAL-MONEY arming gate: `vike_config::Flags` applies the environment layer over the
            // file before this root sees it, so the two sources cannot disagree in principle.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.poly_exec, FoldTier::CredentialStoreFirst)",
        },
    },
    Consumption {
        key: "flags.poly_heartbeat",
        by: Consumer::Not {
            // ⚠ CORRECTED, and the comparison to `poly_exec` is exactly what made the old text
            // misleading: the two reads have the same SHAPE, and `poly_exec`'s runs while this one
            // does not. `crates/bridges/polymarket/src/lib.rs` lists `heartbeat` under "PARKED —
            // built + fixture-tested, NO production caller today", which is the fact this row
            // should have carried from the start.
            why: "BOTH SPELLINGS ARE INERT. `POLY_HEARTBEAT` is read by \
                  crates/bridges/polymarket/src/heartbeat.rs's `heartbeat_enabled` — the same \
                  hybrid env-then-credentials-map read as `poly_exec`, and that resemblance is the \
                  trap: the read is consumed inside `HeartbeatPoller::spawn`, which no composition \
                  root calls (`crates/bridges/polymarket/src/lib.rs` lists the module as PARKED, \
                  with no production caller). In FLAG shape this is the closest of the six to one \
                  line of work — the moment a root spawns the poller the fold is one `FoldTier` \
                  row in `crates/vike-tradehub/src/tradehub_cli.rs` — but its `FLAG_REGISTRY` \
                  disposition is RETIRE, so the registry's own answer is that this key should \
                  cease to exist rather than be wired.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/heartbeat.rs",
                needle: "pub fn heartbeat_enabled(vars: &HashMap<String, String>) -> bool",
                entry: &["HeartbeatPoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.poly_reconcile",
        by: Consumer::At {
            // Same shape and same argument as `flags.poly_exec` above: the second tier of an
            // untouched hybrid read, filled only where nothing else filled it — and permitted to
            // keep that tier for the same reason, `POLY_RECONCILE` being a
            // `crate::arming::CREDENTIAL_FILE_ARMING_REFUSED` row too.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.poly_reconcile, FoldTier::CredentialStoreFirst)",
        },
    },
    Consumption {
        key: "flags.poly_redeem_halt",
        by: Consumer::Not {
            // ⚠ CORRECTED — carefully, because this row is a KILL SWITCH and the correction must
            // not read as a reason to delete it. Both arguments survive intact: the second trip
            // condition (a halt FILE on disk) is still unmodelled, so wiring only the env half
            // would still narrow the switch. What was wrong is only the implied "so use the
            // variable": the switch guards `AutoRedeemPoller`, which nothing spawns, so there is
            // nothing running for either spelling to halt. The row STAYS regardless — deleting a
            // guard while the thing it guards survives puts the guard AFTER its subject, which is
            // the argument `crates/vike-ops/tests/kill_switch_gate.rs`'s RETIRED row already makes.
            why: "BOTH SPELLINGS ARE INERT TODAY, because there is nothing running to halt. \
                  `POLY_REDEEM_HALT` is read by crates/bridges/polymarket/src/settlement/\
                  auto_redeem.rs's `kill_switch_tripped`, by PRESENCE, every tick of \
                  `AutoRedeemPoller` — and nothing outside that file's own tests ever constructs \
                  that poller. Two reasons this is KEPT rather than tombstoned: it has a SECOND \
                  trip condition this type does not model at all, a halt FILE on disk, so wiring \
                  the env half alone would silently NARROW a kill switch; and the poller it guards \
                  still exists, so deleting the switch would leave the guard behind its subject.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/auto_redeem.rs",
                needle: "pub fn kill_switch_tripped(halt_file: &Path) -> bool",
                // The same door as `flags.poly_auto_redeem` above, deliberately: the switch is
                // read on the poller's tick, so the poller's construction is what both rows turn
                // on. Two rows naming one entry is the truth, not a duplication.
                entry: &["AutoRedeemPoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.preflight_skip",
        by: Consumer::At {
            // ⚠ The second SAFETY OVERRIDE, and the same reading as `flags.allow_withdraw_keys`
            // above — INCLUDING the correction. `run_startup_preflight` still sweeps the process
            // env FIRST and ORs its `vars` argument in second
            // (`vike_mount::startup::preflight_skip_requested`), so the file is a second SOURCE and
            // never an override; what makes that true rather than merely claimed is that the fold
            // OVERWRITES this key (`FoldTier::Resolved`) instead of leaving a `VIKE_PREFLIGHT_SKIP`
            // line in the credential store standing to be OR-ed in. Proved by
            // `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `a_process_env_value_beats_a_file_value_for_every_wired_key` and, on the reader's own
            // side, `crates/vike-mount/src/startup.rs`'s
            // `the_preflight_is_not_skipped_when_the_environment_says_zero`.
            //
            // The old row's "deliberately reading process env rather than its `vars` argument" is
            // still true of the first half — the sweep is exactly where it was, for exactly the
            // reason that comment gives (a shell-exported `=1` must not become invisible).
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.preflight_skip, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.reconcile",
        by: Consumer::At {
            // …and crates/vike-desktop/src/main.rs's `App::new`, which folds the same resolved value
            // through the same `reconcile_gate`. Since S2 this flag is one of THREE inputs to that
            // gate rather than the gate itself — it forces the driver on where the armed-live
            // probe reports nothing — so the needle follows the call and not a bare assignment.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "reconcile_config::reconcile_gate(flags.reconcile,",
        },
    },
    Consumption {
        key: "flags.reconcile_balance",
        by: Consumer::At {
            // ⚠ The family did NOT have to move — the map did. `build_recon_config` reads the whole
            // `VIKE_RECONCILE_*` family out of ONE caller-supplied map, which its own module doc
            // calls "purely a TESTABILITY seam"; filling that map is therefore the entire wiring,
            // and the cadences, lookbacks and policy name beside it are untouched. Nothing can
            // disagree about a single pass because there is still exactly one map.
            //
            // `daemon_recon_env` starts from `process_env()` (through
            // `reconcile_config::quarantine_first_default`) and `or_insert`s the resolved flag, so
            // a `VIKE_RECONCILE_BALANCE=0` in a systemd drop-in is already in the map and nothing
            // below replaces it. ⚠ That is a property of the BASE MAP, not of a tier — this family
            // never touches the credential store — and it is pinned by
            // `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `the_process_env_beats_the_resolved_flag_in_the_reconcile_family`, which drives the
            // pure half (`daemon_recon_env_from`) with an already-exported `0`.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.reconcile_balance)",
        },
    },
    Consumption {
        key: "flags.reconcile_generate_missing",
        by: Consumer::At {
            // The same one map, filled in the same place — see `flags.reconcile_balance` above for
            // why filling it is the whole of the wiring and why the environment still wins.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.reconcile_generate_missing)",
        },
    },
    Consumption {
        key: "flags.reconcile_off",
        by: Consumer::At {
            // The REFUSAL half of the same `reconcile_gate` call, in the same two roots. It gets
            // its own row because an operator reading `config show` needs the OFF switch to report
            // `READ: yes` on its own evidence: a safety override merely believed to be wired is the
            // exact failure this table exists to remove.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.reconcile_off,",
        },
    },
    Consumption {
        key: "flags.record_chains",
        by: Consumer::Not {
            // ⚠ CORRECTED, and this row's reader is the emptiest of the six: the other five are
            // reached from a poller nothing spawns, while `ChainRecorder::open_from_env` has NO
            // call site at all, in production or in tests. Its one production site is a tombstone
            // — `crates/vike-desktop/src/main.rs` binds `chain_rec` to `None` with a comment saying
            // `VIKE_RECORD_CHAINS=1` is inert in that binary.
            //
            // ⚠ CORRECTED A SECOND TIME, and the first correction's own MECHANISM was the error.
            // It said the Options tool "went with the desktop cut" and that re-arming needs "a
            // root that FETCHES option chains, and the surviving daemon fetches none". The tool
            // did not go: the desktop still calls `spawn_tool_fetchers` and still polls chains,
            // and the tombstone at that very call site says so — "the Options tool FETCHES exactly
            // as before, it just records nothing". Pointing the next mount at a missing FETCH
            // would have sent it to build the one thing that is already there.
            why: "BOTH SPELLINGS ARE INERT. `VIKE_RECORD_CHAINS` is read by \
                  crates/vike-data/src/chain_rec.rs's `ChainRecorder::{from_env, open_from_env}`, \
                  which also read the `VIKE_RECORD_CHAINS_CADENCE_MS` sibling in the same call — \
                  and NEITHER constructor is called from anywhere in the tree. What is missing is \
                  the STORE, not the fetch: the desktop's Options tool still polls option chains \
                  every 30s through `vike_app_core::tools::spawn_tool_fetchers`, and simply passes \
                  `None` for the recorder, because that binary links vike-data with DEFAULT \
                  features (the trait-only `HistStore` seam) while `open_from_env` is a \
                  `hist-datafusion` constructor — there is no engine there to open. Re-arming it \
                  needs a root that can open a store BESIDE a chain fetch: the desktop has the \
                  fetch and no store, vike-tradehub has a store (behind `record-feeds`) and no \
                  fetch. The recorder and its store kind are intact and unmounted, which is what \
                  `Consumer::Not` is for.",
            reader: Reader::Uncalled {
                file: "crates/vike-data/src/chain_rec.rs",
                // The read is inside `env_snapshot`, the private map both impure wrappers build.
                needle: "fn env_snapshot()",
                // BOTH wrappers, because they are separate doors: `open_from_env` reaches the read
                // through `open_from_vars`, `from_env` through `from_vars`. Naming one would let
                // the other acquire a caller with this row still green.
                entry: &["ChainRecorder::from_env", "ChainRecorder::open_from_env"],
            },
        },
    },
    // ⚠ `flags.record_dvol` stood HERE, as the table's one `Reader::Nothing` row, and the key is
    // DELETED rather than carried. Its own row argued for keeping it (the feature is intact and
    // unmounted, and the `FLAG_REGISTRY` stewardship row is the mechanism that gets it re-mounted)
    // — and that argument is what a field being kept by its paperwork looks like. The tombstone is
    // `crate::flags::DEAD_FLAG_KEYS`, which carries the surviving symbols and what a re-mounting
    // root must supply, so nothing this row was protecting was lost. The CONSUMPTION table has no
    // opinion about a key that does not exist, which is why the row goes rather than turning into
    // a permanent admission.
    Consumption {
        key: "flags.record_properties",
        by: Consumer::At {
            // The old row was right that "this one needs only a non-env constructor in vike-data",
            // and righter than it knew: `PropertiesRecorder::open_from_vars` already existed, as
            // the map half `open_from_env` wraps. The daemon calls the map half instead, over the
            // `vars` the flags fold has already filled.
            //
            // ⚠ **That "wraps" is the whole of why this key is folded with `insert`**, and the
            // first version of this wiring got it wrong. `open_from_env` is
            // `open_from_vars(root, &env_snapshot())` — a map holding this ONE variable, read from
            // the PROCESS ENV — so handing the map half a DIFFERENT map replaces the environment
            // read rather than widening it. With the fold's `or_insert`, a `VIKE_RECORD_PROPERTIES`
            // line already in `<project>/settings/secrets.env` (never a tier for this key) became
            // authoritative in BOTH directions: an exported `=1` stopped arming the recorder and an
            // exported `=0` stopped disarming it. `FoldTier::Resolved` makes the map's value
            // exactly `Flags`' answer, which is the file with the environment applied over it.
            //
            // ⚠ vike-app is no longer one of the binaries this row could name — the desktop mounts
            // no venue and opens no recorder. `vike-tradehub` is the only root left, and only on a
            // `record-feeds`/`materialize` build: a DEFAULT (DataFusion-free) daemon cannot open
            // the store at all and passes `None`, so the flag is inert there whatever sets it.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "flags.record_properties, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.telegram_control",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.flags.telegram_control",
        },
    },
    Consumption {
        key: "flags.tradehub_allow_public_bind",
        by: Consumer::At {
            // Read at the ONE call site that decides whether a remote order-write surface opens,
            // beside the address it guards — `start_observe_server` refuses a non-loopback bind
            // without it.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.flags.tradehub_allow_public_bind",
        },
    },
    Consumption {
        key: "flags.tradehub_control",
        by: Consumer::At {
            // ⚠ RESIDUAL, on the record: `vike-app --observe`'s own client-side control gate is a
            // SECOND read — `vike_app_core::tradehub_control::control_enabled`, a library
            // `std::env::var` — and it still honours the variable only. The DAEMON, which owns the
            // remote order-write surface this flag opens, is wired here.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.flags.tradehub_control",
        },
    },
    Consumption {
        key: "flags.tradehub_live",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.flags.tradehub_live",
        },
    },
    Consumption {
        key: "flags.tradehub_record",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "open_tradehub_recorder(flags.tradehub_record)",
        },
    },
    Consumption {
        key: "flags.venue_catalog_off",
        by: Consumer::At {
            // The REFUSAL half of the venue-catalog gate, on the ONE root that owns the lane.
            //
            // ⚠ This row is the reason `docs/decisions/0066`'s decision 3 is an ORDERING ruling
            // rather than a note: until that PR, `vike-datahub` handed `vike_boot::boot` a
            // `SettingsLoad::Skip`, so `flags.toml` was opened by nothing in that process and the
            // honest row here would have been `Consumer::Not` — `vike-cli config show` printing
            // `READ: NO` beside the very switch the record introduces, and a default-on behaviour
            // with no reachable off switch. The `Skip` reason had predicted its own end ("It joins
            // when it reads them"); this key is what made it join.
            //
            // ⚠ The needle is the ARGUMENT, not the call: `venue_catalog_gate(` and its first
            // argument sit on different LINES once rustfmt has been over them (the call is past
            // `max_width` on one), so a needle spanning both would be satisfied only by a
            // formatting accident. The trailing comma is load-bearing — it is what makes this an
            // argument PASSED to the gate rather than a value assigned to a local nothing reads,
            // which is the distinction `Consumer::At` exists to draw.
            file: "crates/vike-datahub/src/datahub_cli.rs",
            needle: "booted.settings.flags.venue_catalog_off,",
        },
    },
];

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
/// FILE half prints "NEITHER SPELLING DOES ANYTHING" for six flags ([`Reader::verdict`]), while its
/// ENVIRONMENT half lists those same variables in a table headed *"precedence: env > secrets.env >
/// default (READS = what the reader consults)"* — one command, two answers, and the more
/// authoritative-looking half was the wrong one. The env table now names the dead rows underneath
/// itself, from HERE, so the two halves cannot disagree.
///
/// DERIVED, never a second list: [`FLAG_REGISTRY`](crate::FLAG_REGISTRY) already maps each flag
/// field to its variable, and [`CONSUMPTION`] already carries the [`Reader`]. A row added to either
/// changes this answer with no edit here. The filter is `!`[`Reader::env_still_works`], so a
/// [`Reader::Live`] key — where exporting the variable IS the honest advice — deliberately says
/// nothing.
#[must_use]
pub fn env_verdict(var: &str) -> Option<&'static str> {
    let field = crate::FLAG_REGISTRY.iter().find(|m| m.env == var)?.field;
    consumer_of(&format!("flags.{field}"))?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_key_is_not_reported_as_unread() {
        // `policy.*` has its own gate; answering `false` here would make `config show` warn about
        // ceilings that ARE enforced.
        assert!(is_consumed("policy.max_notional_per_order"));
        assert!(is_consumed("no.such.key"));
        assert_eq!(consumer_of("no.such.key"), None);
    }

    #[test]
    fn a_wired_key_and_an_unwired_one_answer_differently() {
        assert!(is_consumed("config.log_dir"));
        // ⚠ `flags.poly_heartbeat`, not `flags.poly_exec`: the unread-settings sweep WIRED
        // `poly_exec`, and an example that has moved to the other column proves the opposite of
        // what it says. The heartbeat flag is one of the seven the owner deliberately DEFERRED.
        assert!(!is_consumed("flags.poly_heartbeat"));
        assert!(consumer_of("config.log_dir").unwrap().file().is_some());
        assert!(consumer_of("flags.poly_heartbeat").unwrap().why_not().is_some());
    }

    /// **The keys a headless box was told it could set.** `config.store_root` and
    /// `preferences.chart_style` reported `READ: yes` while their only reader is the GUI — so on a
    /// tradehub or recorder box, setting one did nothing and the output said it would work.
    ///
    /// ⚠ `config.state_dir` WAS the third. It left this list when the desktop cut deleted
    /// `state_dir_path`, its one reader, and it has now left the TABLE: the unread-settings sweep
    /// deleted the key and refuses both spellings.
    /// `the_deleted_state_dir_key_is_gone_from_this_table_entirely` is what holds it gone —
    /// dropping it silently would have turned a key that configures NOTHING into a key this suite
    /// simply stopped asking about.
    #[test]
    fn a_gui_only_setting_names_the_gui() {
        for key in ["config.store_root", "preferences.chart_style"] {
            let c = consumer_of(key).unwrap_or_else(|| panic!("{key} must have a row"));
            assert!(c.is_consumed(), "{key} is read — that part was never wrong");
            assert_eq!(
                c.binary(),
                Some("desktop"),
                "{key}'s only reader is vike-desktop, and the column has to say so"
            );
        }
    }

    /// The END of `config.state_dir`. It was a `Consumer::At` on the GUI, then a written admission
    /// that its one reader had gone, and it is now **no key at all** — which is what that
    /// admission's own text asked for ("deletion plus a `REMOVED_ENV` refusal, as its own change").
    ///
    /// This test replaces `a_gui_only_setting_that_lost_its_reader_says_so` and is the same
    /// property from the other side: what must never happen is the key coming back WITHOUT a
    /// reader. A row here would put it back in `vike-cli config show`'s table, and the refusals in
    /// `crate::removed` and `crate::config::ConfigPatch` would then contradict it.
    #[test]
    fn the_deleted_state_dir_key_is_gone_from_this_table_entirely() {
        assert!(
            consumer_of("config.state_dir").is_none(),
            "config.state_dir was DELETED — both spellings refuse now. A row here would make \
             `config show` list a key the loader will not accept"
        );
        assert!(
            !unconsumed_keys().contains(&"config.state_dir"),
            "...and it must not be warned about either: there is nothing left to set"
        );
    }

    /// …and a daemon-side key names the daemon, or the column would be a constant.
    #[test]
    fn a_daemon_setting_names_the_daemon() {
        for key in ["config.log_dir", "config.tradehub_addr", "flags.tradehub_control"] {
            assert_eq!(consumer_of(key).and_then(Consumer::binary), Some("tradehub"), "{key}");
        }
    }

    /// An UNREAD row has no binary — there is nothing to name — and neither does a hypothetical
    /// library consumer, whose read belongs to every binary that links it rather than to one.
    #[test]
    fn an_unread_row_and_a_library_consumer_name_no_binary() {
        assert_eq!(consumer_of("flags.poly_heartbeat").and_then(Consumer::binary), None);

        let lib = Consumer::At { file: "crates/vike-ops/src/reconcile_config.rs", needle: "x" };
        assert_eq!(
            lib.binary(),
            None,
            "a library read has no single owning binary, so `config show` must keep saying `yes` \
             rather than invent one"
        );
        // A `src/bin/` entry point resolves like a `main.rs` one, prefix stripped either way.
        let b = Consumer::At { file: "crates/vike-backfill/src/bin/eod_backfill.rs", needle: "x" };
        assert_eq!(b.binary(), Some("eod_backfill"));
        let nested = Consumer::At { file: "crates/bridges/polymarket/src/main.rs", needle: "x" };
        assert_eq!(nested.binary(), Some("polymarket"));
    }

    /// Every consumed row resolves to SOMETHING renderable — a binary name or an honest `None`.
    /// The rule must not panic or produce an empty string on any path in the real table.
    #[test]
    fn the_rule_survives_every_row_in_the_real_table() {
        for row in CONSUMPTION {
            if let Some(b) = row.by.binary() {
                assert!(!b.is_empty(), "{} produced an empty binary name", row.key);
                assert!(!b.contains('/'), "{} produced a path, not a name: {b}", row.key);
            }
        }
    }

    /// **The verdict an operator reads is DERIVED from the variant, and the three differ.**
    ///
    /// The whole point of [`Reader`] is that two of these states must not render as the third.
    /// `flags.poly_heartbeat` said "use the environment variable" while the variable did nothing;
    /// a rendering rule that collapsed the variants would hand that defect straight back.
    #[test]
    fn the_three_reader_states_tell_an_operator_three_different_things() {
        let live = Reader::Live {
            file: "crates/vike-backtest/src/harness/sweep.rs",
            needle: "x",
            caller: "crates/vike-backtest/src/harness/optimize.rs",
            call: "y",
        };
        let uncalled = Reader::Uncalled { file: "f", needle: "n", entry: &["T::spawn"] };

        assert!(live.env_still_works(), "the one state in which exporting the variable helps");
        assert!(!uncalled.env_still_works());
        assert!(!Reader::Nothing.env_still_works());

        // …and the three verdicts are genuinely distinct strings, not one message with decoration.
        let verdicts = [live.verdict(), uncalled.verdict(), Reader::Nothing.verdict()];
        for (i, a) in verdicts.iter().enumerate() {
            for b in &verdicts[i + 1..] {
                assert_ne!(a, b, "two Reader states render identically");
            }
        }
        assert!(live.verdict().contains("still works"));
        assert!(uncalled.verdict().contains("NEITHER SPELLING"));
        assert!(Reader::Nothing.verdict().contains("NEITHER SPELLING"));
    }

    /// The six rows this change was made for: they claim `Uncalled`, and an operator who set one
    /// must be told the environment variable is dead too.
    ///
    /// Pinned by NAME rather than by counting `Uncalled` rows, because the failure being prevented
    /// is a specific row quietly going back to claiming a live variable — a count would stay green
    /// through a swap.
    #[test]
    fn the_six_dead_variable_rows_say_so() {
        for key in [
            "flags.hl_outcome",
            "flags.pm_resolve",
            "flags.poly_auto_redeem",
            "flags.poly_heartbeat",
            "flags.poly_redeem_halt",
            "flags.record_chains",
        ] {
            let c = consumer_of(key).unwrap_or_else(|| panic!("{key} must have a row"));
            let reader = c.reader().unwrap_or_else(|| panic!("{key} must be a Consumer::Not"));
            assert!(
                !reader.env_still_works(),
                "{key}: this row told operators to export a variable that nothing reachable reads"
            );
            assert!(
                c.unread_verdict().is_some_and(|v| v.contains("NEITHER SPELLING")),
                "{key}: `config show` must lead with the fact that neither spelling works"
            );
        }
    }

    /// **NO row claims a live variable today, and that is a state worth asserting rather than
    /// leaving to be noticed.**
    ///
    /// ⚠ This test was `the_one_live_variable_row_says_so`, and its subject was
    /// `preferences.sweep_threads` — the one key where exporting `VIKE_SWEEP_THREADS` genuinely
    /// worked while the FILE key was inert. That key is now WIRED (`Consumer::At`, two composition
    /// roots), so [`Reader::Live`] has no rows left, and the sibling test above stopped having a
    /// row-level counterexample.
    ///
    /// The variant STAYS, and this is the reason: its six `Uncalled` neighbours each become `Live`
    /// the moment something calls the entry point they name, which is a real and expected
    /// transition. What keeps `env_still_works` honest in the meantime is
    /// [`the_three_reader_states_tell_an_operator_three_different_things`] above, which constructs
    /// one of each variant — a unit-level check that cannot rot with the table.
    ///
    /// So what this asserts is the FACT, not a policy: a row added in that state is a finding to
    /// read, not a failure, and the assertion message says which.
    #[test]
    fn no_row_claims_a_live_variable_today() {
        let live: Vec<&str> = CONSUMPTION
            .iter()
            .filter(|c| c.by.reader().is_some_and(|r| r.env_still_works()))
            .map(|c| c.key)
            .collect();
        assert!(
            live.is_empty(),
            "a row is back in the `Reader::Live` state ({live:?}). That is legitimate — it means \
             something now reaches that key's env read — but it also means the sibling test's \
             `!env_still_works` assertion has a real counterexample again: re-point this test at \
             the row rather than deleting it."
        );
    }

    /// A CONSUMED row has no verdict to render — there is nothing unread to explain.
    #[test]
    fn a_consumed_row_has_no_reader_and_no_verdict() {
        let c = consumer_of("config.log_dir").expect("row");
        assert_eq!(c.reader(), None);
        assert_eq!(c.unread_verdict(), None);
    }

    #[test]
    fn the_unconsumed_list_is_exactly_the_not_rows() {
        let listed = unconsumed_keys();
        assert_eq!(listed.len(), CONSUMPTION.iter().filter(|c| !c.by.is_consumed()).count());
        assert!(listed.contains(&"flags.record_chains"));
        assert!(!listed.contains(&"config.tradehub_addr"));
        // ⚠ `flags.record_dvol` was asserted HERE, as the row that went BACKWARDS. The key is
        // deleted (`crate::flags::DEAD_FLAG_KEYS`), so this table has nothing to say about it and
        // the assertion goes with it — asserting the ABSENCE of a deleted key would pin a fact
        // about nothing and would have to be deleted again the day the feed is re-mounted.
    }
}
