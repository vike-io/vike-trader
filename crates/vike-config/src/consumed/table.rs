//! `CONSUMPTION`: one row per `config.*` / `preferences.*` / `flags.*` key, and its reader.

use super::{Consumer, Consumption, Reader};

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
            // no value) and the `vike-cli backtest`/`walkforward`/`study` that DIAL it. The
            // row names the DAEMON's read: it is the one that fails visibly if the key stops being
            // consumed (the server binds the wrong port), where a client's would fail as a refused
            // connection an operator could blame on anything.
            //
            // ⚠ **ONE row, and it has to be one.** Both reads are real, but
            // `consumer_of` is a `find` — the first row wins and the second is unreachable — and
            // `every_setting_has_a_consumption_row` compares SETS, so a duplicate reddens
            // nothing and merely makes this table quietly stop being one-row-per-key. The client
            // read is recorded here instead: `vike-cli`'s ONE boot walk resolves the key and hands
            // it to the `backtest` arm as a parameter (a `src/cmd/` file reads no settings of its
            // own), where `crates/vike-cli/src/cmd/backtest.rs`'s `resolve_addr` folds the ladder
            // `--addr` → this → `vike_config::DEFAULT_BACKTEST_ADDR`.
            // ⚠ The `study` arm is a CALLER of it — both verbs dial the same compute daemon on
            // this one key, so a second copy of the ladder could answer differently about a blank
            // rung.
            file: "crates/vike-backtest/src/backtest_cli/serve.rs",
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
            // ⚠ This read lives in `resolved_datahub_addr`: a move of that method re-keys the row.
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
            file: "crates/vike-tradehub/src/tradehub_cli/banner.rs",
            needle: "settings.config.tradehub_advertise_addr.as_deref()",
        },
    },
    Consumption {
        key: "config.instance_origin",
        by: Consumer::At {
            // The daemon threads it to `live_mount`, which puts it on the core's `CoreConfig`, so
            // every client order id this instance mints names the deployment that placed it.
            // The desktop mounts no venue and mints no client order id, so the daemon is the sole
            // reader.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.instance_origin.clone()",
        },
    },
    Consumption {
        key: "config.journal_dir",
        by: Consumer::At {
            // `vike_core::journal_config_from` reads the directory rung as a PARAMETER (decision
            // 0111: no variable reaches it), and a resolved run profile's `[sinks]` decides above it.
            //
            // The needle is the RESOLUTION, in the one binary that owns it: the daemon builds ONE
            // `profile_rows::JournalRung` from the row, because the live core's
            // `CoreConfig::journal` and the off-path materializer both read it and two resolutions
            // could name two directories.
            //
            // ⚠ It reaches the PAPER mount too, through `vike_mount::PaperMountOpts::journal`. That
            // is not tidiness: a key that enabled the WAL on the live arm and silently did nothing
            // on the rehearsal would be this table's own defect wearing a smaller costume.
            //
            // The desktop's Tearsheet READS the same key from the PC's own database (decision
            // 0111): the directory a journal on that box sits in.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "dir: settings.config.journal_dir.clone()",
        },
    },
    Consumption {
        key: "config.journal_snapshot_every",
        by: Consumer::At {
            // The other field of the same `JournalRung`, beside `config.journal_dir` and for the
            // same reason; `vike_core::journal_config_from` takes it as its cadence override.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "snapshot_every: settings.config.journal_snapshot_every",
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
            file: "crates/vike-cli/src/boot.rs",
            needle: "booted.settings.config.node_addr",
        },
    },
    // ⚠ `config.state_dir` is REMOVED, and BOTH spellings refuse: `crate::REMOVED_ENV` for the
    // variable, `crate::config::ConfigPatch::state_dir` for the file key. A REMOVED key must not
    // keep a row here — a row is precisely what makes `config show` call a value effective.
    Consumption {
        key: "config.store_root",
        by: Consumer::At {
            // The Studio tool's bar store. The data server reads the same row as its hist-store root
            // (`crates/vike-datahub/src/datahub_cli/boot.rs`'s `resolve_hist_store`); the tools that
            // load no settings take `--store` instead (decision 0111).
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
    // config.* — the rows decision 0111's phase P3 added. Every reader below reads the row alone
    // (phase P5 deleted the environment reads; each variable refuses startup naming its row).
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "config.reconcile_policy",
        by: Consumer::At {
            // `daemon_recon_settings` copies the seven `config.reconcile_*` rows into the one
            // `reconcile_config::ReconSettings` value `build_recon_config` reads; an absent policy
            // row is `quarantine` (`parse_policy`'s own default). The loader refuses a word
            // `parse_policy` would not read.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "policy: config.reconcile_policy.clone()",
        },
    },
    Consumption {
        key: "config.reconcile_interval_ms",
        by: Consumer::At {
            // The same fold — see `config.reconcile_policy` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "interval_ms: config.reconcile_interval_ms,",
        },
    },
    Consumption {
        key: "config.reconcile_audit_ms",
        by: Consumer::At {
            // The same fold — see `config.reconcile_policy` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "audit_ms: config.reconcile_audit_ms,",
        },
    },
    Consumption {
        key: "config.reconcile_lookback_ms",
        by: Consumer::At {
            // The same fold — see `config.reconcile_policy` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "lookback_ms: config.reconcile_lookback_ms,",
        },
    },
    Consumption {
        key: "config.reconcile_startup_delay_ms",
        by: Consumer::At {
            // The same fold — see `config.reconcile_policy` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "startup_delay_ms: config.reconcile_startup_delay_ms,",
        },
    },
    Consumption {
        key: "config.reconcile_balance_tol_abs",
        by: Consumer::At {
            // The same fold — see `config.reconcile_policy` above. Consulted by the pass only
            // while `flags.reconcile_balance` is on.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "balance_tol_abs: config.reconcile_balance_tol_abs,",
        },
    },
    Consumption {
        key: "config.reconcile_balance_tol_rel",
        by: Consumer::At {
            // The same fold — see `config.reconcile_balance_tol_abs` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "balance_tol_rel: config.reconcile_balance_tol_rel,",
        },
    },
    Consumption {
        key: "config.tradehub_control_rate",
        by: Consumer::At {
            // `resolve_control_limits` hands the resolver this row, rendered in the resolver's text
            // grammar — one config for the TCP server and the Telegram channel alike.
            file: "crates/vike-tradehub/src/tradehub_cli/settings.rs",
            needle: "boot_config().tradehub_control_rate",
        },
    },
    Consumption {
        key: "config.pin_cores",
        by: Consumer::At {
            // Installed into `vike_exec::affinity` at boot, before any pinned thread is spawned;
            // `pin_current_thread` reads it only where `VIKE_PIN_CORES` is unset. This daemon is the
            // ONLY installer — the database is shared by every process on the box, and a second
            // daemon pinning to the same cores would fight the first for them.
            file: "crates/vike-tradehub/src/tradehub_cli/settings.rs",
            needle: "install_pin_spec(booted.settings.config.pin_cores.clone())",
        },
    },
    Consumption {
        key: "config.datahub_bind_addr",
        by: Consumer::At {
            // The data SERVER's listen address — this row, the compiled-in default otherwise. Read on
            // the DATAHUB's box; `config.datahub_addr` is the different, client-side dial.
            file: "crates/vike-datahub/src/datahub_cli.rs",
            needle: "booted.settings.config.datahub_bind_addr.clone()",
        },
    },
    Consumption {
        key: "config.datahub_live_resident",
        by: Consumer::At {
            // The resident set of the live market-data plane, read only once that plane is armed
            // (`flags.datahub_live`).
            file: "crates/vike-datahub/src/datahub_cli/mounts.rs",
            needle: "settings.config.datahub_live_resident.clone()",
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
        key: "preferences.density",
        by: Consumer::At {
            // The five APPEARANCE rows (design system spec §5) are read by the desktop at start,
            // mapped by `vike_app_core::ui::appearance_settings::appearance_from` into the
            // appearance it installs, and changed live by its Settings window, which saves each
            // change as a row the next start reads HERE. The reads are spelled in the binary rather
            // than in the library that maps them, so `Consumer::binary` names `desktop`: the only
            // program that acts on them. A row set on a headless box does nothing, and the READ
            // column says so.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.density",
        },
    },
    Consumption {
        key: "preferences.header_gradient",
        by: Consumer::At {
            // The appearance family — see `preferences.density` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.header_gradient",
        },
    },
    Consumption {
        key: "preferences.log_file_level",
        by: Consumer::At {
            // THE 341-GB knob. Also set identically in crates/vike-desktop/src/main.rs's `main`,
            // crates/vike-datahub/src/datahub_cli.rs's `run` and, for the `--addr` daemon,
            // crates/vike-backtest/src/backtest_cli.rs's `log_config` (decision 0111 verdict 3).
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
    Consumption {
        key: "preferences.market_colors",
        by: Consumer::At {
            // The appearance family — see `preferences.density` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.market_colors",
        },
    },
    Consumption {
        key: "preferences.max_order_qty",
        by: Consumer::At {
            // The CLIENT's advisory preview cap (decision 0111): `vike-cli`'s one boot walk
            // installs it, and `cmd::verbs::guardrail_caps` reads it. Advisory — the `trade` and
            // `mcp` previews mark an order over it and refuse nothing; the node enforces its own
            // limits.
            file: "crates/vike-cli/src/boot.rs",
            needle: "install_max_order_qty_row(booted.settings.preferences.max_order_qty)",
        },
    },
    Consumption {
        key: "preferences.export_dir",
        by: Consumer::At {
            // The desktop's File → Export chart image… directory (decision 0111, phase P7), read on
            // the PC from the PC's own database; no row is `<exe_dir>/exports`. A row set on a
            // headless box does nothing, and the READ column says so.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.export_dir",
        },
    },
    // ⚠ `preferences.rate_utilization` and the `policy.rate.max_utilization` that clamped it are
    // tombstones that refuse the key by name (`crate::preferences`' module doc carries the
    // argument), and keep no row: a REMOVED key must not keep one here.
    Consumption {
        key: "preferences.sweep_threads",
        by: Consumer::At {
            // ⚠ **WIRED.** This is a PROCESS-WIDE RESOURCE KNOB read at rayon pool CONSTRUCTION,
            // entered from three front doors in two crates (`optimize/evaluator.rs`'s fan-out,
            // `walkforward.rs`, and the public `map_bounded` vike-studio-core takes), none of which
            // carries a settings value — so wiring it needed "a caller-owned process-wide handle,
            // an `init(spec)` + `OnceLock`". `vike_backtest::harness::install_sweep_threads` is the
            // handle, and `sweep_threads` reads the installed row and nothing else (decision 0111:
            // no environment variable stands beside it).
            //
            // ⚠ **The needle names the `--addr` compute server.** The other consuming process is a
            // ONE-SHOT `backtest` run (what `crates/vike-cli/src/cmd/backtest/execute.rs`'s
            // `execute_local` SPAWNS as a child), which installs the same row beside its own
            // settings load in `crates/vike-backtest/src/backtest_cli/open_store.rs`. The row names
            // the root INSIDE the crate that owns the pool, so the evidence stands whether or not
            // any GUI is built.
            file: "crates/vike-backtest/src/backtest_cli/serve.rs",
            needle: "install_sweep_threads(settings.preferences.sweep_threads)",
        },
    },
    Consumption {
        key: "preferences.text_size",
        by: Consumer::At {
            // The appearance family — see `preferences.density` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.text_size",
        },
    },
    Consumption {
        key: "preferences.theme",
        by: Consumer::At {
            // The appearance family — see `preferences.density` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.theme",
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
            // ruling but the SHAPE: `false` is the guarded state, the value folded into the map is
            // the RESOLVED flag, and the fold writes it with `insert` rather than `or_insert`
            // (`FoldTier::Resolved`).
            //
            // ⚠ `binance_withdraw_gate` reads the map the daemon folds the resolved flag into, and
            // nothing else (decision 0095; it reads that map inside the binance bridge, decision
            // 0096), so "nothing but the row can arm it" holds only if the map cannot carry a value
            // the row did not decide — under `or_insert` a `VIKE_ALLOW_WITHDRAW_KEYS=1` row
            // already sitting in the credential store would survive the fold untouched
            // and arm a live-money gate. The credential store is not a tier for this key; it is
            // overwritten.
            // `crates/vike-tradehub/src/tradehub_cli/tests/precedence.rs`'s
            // `the_withdraw_override_is_the_row_alone` and
            // `crates/bridges/binance/src/mount_tests.rs`'s `the_folded_row_is_the_only_source` are
            // the two halves of the proof, meeting at the string the fold writes.
            //
            // ⚠ `vike-mount` accepts no `Flags`, and it does not need to — `make_engine` already
            // takes the `&HashMap` every venue fact travels on, so the composition root FILLS that
            // map and the library reads it.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
            needle: "flags.allow_withdraw_keys, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.hl_outcome",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `enabled` parameter of `OutcomePoller::spawn` \
                  (crates/bridges/hyperliquid/src/outcome_settlement.rs), and nothing outside \
                  tests calls that entry point — so the row configures nothing, and a set \
                  `VIKE_HL_OUTCOME` refuses startup rather than arming anything. What is missing is \
                  not a flag-threading change but a composition root that MOUNTS the settlement \
                  poller, and mounting one means a live venue writing synthetic terminal fills into \
                  the core — its own decision, taken with the FLAG_REGISTRY stewardship row this \
                  key would lose if the field were deleted.",
            reader: Reader::Uncalled {
                file: "crates/bridges/hyperliquid/src/outcome_settlement.rs",
                // The gate on the parameter (D4 of decision 0095). NOT `pub fn spawn(`: the
                // no-caller search would take `spawn` as a free function and find it everywhere.
                needle: "if !enabled {",
                entry: &["OutcomePoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.hyperliquid_hip3",
        by: Consumer::At {
            // The pure core takes the bool (`HyperliquidInstruments::load_from(fetch, recorder,
            // hip3)`), and `vike-mount`'s hyperliquid arm calls its map-taking twin
            // (`load_from_vars`) with the `vars` it was already holding. The fold OVERWRITES this
            // key (`FoldTier::Resolved`): a `HYPERLIQUID_HIP3` entry in the credential store
            // must not become a tier.
            //
            // TWO readers, one row: the needle is the daemon's fold, and the datahub's venue
            // catalog reads the SAME resolved flag — `crates/vike-datahub/src/datahub_cli/mounts.rs` hands
            // `booted.settings.flags.hyperliquid_hip3` to
            // `crates/vike-datahub/src/catalog.rs`'s `real_catalog_table`. The environment spelling
            // is retired (decision 0095) — a set `HYPERLIQUID_HIP3` refuses startup.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
            needle: "flags.hyperliquid_hip3, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.oco_cancel_sibling_on_dead_exit",
        by: Consumer::At {
            // …and the identical line in crates/vike-desktop/src/main.rs's `App::new`, plus the
            // daemon's PAPER arm via `vike_mount::PaperMountOpts`.
            //
            // ⚠ The needle deliberately points at the DAEMON, the server that runs unattended —
            // exactly the deployment where "leave the book flat once protection dies" is most
            // likely to be the wanted answer.
            // Anchoring the row here is what makes deleting the daemon's read fail this gate.
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,",
        },
    },
    Consumption {
        key: "flags.cancel_orders_on_shutdown",
        by: Consumer::At {
            // The daemon's LIVE arm; the PAPER arm reads the same flag through
            // `vike_mount::PaperMountOpts`, and both land on
            // `vike_core::CoreConfig::cancel_orders_on_shutdown`, which the core's teardown block
            // checks before it detaches the client.
            //
            // ⚠ WHAT THIS ROW DOES *NOT* CLAIM. Being consumed is not being reachable: under
            // `deploy/vike-tradehub.service` stdin is `/dev/null` and SIGTERM has no handler, so
            // `systemctl stop` never reaches the teardown this flag gates. The flag is honoured on
            // an interactive `shutdown`/`quit`/Ctrl-D stop. That gap is a property of the STOP
            // PATH, not of the wiring, and no consumption gate can see it —
            // `docs/ops/kill-switches.md` is where it is written down for the operator.
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,",
        },
    },
    Consumption {
        key: "flags.pm_resolve",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `enabled` parameter of `ResolvePoller::spawn`, handed to the private \
                  `ResolvePoller::spawn_with_deps` beside `ResolvePoller::spawn_with_chain`'s own \
                  (crates/bridges/polymarket/src/exec_plane/settlement/resolve.rs), and nothing \
                  outside tests calls either public door — so the row configures nothing, and a \
                  set `VIKE_PM_RESOLVE` refuses startup rather than arming anything. That file's \
                  module tree says the same: \
                  `crates/bridges/polymarket/src/exec_plane/settlement/mod.rs` records that no \
                  composition root constructs anything in the cluster. Re-arming this needs a root \
                  that MOUNTS the resolve poller — a settlement pass that writes synthetic terminal \
                  fills into the core — not a line of threading.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/exec_plane/settlement/resolve.rs",
                // The gate on the parameter, inside `spawn_with_deps` (D4 of decision 0095).
                needle: "if !enabled {",
                // BOTH public doors: `spawn_with_deps` is private, so naming it alone would leave
                // `spawn_with_chain` free to acquire a caller with this row still green.
                entry: &["ResolvePoller::spawn", "ResolvePoller::spawn_with_chain"],
            },
        },
    },
    Consumption {
        key: "flags.poly_auto_redeem",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095) — and the flag is \
                  KEPT anyway, which is the unusual part. The value is the `enabled` parameter of \
                  `AutoRedeemPoller::spawn` (crates/bridges/polymarket/src/exec_plane/settlement/\
                  auto_redeem.rs), and nothing outside tests calls that entry point — so the row \
                  configures nothing, and a set `POLY_AUTO_REDEEM` refuses startup rather than \
                  arming anything. Spawning it is not a wiring task: it is NEW UNATTENDED ON-CHAIN \
                  MONEY MOVEMENT, refused deliberately — \
                  `crates/vike-ops/tests/docs/kill_switch_gate.rs`'s RETIRED row is where that decision \
                  is recorded. The flag's own disposition stays KEEP-as-a-flag because an explicit \
                  per-run opt-in is what such a mount would need on the day it is taken.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/exec_plane/settlement/auto_redeem.rs",
                // The gate on the parameter (D4 of decision 0095). NOT `pub fn spawn(`: the
                // no-caller search would take `spawn` as a free function and find it everywhere.
                needle: "if !enabled {",
                entry: &["AutoRedeemPoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.poly_exec",
        by: Consumer::At {
            // ⚠ Decision 0095 made `poly_exec_enabled` MAP-ONLY: the process environment is no
            // longer consulted at all (a set `POLY_EXEC` refuses startup, `crate::REMOVED_ENV`, and
            // the row is the only source), so the composition root's fold is the
            // ONLY way this flag reaches the mount. It fills the credential map under the old
            // name and OVERWRITES whatever the credential store held there
            // (`FoldTier::Resolved`, like every other folded key).
            //
            // ⚠ **Never `or_insert` here: a real-money hole (decision 0095's review).**
            // `refuse_credential_file_arming` stops the process at `vike-boot` step 3 on an arming
            // line — but that refusal's value grammar is "the text before `#`, trimmed, is exactly
            // `1`", while `poly_exec_enabled` reads the first TOKEN, so under `or_insert`
            // `POLY_EXEC=1 x` armed real-money exec over `flags.poly_exec = false` without
            // tripping it, and `POLY_EXEC=0` silently vetoed a true flag. The refusal is the loud
            // half; the overwrite is the one that holds, and
            // `no_credential_store_line_survives_the_fold_for_any_key` pins it.
            //
            // ⚠ The value folded in is the RESOLVED flag, which is what makes this safe for a
            // REAL-MONEY arming gate: `vike_config::Flags` has no environment arm for this flag
            // any more, so the settings row is the only source and there is nothing for a second
            // source to disagree with.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
            needle: "flags.poly_exec, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.poly_heartbeat",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `enabled` parameter of `HeartbeatPoller::spawn` \
                  (crates/bridges/polymarket/src/exec_plane/heartbeat.rs), and no composition root \
                  calls that entry point (`crates/bridges/polymarket/src/lib.rs` lists the module \
                  as PARKED, with no production caller) — so the row configures nothing, and a set \
                  `POLY_HEARTBEAT` refuses startup rather than arming anything. In FLAG shape this \
                  is the closest of the six to one line of work — the moment a root spawns the \
                  poller it hands this row to `enabled` — but its `FLAG_REGISTRY` disposition is \
                  RETIRE, so the registry's own answer is that this key should cease to exist \
                  rather than be wired.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/exec_plane/heartbeat.rs",
                // The gate on the parameter (D4 of decision 0095). NOT `pub fn spawn(`: the
                // no-caller search would take `spawn` as a free function and find it everywhere.
                needle: "if !enabled {",
                entry: &["HeartbeatPoller::spawn"],
            },
        },
    },
    Consumption {
        key: "flags.poly_reconcile",
        by: Consumer::At {
            // Same shape and same argument as `flags.poly_exec` above: the only feeder of a
            // map-only read (decision 0095), and it OVERWRITES a credential-store line for the
            // same reason — the boot refusal on `POLY_RECONCILE` has the same narrow value grammar.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
            needle: "flags.poly_reconcile, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.poly_redeem_halt",
        by: Consumer::Not {
            // ⚠ This row is a KILL SWITCH. The second trip condition (a halt FILE on disk) is
            // unmodelled, so wiring only the row would narrow the switch. The row STAYS — deleting
            // a guard while the thing it guards survives puts the guard AFTER its subject, which is
            // the argument `crates/vike-ops/tests/docs/kill_switch_gate.rs`'s RETIRED row already
            // makes.
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `halted` parameter of `AutoRedeemPoller::spawn`, beside the halt file — both \
                  read by crates/bridges/polymarket/src/exec_plane/settlement/auto_redeem.rs's \
                  `kill_switch_tripped` every tick of that poller — and nothing outside tests ever \
                  constructs the poller, so there is nothing running to halt. A set \
                  `POLY_REDEEM_HALT` (an empty one included: it used to halt on PRESENCE) refuses \
                  startup. Two reasons the row is KEPT rather than tombstoned: the switch has a \
                  SECOND trip condition this type does not model at all, the halt FILE, so a row \
                  alone would silently NARROW a kill switch; and the poller it guards still \
                  exists, so deleting the switch would leave the guard behind its subject.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/exec_plane/settlement/auto_redeem.rs",
                needle: "pub fn kill_switch_tripped(halted: bool, halt_file: &Path) -> bool",
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
            // ⚠ The second of the two SAFETY OVERRIDES the daemon folds into the mount's map, and
            // like `flags.allow_withdraw_keys` above it is read from the folded map ALONE:
            // `run_startup_preflight` asks `vike_mount::preflight::preflight_skipped` of its `vars`
            // argument and sweeps no process environment (decision 0111 retired
            // `VIKE_PREFLIGHT_SKIP`, which refuses startup). The fold OVERWRITES this key
            // (`FoldTier::Resolved`), so a `VIKE_PREFLIGHT_SKIP` line in the credential store cannot
            // stand to be read either — proved by
            // `crates/vike-tradehub/src/tradehub_cli/tests/precedence.rs`'s
            // `no_credential_store_line_survives_the_fold_for_any_key`.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
            needle: "flags.preflight_skip, FoldTier::Resolved)",
        },
    },
    Consumption {
        key: "flags.reconcile",
        by: Consumer::At {
            // …and crates/vike-desktop/src/main.rs's `App::new`, which folds the same resolved value
            // through the same `reconcile_gate`. This flag is one of THREE inputs to that
            // gate rather than the gate itself — it forces the driver on where the armed-live
            // probe reports nothing — so the needle follows the call and not a bare assignment.
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "reconcile_config::reconcile_gate(flags.reconcile,",
        },
    },
    Consumption {
        key: "flags.reconcile_balance",
        by: Consumer::At {
            // `build_recon_config` reads the whole reconcile family out of ONE
            // `reconcile_config::ReconSettings` value, which `daemon_recon_settings` fills from the
            // boot's resolved rows — so filling it is the entire wiring, and nothing can disagree
            // about a single pass because there is exactly one value. Pinned by
            // `crates/vike-tradehub/src/tradehub_cli/tests/precedence.rs`'s
            // `the_reconcile_settings_are_the_rows`.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "balance: flags.reconcile_balance,",
        },
    },
    Consumption {
        key: "flags.reconcile_generate_missing",
        by: Consumer::At {
            // The same one value, filled in the same place — see `flags.reconcile_balance` above
            // for why filling it is the whole of the wiring.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "generate_missing: flags.reconcile_generate_missing,",
        },
    },
    Consumption {
        key: "flags.reconcile_off",
        by: Consumer::At {
            // The REFUSAL half of the same `reconcile_gate` call, in the same two roots. It gets
            // its own row because an operator reading `config show` needs the OFF switch to report
            // `READ: yes` on its own evidence: a safety override merely believed to be wired is the
            // exact failure this table exists to remove.
            file: "crates/vike-tradehub/src/tradehub_cli/live_mount.rs",
            needle: "flags.reconcile_off,",
        },
    },
    Consumption {
        key: "flags.record_chains",
        by: Consumer::Not {
            // `ChainRecorder::open` has NO call site at all, in production or in tests;
            // `crates/vike-desktop/src/main.rs` binds `chain_rec` to `None`.
            why: "THE ROW IS INERT. crates/vike-data/src/rec/chain_rec.rs's `ChainRecorder::open` \
                  takes this flag as its `enabled` PARAMETER (decision 0111 deleted the recorder's \
                  environment doors, and `VIKE_RECORD_CHAINS` refuses startup) — and nothing in \
                  the tree calls it. What is missing is the STORE, not the fetch: the desktop's \
                  Options tool still polls option chains every 30s through \
                  `vike_app_core::tools::spawn_tool_fetchers`, and simply passes `None` for the \
                  recorder, because that binary links vike-data with DEFAULT features (the \
                  trait-only `HistStore` seam) while `open` is a `hist-datafusion` constructor — \
                  there is no engine there to open. Re-arming it needs a root that can open a \
                  store BESIDE a chain fetch: the desktop has the fetch and no store, and \
                  vike-tradehub had a store (behind `record-feeds`, deleted in #2093) and no \
                  fetch. The recorder and its store kind are intact and unmounted, which is what \
                  `Consumer::Not` is for.",
            reader: Reader::Uncalled {
                file: "crates/vike-data/src/rec/chain_rec.rs",
                // The gate on the parameter, inside the one constructor that takes it.
                needle: "if !enabled {",
                // The one door: `open` is the only constructor that takes the flag.
                entry: &["ChainRecorder::open"],
            },
        },
    },
    // ⚠ `flags.record_dvol` is DELETED. The tombstone is `crate::flags::DEAD_FLAG_KEYS`, which
    // carries the surviving symbols and what a re-mounting root must supply. The CONSUMPTION table
    // has no opinion about a key that does not exist.
    Consumption {
        key: "flags.record_properties",
        by: Consumer::At {
            // The daemon folds the resolved flag into the `vars` its mount threads.
            // `vike_data::PropertiesRecorder::open` takes the flag as its `enabled` PARAMETER now
            // (decision 0111 deleted the recorder's environment door and its map half), so a root
            // that constructs one again hands it the row's answer directly.
            //
            // ⚠ The desktop mounts no venue and opens no recorder, so `vike-tradehub` is the only
            // root, and NO build of it opens the store (#2093):
            // `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s `properties_rec` is always `None`, so
            // the flag is inert whatever sets it and the needle below finds only the fold.
            file: "crates/vike-tradehub/src/tradehub_cli/flags.rs",
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
            // The DAEMON, which owns the remote order-write surface this flag opens, is the reader
            // named here. `vike-desktop` reads the SAME key from the PC's own database as its
            // client-side master gate (`crates/vike-desktop/src/main.rs`'s
            // `settings().flags.tradehub_control`, decision 0111), so the row means "this box may
            // drive orders" on either side.
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
        key: "flags.venue_catalog_off",
        by: Consumer::At {
            // The REFUSAL half of the venue-catalog gate, on the ONE root that owns the lane.
            //
            // ⚠ This row is the reason `docs/decisions/0066`'s decision 3 is an ORDERING ruling
            // rather than a note: were `vike-datahub` to hand `vike_boot::boot` a
            // `SettingsLoad::Skip`, the honest row here would be `Consumer::Not` — `vike-cli
            // config show` printing `READ: NO` beside the very switch the record introduces, and a
            // default-on behaviour with no reachable off switch.
            //
            // ⚠ The needle is the ARGUMENT, not the call: `venue_catalog_gate(` and its first
            // argument sit on different LINES once rustfmt has been over them (the call is past
            // `max_width` on one), so a needle spanning both would be satisfied only by a
            // formatting accident. The trailing comma is load-bearing — it is what makes this an
            // argument PASSED to the gate rather than a value assigned to a local nothing reads,
            // which is the distinction `Consumer::At` exists to draw.
            file: "crates/vike-datahub/src/datahub_cli/mounts.rs",
            needle: "booted.settings.flags.venue_catalog_off,",
        },
    },
    // The data daemon's three toggles (decision 0111). Each is read from its row alone; the deploy
    // probe disarms the live plane by writing `flags.datahub_live = false` into its COPIED database.
    Consumption {
        key: "flags.datahub_live",
        by: Consumer::At {
            file: "crates/vike-datahub/src/datahub_cli/mounts.rs",
            needle: "if settings.flags.datahub_live {",
        },
    },
    Consumption {
        key: "flags.datahub_chart_seed",
        by: Consumer::At {
            file: "crates/vike-datahub/src/datahub_cli/mounts.rs",
            needle: "if flags.datahub_chart_seed {",
        },
    },
    Consumption {
        key: "flags.datahub_allow_public_bind",
        by: Consumer::At {
            // ONE row for two readers, `config.backtest_addr`'s shape: the data daemon's bind guard
            // is named here, and the compute daemon (`backtest --addr`) reads the same row the same
            // way in `crates/vike-backtest/src/backtest_cli/serve.rs`'s `run_serve`.
            file: "crates/vike-datahub/src/datahub_cli/boot.rs",
            needle: "let allow_public = flags.datahub_allow_public_bind;",
        },
    },
];
