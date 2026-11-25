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
            // The backtesthub's address (ruling 7 of
            // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), read by BOTH
            // sides of that wire out of ONE key — the daemon that BINDS it (`backtesthub --addr`, with
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
            // own), where `crates/vike-client/src/resolved.rs`'s `backtesthub_dial_address` folds the ladder
            // `--addr` → this → `vike_config::DEFAULT_BACKTEST_ADDRESS`.
            // ⚠ The `study` arm is a CALLER of it — both verbs dial the same backtesthub on
            // this one key, so a second copy of the ladder could answer differently about a blank
            // rung.
            file: "crates/vike-backtesthub/src/backtest_cli/serve.rs",
            needle: "settings.config.backtest_addr",
        },
    },
    Consumption {
        key: "config.datahub_addr",
        by: Consumer::At {
            // The Studio's remote-store branch (split-plane B12): set → the desktop's Studio dials
            // a `HistoryStoreDatahub` at this address instead of opening the local DataFusion store;
            // unset → local, exactly as before. NB the key is the CLIENT dial address: the
            // `vike-datahub` SERVER bin still reads `VIKE_DATAHUB_ADDR` itself for its LISTEN
            // address (it loads no settings — it does not depend on vike-config).
            // ⚠ This read lives in `resolved_datahub_address`: a move of that method re-keys the row.
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
            // declaration against `vike_hub_protocol::bind::bind_exposure` and refuses the capability when the bind
            // disagrees. Read on the DAEMON's box, at startup, once.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.tradehub_account_admin.as_deref()",
        },
    },
    Consumption {
        key: "config.tradehub_advertise_addr",
        by: Consumer::At {
            // The OVERRIDE half of the daemon's self-report: the daemon composes the
            // `HubIdentityWire::advertise_addr` every published frame carries, and a configured
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
            // The daemon threads it to `live_runtime`, which puts it on the core's `CoreConfig`, so
            // every order client id this instance mints names the deployment that placed it.
            // The desktop mounts no venue and mints no order client id, so the daemon is the sole
            // reader.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.instance_origin.clone()",
        },
    },
    Consumption {
        key: "config.journal_dir",
        by: Consumer::At {
            // `vike_tradehub_core::journal_config_from` reads the directory as a PARAMETER (decision 0111:
            // no variable reaches it), and THIS ROW is the journal's only switch and only
            // directory (0111, amended 2026-10-11): a run profile's `[sinks.journal]` only tunes
            // it.
            //
            // The needle is the RESOLUTION, in the one binary that owns it: the daemon builds ONE
            // `profile_rows::JournalRung` from the row, because the live core's
            // `CoreConfig::journal` and the execution stream's publisher both read it and two
            // resolutions could name two directories.
            //
            // ⚠ It reaches the PAPER mount too, through `vike_tradehub_core::StrategyCorePaperConfig::journal`. That
            // is not tidiness: a key that enabled the WAL on the live arm and silently did nothing
            // on the rehearsal would be this table's own defect wearing a smaller costume.
            //
            // The desktop reads NO journal: its Tearsheet reads the datahub's store (2026-10-11), so a
            // `config.journal_dir` row in a PC's database is read by nothing there.
            //
            // The DATA daemon reads no journal since the execution stream replaced its journal
            // materializer (2026-11-18, decision 0295): the tradehub streams its executions instead. No
            // run profile names a directory (`crates/vike-tradehub/src/profile_rows.rs`'s
            // `journal_config_for` takes it from this row alone, and `vike_config::ProfileRun::validate`
            // refuses the retired `sinks.journal.dir` by name).
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "dir: settings.config.journal_dir.clone()",
        },
    },
    Consumption {
        key: "config.journal_snapshot_every",
        by: Consumer::At {
            // The other field of the same `JournalRung`, beside `config.journal_dir` and for the
            // same reason; `vike_tradehub_core::journal_config_from` takes it as its cadence override (a run
            // profile's `sinks.journal.snapshot_every`, when named, beats it).
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "snapshot_every: settings.config.journal_snapshot_every",
        },
    },
    // The journal archive's four rows (decision 0304): read once at the tradehub's start into the
    // `vike_journal::JournalArchivePolicy` its archive thread runs with (no library reads a setting).
    Consumption {
        key: "config.journal_keep_hours",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "config.journal_keep_hours.unwrap_or(DEFAULT_JOURNAL_KEEP_HOURS)",
        },
    },
    Consumption {
        key: "config.journal_archive_max_gib",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "config.journal_archive_max_gib.unwrap_or(DEFAULT_JOURNAL_ARCHIVE_MAX_GIB)",
        },
    },
    Consumption {
        key: "config.journal_archive_max_days",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "config.journal_archive_max_days.unwrap_or(DEFAULT_JOURNAL_ARCHIVE_MAX_DAYS)",
        },
    },
    Consumption {
        key: "config.journal_archive_full",
        by: Consumer::At {
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "at_limit: config.journal_archive_full.unwrap_or_default()",
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
            // where `vike-cli backend status` looks (no verb defaults `--node` from it), resolved in
            // that dispatcher's ONE boot walk and handed down as a parameter (a `src/cmd/` file reads no
            // settings of its own). `vike-tradehub` never reads it — it is not that daemon's
            // address, it is where a client looks for one.
            file: "crates/vike-client/src/resolved.rs",
            needle: "booted.settings.config.node_addr",
        },
    },
    Consumption {
        key: "config.node_ack_wait_ms",
        by: Consumer::At {
            // The CLIENT's wait for the node's verdict on a command it sent, read in `vike-cli`'s
            // ONE boot walk like its pair `config.node_addr` above and handed to the `trade` and
            // `mcp` arms as a PARAMETER (a `src/cmd/` file reads no settings of its own): it ends up
            // as the timeout of `TradehubControlHandle::await_outcome` in the REPL's and the one-shot
            // verbs' write path and in the `vike-mcp`'s write tools. It changes only when the client says
            // "outcome NOT known"; it never cancels or re-sends a command. `vike-tradehub` never
            // reads it.
            file: "crates/vike-client/src/resolved.rs",
            needle: "booted.settings.config.node_ack_wait_ms",
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
            // `daemon_reconcile_settings` copies the seven `config.reconcile_*` rows into the one
            // `reconcile_config::ReconcileSettings` value `build_reconcile_config` reads; an absent policy
            // row is `quarantine` (`parse_policy`'s own default). The loader refuses a word
            // `parse_policy` would not read.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "policy: config.reconcile_policy",
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
            needle: "balance_tolerance_abs: config.reconcile_balance_tol_abs,",
        },
    },
    Consumption {
        key: "config.reconcile_balance_tol_rel",
        by: Consumer::At {
            // The same fold — see `config.reconcile_balance_tol_abs` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "balance_tolerance_rel: config.reconcile_balance_tol_rel,",
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
        key: "config.observer_frame_compression",
        by: Consumer::At {
            // The node server's observer stream, read once at the start into the publisher
            // (`vike_tradehub::publish::ObserverStream`), beside the cap below.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.observer_frame_compression",
        },
    },
    Consumption {
        key: "config.observer_frames_per_second",
        by: Consumer::At {
            // The same read, the same struct.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.observer_frames_per_second",
        },
    },
    Consumption {
        key: "config.pin_cores",
        by: Consumer::At {
            // Installed into `vike_execution::affinity` at boot, before any pinned thread is spawned;
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
        key: "config.gatewayhub_addr",
        by: Consumer::At {
            // The gatewayhub's address, read by BOTH sides of that wire out of ONE key, the
            // `config.backtest_addr` shape: the daemon that BINDS it (`vike-gatewayhub`, whose bind
            // guard refuses a non-loopback value) and the data daemon that DIALS it (wave 1's T7a).
            // The row names the DAEMON's read, for `config.backtest_addr`'s reason: it is the one
            // that fails visibly when the key stops being consumed (the daemon binds the default).
            //
            // ⚠ ONE row for two readers (`consumer_of` is a `find`): the datahub's dial reads the
            // same key through the datahub's own boot walk and is not recorded here.
            file: "crates/vike-gatewayhub/src/gatewayhub_cli.rs",
            needle: "booted.settings.config.gatewayhub_addr.clone()",
        },
    },
    Consumption {
        key: "config.gatewayhub_connect_wait_ms",
        by: Consumer::At {
            // How long the TRADEHUB keeps asking for the gatewayhub at start before its `demo`/`live`
            // accounts stop with `ArmingBlock::GatewayUnavailable` (decision 0164, D7). Read ONCE by
            // `GatewayDial::from_config` in the daemon's composition root and handed down as a
            // PARAMETER (`vike-tradehub-core` reads no setting); asked only when an account is active
            // at a non-paper tier.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "config.gatewayhub_connect_wait_ms.unwrap_or(",
        },
    },
    Consumption {
        key: "config.datahub_live_resident",
        by: Consumer::At {
            // The resident set of the live market-data plane, read only once that plane is armed
            // (`flags.datahub_live`).
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "settings.config.datahub_live_resident.clone()",
        },
    },
    Consumption {
        key: "config.datahub_md_attach_venues",
        by: Consumer::At {
            // Which credentialed venues the live market-data plane serves, read only once that plane
            // is armed (`flags.datahub_live`) and handed to the hub as a parameter.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "settings.config.datahub_market_data_attach_venues.as_deref()",
        },
    },
    Consumption {
        key: "config.node_idle_read_timeout_ms",
        by: Consumer::At {
            // The idle read timeout of a node daemon's connection, read by BOTH daemons that
            // take it: the data daemon hands the row to `vike_datahub::server::ServerTimings::from_rows`
            // (so it reaches `serve_with_lanes`' connection loop), and the backtesthub hands it to
            // `vike_hub_protocol::limits::server_idle_read_timeout` before `serve_authed`. ONE row,
            // `config.journal_dir`'s shape (`consumer_of` is a `find`, so a second row would be
            // unreachable): the data daemon's read is the one named, the backtesthub's is
            // `crates/vike-backtesthub/src/backtest_cli/serve.rs`'s `run_serve`, and
            // `crates/vike-backtesthub/tests/idle_read_timeout.rs` drives it end to end.
            file: "crates/vike-datahub/src/datahub_cli.rs",
            needle: "booted.settings.config.node_idle_read_timeout_ms",
        },
    },
    Consumption {
        key: "config.md_heartbeat_ms",
        by: Consumer::At {
            // The market-data stream's heartbeat period, read on the DATAHUB's box only: the data
            // daemon hands it to `ServerTimings::from_rows` beside the idle timeout, and the stream
            // writer sends it to the client in `MarketDataSubscribed.heartbeat_ms`.
            file: "crates/vike-datahub/src/datahub_cli.rs",
            needle: "booted.settings.config.market_data_heartbeat_ms",
        },
    },
    // ---------------------------------------------------------------------------------------
    // config.* — the trading-mount batch. Every reader is `MountConfigRows::from_config` in the
    // daemon's composition root; `live_runtime` hands the value on as a PARAMETER (neither
    // `vike-tradehub-core` nor `vike-core` reads a setting). No variable ever carried any of these.
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "config.paper_slippage",
        by: Consumer::At {
            // `MountConfigRows::from_config` reads the row; `live_runtime_with` puts it on
            // `vike_tradehub_core::AdapterPolicy::paper_slippage`, which `paper_engine` folds over its
            // compiled default into the paper book's slippage. ⚠ Only a `paper`-tier ACCOUNT's
            // book (`flags.tradehub_live` on): the daemon's own paper arm takes its slippage from
            // the mount config and never reads this row.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "paper_slippage: config.paper_slippage",
        },
    },
    Consumption {
        key: "config.paper_slippage_table",
        by: Consumer::At {
            // `MountConfigRows::from_config` reads the row through `vike_config::config::paper_slippage_table`
            // (the one reading `Config::apply` judged it by); `live_runtime_with` puts the table on
            // `vike_tradehub_core::AdapterPolicy::paper_slippage_table`, which `paper_engine` resolves
            // through `vike_fills::market_impact::slippage_terms` into every paper book of the
            // engine. ⚠ The same books as `config.paper_slippage`, and never beside it.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "paper_slippage_table: paper_table(config.paper_slippage_table.as_deref()),",
        },
    },
    Consumption {
        key: "config.preflight_disk_warn_mib",
        by: Consumer::At {
            // `MountConfigRows::from_config` turns the two rows into the one `PreflightDiskThresholds` value (an
            // absent row is that floor's compiled default); `live_runtime_with` puts it on
            // `vike_tradehub_core::AdapterPolicy::preflight_disk`, which `run_startup_preflight` reads into
            // the disk leg's `PreflightConfig`. ⚠ The leg watches the journal directory alone
            // (`config.journal_dir`): with no journal row there is nothing to measure. It measures
            // on unix only, and it never stops a venue.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "config.preflight_disk_warn_mib,",
        },
    },
    Consumption {
        key: "config.preflight_disk_fail_mib",
        by: Consumer::At {
            // The same fold — see `config.preflight_disk_warn_mib` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "config.preflight_disk_fail_mib,",
        },
    },
    Consumption {
        key: "config.journal_size_alert_mib",
        by: Consumer::At {
            // `run` builds the journal watch (`crates/vike-tradehub/src/journal_watch.rs`'s
            // `JournalWatch`) from the two rows when a `config.journal_dir` row names a journal; the
            // summary thread asks it on every tick and it measures at most once a minute. An absent
            // row is its compiled default.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.journal_size_alert_mib,",
        },
    },
    Consumption {
        key: "config.journal_growth_alert_mib_per_hour",
        by: Consumer::At {
            // The same watch — see `config.journal_size_alert_mib` above.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.config.journal_growth_alert_mib_per_hour,",
        },
    },
    Consumption {
        key: "config.inflight_confirm_ms",
        by: Consumer::At {
            // `MountConfigRows::from_config` turns the row into the cadence (`0` is off, as is no row) and
            // `live_runtime_with` sets it on the live core's `vike_core::CoreConfig::inflight_confirm`,
            // the field the core arms its `InflightConfirm` timer from. The first root wiring of that
            // field. The LIVE mount only: the paper variant has no venue to re-query.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "inflight_confirm: cadence_from_ms(config.inflight_confirm_ms),",
        },
    },
    Consumption {
        key: "config.strategy_state_save_ms",
        by: Consumer::At {
            // `MountConfigRows::from_config` turns the row into the cadence (`0` is off, as is no row) and
            // BOTH daemon mounts hand it to the core: the live mount sets it on its
            // `vike_core::CoreConfig::state_save`, the paper arm on `StrategyCorePaperConfig::state_save`
            // (which the paper builders put on the same field). The core's
            // `maintain_state_save_timer` arms a timer on the existing wheel only while
            // `CoreConfig::state_dir` is also set — the project's strategy-state directory.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "state_save: cadence_from_ms(config.strategy_state_save_ms),",
        },
    },
    // ---------------------------------------------------------------------------------------
    // config.* — the data plane's per-box tuning rows. Each is read ONCE, by the data daemon's
    // composition root (`crates/vike-datahub/src/datahub_cli.rs`'s `run`), which hands the value
    // down as a PARAMETER to the library that uses it: no library below reads a row, and an unset
    // row hands down the compiled-in value.
    // ---------------------------------------------------------------------------------------
    Consumption {
        key: "config.datahub_catalog_ttl_secs",
        by: Consumer::At {
            // `start_catalog_lane` resolves the row and gives it to `CatalogLane::new` as the memo's
            // TTL; unset is `CATALOG_TTL`.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_catalog_ttl_secs",
        },
    },
    Consumption {
        key: "config.recorder_silent_secs",
        by: Consumer::At {
            // `to_record_args` folds `--silent-secs`, then this row, then the compiled-in 300 into
            // `RecorderArgs::silent_secs`, which `recorder::record` hands to the watchdog and to the
            // never-resolved escalation. `run` calls it only for a recording.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "config.recorder_silent_secs",
        },
    },
    Consumption {
        key: "config.recorder_tick_secs",
        by: Consumer::At {
            // The same fold — see `config.recorder_silent_secs` above — for `--tick-secs`.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "config.recorder_tick_secs",
        },
    },
    Consumption {
        key: "config.recorder_flush_max_rows",
        by: Consumer::At {
            // The same fold, with no flag above it: `RecorderArgs::flush_max_rows`, which
            // `recorder::record` lays over `RecorderConfig`'s defaults.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "config.recorder_flush_max_rows",
        },
    },
    Consumption {
        key: "config.recorder_flush_max_age_secs",
        by: Consumer::At {
            // The same fold — see `config.recorder_flush_max_rows` above.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "config.recorder_flush_max_age_secs",
        },
    },
    Consumption {
        key: "config.recorder_bar_flush_max_age_secs",
        by: Consumer::At {
            // The same fold, into `RecorderArgs::bar_flush_max_age`, which only the live-bar recorder's
            // policy reads (`crate::recorder::bar_flush_policy`); unset is the default hour.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "config.recorder_bar_flush_max_age_secs",
        },
    },
    Consumption {
        key: "config.datahub_chain_cadence_secs",
        by: Consumer::At {
            // `start_options_lane` hands the row to the options lane's chain recorder
            // (`ChainRecorder::with_cadence`), which exists only while `flags.record_chains` arms
            // it; unset is `DEFAULT_CHAIN_CADENCE_MS`.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_chain_cadence_secs",
        },
    },
    Consumption {
        key: "config.datahub_options_poll_secs",
        by: Consumer::At {
            // `start_options_lane` hands the row to the options lane's REST poll thread; unset is
            // `POLL_INTERVAL`.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_options_poll_secs",
        },
    },
    Consumption {
        key: "config.datahub_options_focus_underlyings",
        by: Consumer::At {
            // `start_options_lane` hands the row, parsed by the loader's own grammar, to the options
            // lane's ticker feed; unset names no underlying (no compiled default), said at start.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_options_focus_underlyings",
        },
    },
    Consumption {
        key: "config.datahub_options_focus_strikes",
        by: Consumer::At {
            // Its twin, read on the same line; unset is `DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES`.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_options_focus_strikes",
        },
    },
    Consumption {
        key: "config.recorder_retention_days_by_kind",
        by: Consumer::At {
            // The per-kind retention the data daemon's maintenance pass applies. Read ONCE at
            // startup, before the bind guard (a row the reader cannot parse exits 2 there), and
            // handed to `crate::recorder::record` as a parameter; only a process that RUNS
            // maintenance acts on it, and one that does not says so at startup.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "booted.settings.config.recorder_retention_days_by_kind.as_deref()",
        },
    },
    Consumption {
        key: "config.datahub_news_poll_secs",
        by: Consumer::At {
            // The tool-feeds lane's news cadence, folded into `ToolFeedCadences` at the mount. Read on the
            // DATAHUB's box only.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_news_poll_secs",
        },
    },
    Consumption {
        key: "config.datahub_calendar_poll_secs",
        by: Consumer::At {
            // The economic calendar's cadence, beside the news one.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_calendar_poll_secs",
        },
    },
    Consumption {
        key: "config.datahub_equity_poll_secs",
        by: Consumer::At {
            // The one cadence of the three equity feeds (earnings, dividends, IPOs).
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_equity_poll_secs",
        },
    },
    Consumption {
        key: "config.datahub_sec_user_agent",
        by: Consumer::At {
            // The User-Agent the SEC download declares; unset, it downloads nothing and says so.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.config.datahub_sec_user_agent",
        },
    },
    Consumption {
        key: "config.saved_runs_per_strategy",
        by: Consumer::At {
            // The saved-run retention, read by the backtesthub DAEMON at its start and handed to the save
            // path as a `vike_runs::RunRetention`. `vike-cli backtest prune --dry-run` reads it too, to list
            // what that rule would remove (`crates/vike-client/src/resolved.rs`'s `Resolved`); the daemon's
            // read is the one named, `config.node_idle_read_timeout_ms`'s shape.
            file: "crates/vike-backtesthub/src/backtest_cli/serve.rs",
            needle: "settings.config.saved_runs_per_strategy",
        },
    },
    Consumption {
        key: "config.optional_writes_min_free_mib",
        by: Consumer::At {
            // The ONE read: `DiskFloor::of` resolves the row (absent = its default) for each daemon that runs an
            // optional writer, at its start: the backtesthub's run saving, the datahub's live-bar and text
            // recorders and the execution stream's forecast rows, the tradehub's forward runs.
            file: "crates/vike-boot/src/disk_floor.rs",
            needle: "config.optional_writes_min_free_mib",
        },
    },
    Consumption {
        key: "config.backtest_bar_cache_mib",
        by: Consumer::At {
            // The backtesthub DAEMON's bar cache, sized once at its start around the datahub handle it serves
            // every run from (`vike_backtesthub::bar_cache`).
            file: "crates/vike-backtesthub/src/backtest_cli/serve.rs",
            needle: "settings.config.backtest_bar_cache_mib",
        },
    },
    Consumption {
        key: "config.backtest_tick_cache_mib",
        by: Consumer::At {
            // ...and its tick cache beside it, sized at the same start around the same handle.
            file: "crates/vike-backtesthub/src/backtest_cli/serve.rs",
            needle: "settings.config.backtest_tick_cache_mib",
        },
    },
    // ---------------------------------------------------------------------------------------
    // preferences.* — taste and tuning
    // ---------------------------------------------------------------------------------------
    // The desktop's tuning rows and the Studio chat's. Every read is spelled in
    // `crates/vike-desktop/src/main.rs` (the accessor functions beside `startup_appearance`), so
    // `Consumer::binary` names `desktop` — a row set on a headless box does nothing, and the READ
    // column says so. The mapping of each value into the code that uses it is one crate down, where
    // CI runs it.
    Consumption {
        key: "preferences.ai_anthropic_model",
        by: Consumer::At {
            // The Studio chat's Anthropic model id: `chat_settings` hands it to
            // `vike_studio::StudioState::set_chat_settings`, and `StudioChatPane::send` passes it to
            // `vike_ai::make_client`. An id the provider does not know comes back as an HTTP error
            // line in the chat pane; nothing else changes.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.ai_anthropic_model",
        },
    },
    Consumption {
        key: "preferences.ai_cerebras_model",
        by: Consumer::At {
            // The Cerebras twin of `preferences.ai_anthropic_model` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.ai_cerebras_model",
        },
    },
    Consumption {
        key: "preferences.ai_timeout_secs",
        by: Consumer::At {
            // The whole-request HTTP timeout of the chat's provider calls, the third field of the
            // same `chat_settings` value.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.ai_timeout_secs",
        },
    },
    Consumption {
        key: "preferences.backfill_lookback_days",
        by: Consumer::At {
            // The Data Manager's bulk Backfill / Update default window for a series with no
            // recorded gap: `backfill_lookback_ms` hands it to `App::maybe_spawn_stored_backfill`,
            // which passes it to `plan_backfill_jobs` (still clamped per venue by the datahub's
            // history floors).
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.backfill_lookback_days",
        },
    },
    Consumption {
        key: "preferences.chart_history_bars",
        by: Consumer::At {
            // How many bars back a chart reads from a datahub that does not resample:
            // `chart_history_bars` hands it to `seed_charts_from_store`, which passes it to
            // `vike_app_core::data::stored::spawn_store_seed`.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.chart_history_bars",
        },
    },
    Consumption {
        key: "preferences.observe_poll_ms",
        by: Consumer::At {
            // The sleep between the observe bridge's reads of the node's pushed snapshot:
            // `observe_poll` hands it to both `connect_backend` call sites (`App::new`'s observe
            // arm and `App::apply_backend_action`'s backend switch), which pass it to
            // `vike_app_core::backend::observe_bridge::spawn_bridge`. The bridge runs on its own
            // thread, never the frame thread.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.observe_poll_ms",
        },
    },
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
            // crates/vike-datahub/src/datahub_cli.rs's `run`, and — through
            // crates/vike-config/src/file_level.rs's `log_file_level` — the batch tools:
            // crates/vike-backfill/src/cli.rs's `log_config` and
            // crates/vike-backtesthub/src/backtest_cli.rs's `one_shot_file_level`; the `--addr`
            // daemon's is that file's `log_config` (decision 0111 verdict 3).
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
            // installs it, and `vike_client::verbs::guardrail_capabilities` reads it. Advisory — the `trade` and
            // `mcp` previews mark an order over it and refuse nothing; the node enforces its own
            // limits.
            file: "crates/vike-client/src/resolved.rs",
            needle: "install_max_order_quantity_row(booted.settings.preferences.max_order_qty)",
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
    Consumption {
        key: "preferences.observe_stall_timeout_ms",
        by: Consumer::At {
            // The CLIENT's patience on the observe stream, read on the CLIENT's box from its own
            // database. ONE row for two readers, as `config.backtest_addr`'s note argues: the
            // desktop's `link_patience` hands it to `vike_app_core`'s bridge, and `vike-cli`'s one
            // boot walk puts it on `Resolved::link_patience` for `trade` and `vike-mcp` (a `src/cmd/`
            // file reads no settings of its own). The row names the desktop's read, so the READ
            // column says `desktop`; a headless operator's `trade`/`mcp` read it too.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.observe_stall_timeout_ms",
        },
    },
    Consumption {
        key: "preferences.control_reply_timeout_ms",
        by: Consumer::At {
            // The twin of `preferences.observe_stall_timeout_ms` above, for the write channel's
            // reply wait: the desktop's `link_patience` and `vike-cli`'s `Resolved::link_patience`.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.control_reply_timeout_ms",
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
            // an `init(spec)` + `OnceLock`". `vike_backtest::search::paramscan::install_paramscan_threads` is the
            // handle, and `sweep_threads` reads the installed row and nothing else (decision 0111:
            // no environment variable stands beside it).
            //
            // ⚠ **The needle names the `--addr` backtesthub.** The other consuming process is a
            // ONE-SHOT `backtest` run (what `crates/vike-cli/src/cmd/backtest/execute.rs`'s
            // `execute_local` SPAWNS as a child), which installs the same row beside its own
            // settings load in `crates/vike-backtesthub/src/backtest_cli/open_store.rs`. The row names
            // the root INSIDE the crate that owns the pool, so the evidence stands whether or not
            // any GUI is built.
            file: "crates/vike-backtesthub/src/backtest_cli/serve.rs",
            needle: "install_paramscan_threads(settings.preferences.sweep_threads)",
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
    Consumption {
        key: "preferences.vsync",
        by: Consumer::At {
            // The desktop's swap-chain present mode, read ONCE in `main` when it builds the GUI
            // library's options (`AutoVsync` for `true`, `AutoNoVsync` for `false`, never an
            // explicit mode) and handed to `vike_app_core::ui::gpu_startup`'s ladder. Startup-only,
            // so the Settings window labels it "requires restart". The read is spelled in the
            // binary so `Consumer::binary` names `desktop`: a row set on a headless box does
            // nothing, and the READ column says so.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.vsync",
        },
    },
    Consumption {
        key: "preferences.gpu_candles",
        by: Consumer::At {
            // The desktop's two LIVE display rows, read at start by `main.rs`'s `startup_display`
            // and handed to `vike_app_core::ui::appearance_settings::ThemeAppearanceSession`, whose
            // Settings window applies a change at once and saves it as a row the next start reads
            // HERE. Spelled in the binary so `Consumer::binary` names `desktop`.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.gpu_candles",
        },
    },
    Consumption {
        key: "preferences.display_timezone",
        by: Consumer::At {
            // The second live display row — see `preferences.gpu_candles` above.
            file: "crates/vike-desktop/src/main.rs",
            needle: "settings().preferences.display_timezone",
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
            // ruling but the SHAPE: `false` is the guarded state, and the gatewayhub hands every start
            // the RESOLVED row as the bool `AdapterFlags::allow_withdraw_keys`, which the one withdraw
            // gate (`crates/vike-secrets/src/key_permissions.rs`'s `withdraw_gate_before_start`) takes
            // as its override. No credential map carries the flag, so a `VIKE_ALLOW_WITHDRAW_KEYS` row
            // in the credential store is read by nothing and cannot arm a live-money gate;
            // `crates/bridges/binance/src/adapter_tests.rs`'s `the_flag_is_the_only_source` pins it.
            file: "crates/vike-gatewayhub/src/trade/adapters/contract.rs",
            needle: "allow_withdraw_keys: flags.allow_withdraw_keys,",
        },
    },
    Consumption {
        key: "flags.hl_outcome",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). Nothing takes the \
                  value: it was the `enabled` parameter of the settlement poller, which had no \
                  caller and was deleted, leaving the one-pass core `settle_once` \
                  (crates/bridges/hyperliquid/src/execution/settlement.rs), which nothing outside \
                  tests calls — so the row configures nothing, and a set `VIKE_HL_OUTCOME` \
                  refuses startup rather than arming anything. What is missing is a composition \
                  root that MOUNTS the settlement pass on a timer, and mounting one means a live \
                  venue writing synthetic terminal fills into the core — its own decision, taken \
                  with the FLAG_REGISTRY stewardship row this key would lose if the field were \
                  deleted.",
            reader: Reader::Nothing,
        },
    },
    Consumption {
        key: "flags.hyperliquid_hip3",
        by: Consumer::At {
            // The pure core takes the bool (`HyperliquidInstruments::load_from(fetch, recorder,
            // hip3)`), and the bridge's start is handed it as `AdapterInputs::flags` (the gatewayhub's
            // `adapter_flags` builds it from the resolved row; decision 0095's amendment of
            // 2026-11-22). No credential map carries it, so a `HYPERLIQUID_HIP3` entry in the
            // credential store is read by nothing.
            //
            // THREE readers, one row: the needle is the start's flags; the gatewayhub also reads
            // the SAME resolved flag out of its own project folder
            // (`crates/vike-gatewayhub/src/gatewayhub_cli/wire.rs`'s `production_lanes` builds
            // `VenueRest::new` over it) and it is what decides the HIP-3 universe the venue catalog
            // lists, because the wire's `CatalogList` carries no flag (decision 0164); and the
            // datahub's mount still reads it and hands it to
            // `crates/vike-datahub/src/catalog.rs`'s `real_catalog_table`, where it only logs. The
            // environment spelling is retired (decision 0095) — a set `HYPERLIQUID_HIP3` refuses startup.
            file: "crates/vike-gatewayhub/src/trade/adapters/contract.rs",
            needle: "hyperliquid_hip3: flags.hyperliquid_hip3,",
        },
    },
    Consumption {
        key: "flags.oco_cancel_sibling_on_dead_exit",
        by: Consumer::At {
            // …and the identical line in crates/vike-desktop/src/app_setup.rs's `App::new`, plus the
            // daemon's PAPER arm via `vike_tradehub_core::StrategyCorePaperConfig`.
            //
            // ⚠ The needle deliberately points at the DAEMON, the server that runs unattended —
            // exactly the deployment where "leave the book flat once protection dies" is most
            // likely to be the wanted answer.
            // Anchoring the row here is what makes deleting the daemon's read fail this gate.
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,",
        },
    },
    Consumption {
        key: "flags.cancel_orders_on_shutdown",
        by: Consumer::At {
            // The daemon's LIVE arm; the PAPER arm reads the same flag through
            // `vike_tradehub_core::StrategyCorePaperConfig`, and both land on
            // `vike_core::CoreConfig::cancel_orders_on_shutdown`, which the core's teardown block
            // checks before it detaches the client.
            //
            // ⚠ WHAT THIS ROW DOES *NOT* CLAIM. Being consumed is not being reachable, and the stop
            // path decides reachability: under `deploy/vike-tradehub.service` stdin is `/dev/null`,
            // so the interactive `shutdown`/`quit`/Ctrl-D word never arrives, and the teardown this
            // flag gates runs only if SIGTERM raises the stop flag. It does since 2026-08-07
            // (`vike_boot::stop`'s `install_handlers` points SIGTERM and SIGINT at the flag the
            // stdio word raises), and `docs/ops/graceful-stop.md` carries the measurements; a binary
            // OLDER than that took the OS default and ran no Rust at all. That is a property of the
            // STOP PATH, not of the wiring, and no consumption gate can see it —
            // `docs/ops/kill-switches.md` section D is where it is written down for the operator.
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,",
        },
    },
    Consumption {
        key: "flags.pm_resolve",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `enabled` parameter of `ResolvePoller::spawn`, handed to the private \
                  `ResolvePoller::spawn_with_dependencies` beside `ResolvePoller::spawn_with_chain`'s own \
                  (crates/bridges/polymarket/src/settlement/resolve.rs), and nothing \
                  outside tests calls either public door — so the row configures nothing, and a \
                  set `VIKE_PM_RESOLVE` refuses startup rather than arming anything. That file's \
                  module tree says the same: \
                  `crates/bridges/polymarket/src/settlement/mod.rs` records that no \
                  composition root constructs anything in the cluster. Re-arming this needs a root \
                  that MOUNTS the resolve poller — a settlement pass that writes synthetic terminal \
                  fills into the core — not a line of threading.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/resolve.rs",
                // The gate on the parameter, inside `spawn_with_dependencies` (D4 of decision 0095).
                needle: "if !enabled {",
                // BOTH public doors: `spawn_with_dependencies` is private, so naming it alone would leave
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
                  `AutoRedeemPoller::spawn` (crates/bridges/polymarket/src/settlement/\
                  auto_redeem.rs), and nothing outside tests calls that entry point — so the row \
                  configures nothing, and a set `POLY_AUTO_REDEEM` refuses startup rather than \
                  arming anything. Spawning it is not a wiring task: it is NEW UNATTENDED ON-CHAIN \
                  MONEY MOVEMENT, refused deliberately — \
                  `crates/vike-ops/tests/docs/kill_switch_gate.rs`'s RETIRED row is where that decision \
                  is recorded. The flag's own disposition stays KEEP-as-a-flag because an explicit \
                  per-run opt-in is what such a mount would need on the day it is taken.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/auto_redeem.rs",
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
            // ⚠ A REAL-MONEY arming gate, and the settings row is its only source: a set `POLY_EXEC`
            // refuses startup (`crate::REMOVED_ENV`), and the Polymarket start is handed the RESOLVED
            // row as the bool `AdapterFlags::poly_exec` (decision 0095's amendment of 2026-11-22). No
            // credential map carries it, so a credential-store row `POLY_EXEC=1 x` — which once armed
            // real-money exec over `flags.poly_exec = false` (decision 0095's review) — is read by
            // nothing, and there is no second source to disagree with the row.
            file: "crates/vike-gatewayhub/src/trade/adapters/contract.rs",
            needle: "poly_exec: flags.poly_exec,",
        },
    },
    Consumption {
        key: "flags.poly_heartbeat",
        by: Consumer::Not {
            why: "INERT, and its environment spelling is RETIRED (decision 0095). The value is the \
                  `enabled` parameter of `HeartbeatPoller::spawn` \
                  (crates/bridges/polymarket/src/execution/heartbeat.rs), and no composition root \
                  calls that entry point (`crates/bridges/polymarket/src/lib.rs` lists the module \
                  as PARKED, with no production caller) — so the row configures nothing, and a set \
                  `POLY_HEARTBEAT` refuses startup rather than arming anything. In FLAG shape this \
                  is the closest of the six to one line of work — the moment a root spawns the \
                  poller it hands this row to `enabled` — but its `FLAG_REGISTRY` disposition is \
                  RETIRE, so the registry's own answer is that this key should cease to exist \
                  rather than be wired.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/execution/heartbeat.rs",
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
            // Same shape and same argument as `flags.poly_exec` above: the start is handed the
            // resolved row as `AdapterFlags::poly_reconcile`, and no map carries it.
            file: "crates/vike-gatewayhub/src/trade/adapters/contract.rs",
            needle: "poly_reconcile: flags.poly_reconcile,",
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
                  read by crates/bridges/polymarket/src/settlement/auto_redeem.rs's \
                  `kill_switch_tripped` every tick of that poller — and nothing outside tests ever \
                  constructs the poller, so there is nothing running to halt. A set \
                  `POLY_REDEEM_HALT` (an empty one included: it used to halt on PRESENCE) refuses \
                  startup. Two reasons the row is KEPT rather than tombstoned: the switch has a \
                  SECOND trip condition this type does not model at all, the halt FILE, so a row \
                  alone would silently NARROW a kill switch; and the poller it guards still \
                  exists, so deleting the switch would leave the guard behind its subject.",
            reader: Reader::Uncalled {
                file: "crates/bridges/polymarket/src/settlement/auto_redeem.rs",
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
            // ⚠ The second of the two SAFETY OVERRIDES, and like `flags.allow_withdraw_keys` above the
            // resolved row is its only source: the root sets `AdapterPolicy::preflight_skip` from it in
            // the live mount's policy literal, and `run_startup_preflight` reads that field at the boot
            // and at every hot start, sweeping no process environment and no credential map (decision
            // 0111 retired `VIKE_PREFLIGHT_SKIP`, which refuses startup). So a `VIKE_PREFLIGHT_SKIP`
            // line in the credential store is read by nothing.
            // `crates/vike-tradehub/src/tradehub_cli/tests/precedence.rs`'s
            // `the_preflight_skip_row_reaches_the_live_mounts_policy` pins the line this needle names.
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "preflight_skip: flags.preflight_skip,",
        },
    },
    Consumption {
        key: "flags.reconcile",
        by: Consumer::At {
            // …and crates/vike-desktop/src/app_setup.rs's `App::new`, which folds the same resolved value
            // through the same `reconcile_gate`. This flag is one of THREE inputs to that
            // gate rather than the gate itself — it forces the driver on where the armed-live
            // probe reports nothing — so the needle follows the call and not a bare assignment.
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "reconcile_config::reconcile_gate(flags.reconcile,",
        },
    },
    Consumption {
        key: "flags.reconcile_balance",
        by: Consumer::At {
            // `build_reconcile_config` reads the whole reconcile family out of ONE
            // `reconcile_config::ReconcileSettings` value, which `daemon_reconcile_settings` fills from the
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
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "flags.reconcile_off,",
        },
    },
    Consumption {
        key: "flags.reconcile_restore_off",
        by: Consumer::At {
            // The ORDER-RESTORE refusal: the live mount's `CoreConfig` literal hands it to
            // `vike_core::CoreConfig::restore_orders_off`, which the core reads at assembly and at a
            // runtime mount's takeover of pending ownership. The paper arm runs no reconcile pass and
            // leaves the field at its default, so this live-mount line is the one reader.
            file: "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
            needle: "restore_orders_off: flags.reconcile_restore_off,",
        },
    },
    Consumption {
        key: "flags.record_chains",
        by: Consumer::At {
            // The `kind=chain` RECORDER, armed on the data daemon — the one process that holds both
            // the store and (since the Options tool moved onto the datahub) the option chain poll
            // (`docs/decisions/0084-only-the-datahub-touches-the-store.md`). The mount hands the
            // row to `vike_datahub::options::gateway`'s `real_options_lane` as a `ChainRecorder` over the
            // SERVED store handle (`ChainRecorder::new`: the daemon already owns the root), so every
            // polled chain is recorded when it is on; off (the default) builds no recorder. On, in a
            // process that compacts nothing (no recording profile with maintenance), it is REFUSED
            // with a warning naming the fix. A build without `options-serve` reads the row only to
            // warn that it records nothing.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.flags.record_chains,",
        },
    },
    // ⚠ `flags.record_dvol` is DELETED. The tombstone is `crate::flags::DEAD_FLAG_KEYS`, which
    // carries the surviving symbols and what a re-mounting root must supply. The CONSUMPTION table
    // has no opinion about a key that does not exist.
    Consumption {
        key: "flags.record_properties",
        by: Consumer::At {
            // The `kind=properties` WRITER, armed on the data daemon: the one process that holds the
            // store (decision 0084) and the catalog lane it lists from
            // (`crates/vike-datahub/src/properties_tape.rs`'s module doc). ON by default (the owner's
            // ruling of 2026-11-09); a `false` row starts nothing.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.flags.record_properties",
        },
    },
    Consumption {
        key: "flags.telegram_control",
        by: Consumer::At {
            // The Telegram channel's own process (decision 0256): read at its one gate, beside
            // `flags.tradehub_control`, before the credential store is opened.
            file: "crates/vike-telegram/src/telegram_cli.rs",
            needle: "booted.settings.flags.telegram_control",
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
            // `live_switch`, the one read: the switch believed only from settings that READ
            // (decision 0069's 2026-11-23 addendum).
            file: "crates/vike-tradehub/src/tradehub_cli/settings.rs",
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
            // ⚠ The needle is the CALL around the value: it is what makes this an argument PASSED to the
            // gate rather than a value assigned to a local nothing reads, which is the distinction
            // `Consumer::At` exists to draw.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "venue_catalog_gate(booted.settings.flags.venue_catalog_off)",
        },
    },
    // The data daemon's three toggles (decision 0111). Each is read from its row alone; the deploy
    // probe disarms the live plane by writing `flags.datahub_live = false` into its COPIED database.
    Consumption {
        key: "flags.datahub_live",
        by: Consumer::At {
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "if settings.flags.datahub_live {",
        },
    },
    Consumption {
        key: "flags.datahub_chart_seed",
        by: Consumer::At {
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "if flags.datahub_chart_seed {",
        },
    },
    Consumption {
        key: "flags.datahub_allow_public_bind",
        by: Consumer::At {
            // ONE row for two readers, `config.backtest_addr`'s shape: the data daemon's bind guard
            // is named here, and the backtesthub (`backtesthub --addr`) reads the same row the same
            // way in `crates/vike-backtesthub/src/backtest_cli/serve.rs`'s `run_serve`.
            file: "crates/vike-datahub/src/datahub_cli/boot.rs",
            needle: "let allow_public = flags.datahub_allow_public_bind;",
        },
    },
    Consumption {
        key: "flags.record_strat_quotes_off",
        by: Consumer::At {
            // The recording phase folds it into `RecorderArgs::strategy_quotes` (`!flags.record_strat_quotes_off`),
            // the one fact `crate::recorder::record` mounts the strategy-following feeds by (decision 0193).
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "!flags.record_strat_quotes_off",
        },
    },
    Consumption {
        key: "flags.start_history_from_store",
        by: Consumer::At {
            // The strategy lane's arming phase hands it to `StartHistory` as the one fact the start history is
            // served from the store by (decision 0195); off, the lane only SAVES and compares.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "flags.start_history_from_store",
        },
    },
    Consumption {
        key: "flags.datahub_csv_upload",
        by: Consumer::At {
            // The import lane's mount arms the upload on it (decision 0272): off, `csv_upload` is not
            // advertised and the verb is refused having read nothing.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.flags.datahub_csv_upload",
        },
    },
    Consumption {
        key: "flags.record_texts",
        by: Consumer::At {
            // The text recorder's phase arms on it (decision 0143's 2026-11-16 amendment): off, nothing
            // is recorded and the tool-feeds lane polls lazily as before.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "booted.settings.flags.record_texts",
        },
    },
    Consumption {
        key: "flags.record_market_rules",
        by: Consumer::At {
            // The recording phase folds it into `RecorderArgs::market_rules`, the one fact
            // `crate::recorder::record` builds the market-rules writer by (book plan W38): off, no rules
            // are read or stored.
            file: "crates/vike-datahub/src/datahub_cli/recording.rs",
            needle: "args.market_rules = flags.record_market_rules",
        },
    },
    Consumption {
        key: "flags.record_bars",
        by: Consumer::At {
            // The strategy lane's arming phase attaches the live-bar recorder on it (on by default, the
            // owner's ruling of 2026-11-17): off, the lane hands no close to a recorder.
            file: "crates/vike-datahub/src/datahub_cli/lanes.rs",
            needle: "flags.record_bars",
        },
    },
    Consumption {
        key: "flags.record_forward_runs",
        by: Consumer::At {
            // The trade daemon's forward-run recorder starts on it (ON by default, the owner's ruling of
            // 2026-11-17): a `false` row starts no recorder and writes no run.
            file: "crates/vike-tradehub/src/tradehub_cli.rs",
            needle: "settings.flags.record_forward_runs",
        },
    },
];
