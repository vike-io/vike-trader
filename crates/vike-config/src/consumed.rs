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
//! Every `Not` row below is a setting whose variable IS read today — by a LIBRARY, deep in a venue
//! adapter or a recorder, through `std::env::var` (or through a map plus its own `.env` file read).
//! Wiring one is not a line change: it is threading a value from a composition root down through
//! `vike_run::NodeConfig` / `vike_mount::make_engine` into the adapter, per flag, which is Phase 6
//! of the settings-unification design and is deliberately done a flag at a time, together with the
//! env read it replaces. A flag living in two places that DISAGREE would be worse than the state
//! this program is fixing.
//!
//! What must NOT wait for that is the CLAIM. So until a row's reader moves, the row says `Not`, the
//! gate holds it to a written reason, and `config show` tells the operator to use the environment
//! variable instead of believing the file. The alternative the design offers — delete the field —
//! is wrong for these: they are genuinely file-settable settings whose readers have not moved yet,
//! and deleting them would also delete their `FLAG_REGISTRY` stewardship rows (owner + review date
//! + disposition), which are the mechanism that makes the move happen.

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
    /// reader that owns the variable today, so this doubles as the work list.
    Not {
        /// Which code reads the environment variable instead, and what wiring it would take.
        why: &'static str,
    },
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
            Consumer::Not { why } => Some(*why),
        }
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
            // it to the `study` arm as a parameter (a `src/cmd/` file reads no settings of its
            // own), where `crates/vike-cli/src/cmd/study.rs`'s `resolve_addr` folds the ladder
            // `--addr` → this → `vike_config::DEFAULT_BACKTEST_ADDR`.
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
        by: Consumer::Not {
            why: "`VIKE_JOURNAL_DIR` is read by crates/vike-core/src/run_profile.rs's \
                  `journal_config_from_env`, which reads THREE variables at once \
                  (`VIKE_RUN_PROFILE`, `VIKE_JOURNAL_DIR`, `VIKE_JOURNAL_SNAPSHOT_EVERY`) and is \
                  called from vike-app, vike-tradehub and vike-run. Splitting it needs the whole \
                  family to move together, so the WAL directory stays an environment variable.",
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
    Consumption {
        key: "config.state_dir",
        by: Consumer::Not {
            // ⚠ THIS ROW WENT BACKWARDS on 2026-09-09, and saying so is the point of the row.
            //
            // It was `Consumer::At` on the GUI shell's `state_dir_path`, which resolved the
            // STRATEGY-STATE sidecar directory every mounted strategy wrote its `<mount_id>.json`
            // into. The desktop cut deleted the local core (rulings 1 and 2 of
            // `docs/superpowers/specs/2026-09-09-vike-backend-desktop-cli-rename-design.md`), which
            // left that function with no callers, and deleting it took the key's ONE reader with it.
            //
            // ⚠ `VIKE_STATE_DIR`, this field's variable, is the desktop's strategy-state SIDECAR
            // directory — NOT the `VIKE_STATE_ROOT` state ROOT that vike-tradehub, vike-app-core and
            // vike-studio resolve. `vike_model::state_path`'s module doc records the collision, and
            // it is why the daemon's surviving state handling does NOT inherit this key: the
            // `config.state_dir` reads in `crates/vike-core/src/runtime/mod.rs` are `CoreConfig`'s
            // OWN field of the same name, populated by its caller, not this settings key.
            //
            // ⚠ SO THIS KEY IS NOW UNREAD, and this file's own module doc says that is worse than
            // an unimplemented feature — a declared-but-unread key hands the operator positive
            // confirmation of something false, which is what got `Policy::max_total_exposure`
            // DELETED. The honest end state is deletion plus a `REMOVED_ENV` refusal, so a box that
            // set it fails loudly instead of trading with a sidecar directory that configures
            // nothing. That is deliberately NOT bundled here: removing an operator-facing key is its
            // own change with its own migration, and hiding it inside a rename is how it would ship
            // unreviewed. The row is the admission until then.
            why: "no reader survives. `crates/vike-desktop/src/main.rs`'s `state_dir_path` was the \
                  only one, and the desktop cut deleted it with the local core it served. Slated \
                  for deletion with a REMOVED_ENV refusal, as its own change.",
        },
    },
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
        by: Consumer::Not {
            why: "`VIKE_SWEEP_THREADS` is read by crates/vike-backtest/src/harness/sweep.rs's \
                  `sweep_threads`, called from `install_bounded` deep inside a rayon fan-out. Its \
                  callers are the vike-backtest bins and vike-studio, none of which load settings, \
                  so there is no composition root here to resolve it at yet.",
        },
    },
    // ---------------------------------------------------------------------------------------
    // flags.* — operator toggles. Every `Not` row names the LIBRARY that owns the read today;
    // `FLAG_REGISTRY` carries each one's owner, review date and expected disposition.
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "flags.allow_withdraw_keys",
        by: Consumer::Not {
            why: "`VIKE_ALLOW_WITHDRAW_KEYS` looks injected — `vike_bridge_core::key_permissions::\
                  allow_withdraw_keys` is a pure map lookup — but the `std::env::vars()` sweep that \
                  feeds it lives in a LIBRARY: crates/vike-mount/src/arming.rs's \
                  `binance_withdraw_gate`. Neither vike-mount nor vike-run accepts a `Flags`, so \
                  wiring it means widening that seam.",
        },
    },
    Consumption {
        key: "flags.binance_trade_lite_fill",
        by: Consumer::Not {
            why: "`VIKE_BINANCE_TRADE_LITE_FILL` is read by crates/bridges/binance/src/\
                  perp_user_data.rs's `trade_lite_fill_from_env`, once at user-data pump \
                  construction, inside the venue adapter. Reaching it from a settings file means \
                  threading the flag through `vike_run::NodeConfig` and `vike_mount::make_engine`.",
        },
    },
    Consumption {
        key: "flags.bybit_fast_exec",
        by: Consumer::Not {
            why: "`VIKE_BYBIT_FAST_EXEC` is read by crates/bridges/bybit/src/user_data.rs's \
                  `fast_exec_enabled`, inside the venue adapter's WS pump spawn. Same seam as every \
                  other per-venue toggle: the flag has to reach `make_engine` before a file can set \
                  it.",
        },
    },
    Consumption {
        key: "flags.hl_outcome",
        by: Consumer::Not {
            why: "`VIKE_HL_OUTCOME` is read by crates/bridges/hyperliquid/src/\
                  outcome_settlement.rs's `hl_outcome_enabled`, called from the settlement poller's \
                  own `spawn` inside the adapter — no composition root sees the decision.",
        },
    },
    Consumption {
        key: "flags.hyperliquid_hip3",
        by: Consumer::Not {
            why: "`HYPERLIQUID_HIP3` is read by crates/bridges/hyperliquid/src/instruments.rs's \
                  `hip3_enabled` (via `consts::HIP3_ENV`) during \
                  `HyperliquidInstruments::load_with_recorder` — inside symbology loading, several \
                  layers below any settings-loading binary.",
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
            why: "`VIKE_PM_RESOLVE` is read by crates/bridges/polymarket/src/settlement/resolve.rs's \
                  `pm_resolve_enabled`, called from `ResolutionPoller::spawn_with_deps` inside the \
                  adapter. Same per-venue seam as the rest of the Polymarket family.",
        },
    },
    Consumption {
        key: "flags.poly_auto_redeem",
        by: Consumer::Not {
            why: "`POLY_AUTO_REDEEM` is read by crates/bridges/polymarket/src/settlement/\
                  auto_redeem.rs's `auto_redeem_enabled`, which gates a poller that MOVES REAL \
                  MONEY on-chain. Its disposition is KEEP-as-a-flag precisely because it is an \
                  explicit per-run opt-in; the read still has to move to a parameter first.",
        },
    },
    Consumption {
        key: "flags.poly_chain_proxy",
        by: Consumer::Not {
            why: "`POLY_CHAIN_PROXY` is read by crates/bridges/polymarket/src/settlement/chain.rs's \
                  `ChainRpc::build_agent` through `env_flag`/`chain_var`, a two-tier read (process \
                  env, then that file's OWN cached `.env` load) with a DYNAMIC key. A file layer \
                  has to replace both tiers at once or the two sources disagree.",
        },
    },
    Consumption {
        key: "flags.poly_chain_watch",
        by: Consumer::Not {
            why: "`POLY_CHAIN_WATCH` is read by crates/bridges/polymarket/src/settlement/chain.rs's \
                  `chain_watch_enabled`, same two-tier `env_flag`/`chain_var` path as \
                  `poly_chain_proxy` — process env plus that module's own cached `.env` read.",
        },
    },
    Consumption {
        key: "flags.poly_exec",
        by: Consumer::Not {
            why: "`POLY_EXEC` is read by crates/bridges/polymarket/src/mount.rs's \
                  `poly_exec_enabled`, a HYBRID read: process env first, then the credentials map \
                  normalized through `first_token` so a trailing `.env` comment still arms it. A \
                  `Flags` field replaces only one of those two tiers, so the swap has to happen \
                  with the mount, not before it.",
        },
    },
    Consumption {
        key: "flags.poly_heartbeat",
        by: Consumer::Not {
            why: "`POLY_HEARTBEAT` is read by crates/bridges/polymarket/src/heartbeat.rs's \
                  `heartbeat_enabled`, the same hybrid env-then-credentials-map read as `poly_exec`, \
                  consumed inside `HeartbeatPoller::spawn`.",
        },
    },
    Consumption {
        key: "flags.poly_presubmit_register",
        by: Consumer::Not {
            why: "`POLY_PRESUBMIT_REGISTER` is read by crates/bridges/polymarket/src/mount.rs's \
                  `presubmit_register_enabled` and consumed by `live_mount_from_vars` in the same \
                  file — the same hybrid read as `poly_exec`, inside the venue mount.",
        },
    },
    Consumption {
        key: "flags.poly_rate_gate",
        by: Consumer::Not {
            why: "`POLY_RATE_GATE` is read by crates/bridges/polymarket/src/exec.rs's \
                  `rate_gate_enforced`, whose second tier — \
                  crates/bridges/polymarket/src/egress.rs's `dotenv_rate_gate`, homed there with \
                  its sibling resolvers by the feeds/exec split — performs its OWN workspace-.env \
                  read through the `load_workspace_dotenv` reader, cached behind a `OnceLock` — a \
                  third source of truth for one flag, and one this crate cannot displace from the \
                  outside.",
        },
    },
    Consumption {
        key: "flags.poly_reconcile",
        by: Consumer::Not {
            why: "`POLY_RECONCILE` is read by crates/bridges/polymarket/src/recon_client.rs's \
                  `poly_reconcile_enabled`, the same hybrid env-then-credentials-map read as \
                  `poly_exec`, consumed in `vike_mount::make_engine`'s polymarket arm.",
        },
    },
    Consumption {
        key: "flags.poly_redeem_halt",
        by: Consumer::Not {
            why: "`POLY_REDEEM_HALT` is read by crates/bridges/polymarket/src/settlement/\
                  auto_redeem.rs's `kill_switch_tripped`, by PRESENCE, every poller tick — and it \
                  has a SECOND trip condition this type does not model at all, a halt FILE on disk. \
                  Wiring only the env half would silently narrow a kill switch, which is the worst \
                  regression available here.",
        },
    },
    Consumption {
        key: "flags.preflight_skip",
        by: Consumer::Not {
            why: "`VIKE_PREFLIGHT_SKIP` looks injected — `vike_mount::preflight::preflight_skipped` \
                  is a pure map lookup — but the `std::env::vars()` sweep feeding it lives in a \
                  LIBRARY: crates/vike-mount/src/startup.rs's `run_startup_preflight`, deliberately \
                  reading process env rather than its `vars` argument. Same widening as \
                  `allow_withdraw_keys`.",
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
        by: Consumer::Not {
            why: "`VIKE_RECONCILE_BALANCE` is read inside crates/vike-ops/src/reconcile_config.rs's \
                  `build_recon_config`, which builds the WHOLE `VIKE_RECONCILE_*` family (cadences, \
                  lookbacks, policy name, tolerances) from one map. Only the master gate was lifted \
                  out; the family moves together or its parts disagree about one pass.",
        },
    },
    Consumption {
        key: "flags.reconcile_generate_missing",
        by: Consumer::Not {
            why: "`VIKE_RECONCILE_GENERATE_MISSING` is read inside crates/vike-ops/src/\
                  reconcile_config.rs's `build_recon_config`, alongside the rest of the \
                  `VIKE_RECONCILE_*` family — see `flags.reconcile_balance` for why the family has \
                  to move as one unit rather than a field at a time.",
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
            why: "`VIKE_RECORD_CHAINS` is read by crates/vike-data/src/chain_rec.rs's \
                  `ChainRecorder::{from_env, open_from_env}`, which also read the \
                  `VIKE_RECORD_CHAINS_CADENCE_MS` sibling in the same call — a recorder \
                  CONSTRUCTOR that owns its own configuration, and the pair has to move together.",
        },
    },
    Consumption {
        key: "flags.record_dvol",
        by: Consumer::Not {
            // ⚠ THIS ROW WENT BACKWARDS, and saying so is the point of the row.
            //
            // It began as `Consumer::Not` ("NO PRODUCTION READER AT ALL"); it was promoted to
            // `Consumer::At` when the GUI shell finally mounted the feed the flag gates — the
            // public keyless `deribit_volatility_index.{btc_usd,eth_usd}` subscription plus the
            // `DvolRecorder` that persists it. That mount was this flag's ONE production consumer,
            // and it went with the desktop shell's whole local market-data plane: the desktop opens
            // no venue socket, so there is no `if flags.record_dvol {` in the tree any more and the
            // resolved flag reaches nothing. Demoting it back is the only honest answer — an `At`
            // row here would make `vike-cli config show` report `READ: yes` for a key that changes
            // nothing, which is the precise failure this whole table exists to prevent.
            //
            // ⚠ NOT deleted with its mount, unlike `preferences.rate_utilization` above, and the
            // distinction is real rather than sentimental: that field's every half was gone, while
            // `vike_deribit::{DvolRecorder, spawn_deribit_dvol_feed}` both SURVIVE, are both still
            // exported, and both still take this flag as a PARAMETER. The feature is intact and
            // unmounted, not removed — a genuinely file-settable setting waiting on a root that
            // mounts it, which is the shape `Consumer::Not` is for. Its `FLAG_REGISTRY` stewardship
            // row (owner · review date · GRADUATE) is the mechanism that gets it re-mounted, and
            // deleting the field would delete that too.
            why: "NOTHING acts on it — and this is the one row here with no library reader either, \
                  so it is a stronger admission than its neighbours rather than the same one. \
                  `VIKE_RECORD_DVOL` is resolved by this crate's own `Flags::apply_env` and by \
                  nothing else in the tree (the wiring that promoted this row also deleted the \
                  recorder's library env read, so there is no second authority left to name). The \
                  reader that owned the resolved flag was the GUI shell's DVOL mount, deleted with \
                  the desktop's local market-data plane; `vike_deribit::DvolRecorder::from_flag` \
                  and `vike_deribit::spawn_deribit_dvol_feed` still exist and still take it as a \
                  parameter, with no caller outside deribit's own `#[cfg(test)]` module. Re-wiring \
                  it needs a composition root that mounts the feed — vike-tradehub is the \
                  candidate, being the surviving root with a live deribit connection — and not one \
                  line of change in this crate.",
        },
    },
    Consumption {
        key: "flags.record_properties",
        by: Consumer::Not {
            why: "`VIKE_RECORD_PROPERTIES` is read by crates/vike-data/src/properties_rec.rs's \
                  `PropertiesRecorder::{from_env, open_from_env}` — a recorder constructor that \
                  gates itself. The binaries that call it (vike-app, vike-tradehub) do load \
                  settings, so this one needs only a non-env constructor in vike-data, not a new \
                  seam.",
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
        assert!(!is_consumed("flags.poly_exec"));
        assert!(consumer_of("config.log_dir").unwrap().file().is_some());
        assert!(consumer_of("flags.poly_exec").unwrap().why_not().is_some());
    }

    /// **The keys a headless box was told it could set.** `config.store_root` and
    /// `preferences.chart_style` reported `READ: yes` while their only reader is the GUI — so on a
    /// tradehub or recorder box, setting one did nothing and the output said it would work.
    ///
    /// ⚠ `config.state_dir` WAS the third, and it is no longer in this list because it is no longer
    /// read by anything at all: the desktop cut deleted `state_dir_path`, its one reader. Its row
    /// carries the admission and the plan. It is checked by
    /// [`a_gui_only_setting_that_lost_its_reader_says_so`] instead — dropping it silently would have
    /// turned a key that now configures NOTHING into a key this suite simply stopped asking about.
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

    /// The regression the row above was moved OUT of this suite for: `config.state_dir` must keep
    /// reporting that nothing reads it, so `vike-cli config show`'s `READ` column cannot tell an
    /// operator a sidecar directory is configured when the code that read it is gone.
    ///
    /// ⚠ This FAILS if somebody re-attaches a reader without moving the row back — which is exactly
    /// when the promotion should be noticed, the same shape `flags.record_dvol`'s row uses.
    #[test]
    fn a_gui_only_setting_that_lost_its_reader_says_so() {
        let c = consumer_of("config.state_dir").expect("config.state_dir must have a row");
        assert!(
            !c.is_consumed(),
            "config.state_dir's only reader was the desktop's `state_dir_path`, deleted with the \
             local core — if it is read again, move the row back into `a_gui_only_setting_names_the_gui`"
        );
        assert!(
            c.why_not().is_some_and(|w| w.contains("state_dir_path")),
            "the row must NAME the reader that went, or the next reader cannot tell whether this \
             key was never wired or was un-wired"
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
        assert_eq!(consumer_of("flags.poly_exec").and_then(Consumer::binary), None);

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

    #[test]
    fn the_unconsumed_list_is_exactly_the_not_rows() {
        let listed = unconsumed_keys();
        assert_eq!(listed.len(), CONSUMPTION.iter().filter(|c| !c.by.is_consumed()).count());
        assert!(listed.contains(&"flags.record_chains"));
        assert!(!listed.contains(&"config.tradehub_addr"));
        // …and `flags.record_dvol` is BACK on the list, which is the direction this table is not
        // supposed to move in — so it is asserted rather than quietly allowed. It left when the GUI
        // shell mounted its feed and returned when that shell's local market-data plane was
        // deleted; the row itself carries the argument. The value of pinning the regression is that
        // re-mounting the feed must fail this line, which is where the promotion gets noticed.
        assert!(listed.contains(&"flags.record_dvol"));
    }
}
