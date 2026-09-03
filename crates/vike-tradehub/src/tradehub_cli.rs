//! `vike-tradehub` — the live trading daemon's command line, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/main.rs`'s body until the multicall merge. `main` became [`run`], taking
//! the environment, the working directory and argv as parameters.
//!
//! ⚠ **THE ORDER OF THE FIRST STATEMENTS IS THE PROPERTY, NOT A STYLE.**
//! `crates/vike-ops/tests/graceful_stop_pin.rs`'s
//! `the_tradehub_handler_is_installed_before_anything_can_place_an_order` checks BY POSITION IN
//! THIS FILE that `install_handlers` precedes every mount call. Between a mount returning and the
//! handler installing, the maker is already folding ticks and may already hold RESTING ORDERS at
//! the venue while SIGTERM still carries the OS default disposition — the process would die where
//! it stands, no teardown, no cancel sweep, book abandoned. Do not reorder the opening of [`run`].
//!
//! ⚠ The environment reaches the five scattered readers through [`PROCESS_ENV`], seeded ONCE by
//! [`resolve_settings`] from the caller's map rather than swept again here. That is what keeps
//! `HYPERLIQUID_MAINNET` — the flag deciding mainnet versus testnet on a venue that trades real
//! money — reading the SAME map the boot consumed.
//!
//! Everything below is the binary's own documentation, unchanged.
//! Runs a strategy on a server with NO GUI, controlled by a TOML profile + newline-JSON stdio, and
//! survives an SSH disconnect (it is a systemd service, detached from the login session — steal R7:
//! closing the laptop lid never affects the daemon).
//!
//! ## WHICH strategy — any registered one, not only the A-S maker
//! The profile's optional `[strategy] name = "…"` + `[strategy.params]` table names a strategy from
//! the SHARED registry (`vike_strategy::strategy_by_name`, the same one `vike-backtest`'s harness
//! resolves through — it is generic over the broker precisely so both can), and the daemon mounts
//! it through [`vike_run::build_paper_strategy_core_with`] / [`vike_run::build_live_strategy_core`].
//! So the profile that was backtested is the profile that trades.
//!
//! ABSENT `[strategy]`, the mounted strategy is the Avellaneda–Stoikov `vike_mm::SpreadMaker` built
//! from the profile's own maker fields by [`vike_run::build_maker`] — the same function the
//! `build_*_maker_core` entry points have always called, so the default path is byte-identical to
//! every daemon that shipped before this table existed. The A-S maker is now ONE mountable strategy
//! rather than the hardcoded one; it is still the DEFAULT one.
//!
//! `DaemonProfile::validate` rejects, at profile LOAD, a name that is unknown, simulator-only, or
//! (the important one) one that would RESOLVE and then never trade — see
//! `vike_strategy::LIVE_CAPABLE`.
//!
//! ## Two variants — PAPER (default) and the opt-in LIVE build_node core
//! By default the daemon proves the LIFECYCLE + stdio control over the PAPER exchange, CI-testable,
//! with zero `vike-app`: it mounts the resolved strategy on the PRODUCTION live core
//! ([`vike_core::spawn_core`]) with the paper exchange as the `ExecutionClient` — so there is ZERO
//! real-money / credential / geo risk and NO live feed (a maker only quotes once a feed drives it).
//!
//! The `tradehub_live` FLAG (`<project>/settings/flags.toml`, still overridden by
//! `VIKE_TRADEHUB_LIVE`) instead mounts the SAME strategy on the REAL twelve-venue [`vike_run::build_node`] core
//! with per-venue credential-gated exec AND wires the venue's own
//! LIVE feed. OFF (the default) is byte-identical to the pure-paper daemon. The path is defended by
//! FIVE gates: (1) the `tradehub_live` master gate; (2) per-venue creds in the workspace `.env`
//! (absent creds keep that venue paper even with the gate on); (3) each venue's own second gate
//! (Hyperliquid stays TESTNET unless `HYPERLIQUID_MAINNET=1`); (4) the daemon's own venue ALLOW-LIST
//! ([`live_mount`] hard-errors on any unwired venue — never a silent paper fallback); (5) the
//! venue+symbol validation against build_node's own `WIRED_MARKETS` table
//! ([`crate::config::DaemonProfile::validate_for_live`]). The live-wired venue set is
//! [`crate::config::LIVE_WIRED_VENUES`] — pinned equal to `venue_feed_plan`'s arms by
//! `tests/daemon/live_wired_venues_pin.rs`, so it is not restated here (a previous sentence here
//! named the set and was two venues stale within a month). Each venue quotes in its own price
//! domain: Polymarket via [`MakerMountConfig::polymarket`] (the [0,1] outcome-token domain), every
//! other venue via [`MakerMountConfig::crypto`] (`vike_model::PriceDomain::Unbounded` +
//! RawLocal/ConstantTau) — the $-scale domain that lifts the old A-S `[tick, 1−tick]` wall clamp
//! (`vike_mm::avellaneda` §2.2) which used to pin a $64k quote to `None`.
//!
//! ## Reconcile-on-restart (`VIKE_RECONCILE=1`, OFF by default, QUARANTINE-FIRST)
//! So a restarted LIVE daemon re-adopts open venue orders/positions instead of mounting a blind fresh
//! core, [`live_mount`] mounts the reconciliation engine ([`vike_core::spawn_recon`]) exactly as
//! `vike-app` does — same [`vike_ops::reconcile_config`] env parser, same
//! `node.recon_clients`/`node.recon_trigger`, same per-venue `ReconConfig`. It is gated on the master
//! `reconcile` FLAG (`flags.toml`, still overridden by `VIKE_RECONCILE`; the rest of the
//! `VIKE_RECONCILE_*` family is still read from the process env by
//! [`vike_ops::reconcile_config::build_recon_config`]). **OFF (the default) is
//! byte-identical to the pre-reconcile daemon**: `recon_enabled` is `false`, so `build_node` builds no
//! reconnect-trigger channel, no `ReconConfig` is built, `spawn_recon` is never called, and NO
//! `vt-core-recon` thread is spawned — every new path is inside `if recon_enabled`. The daemon defaults
//! the POLICY to **`quarantine`** (holds every divergence for operator confirm; auto-folds NOTHING)
//! unless the operator sets `VIKE_RECONCILE_POLICY`, per CLAUDE.md's `VIKE_RECONCILE` rollout rule —
//! under `hybrid` a fresh daemon would auto-apply `PositionDrift`, rewriting position size and
//! booking realized PnL at the venue's price before anyone has confirmed the venue's report is
//! complete. (The rule used to be justified by "an incomplete open-order fetch under `hybrid`
//! auto-cancels `OrphanLocalOrder`s"; that was false — the kind resolves to zero events under every
//! policy. The default stands on the `PositionDrift` ground instead. See
//! `crates/vike-exec/tests/recon/recon_policy_pin.rs`.) Health gate: the
//! daemon runs no market feeds `build_node` exposes ([`vike_run::build_node`] leaves feeds to the
//! caller), so reconcile mounts with an EMPTY feed-status map — every reconciled venue reads
//! [`vike_core::ReconHealth::Healthy`] and is never health-blocked (the exec-only-venue shape; a pass
//! against a briefly-down venue fails soft, whereas a wrongly-suppressed pass can stay suppressed).
//!
//! ## Optional observe + control server (PR-11/12/13)
//! When `config.toml`'s `tradehub_addr` (or `VIKE_TRADEHUB_ADDR`) is set, the daemon also starts an authenticated node
//! server (`crate::server` + `publish`) so a laptop GUI (`vike-app --observe`) can WATCH this
//! live node's snapshots over an SSH tunnel — and, when the `tradehub_control` FLAG is on and a
//! `VIKE_TRADEHUB_CONTROL_KEY` is set, TRADE it (place/cancel orders over a `Scope::Control`
//! connection). When `config.toml`'s `datahub_advertise_addr` (or `VIKE_DATAHUB_ADVERTISE_ADDR`)
//! is ALSO set, every `Welcome.features` carries `datahub=<addr>` — the REQ-2 advertisement of
//! the datahub this backend fronts, so a client configures one address (advertisement, never
//! proxying). OFF by default: with no address the daemon is byte-identical to the pure-stdio PR-9
//! daemon; absent the control gate/key the server is read-only (an Observe peer's command is refused).
//! The publisher NEVER touches the vike-core fold (it reads only the arc-swap snapshot cell), so the
//! p99 latency gate is unaffected. Control is DOUBLE-GATED (key + `CommandSink`) and, at the server
//! edge, notional/rate-limited (the notional ceiling from `<vike home>/policy.toml`'s
//! `max_notional_per_order`, the rate from `VIKE_TRADEHUB_CONTROL_RATE` — both resolved ONCE at
//! startup and passed into `serve` as a `ControlLimitsConfig`, audit F13) as defense-in-depth
//! beyond the core `RiskGate` every remote order still passes through. ⚠ The ceiling was
//! `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` until Phase 5 of the settings-unification design removed it;
//! a daemon that still finds that variable set REFUSES TO START (see `resolve_settings`).
//!
//! ## The settings FILES are the standing configuration (settings-unification Phase 6c/6d)
//! `resolve_settings` loads `<project>/settings` exactly once at startup — BEFORE the log subscriber,
//! because the log destination and both log levels are themselves settings — and returns the whole
//! `vike_config::Settings`. These of its fields reach this daemon — no count is written down,
//! because `vike_config::CONSUMPTION` is the machine-checked list and a prose number beside it only
//! ever rots — each still overridden by its own environment variable (`env > file > default`,
//! resolved inside the loader): `config.log_dir` plus
//! `preferences.log_level`/`log_file_level` build the `vike_log::LogConfig`; `config.tradehub_addr`
//! and `flags.tradehub_control` gate the node server; `flags.telegram_control` the Telegram channel;
//! `flags.tradehub_live` the live mount; `flags.tradehub_record` the recorder; `flags.reconcile` the
//! reconciliation engine; `flags.oco_cancel_sibling_on_dead_exit` the bracket-sibling behaviour on
//! BOTH mounts. `vike_config::CONSUMPTION` is the machine-checked record of that list
//! (`crates/vike-config/tests/settings_are_consumed.rs`) AND of every setting this daemon still does
//! not read — a settings file that validates and is displayed as effective while nothing reads it is
//! worse than an unimplemented feature, and that gate is what makes it un-shippable.
//!
//! The POLICY half comes from the same one load: `live_mount` projects it onto
//! `vike_run::MountPolicy` and threads it into all twelve `vike_mount::make_engine` arms.
//!
//! ⚠ **`policy.venues` is the field that decides whether this daemon trades at all.** It is the
//! per-venue ARMING CEILING (`paper` / `demo` / `live`), read at the top of `make_engine` above the
//! credential read, and its default is `paper` for EVERY venue — so **no `policy.toml` on this box
//! means an ALL-PAPER daemon**, whatever the credential store holds. That is deliberate: this
//! daemon's own startup log once printed nine live authenticated venues for a run profile that
//! named one, because credential presence was the only gate. The first start after that ceiling
//! landed warns once, naming this file, the `[venues]` key and every venue it refused. See
//! `vike_mount::venue_arming_migration`.
//!
//! `market_slippage` is the other field that binds a venue arm — the aggression band a venue with
//! NO NATIVE MARKET ORDER prices its emulated market (and tripped stop-MARKET) orders at.
//! Hyperliquid is the only such venue on the roster and this daemon's primary live venue, so that
//! band is the worst price every one of its market intents is allowed to reach. For THAT field, no
//! `policy.toml` is byte-identical: `Policy::default()` carries `None` and hyperliquid keeps its own
//! compiled-in literal.
//!
//! ## Optional TELEGRAM control channel (behind the `telegram` FEATURE, then FOUR runtime gates)
//! Compiled ONLY under the crate's off-by-default `telegram` Cargo feature — a default daemon does
//! not contain this code, so no runtime misconfiguration (a stale systemd unit, an inherited
//! environment) can reach it. With the feature on: the `tradehub_control` **and**
//! `telegram_control` FLAGS (`flags.toml`, each still overridden by its own variable) **and** a `VIKE_TELEGRAM_BOT_TOKEN` **and** a non-empty
//! `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` in the workspace `.env` mount `crate::telegram` — a
//! `getUpdates` long-poll (outbound HTTPS only, NO inbound listener) that lets an allowlisted chat
//! drive this node from a phone. Absent ANY of the four ⇒ nothing is constructed: no thread, no bot
//! token read into memory, no network call (the two process flags are checked purely FIRST, and the
//! `.env` loader is passed as a FUNCTION, so the OFF path never even opens it).
//!
//! ⚠ **Accepted risk, stated once:** with this on, the bot token plus membership in an allowlisted
//! chat is sufficient to place REAL orders — Telegram is a third party in an order-origination path.
//! Every write is preview + `/confirm`-gated (single-use token, 60 s, bound to the exact command and
//! chat), an unlisted chat is ignored and NEVER answered, and every confirmed command goes through
//! the SAME [`crate::server::accept_command`] the TCP control path uses — the same
//! `ControlLimits` notional/rate caps, the same audit record, and the same core `RiskGate`.
//!
//! ## Optional alerting (the rules FILE is the gate, OFF by default)
//! When `$VIKE_ALERTS` (default `<project>/settings/state/alerts.json`) names a rules file with at
//! least one ENABLED rule, the daemon mounts `vike_ops::alerting`'s `AlertEngine`
//! ([`crate::alerts`]) so alerts keep firing on an unattended node instead of dying with a
//! GUI window. It is a strict OFF-FOLD consumer: the engine is MOVED onto the existing periodic
//! snapshot thread and fed the same lossy `arc-swap` read the JSON summary already does, so the
//! `p99 < 10µs` core gate is untouched. Delivery is a `tracing` record always (stdout stays
//! protocol-only) plus any Telegram/webhook target the workspace `.env` configures. Absent file,
//! zero rules, or all-disabled rules ⇒ nothing is constructed (no engine, no sink, no resolved
//! webhook) and the daemon is byte-identical to before. Only SNAPSHOT-driven triggers
//! (`Price`/`Drawdown`/`ReconAlert`) have a source here today; `Fill`/`OrderRejected`/`Indicator`/
//! `Feed`/`FillRateBreaker`/`PolymarketResolution` rules load but cannot fire — see
//! [`crate::alerts`]'s module doc for why each is unfed.
//!
//! ## The lifecycle, in one place
//! 0. [`vike_ops::stop::install_handlers`] — the FIRST statement of [`main`], before argv and before
//!    the settings load. Until it returns, SIGTERM carries the OS default disposition, so every
//!    later step of this list is a window in which a service stop runs no teardown at all.
//! 1. [`vike_log::init`] (the daemon HOLDS the returned guards for the whole process), then the
//!    STARTUP DISCLOSURE — `vike_buildinfo::version_line` (which commit this binary is),
//!    `vike_config::boot_lines` (the settings directory that answered, each file present or absent,
//!    the resolved ceilings, and whether a credential store sits beside them) — and
//!    `log_handler_outcome`, reporting what step 0 did. All four are the first moment there is a
//!    subscriber to say anything through, which is why none of them can happen earlier.
//! 2. Parse the `--config` [`DaemonProfile`](crate::config::DaemonProfile) → a
//!    [`vike_run::MakerMountConfig`].
//! 3. [`vike_run::build_paper_maker_core`] → the live [`vike_core::CoreHandle`].
//! 4. A snapshot-summary thread prints a one-line JSON summary of the arc-swap snapshot to STDOUT on
//!    the profile cadence (a LOSSY reader — never touches the core fold).
//! 5. The stdin control thread reads newline-JSON [`vike_exec::Command`]s and lowers each through
//!    [`vike_core::CommandSink::send_blocking`] — the same ingest lane
//!    [`vike_core::CoreHandle::send_command`] feeds, and the EXACT seam the network control verb
//!    reuses (that one deliberately keeps the non-blocking `try_command`).
//! 6. The main thread waits on the stop flag ([`vike_ops::stop::StopSignal`]) — raised by the stdio
//!    word, by Ctrl-D on a TTY, or by SIGTERM/SIGINT.
//! 7. A bounded, graceful teardown ([`vike_ops::shutdown::run_with_deadline`] wrapping
//!    [`vike_core::CoreHandle::shutdown_and_join`]) — hard-capped so a wedged join can never hang exit.
//!
//! ## stdout is protocol-only
//! STDOUT carries ONLY the newline-JSON protocol (summaries + acks); every log/diagnostic rides the
//! `tracing` file/stderr layer ([`vike_log`]), never stdout.
//!
//! ## Shutdown trigger — ONE STOP FLAG, MANY TRIGGERS
//! A `shutdown`/`quit` word on stdin, Ctrl-D on a TTY, **SIGTERM/SIGINT** on unix, the console
//! control events on Windows, and — on Windows only — the appearance of
//! `<state>/`[`vike_ops::stop::STOP_FILE_NAME`] all raise the SAME
//! `AtomicBool`, and the main thread waits on it and then runs the bounded teardown exactly once.
//! That last one is what makes a DETACHED Windows run stoppable at all: a console event needs a
//! console, and a background host has none, so without it `taskkill` — no teardown, no cancel
//! sweep, the book abandoned — was the only stop. `docs/ops/tradehub-windows.md` is the operator's
//! page; `vike_ops::stop`'s module doc is why it is `cfg(windows)` and why unix keeps its refusal.
//! `vike_ops::stop` is the primitive and its module doc is the contract; the handler it installs
//! only STORES A BOOL, which is the only thing legal in a signal context. A NON-tty EOF (e.g.
//! systemd `StandardInput=null`) still does NOT stop the daemon — the control thread logs and ends,
//! and the daemon keeps trading headless until a signal arrives.
//!
//! Two ordering rules make that one stop rather than several, and both were found by review rather
//! than by testing:
//!
//! * the handler is installed **first**, above everything (step 0 of the lifecycle). A handler
//!   installed after the mount leaves a window in which the maker already has resting orders at the
//!   venue while SIGTERM still kills the process outright;
//! * the LOSER of [`vike_ops::stop::StopSignal::begin_teardown`] **waits** for the winner rather than
//!   returning. Returning from `main` ends the process, so the guard against a second teardown would
//!   otherwise be truncating the first one — a half-finished cancel sweep instead of a duplicated
//!   one.
//!
//! ### Why this is the change that makes `cancel_orders_on_shutdown` mean anything
//! `cancel_orders_on_shutdown` (`<project>/settings/flags.toml`) makes the bounded teardown cancel
//! every resting order on the way out. Until this wiring, the teardown was what SIGTERM never
//! reached: the daemon's headless arm ended in `loop { std::thread::park(); }` and the WHOLE
//! teardown was written directly below it — correct, tested, and reachable only from a TTY. So
//! under `deploy/vike-tradehub.service` the flag was honoured on an interactive stop and BYPASSED by
//! `systemctl stop`, and anyone who turned it on had never once had it run as a service: the resting
//! book was abandoned at the venue, while `systemctl status` reported success.
//!
//! Three shapes were considered and two refused (`docs/ops/graceful-stop.md` has the the CI box
//! measurements behind each):
//!
//! * **`ExecStop=` cannot carry the stdio word.** It runs a SEPARATE process with its own stdio and
//!   has no way to write to this daemon's stdin, which systemd has already wired to `/dev/null`.
//!   Re-pointing stdin at a FIFO (`StandardInput=file:`) does not rescue it either: the reader sees
//!   EOF the moment no writer is attached, i.e. at startup.
//! * **A STOP sentinel an `ExecStop=` touches** does work — `ExecStop=` genuinely runs BEFORE
//!   SIGTERM with this process still alive — but only if the command WAITS on `$MAINPID`, and its
//!   correctness then lives in the unit FILE: a deployment running an older unit polls a sentinel
//!   nothing touches and behaves exactly as it did, with no error. It also covers no bare `kill`.
//! * **Hand-rolled `libc::sigaction`** saves the one package and costs an `unsafe` block plus an
//!   `UNSAFE_EXEMPT` carve-out in a binary that signs orders, to re-derive by hand what the package
//!   already gets right. Strictly dominated.
//!
//! `crates/vike-ops/tests/graceful_stop_pin.rs` pins the wiring that landed, so this paragraph
//! cannot outlive it.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::net::{TcpListener, ToSocketAddrs};
use std::path::Path;
// Used by `alerts_path` in every build, and by the feature-gated `tick_store_root`.
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vike_core::{CoreHandle, CoreSnapshot};
use vike_ops::reconcile_config;
use vike_ops::shutdown::{run_with_deadline, ShutdownOutcome};
use vike_ops::stop::{self, StopSignal};
// LIVE path only: the live feed is wired through a `LiveDataSink` onto the core lanes. `DataClient`
// itself is no longer named here — Task 3 moved every site that named it directly (`FeedCtors`,
// `LiveFeeds`'s `Oanda`/`Deribit` variants) into `feeds.rs`.
use crate::alerts::{self, AlertMount};
use crate::config::{resolve_paper_risk_limits, DaemonProfile};
use vike_data::LiveDataSink;
use vike_exec::Command;
use vike_run::{MakerMountConfig, NodeConfig};
// `LiveFeeds`/`CexBars`/`CexTicks`/`FeedCtors`/`ProdFeedCtors`/`PostFeeds`/`LiveTeardown` used to
// be private to this file; all moved to the library (`feeds.rs`) for the SAME reason `CexVenue`/
// `VenuePlan` did — `venue_feed_plan`/`wire_venue_feeds`/`recon_feed_statuses_of` name them in
// their own signatures, and a library cannot name a type owned by the binary that depends on it.
// `check_poly_token_intervals` is imported separately below: it exists in the library only under
// the `polymarket` feature, so an unconditional import here would fail an OFF build.
#[cfg(feature = "polymarket")]
use crate::feeds::check_poly_token_intervals;
use crate::feeds::{
    recon_feed_statuses_of, venue_feed_plan, wire_venue_feeds, FeedCtors, LiveFeeds, LiveTeardown,
    PostFeeds, ProdFeedCtors,
};
// `CexBars` has no production call site left in this file (the arm that built one, `wire_venue_feeds`,
// moved to `feeds.rs`) — only `#[cfg(test)] mod tests`'s own `cex_bars_for` helper still names it.
#[cfg(test)]
use crate::feeds::CexBars;
use crate::hot_reload;
use crate::publish::{self, PublisherHandle};
use crate::server;
// `CexVenue`/`VenuePlan` used to be private to this file; both moved to the library (`types.rs`)
// because `venue_plan`'s plan-construction functions — moved for the SAME reason, so this crate's
// integration tests can reach them — needed to name them, and a library cannot name a type owned
// by the binary that depends on it. The per-venue arming helpers moved for the identical reason,
// into `venue_arming`. `ResolvedMount` joined `types.rs` later (main-split Task 3), for the
// identical reason again: `feeds::check_poly_token_intervals` needed to name it.
//
// ⚠ Task 3 also moved `venue_feed_plan`/`wire_venue_feeds` — the PRODUCTION callers of most of the
// names below — into `feeds.rs`, which imports its own copies from `venue_arming`/`venue_plan`
// directly. So most of what this file used to pull in for THOSE callers is now consumed ONLY by
// this file's own `#[cfg(test)] mod tests` (and `feed_splice_seam_tests`, gated identically) —
// gated below accordingly rather than left unconditional and reported unused by a default-build
// clippy run. `withhold_exec_credentials`/`check_ctrader_intervals`/`ResolvedMount`/`VenuePlan`
// keep a REAL production call site here (`live_mount_with`'s withhold pass, its interval gate, and
// the mount-resolution loop) and stay unconditional. `CexArming`/`cex_mainnet_enabled` had no real
// use site left ANYWHERE — production or test — and were deleted outright rather than gated.
use crate::venue_arming::withhold_exec_credentials;
#[cfg(test)]
use crate::venue_arming::{
    alpaca_arming, cex_arming, ctrader_arming, data_only_arming, deribit_arming, exec_badge,
    ig_arming, oanda_arming, other_live_accounts, with_other_live_accounts, EXEC_LIVE,
    EXEC_LIVE_MULTI_ACCOUNT, EXEC_OTHER_ACCOUNT_LIVE, EXEC_PAPER,
};
use crate::venue_plan::check_ctrader_intervals;
#[cfg(test)]
use crate::venue_plan::{
    alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan, wired_symbol_for,
};
#[cfg(test)]
use crate::CexVenue;
use crate::{ResolvedMount, VenuePlan};
use vike_tradehub_client::{NodeKeys, Scope};

const USAGE: &str = "\
usage: vike-tradehub --config <profile.toml> [--profile <run_profile.toml>]

  --config PATH   the daemon profile TOML (venue / token_id / A-S mount shape) — REQUIRED
  --profile PATH  the operator-budget RunProfile TOML ([risk] ceilings); absent, $VIKE_RUN_PROFILE
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

/// The alerting rule-file override, read HERE rather than in the library so the env read stays in
/// the binary — the settings-registry rule. Since settings STEP 2 this is the workspace's ONLY
/// `$VIKE_ALERTS` read (`vike_ops::alerting::persist` no longer reads it at all). Unset ⇒
/// `<project>/settings/state/alerts.json` (see [`alerts_path`]). Absent file ⇒ no rules ⇒ no
/// engine (see [`maybe_mount_alerts`]).
const ALERTS_ENV: &str = "VIKE_ALERTS";

/// The STATE-ROOT override — the one root for every file the program writes and no human edits.
/// Unset, it is `<project>/settings/state` (`vike_model::state_path`). Read here in the BINARY,
/// which is the correct shape; the resolver itself is pure.
///
/// ⚠ `_ROOT`, not `_DIR`: `VIKE_STATE_DIR` is already `vike-app`'s strategy-state SIDECAR
/// directory and means something else entirely — see `vike_model::state_path`'s module doc.
const STATE_ROOT_ENV: &str = "VIKE_STATE_ROOT";

/// A resolved boolean flag, spelled as the exact `"1"` string the pure gate parses.
///
/// The gates this feeds (`crate::telegram::control_gates_open`) keep their own
/// exact-`"1"` grammar and their own tests; only the RESOLUTION moved into `vike_config`, which
/// already applied `env > file > default` and already rejects a truthy typo by name. `false` is
/// passed as `None` — absent — which that grammar has always meant "closed", so the OFF path is
/// byte-identical to an unset variable.
#[cfg(feature = "telegram")]
fn as_gate(on: bool) -> Option<&'static str> {
    on.then_some("1")
}

/// What the ready banner's `venue=` renders when the LIVE gate is on and the mount armed NOTHING —
/// an empty credential store, or a store holding no key for any wired venue.
///
/// It is a SENTINEL in a field whose other values are venue ids, so it must not be one: `main`'s
/// `banner_sentinel_is_not_a_venue_id` pins it against `vike_model::VENUES` (the roster is derived
/// from the `crates/bridges/*` tree, so a future bridge named `none` reddens that test rather than
/// silently making this line ambiguous).
///
/// ⚠ It is not `PAPER`. The gate being ON is an operator-visible fact independent of what armed:
/// the venue FEEDS are live, the `[[mounts]]` strategies are quoting against real prices, the B11
/// live-account locks are held, and one credential appearing in the store arms real exec on the
/// next start with no config change. Collapsing that to `PAPER` would hide the arm; rendering an
/// empty `LIVE (venue=)` would read as a truncated line rather than as a statement.
const NO_VENUE_ARMED: &str = "none";

/// The ready banner's `"mode"` — the string `docs/ops/tradehub-the CI box.md` and
/// `deploy/vike-tradehub-project.service` both name as the ONE authority on paper-vs-live, and which
/// operators grep for (`grep '"kind":"ready"'`).
///
/// ⚠ **It is a function of the ARMING RECORD, and the profile's mount set is not a parameter.**
/// That signature is the fix. The string used to be built from the daemon's `mount_venues` — the
/// DISTINCT venues the profile mounts a strategy on — which answers a different question and is
/// neither a subset nor a superset of what armed: `vike_run::build_node` calls
/// `vike_mount::make_engine_with_legs` straight-line for every `WIRED_MARKETS` row and arms a real
/// exec client wherever the credential store answers, so the armed set is decided by the STORE,
/// not by the profile. MEASURED on the CI box, one startup of the shipped daemon, two lines apart:
///
/// ```text
/// live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}
/// {"kind":"ready","mode":"LIVE (venue=bybit)"}
/// ```
///
/// Nine live authenticated exec sessions; the banner named the one venue the profile mounted.
///
/// `venues` is `vike_run::build_node`'s own `live_venues` — a venue is in it exactly when
/// `make_engine_with_legs` constructed a REAL `ExecutionClient` for it, so the set excludes a venue
/// with no credentials, a `data_only = true` venue whose keys [`withhold_exec_credentials`] took
/// away, a ctrader/ibkr venue whose synchronous connect failed and demoted it, and polymarket's
/// recon-only fallback. Sorted before rendering: a `HashSet` iterates in an order that changes
/// between runs of the same binary, and an operator diffing two startups must not read a
/// reordering as a change.
fn ready_mode_line(live: bool, venues: &std::collections::HashSet<String>) -> String {
    if !live {
        // ⚠ UNCHANGED, and deliberately not routed through the `venue=` rendering below. A PAPER
        // daemon builds no live client by any path, so there is no set to name and nothing an
        // operator's existing `grep PAPER` should have to learn.
        return "PAPER".to_string();
    }
    let mut armed: Vec<&str> = venues.iter().map(String::as_str).collect();
    armed.sort_unstable();
    let named = if armed.is_empty() { NO_VENUE_ARMED.to_string() } else { armed.join("+") };
    format!("LIVE (venue={named})")
}

/// Complete the per-mount `StrategyStatus` rows from the mount's ARMING RECORD.
///
/// `WireMountRow::live` is documented as "true iff this MOUNT trades LIVE" — a per-VENUE question.
/// It used to be filled with `flags.tradehub_live`, a per-PROCESS boolean resolved before any venue
/// was mounted and never revised, so it over-claimed in two reachable ways at once:
///
/// - a mount whose venue has NO credentials reported `live: true` while `vike_mount::make_engine`
///   had put it on the paper exchange (`vike_run::build_node`'s whole credential gate), and
/// - a `data_only = true` mount reported `live: true` while [`withhold_exec_credentials`] had taken
///   its keys away from exec BY DECLARATION — the one combination the profile loader guarantees is
///   reachable, since it REFUSES `data_only` unless the live gate is on.
///
/// `venues` excludes both by construction (no real `ExecutionClient` was built for either), along
/// with a ctrader/ibkr venue whose synchronous connect demoted it and polymarket's recon-only
/// fallback — so one substitution fixes every case.
fn wire_mount_rows(
    seeds: Vec<WireMountSeed>,
    venues: &std::collections::HashSet<String>,
) -> Vec<vike_tradehub_client::wire::WireMountRow> {
    seeds
        .into_iter()
        .map(|s| vike_tradehub_client::wire::WireMountRow {
            strategy: s.strategy,
            params: s.params,
            live: venues.contains(&s.venue),
        })
        .collect()
}

/// One mount's wire row, MINUS the fact that does not exist yet.
///
/// `main` must capture the per-mount strings BEFORE the mount (both arms MOVE `resolved`) and can
/// only fill in `live` AFTER it (that answer is `build_node`'s arming record, which the mount
/// produces). The seam is a struct rather than a tuple so neither half can be silently reordered,
/// and `venue` is carried explicitly because it is the KEY the arming record is queried with —
/// `WireMountRow` itself deliberately holds no addressing field (see its doc).
struct WireMountSeed {
    strategy: String,
    params: String,
    /// The mount's venue id — looked up in `build_node`'s `live_venues` to decide `live`, then
    /// dropped. NOT published: a `WireMountRow` carries no addressing fields yet.
    venue: String,
}

/// One mount's SELF-ADDRESSED params line — the `[[mounts]]` rendering of
/// `DaemonProfile::effective_params`, prefixed with the mount's own venue/symbol/interval because
/// a `WireMountRow` deliberately carries no structured addressing fields yet (see that struct's
/// doc): without the prefix, N rows of `size=2`-style strings would be indistinguishable.
fn mounts_wire_params(m: &ResolvedMount) -> String {
    format!(
        "venue={} symbol={} interval={} :: {}",
        m.spec.venue,
        m.spec.symbol,
        m.spec.interval,
        m.row.effective_params(&m.cfg)
    )
}

/// The parsed command line. `profile_path` is the OPERATOR-BUDGET `RunProfile` (RunProfile wiring,
/// Settings STEP 2 PR 1, Task 2) — independent of (and layered ON TOP of) `config_path`'s
/// [`DaemonProfile`], which still owns the venue/token_id/A-S mount shape. `None` here (no
/// `--profile` flag) falls through to [`vike_core::resolve_profile`]'s `VIKE_RUN_PROFILE` env
/// fallback; both absent ⇒ `Ok(None)` ⇒ the daemon's risk budget is byte-identical to today.
///
/// `Debug` on both this and [`Parsed`] so a parse that was supposed to FAIL can report what it
/// produced instead (`Result::expect_err` requires it) — the same reason
/// `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    config_path: String,
    profile_path: Option<String>,
}

/// What a successful parse produced. `-h`/`--help` is NOT an error — there is nothing to run, but
/// nothing went wrong either, and it used to be spelled as `Err("help requested")`, which the
/// caller then printed to stderr and exited **1** for. A non-zero `--help` breaks `set -e`,
/// packaging smoke tests and every wrapper that checks a status, and the token itself is internal
/// control flow that reads to a user like an escaped error string. A variant, not a magic string,
/// makes the two outcomes impossible to confuse again.
#[derive(Debug)]
enum Parsed {
    Args(Args),
    Help,
    /// `--version`/`-V`: the name and version on stdout, exit 0. It was UNRECOGNISED, so it fell
    /// into the `unknown argument` arm and exited **1** on stderr — the same defect class as a
    /// failing `--help`, and the first thing a packaging script or a bug report asks a binary.
    /// `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box.
    Version,
}

pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, argv: &[String]) -> ExitCode {
    // ⚠ FIRST — before argv, before the settings load, before the log subscriber, and a long way
    // before anything that can place an order. ONE STOP FLAG, MANY TRIGGERS (`vike_ops::stop`'s
    // module doc is the contract): the stdio `shutdown`/`quit` word raises it, Ctrl-D on a TTY
    // raises it, and SIGTERM/SIGINT raise it too, so `systemctl stop`, a bare `kill` and a container
    // stop all reach the SAME teardown at the bottom of this function.
    //
    // ⚠ **Installing it late is a hole, not a style question**, and the first cut of this change had
    // one: the call sat ~140 lines further down, AFTER the core was mounted. Between the mount
    // returning and the handler installing, the maker is already folding ticks and can already have
    // RESTING ORDERS AT THE VENUE while SIGTERM still carries the OS default disposition — the
    // process dies where it stands, no teardown, no cancel sweep, book abandoned. On the LIVE arm
    // that window is the widest part of startup: per-venue blocking handshakes and instrument
    // pre-fetches.
    //
    // Nothing here needs the subscriber or the settings: the handler stores a bool into an
    // already-allocated flag, which is the only thing legal in a signal context anyway. The one
    // thing it cannot do yet is LOG, so the outcome is carried as DATA and reported by
    // [`log_handler_outcome`] the moment a subscriber exists — the same shape `resolve_settings`
    // already uses for its warnings, and for the same reason.
    let stop = StopSignal::new();
    let handler_outcome = stop::install_handlers(&stop.flag());

    let args = match parse_args(argv) {
        // `--help`: print the usage to STDOUT and exit 0. ⚠ **This daemon's stdout is a PROTOCOL**
        // (a periodic one-line JSON snapshot summary; logs go to the vike-log file/stderr layer),
        // and stdout is still the right stream — the two never coexist. `--help` is answered right
        // here, before the log subscriber, the core, any venue mount or the first snapshot line, so
        // there is no protocol on that stream to corrupt; a reader parsing snapshot lines is by
        // definition reading a daemon that is RUNNING. Putting help on stderr to protect a stream
        // that is not in use yet would make this the one binary whose `--help | less` is blank.
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        // `<name> <version> (<build identity>)` on stdout, exit 0 — the shape every `--version` on
        // the box prints (`git version 2.x`, `cargo 1.x`), and the same one `vike_cli::print_version`
        // emits, so a packaging script reads the first two tokens without knowing anything about
        // this binary. Answered here for exactly the reason `--help` is: before the subscriber, the
        // core and any venue mount. The parenthesised half is the COMMIT this daemon was built from
        // — see `crates/vike-buildinfo/src/lib.rs` for the near-miss on the live recorder that made
        // a bare version number insufficient.
        Ok(Parsed::Version) => {
            println!(
                "{}",
                vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            );
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Args(a)) => a,
        Err(msg) => {
            eprintln!("vike-tradehub: {msg}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    // SETTINGS FIRST — before LOGGING, before the profile, before the core, before any venue mount.
    //
    // Two reasons it must precede `vike_log::init`, in order of weight: the log DIRECTORY
    // (`config.log_dir`) and both log LEVELS (`preferences.log_level` / `preferences.log_file_level`)
    // are settings, so a subscriber built before the load could only ever honour the environment —
    // which is exactly the defect this ordering fixes; and a stale risk-ceiling variable must stop a
    // daemon that signs real orders before it does anything at all. Nothing in the load logs:
    // `resolve_settings` is pure and reports through its `Err` and through `Settings::warnings`,
    // which is precisely why it CAN run before a subscriber exists. Its failures go to stderr, where
    // they were going anyway.
    let booted = match resolve_settings(env, cwd) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vike-tradehub: {e}");
            return ExitCode::FAILURE;
        }
    };
    let settings = &booted.settings;

    // Hold the guards for the whole process — dropping them would stop the non-blocking file writer.
    //
    // THREE layers of this config are settings rather than defaults now, and every one of them is
    // still beaten by its environment variable INSIDE `vike_log::init`. That is not a conflict but
    // the same precedence stated twice: `vike-config` resolved env > file > default, and vike-log
    // re-checks env over whatever it was handed. The two orders AGREE, so passing an
    // already-env-resolved value into a slot env also outranks is idempotent — and a caller that
    // never loads settings at all still gets the environment honoured.
    //
    // `project_dir` is the DEFAULT log directory: `<state root>/logs`, the state directory this
    // daemon already resolves for `alerts.json` (`$VIKE_STATE_ROOT`, else `<project>/settings/state`).
    // It sits BELOW `config.log_dir` — a value a human wrote in a file beats one the program derived
    // for itself — and above vike-log's `<exe_dir>/logs` last resort.
    // `init_with_reload` rather than `init`: the returned handles are the two filters' live
    // reload seam — what lets a `SetSetting` on `preferences.log_level`/`log_file_level` apply to
    // THIS process (REQ-7 v2, `crate::hot_reload`) instead of waiting for the next boot.
    let (_log_guards, log_reload) = vike_log::init_with_reload(vike_log::LogConfig {
        file_prefix: "vike-tradehub".to_string(),
        console_level: settings.preferences.log_level.clone(),
        file_level: settings.preferences.log_file_level.clone(),
        dir: settings.config.log_dir.clone(),
        project_dir: log_dir(),
        ..Default::default()
    });

    // WHICH BINARY IS THIS — first line in the log, before anything it might be blamed for.
    // `Booted::identity_line` is the same string `--version` prints, so "which commit is running on
    // this box" is answerable from the log alone AND from the binary alone, and the two cannot
    // disagree. A release binary was once built on a test clone from a bare repo four commits
    // behind `main` and nearly installed on the live recorder.
    tracing::info!("{}", booted.identity_line);

    // …and the KILL SWITCH's default path takes its project from that SAME one walk.
    //
    // `vike_bridge_core::halt` is a library and may not read `$VIKE_SETTINGS_DIR` for itself
    // (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet that may shrink
    // and never grow), so the override reaches rung 2 — `<project>/settings/state/HALT` — only as a
    // PARAMETER from here. Without it the sentinel's default was decided by the WORKING DIRECTORY
    // even under the override; on the CI box the two happen to name the same project, so the defect was
    // LATENT there rather than live, and `deploy/vike-tradehub.service`'s own comment says the
    // variable is there to make the answer "independent of WorkingDirectory".
    //
    // ⚠ It is `Booted::state_dir` and NOT [`state_dir`]: `$VIKE_STATE_ROOT` relocates this daemon's
    // STATE tree (alerts.json, the telegram ledger, the log home) and has never moved the sentinel.
    // Layering it here would be a second, unasked-for behaviour change to a kill switch — and the
    // shipped units' `ReadWritePaths=` grants `<project>/settings/state`, which is this rung.
    //
    // Placed HERE rather than inside [`resolve_settings`] because it can fail — a declaration that
    // arrives after the path was already resolved changes nothing, and saying so is the point — and
    // this is the first line at which a subscriber exists. Nothing between the boot and here
    // resolves the sentinel: every caller is a venue mount, the paper book, or the advisory below.
    if let Err(e) = vike_bridge_core::halt::declare_project_state_dir(booted.state_dir.clone()) {
        tracing::error!("{e}");
    }

    // …and only NOW can the loader's non-fatal resolutions be emitted. `vike_config` returns them as
    // DATA rather than logging them (it does not depend on `tracing`, because a library that writes
    // to a caller's stderr on its own initiative cannot be used by a binary whose stdout is a
    // protocol — which this daemon's is), so the obligation to emit them is the binary's.
    for line in settings_warning_lines(settings) {
        tracing::warn!("{line}");
    }
    // The signal handler was installed at the very top of `main`, before any of the above could
    // block; this is the first moment there is a subscriber to say so through. Same DATA-not-logging
    // discipline as the settings warnings directly above.
    log_handler_outcome(&handler_outcome);
    // ⚠ WINDOWS ONLY — the stop route for a daemon that has no console to be Ctrl-C'd in
    // (split-plane I13). `install_handlers` above catches console control events, and a background
    // host — a hidden `Start-Process`, a Task Scheduler job at boot, anything that is the Windows
    // analogue of `systemctl start` — has no console for one to be delivered from; stdin is not a
    // TTY there for the same reason it is not one under systemd. Without this the only stop left is
    // `taskkill`, which runs NO teardown: no cancel sweep, no state save, no terminal journal
    // snapshot, the resting book abandoned at the venue. One flag, many triggers — this adds a
    // trigger and nothing else. `docs/ops/tradehub-windows.md` is the operator's page for it.
    //
    // It is armed HERE, and here is load-bearing for the same reason `install_handlers` sits at the
    // top of `main`: this is the earliest point at which the state directory is known (the ONE boot
    // walk resolved it just above, and `vike_ops::stop` may not walk for itself), and it is still a
    // long way above every mount — so there is no window in which the maker is resting orders at a
    // venue while the only stop is a `taskkill`. The path is `Booted::state_dir`, the same rung
    // `declare_project_state_dir` just handed the HALT sentinel and the same one the shipped units'
    // write grant covers, so an operator has ONE directory to know about rather than two.
    #[cfg(windows)]
    match booted.state_dir.as_deref() {
        Some(dir) => {
            match vike_ops::stop::arm_stop_file(
                &dir.join(vike_ops::stop::STOP_FILE_NAME),
                &stop.flag(),
            ) {
                vike_ops::stop::StopFileOutcome::Armed(path) => tracing::info!(
                    stop_file = %path.display(),
                    "background stop armed — creating this file stops the daemon gracefully, the \
                     same teardown Ctrl-C runs (docs/ops/tradehub-windows.md)"
                ),
                // The one case an operator MUST see, and the same reasoning as a failed handler
                // install: a detached daemon whose stop file is not watched can only be `taskkill`ed.
                vike_ops::stop::StopFileOutcome::Failed(e) => tracing::error!(
                    error = %e,
                    "could NOT arm the background stop file — if this daemon is running detached, \
                     the ONLY stop left is a hard kill, which runs no teardown and leaves resting \
                     orders live at the venue; close the book yourself first \
                     (docs/ops/kill-switches.md section C)"
                ),
            }
        }
        // No project, hence no state directory — the same condition that leaves `alerts.json` and
        // the strategy sidecars unwritten. There is nowhere to put a stop file, and inventing a
        // location beside the executable is precisely the mistake the HALT sentinel's default made.
        None => tracing::warn!(
            "no project state directory resolved, so no background stop file is watched — a \
             detached run on this box can only be hard-killed. Set VIKE_SETTINGS_DIR."
        ),
    }
    // Disclose what was actually resolved — the operator otherwise has no way to tell an absent
    // `policy.toml` from one this process failed to find (the two produce identical behaviour,
    // which is the point, and therefore identical silence). On the CI box the walk answered with an
    // unrelated directory and this daemon ran with no policy and NO CREDENTIALS, every venue
    // silently on paper.
    //
    // ⚠ This REPLACES the four-field line that stood here. That line named the two policy ceilings
    // and the log destination, which is the part of the answer this daemon happens to consume — and
    // the failure it exists for is the one where NOTHING was consumed, where the useful fact is the
    // DIRECTORY, whether each file was there at all, and whether a credential store sits beside
    // them. `Booted::boot_lines` (`vike_config::boot_lines`) renders all of it, the same rows
    // `vike-cli config show` prints, from the settings directory this process really loaded from —
    // `vike-boot` hands the renderer its OWN walk's answer, so nothing here can walk a second time.
    // It NEVER opens the credential store — see that module's doc — so no key name and no key value
    // can reach this log.
    for line in &booted.boot_lines {
        tracing::info!("{line}");
    }
    // The LIVE mount below takes the venue-facing projection (`vike_run::MountPolicy`) from this
    // value, while the control-server ceiling rides the `POLICY_MAX_NOTIONAL` OnceLock
    // `resolve_settings` also set — ONE load, two consumers, no chance of the two disagreeing about
    // what this machine's `policy.toml` says.
    let policy = settings.policy.clone();

    // …and the DURABLE anchor for those same ceilings — one JSONL line per start in
    // `<state root>/changes/`, actor origin `boot`.
    //
    // The disclosure emitted directly above is the console/journald copy and it is not a record a
    // week later: `vike_log::DEFAULT_MAX_LOG_FILES` prunes the rolling file, and
    // `deploy/vike-tradehub.service` sets `VIKE_LOG_FILE_LEVEL=warn`, which silences the `info`
    // layer the boot lines ride outright. `vike_model::change_journal`'s module doc carries the
    // measurement from this very daemon's live log file; the anchor survives both.
    //
    // ⚠ **A BRACKET, not a detector.** Nothing in this daemon observes a hand edit of
    // `<project>/settings/policy.toml` — there is no file watcher in the tree and this process does
    // not notice the edit at all. Two consecutive anchors that DISAGREE prove something changed
    // between them, and that is the whole claim; `vike_boot::journal_boot_settings` carries the
    // argument and the rate arithmetic.
    //
    // ⚠ It is [`state_dir`] and NOT `booted.state_dir`: `$VIKE_STATE_ROOT` relocates this daemon's
    // whole STATE tree, and the anchor belongs in the same one as the rolling log ([`log_dir`]),
    // `alerts.json` and the telegram ledger — the shipped units' `ReadWritePaths=` grants exactly
    // that tree. `None` (no override, no project) writes NOTHING rather than inventing a location.
    //
    // Placed HERE rather than in [`resolve_settings`] for the reason `declare_project_state_dir`
    // above is: the write can fail, and this is the first point at which a subscriber exists to say
    // so through. That function runs before one does and returns everything it has to say as data.
    if let Some(Err(e)) = vike_boot::journal_boot_settings(
        state_dir().as_deref(),
        &policy,
        env!("CARGO_PKG_VERSION"),
        now_ms(),
    ) {
        tracing::warn!("the boot anchor was not journalled: {e}");
    }

    let profile = match DaemonProfile::load(&args.config_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("bad profile {}: {e}", args.config_path);
            eprintln!("vike-tradehub: bad profile {}: {e}", args.config_path);
            return ExitCode::FAILURE;
        }
    };
    // Per-mount resolution (split-plane I10): the historical single-mount profile is ONE row
    // (`mount_rows` returns the profile itself), a `[[mounts]]` profile is N — and every row runs
    // the SAME lowering + resolve a single-mount profile always has, so the two spellings cannot
    // disagree about what one mount means.
    let multi = !profile.mounts.is_empty();
    let mut resolved: Vec<ResolvedMount> = Vec::new();
    for (i, row) in profile.mount_rows().into_iter().enumerate() {
        let cfg = row.to_mount_config();
        // The strategy-free projection of that lowering (venue / symbol / interval / seed_cash /
        // the paper fee scalars) — what BOTH generic mount builders take. ONE derivation, so a
        // `[strategy]` mount and the default A-S mount cannot disagree about the mount identity or
        // the fee model.
        let mut spec = row.to_mount_spec();
        // A `[[mounts]]` row mounts under its DERIVED controller id (venue/symbol/interval +
        // strategy identity — `DaemonProfile::derived_controller_id`, duplicates already refused
        // at load). A single-mount profile keeps `None` — the runtime's legacy triple derivation,
        // so an existing deployment's state sidecar / journal attribution keys are untouched.
        if multi {
            spec.controller_id = Some(row.derived_controller_id());
        }
        // WHICH strategy. The A-S maker — absent `[strategy]` OR named
        // `spread_maker`/`gueant_maker` — comes from `DaemonProfile::mounted_maker`, the ONE
        // construction site, which is exactly `vike_run::build_maker(&cfg)`: the very function
        // `build_paper_maker_core_with` / `build_live_maker_core` call. So the default path is the
        // historical A-S path with the `Box::new` moved one frame outward, and the NAMED path is
        // provably the same maker rather than the registry's `SpreadMaker::from_params`, which
        // reads `[strategy.params]` alone and would mount a materially different maker (see
        // `config::AS_MAKER_NAMES`). Every other name resolves through the shared `vike_strategy`
        // registry, the one a backtest profile resolves through.
        //
        // `DaemonProfile::validate` already rejected every unmountable name at LOAD (unknown /
        // simulator-only / resolves-but-cannot-trade) AND every params key the named strategy does
        // not read, so an error here means the two disagreed — worth failing loudly rather than
        // unwrapping.
        let strategy = match row.resolve_strategy(&cfg) {
            Ok(s) => s,
            Err(e) => {
                let at = if multi { format!("mounts[{i}]: ") } else { String::new() };
                tracing::error!("{at}strategy resolve failed: {e}");
                eprintln!("vike-tradehub: {at}strategy resolve failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        resolved.push(ResolvedMount { row, cfg, spec, strategy });
    }
    // The PRIMARY mount — the daemon's historical singular identity (summary token, mode line,
    // seed policy). On a single-mount profile this IS the mount, byte-identically.
    let cfg = resolved[0].cfg.clone();
    let strategy_name = if multi {
        resolved.iter().map(|m| m.row.strategy_name()).collect::<Vec<_>>().join("+")
    } else {
        profile.strategy_name().to_string()
    };
    // ⚠ ECHO WHAT WAS ACTUALLY MOUNTED, not just its name. Logging `strategy = <name>` alone left an
    // operator unable to tell — at startup or afterwards from the log — which numbers the strategy is
    // running, which is half of what made a silently-dropped params key invisible. `validate` makes
    // such a key impossible; this makes the resolved configuration READABLE. For the A-S maker it
    // reports knobs the profile never states (the venue-selected price domain / variance mode),
    // because those are the ones that decide whether it quotes at all. A `[[mounts]]` profile
    // echoes ONE self-addressed line per row (`mounts_wire_row` — the same rendering the
    // StrategyStatus wire rows carry), joined; a single-mount profile keeps the historical
    // one-line echo byte-identically.
    let strategy_params = if multi {
        resolved.iter().map(mounts_wire_params).collect::<Vec<_>>().join(" | ")
    } else {
        profile.effective_params(&cfg)
    };

    // The OPERATOR risk-budget RunProfile (RunProfile wiring, Settings STEP 2 PR 1, Task 2) —
    // INDEPENDENT of `profile` (the `DaemonProfile` above, which owns the venue/token_id/A-S mount
    // shape): `--profile`/`VIKE_RUN_PROFILE` names a `vike_core::RunProfile` this daemon consumes
    // ONLY for its `[risk]` table (`run_profile.mode`/`event_source`/`broker` are ignored here — the
    // `DaemonProfile` above already fully owns that shape). Resolved from the REAL process env (the
    // `VIKE_RECONCILE` idiom), never the `.env` creds map. `Ok(None)` (no explicit path AND no env
    // var) is the untouched default; a resolved-but-broken profile is a loud startup failure, never
    // a silent fall-through to the hardcoded risk defaults.
    let run_profile_vars: HashMap<String, String> = process_env().clone();
    let run_profile = match vike_core::resolve_profile(
        args.profile_path.as_deref().map(Path::new),
        &run_profile_vars,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("bad run profile: {e}");
            eprintln!("vike-tradehub: bad run profile: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ This used to be a blanket `if p.guards != Guards::default() { warn!("[guards] … is set but
    // NOT consumed …") }`. It is gone because the statement is no longer true: the LIVE arm applies
    // `[guards]` and `[sinks]` to its `CoreConfig` through
    // `vike_core::RunProfile::apply_guards_and_sinks`, and discloses — BY KEY — only the two
    // guards and three sinks that genuinely still reach nothing. A blanket warning over a section
    // that is now mostly wired would be the mirror image of the original defect: an operator told
    // their armed guard was ignored.
    //
    // The PAPER arm still consumes `[risk]` alone; its `CoreConfig` is built elsewhere and wiring
    // it is a separate change, so it is not claimed here.
    // The PAPER mount's operator risk budget — `RiskLimits::new()` (byte-identical to the
    // pre-Task-2 daemon) absent a profile, else the profile's `[risk]` table applied via
    // `RunProfile::apply_risk` (see `resolve_paper_risk_limits`'s doc: the `GridSource` it uses is
    // derived from the profile's own `mode`, not chosen at this call site). The LIVE arm below now
    // ALSO consumes this same `run_profile` (via `risk_for_live_venue_mount`, gated on
    // `mode == Mode::Live`) and threads it straight into `vike_mount::make_engine`'s 12-venue path
    // — see that call site below for the mode guard this needed once it stopped being out of scope.
    let paper_risk_limits = match resolve_paper_risk_limits(run_profile.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("run profile risk budget error: {e}");
            eprintln!("vike-tradehub: run profile risk budget error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if run_profile.is_some() {
        tracing::info!(
            max_notional_per_order = ?paper_risk_limits.max_notional_per_order,
            max_total_exposure = ?paper_risk_limits.max_total_exposure,
            max_orders_per_window = ?paper_risk_limits.max_orders_per_window,
            max_leverage = ?paper_risk_limits.max_leverage,
            // The ENFORCED form of `max_leverage` (issue #822) — logged alongside the declared
            // cap so an operator can see the buying-power check actually armed, not just the
            // number they typed.
            im_requirement = ?paper_risk_limits.im_requirement,
            "RunProfile loaded — operator risk budget armed on the PAPER mount"
        );
    }

    // The LIVE master gate (safety gate #1) — `tradehub_live` in `<project>/settings/flags.toml`,
    // still overridden by `VIKE_TRADEHUB_LIVE` (resolved env > file > default, inside the loader).
    // OFF (the default, and the reading of an absent flags file) ⇒ the PAPER mount below,
    // byte-identical to the pre-live daemon: no build_node, no live feed, no creds. ON ⇒ the
    // credential-gated build_node core with the venue's live feed — real orders MAY be placed on any
    // venue whose creds are in the credential store.
    //
    // ⚠ Its `FLAG_REGISTRY` disposition is KEEP, and that argument survives the file layer intact:
    // a real-money arm must stay a deliberate decision. A file is not a weaker decision than an
    // exported variable — it is a REVIEWABLE one, which is the direction of travel — but the reason
    // the flag stays a flag is unchanged, and so is the one-line `warn!` on the arm below.
    let live = settings.flags.tradehub_live;
    // The DISTINCT venues across the mount set, in mount order — one entry for a single-mount
    // profile. It is a LOG-LINE vec and nothing else: the two startup lines below render it as
    // "what did the operator ask for".
    //
    // ⚠ IT USED TO DRIVE THE B11 LOCK CLAIMS TOO, AND THAT WAS THE DEFECT. The lock is per venue
    // ACCOUNT and must cover what actually ARMS; this vec answers a different question, and the
    // measurement below is what it costs to confuse them. The claims moved to
    // [`live_mount_with`]'s safety-gate-#6 block, over `vike_run::armed_live_venues`.
    //
    // ⚠ THIS IS THE PROFILE'S SET, AND IT IS NOT WHAT THE READY BANNER MAY NAME. The two answer
    // different questions and neither contains the other: the profile decides which (venue,
    // symbol) pairs this daemon MOUNTS A STRATEGY ON, while `vike_run::build_node` calls
    // `vike_mount::make_engine_with_legs` straight-line for every `WIRED_MARKETS` row and arms a
    // REAL exec client wherever the credential store answers — credential presence is the whole
    // gate. MEASURED on the CI box, from one startup of the shipped daemon, two lines apart:
    //
    //   live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}
    //   {"kind":"ready","mode":"LIVE (venue=bybit)"}
    //
    // Nine live authenticated exec sessions; the banner named one, and NINE ACCOUNTS SAT BEHIND
    // ONE LOCK, because both were rendered from THIS vec. `mode` is now computed AFTER the mount
    // from the mount's own arming record (see its `let` below the mount arms) and the lock claims
    // are computed BEFORE it from the arming probe. This vec keeps the one job it is genuinely the
    // authority for: the "what did the operator ask for" half of the startup log lines.
    let mut mount_venues: Vec<String> = Vec::new();
    for m in &resolved {
        if !mount_venues.contains(&m.cfg.venue) {
            mount_venues.push(m.cfg.venue.clone());
        }
    }

    // The per-mount wire rows the `StrategyStatus` read verb answers with (split-plane B4 → I10:
    // that Vec was designed for exactly this), captured HERE as SEEDS and completed into rows
    // AFTER the mount. Two reasons it cannot simply be built here: both mount arms below MOVE
    // `resolved`, and each row's `live` is a per-VENUE ARMING FACT that does not exist until the
    // mount has run. See `wire_mounts` below the mount arms.
    let wire_mount_seeds: Vec<WireMountSeed> = resolved
        .iter()
        .map(|m| WireMountSeed {
            // A `[[mounts]]` daemon publishes one SELF-ADDRESSED row per mount (the `params`
            // string opens with the mount's own venue/symbol/interval, since the row struct
            // deliberately carries no addressing fields yet); a single-mount daemon's one row
            // carries the identity block's own two strings — byte-identical to the row
            // `server.rs`'s `Request::StrategyStatus` arm used to DERIVE from the identity when
            // this daemon published no rows at all. Only that row's `live` changes, and it
            // changes from a lie to a fact.
            strategy: if multi { m.row.strategy_name().to_string() } else { strategy_name.clone() },
            params: if multi { mounts_wire_params(m) } else { strategy_params.clone() },
            venue: m.cfg.venue.clone(),
        })
        .collect();

    // Build the mount, uniformly reducing either arm to a live `CoreHandle` (+ the LIVE arm's extra
    // teardown handles). The PAPER arm stays behaviorally byte-identical to the pre-live daemon: the
    // returned handle's `fills` log is simply dropped (main never read it).
    // The B11 live-account lock guards — one per venue this mount ARMS, claimed inside
    // [`live_mount_with`] (after the `data_only` withhold, before `build_node` builds a single exec
    // client) and bound HERE, so the claims span the whole session; the underscore prefix keeps
    // drop-at-end semantics without a read.
    //
    // ⚠ They are no longer claimed in this function, and the move is the fix rather than tidying:
    // the loop that stood here iterated `mount_venues` — the RUN PROFILE's venues — which is not
    // the set that arms. See the `mount_venues` comment above for the measured the CI box startup
    // (nine live venues, one lock) and [`live_mount_with`]'s safety-gate-#6 block for the two
    // properties this site could not have: the ARMED set, and the post-withhold credential map.
    let mut _live_account_locks: Vec<vike_ops::live_lock::LiveLock> = Vec::new();
    // The third element is the mount's ARMING RECORD — `vike_run::build_node`'s own `live_venues`
    // set, into which `vike_mount::make_engine_with_legs` inserts a venue exactly when it
    // constructed a REAL exec client for it. It is the only value in this function that answers
    // "which venues can place a real order in this process", and everything that REPORTS
    // paper-vs-live is derived from it below. The PAPER arm arms nothing, so its record is empty
    // by construction rather than by assertion.
    let (handle, live_extras, live_venues): (
        CoreHandle,
        Option<LiveTeardown>,
        std::collections::HashSet<String>,
    ) = if live {
        // Safety gate #5 (profile side): reject a (venue, symbol) that isn't a live-wired pair before
        // any core spawns.
        if let Err(e) = profile.validate_for_live() {
            tracing::error!("the live gate is ON but the profile is not live-wireable: {e}");
            eprintln!("vike-tradehub: {e}");
            return ExitCode::FAILURE;
        }
        // Safety gate #6's ANCHOR — the directory the live-account sentinels are claimed in. The
        // claims themselves are made inside [`live_mount_with`], over the ARMED set and the
        // post-withhold credential map (neither of which exists here); this site resolves the
        // directory and refuses when there is none.
        let Some(lock_dir) = booted.state_dir.as_deref() else {
            // No project resolved ⇒ no state dir to anchor the claim — and no credential store
            // either, so a live mount here was never going to trade. Refusing is strictly clearer
            // than failing later at the credential gate with the live flag on.
            tracing::error!(
                "refusing LIVE mount: no project settings dir resolved (the live-account lock and \
                 the credential store both live under <project>/settings)"
            );
            eprintln!("vike-tradehub: refusing LIVE mount: no project settings dir resolved");
            return ExitCode::FAILURE;
        };
        tracing::warn!(
            venue = %mount_venues.join("+"),
            token = %cfg.token_id,
            interval = %cfg.interval,
            mounts = resolved.len(),
            strategy = %strategy_name,
            strategy_params = %strategy_params,
            "the live gate is ON (flags.toml `tradehub_live`, or VIKE_TRADEHUB_LIVE) — mounting the LIVE \
             build_node core; real orders MAY be placed on any venue whose creds are in the credential \
             store (Hyperliquid stays TESTNET unless HYPERLIQUID_MAINNET=1)"
        );
        // `live_mount` threads this straight into `vike_mount::make_engine`'s 12-venue path, which
        // hardcodes `GridSource::VenueFetched` for every venue and never sees `run_profile`'s own
        // `mode` at all. A `backtest`/`paper`-mode profile may LEGALLY set the venue-owned
        // instrument grid fields (its own mode implies `GridSource::NoGridFetched`), which would
        // make EVERY venue's merge inside `make_engine` reject those fields — degrading (narrowly,
        // via `merge_operator_budget`'s fallback, but still) rather than failing loud at the one
        // place that actually knows this profile's mode. Guard it here instead.
        let live_risk_profile = match run_profile.as_ref().map(|p| p.risk_for_live_venue_mount()) {
            Some(Ok(risk)) => Some(risk.clone()),
            Some(Err(e)) => {
                tracing::error!("VIKE_TRADEHUB_LIVE=1 but the run profile is not live-mode: {e}");
                eprintln!("vike-tradehub: run profile is not live-mode: {e}");
                return ExitCode::FAILURE;
            }
            None => None,
        };
        match live_mount(
            resolved,
            live_risk_profile,
            run_profile.clone(),
            &policy,
            settings.flags,
            lock_dir,
        ) {
            Ok((handle, teardown, live_venues, locks)) => {
                _live_account_locks = locks;
                (handle, Some(teardown), live_venues)
            }
            Err(e) => {
                tracing::error!("live mount failed: {e}");
                eprintln!("vike-tradehub: live mount failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        // The data-only declaration is a LIVE-mount fact (it tells `live_mount` which venue's
        // credentials to withhold from exec) — the paper daemon mounts no venue feed and exec is
        // already paper, so a declared key here would configure NOTHING while reading as real,
        // the declared-but-unread failure the settings rule exists for. Refuse, like the removed
        // env variables do, rather than warn: an operator who wrote it believes it decides
        // something about THIS run.
        if resolved.iter().any(|m| m.row.data_only_effective()) {
            let msg = "the profile declares `data_only = true` but the live gate is OFF — the \
                       PAPER daemon mounts no venue feed and exec is already paper, so the key \
                       configures nothing here. Drop it, or arm the live gate (`tradehub_live = \
                       true` in flags.toml, or VIKE_TRADEHUB_LIVE=1) for the data-only live mount";
            tracing::error!("{msg}");
            eprintln!("vike-tradehub: {msg}");
            return ExitCode::FAILURE;
        }
        // ⚠ NO `qty` AND NO `resolution_ts` FIELD HERE — their absence IS the fix, not an omission.
        //
        // Both are A-S MAKER knobs read off `MakerMountConfig`, and this daemon no longer mounts
        // only that maker. On a `[strategy] name = "grid"` mount NOTHING reads either one, so
        // `qty = cfg.qty` printed the A-S default beside `strategy=grid` while the grid's real size
        // sat in `strategy_params`. MEASURED: a grid profile with `[strategy.params] size = 2.0`
        // logged `… qty=20.0 strategy=grid strategy_params=… size=2 …`. `qty` is the field an
        // operator scans for ORDER SIZE, so that line made a positive claim about a knob that
        // configures nothing — the same class as echoing the raw `[strategy.params]` table, which
        // `DaemonProfile::effective_params`' own doc records this daemon having done and undone.
        //
        // Nothing is lost where the claim IS true: on a maker mount `effective_params` reports
        // `qty=…` and `resolution_ts=…` INSIDE `strategy_params` (its A-S branch), so both still
        // print — from the one place that knows which strategy was mounted, instead of from a second
        // copy that cannot. That is also why the cure is deletion rather than a relabel: a second
        // field would be a second authority for the same number, and the two can then disagree.
        // `crates/vike-tradehub/tests/daemon/any_strategy_mount.rs`'s
        // `the_mount_line_states_order_size_only_where_it_is_true` pins both directions.
        tracing::info!(
            venue = %mount_venues.join("+"),
            token = %cfg.token_id,
            interval = %cfg.interval,
            mounts = resolved.len(),
            strategy = %strategy_name,
            strategy_params = %strategy_params,
            summary_ms = profile.daemon.summary_ms,
            "mounting the PAPER strategy (headless daemon; no live feed — set `tradehub_live = true` in flags.toml, or VIKE_TRADEHUB_LIVE=1, for the live build_node core)"
        );
        // ⚠ SAY OUT LOUD WHICH SENTINEL THIS MOUNT IS WATCHING.
        //
        // This line said the OPPOSITE until the paper mount was armed, and the reversal is the
        // point of the comment. It used to read "the HALT sentinel reaches NOTHING on a PAPER
        // mount", argued from a grep that returned nothing for `halt` anywhere under
        // `crates/vike-paper/src/`, and every word of it was true when it was written. It became
        // false in the commit that armed `crates/vike-run/src/lib.rs`'s `paper_client_for` — and a
        // startup advisory that survives the fix it describes is WORSE than no advisory, because an
        // operator reads a `warn!` in today's journal as a statement about today's binary. So this
        // is the fix and the tombstone in one.
        //
        // What was always wrong, and is what this line is actually for, is the SILENCE. A PAPER
        // mount builds no `ExecActor`, so nothing calls `halt::halt_path_from_env()` at spawn and
        // nothing emits the arming report `docs/ops/kill-switches.md` section C tells an operator to
        // grep for. An absent report is ALSO what an older binary and a not-yet-mounted venue look
        // like, so the three were indistinguishable at exactly the moment the difference matters —
        // the shape `crates/vike-bridge-core/src/halt.rs` exists to refuse: a kill switch that is
        // not armed looks exactly like one that is, right up until it is needed.
        //
        // It NAMES THE PATH now, and that reversal is deliberate too. The old comment refused to
        // print one on the grounds that "a printed path reads as a working target rather than an
        // inert one" — correct while the mount was inert, and exactly backwards now: the path IS
        // the working target, it is the file an operator has to `touch`, and the resolution is a
        // three-rung precedence (`VIKE_HALT_FILE` > `<project>/settings/state/HALT` > `<exe_dir>`)
        // that they cannot compute from the unit file alone. `halt_path_from_env` is the same
        // memoized resolver the mount itself uses (via `vike_mount::halt`), so this cannot print a
        // different answer from the one being enforced, and it adds NO new `env::var` call site —
        // `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is untouched.
        //
        // `warn!`, not `info!`: an operator scanning for what stops trading must find this without
        // knowing to look for it.
        //
        // Bound to a NAMED const so `crates/vike-ops/tests/kill_switch_gate.rs` can key its row on
        // an identifier instead of on this prose: the wording will be improved, and a gate keyed on
        // the message text would either break on every reword or (worse) be quietly loosened until
        // deleting the line stopped reddening it. Deleting `PAPER_MOUNT_HALT_ADVISORY` is what must
        // fail the gate.
        // ⚠ The tail used to read "This mount emits no ExecActor arming report … so this line is the
        // report", and that inference was FALSE — the second false claim this one advisory has
        // carried. No `ExecActor` is built here, true; but the report is emitted by
        // `halt_path_from_env`'s own one-per-process resolution, and the `halt_sentinel` field below
        // IS a call to it. So the `NOT ARMABLE` error reaches a paper node too, which is the half an
        // operator rehearsing the switch is told to check for.
        // ⚠ That last sentence follows from the CALL GRAPH, not from a measurement — and the draft
        // that replaced the false claim above carried a false citation of its own, "MEASURED
        // read-only on the CI box's PAPER tradehub". the CI box's journal does not say that: it holds exactly
        // one `vike_bridge_core::halt` line, from the LIVE bybit mount's pid, while eleven distinct
        // pids logged a paper mount and none emitted one. Their silence is not counter-evidence
        // (they predate this advisory) — it is why the argument may not be a journal grep. The third
        // false claim on one advisory would have been a citation, which is the kind a reader trusts
        // most.
        const PAPER_MOUNT_HALT_ADVISORY: &str =
            "HALT kill switch is ARMED on this PAPER mount: `touch` the sentinel below and every \
             order that OPENS risk is refused (a reduce_only submit still passes, so you can always \
             flatten). This mount builds no venue adapter, so nothing else here would name the file \
             — the halt_sentinel field is that name, and resolving it emits the usual \
             vike_bridge_core::halt arming report beside this line \
             (docs/ops/kill-switches.md section C).";
        tracing::warn!(
            halt_sentinel = %vike_bridge_core::halt::halt_path_from_env().display(),
            "{PAPER_MOUNT_HALT_ADVISORY}"
        );
        // Build + spawn the paper maker core (the PRODUCTION runtime) — the SAME mount the offline test
        // + the polymarket_maker_paper bin use, plus the resolved operator risk budget (Task 2) armed
        // via `PaperMountOpts::risk_limits` (audit F12 collapsed the former `_with_risk_limits` twin
        // into the one options variant). `paper_risk_limits` is `RiskLimits::new()` absent a profile —
        // the `PaperMountOpts::default()` value — so this call is BYTE-IDENTICAL to
        // `vike_run::build_paper_maker_core(&cfg)` in that case; see `resolve_paper_risk_limits`'s
        // doc. Feature-free: no live feed, no creds.
        // `oco_cancel_sibling_on_dead_exit` rides the SAME flag the live arm reads, so a paper
        // rehearsal of a bracket cannot disagree with the live run about what happens when a
        // released exit dies unfilled. Off (the default) ⇒ this call is still byte-identical to
        // `build_paper_maker_core(&cfg)` with the resolved risk budget.
        // `cancel_orders_on_shutdown` rides the same flag as the live arm for the same reason: a
        // paper rehearsal is where an operator should discover what their stop does to the book,
        // and it is the ONLY place they can discover it without pulling real quotes.
        //
        // ⚠ The call is `build_paper_strategy_core_with(strategy, &spec, …)`, not the maker-shaped
        // `build_paper_maker_core_with(&cfg, …)` it used to be. That is not a behaviour change on
        // the default path: `build_paper_maker_core_with(cfg, opts)` IS
        // `build_paper_strategy_core_with(Box::new(build_maker(cfg)), &cfg.mount_spec(), opts)`, and
        // with no `[strategy]` table `strategy`/`spec` above are exactly those two expressions.
        let opts = vike_run::PaperMountOpts {
            risk_limits: paper_risk_limits,
            oco_cancel_sibling_on_dead_exit: settings.flags.oco_cancel_sibling_on_dead_exit,
            cancel_orders_on_shutdown: settings.flags.cancel_orders_on_shutdown,
            // Runtime mount/unmount (split-plane B5): the SAME resolver the live arm injects,
            // so a paper rehearsal — single-mount OR the `[[mounts]]` multi (I10) — answers
            // `MountStrategy` the way the live node would.
            strategy_factory: Some(crate::mount_factory::strategy_factory()),
            // Durable strategy state + runtime-mount topology (B5 residual closed): the same
            // directory the live arm passes, so a rehearsal's runtime mounts survive a restart
            // exactly the way the live node's do. `None` (no project) disarms both, as before.
            state_dir: strategy_state_dir(),
            ..Default::default()
        };
        let handle = if multi {
            // The `[[mounts]]` rehearsal (split-plane I10): N strategies on ONE paper core — one
            // engine per distinct venue, one single-symbol book per distinct `(venue, symbol)`
            // (`vike_run::build_paper_multi_strategy_core_with`'s own doc carries the layout
            // argument). Same `opts` as the single-mount call, so a multi rehearsal arms the same
            // risk budget, halt sentinel and shutdown flags a single one would.
            vike_run::build_paper_multi_strategy_core_with(
                resolved
                    .into_iter()
                    .map(|m| vike_run::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
                    .collect(),
                opts,
            )
            .handle
        } else {
            let m0 = resolved.remove(0);
            vike_run::build_paper_strategy_core_with(m0.strategy, &m0.spec, opts).handle
        };
        // Resurrect RUNTIME strategy mounts (B5 residual closed): replay the topology sidecar
        // through the SAME lossless command lane a wire MountStrategy is lowered into — after
        // the core spawned, and (trivially here: a paper mount wires no feeds) before any market
        // message can fold, per `resurrect_runtime_mounts`'s ordering contract.
        if let Some(dir) = strategy_state_dir() {
            let outcome =
                crate::mount_factory::resurrect_runtime_mounts(&dir, |c| handle.send_command(c));
            if outcome.sent + outcome.skipped > 0 {
                tracing::info!(
                    sent = outcome.sent,
                    skipped = outcome.skipped,
                    "runtime-mount resurrect replayed the topology sidecar"
                );
            }
        }
        // No live client is constructed on this arm — no venue, no credential, no exception — so
        // the arming record is EMPTY, and every paper-vs-live report derived from it below reads
        // paper without a second boolean having to agree with this one.
        (handle, None, std::collections::HashSet::new())
    };

    // ⚠ THE PAPER-VS-LIVE BANNER, and it names the set that is ACTUALLY ARMED — never the set the
    // profile mounts. See the `mount_venues` comment far above for the measured the CI box startup this
    // replaces: nine armed venues under a banner reading `LIVE (venue=bybit)`, because the string
    // was built from the profile. `live_venues` is `build_node`'s own record, so the banner can no
    // longer disagree with the mount that produced it.
    //
    // SORTED, because a `HashSet` iterates in an order that changes between runs of the same
    // binary — an operator diffing two startups must not see a reordering and read it as a change.
    //
    // The PAPER arm is untouched: the string is still exactly `"PAPER"`.
    //
    // The rendering itself is [`ready_mode_line`] — a pure function of exactly these two values, so
    // that what the banner says can be TESTED rather than read off this call site, and so that the
    // profile's mount set is not merely unused here but structurally out of reach of the renderer.
    let mode = ready_mode_line(live, &live_venues);

    // ...and the SAME arming record decides each wire row's `live` — ONE value feeding both
    // reports, so the banner and the `StrategyStatus` verb cannot disagree about one startup.
    let wire_mounts = wire_mount_rows(wire_mount_seeds, &live_venues);

    // Unified-journaling #2 (the live-tearsheet SINK): when the WAL is enabled, materialize its
    // fill/order records off-path into the Tier-2 `kind=exec_fill`/`kind=exec_order` store series
    // `vike-report` reads (`fills_from_store`). DataFusion-gated behind the `materialize` feature so a
    // default daemon build stays DataFusion-free; `None` (byte-identical to the pre-materializer
    // daemon) unless the feature is built AND the WAL is on. Held to the end of `run` so its Drop does
    // a FINAL WAL drain AFTER the core join below flushes the last records. Best-effort — a store-open
    // ⚠ THE FIRST POINT WHERE A LATCHED SIGNAL CAN BE ACTED ON. `install_handlers` is the first
    // statement of `main`, so a SIGTERM arriving during startup is RECORDED from the very beginning —
    // but recording is not acting, and everything above this line is the widest part of a LIVE
    // startup: per-venue blocking handshakes, instrument pre-fetches, ctrader/ibkr synchronous
    // connects. Without this check the daemon would finish mounting every venue, wire the feeds and
    // let the maker start quoting before `stop.wait()` far below ever looked at the flag — and the
    // unit's `TimeoutStopSec=` is counted from the signal, not from here, so a slow mount could be
    // SIGKILLed with MORE venue state alive than the pre-handler build would have had at the same
    // instant.
    //
    // Falling through rather than exiting here is deliberate: the mount above may already hold
    // resting orders at a venue, and the cancel sweep lives in the teardown far below. This is the
    // earliest line at which that is true, which is why the check sits here and not higher.
    //
    // What this does NOT do is skip the wiring between here and `stop.wait()` — the observe server,
    // the optional Telegram and alert mounts, the summary and stdio threads. Those are local and
    // cheap, unlike the venue mount above, and `StopSignal::wait` tests the flag BEFORE it sleeps, so
    // once they are up the daemon goes straight into teardown without ever quoting.
    if stop.is_requested() {
        tracing::warn!(
            "stop requested during startup — mount completed, going straight to teardown without trading"
        );
    }

    // failure logs and skips, never failing the mount. Both mount arms journal (the paper maker's
    // synthesized fills are journaled too), so a headless PAPER daemon produces a tearsheet as well.
    let _materializer = maybe_spawn_materializer();

    // Optional authenticated, READ-ONLY observe server (PR-11). OFF by default — no address ⇒ NOT
    // started, so this is byte-identical to the pure-stdio PR-9 daemon. When set, a laptop watches
    // this live node's snapshots over an SSH tunnel. It NEVER touches the core fold: the publisher
    // reads only the arc-swap snapshot cell (the p99 latency gate is the merge condition). Held for
    // the bounded teardown below.
    //
    // BOTH inputs are now settings — `tradehub_addr` in `config.toml` and `tradehub_control` in
    // `flags.toml`, each still overridden by its variable. They arrive as PARAMETERS rather than
    // being read inside the function so this call site is the one place that decides whether a
    // remote order-write surface opens, and so that decision is visible in a diff.
    // WHO this daemon is, stamped into every published frame (split-plane B3) so an observer
    // holding several backends can label them and tell paper from live. A struct literal over
    // already-resolved locals — no new reads, no logic (the wiring-only rule).
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: Path::new(&args.config_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tradehub".to_string()),
        strategy: strategy_name.clone(),
        params: strategy_params.clone(),
        live,
        build: vike_buildinfo::summary(),
    };
    // The HOT-APPLY seam (REQ-7 v2): the server end goes into `SettingsShowSource.hot` below;
    // the ticker end is moved onto the summary thread, which drains it on every wake — so every
    // runtime apply executes on that ONE existing off-fold thread, never on a connection thread.
    let (hot_handle, hot_ticker) = hot_reload::hot_apply_channel();
    let observe = start_observe_server(
        &handle,
        identity,
        wire_mounts,
        settings.config.tradehub_addr.as_deref(),
        settings.flags.tradehub_control,
        settings.flags.tradehub_allow_public_bind,
        // The REQ-7 settings source, built HERE because this binary owns all three of its facts:
        // the settings DIRECTORY the one boot walk resolved (so the wire can never describe a
        // different project than the one that is running), this binary's ONE startup env sweep
        // (so the wire's rows resolve the same layers the daemon's own settings load did), and
        // the hot-apply seam — whose `Some` is what makes `restart_required: false` REACHABLE at
        // all: the write arm consults it only for a key the classification calls hot-safe, and
        // waits for the summary tick's verdict before answering.
        server::SettingsShowSource {
            settings_dir: booted.settings_dir.clone(),
            env: process_env().clone(),
            hot: Some(hot_handle),
        },
        // The REQ-2 advertisement: the datahub dial address this backend FRONTS, stamped into
        // every Welcome so a client configures ONE address. A settings key like the source above —
        // the parameter shape keeps this call site the one place the node surface is decided.
        settings.config.datahub_advertise_addr.as_deref(),
    );

    // Optional TELEGRAM control channel (PR item 4) — OFF unless all FOUR gates are open, in which
    // case an allowlisted chat can place REAL orders behind a preview + `/confirm`. `None` means
    // NOTHING was constructed (no thread, no bot token in memory, no network call), so the default
    // daemon is byte-identical to the pre-Telegram one. Its teardown rides the BOUNDED task fan-out
    // below, not the sequential tail: an in-flight `getUpdates` long-poll can take up to
    // `LONG_POLL_SECS` to return, and a control channel must never be able to outlive the shutdown
    // deadline.
    //
    // Absent the `telegram` FEATURE this statement does not exist at all: no call, no binding, and
    // `crate::telegram` is not even compiled — a remote order-origination path that no
    // runtime misconfiguration can reach because it is not in the binary.
    #[cfg(feature = "telegram")]
    let telegram = maybe_start_telegram(
        &handle,
        settings.flags.tradehub_control,
        settings.flags.telegram_control,
    );

    // The HEADLESS alerting mount — the whole point of running alerts on a daemon rather than in a
    // GUI that dies with its window. `None` unless a rules file with ≥1 ENABLED rule exists, and
    // `None` means NOTHING is built (no engine, no sink, no webhook target resolved), so the
    // summary thread below is byte-identical to the pre-alerting daemon. `mut` because the engine
    // carries the per-rule edge/latch/cooldown state and is MUTATED (off-fold) on each tick after
    // the move into that thread.
    let mut alerts = maybe_mount_alerts();

    // Periodic snapshot SUMMARY to stdout (the protocol/result surface). A LOSSY reader off the
    // arc-swap cell (`snapshot_cell`) — it never touches the core fold; its own thread so the stdin
    // control loop can block on the main thread. Sleeps in small increments so `stop` is prompt.
    // It is ALSO the alerting engine's one input tick (see `maybe_mount_alerts`): the engine is
    // MOVED onto this thread, so every rule evaluation happens off-fold, on the same lossy read the
    // summary line already does — never inside the core.
    let summary_stop = Arc::new(AtomicBool::new(false));
    let summary_handle = {
        let cell = handle.snapshot_cell();
        let token = cfg.token_id.clone();
        let interval = profile.summary_interval();
        let stop = Arc::clone(&summary_stop);
        // The tick side of the hot-apply seam (REQ-7 v2): the applier holds the log-filter
        // reload handles plus the SAME boot facts the `SettingsShowSource` holds (directory +
        // the one env sweep), so a hot apply re-resolves exactly the layers a restart would.
        let hot_applier = hot_reload::LogLevelApplier {
            handles: log_reload,
            settings_dir: booted.settings_dir.clone(),
            env: process_env().clone(),
        };
        std::thread::Builder::new()
            .name("vt-tradehub-summary".into())
            .spawn(move || {
                let step = interval.min(Duration::from_millis(100)).max(Duration::from_millis(1));
                let mut waited = Duration::ZERO;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(step);
                    // Hot applies drain on EVERY wake (≤100 ms), not only on summary prints: the
                    // server thread is holding a `SetSetting` peer against HOT_APPLY_DEADLINE,
                    // and a 5 s summary cadence must not be what times an apply out. Same
                    // thread, same tick loop — still never the fold, never the wire thread.
                    hot_ticker.drain(|key| hot_applier.apply(key));
                    waited += step;
                    if waited < interval {
                        continue;
                    }
                    waited = Duration::ZERO;
                    let snap = cell.load_full();
                    println!("{}", summary_line(&snap, &token));
                    let _ = std::io::stdout().flush();
                    // Alerting rides the SAME tick, strictly AFTER the protocol line so stdout
                    // ordering is untouched. Absent a mount this `if let` is skipped entirely.
                    if let Some(m) = alerts.as_mut() {
                        m.on_snapshot(&snap, now_ms());
                    }
                }
            })
            .expect("spawn vt-tradehub-summary thread")
    };

    // stdio control on its OWN thread — it used to own the MAIN thread and block it inside
    // `stdin.lock().lines()`. That is not tidiness: a POSIX signal does not break a blocking read
    // out of the kernel (signal-hook registers with `SA_RESTART`, and std retries `EINTR`
    // regardless), so a main thread parked in a stdin read would never look at the stop flag. The
    // thread that must observe the flag has to be free to look.
    spawn_stdin_control(&handle, cfg.token_id.clone(), mode.clone(), stop.flag());

    // …and this is where a headless daemon spends its life. It replaces `loop { thread::park(); }`,
    // under which EVERYTHING below this line — the resting-order cancel sweep, the strategy-state
    // save, the terminal journal snapshot, the feed quiesce, the bounded core join — was written,
    // correct, tested, and unreachable from the only stop a systemd box has. A non-tty EOF still
    // does NOT stop the daemon: the control thread logs and exits, this wait continues, and the
    // daemon keeps trading headless exactly as it did under the park.
    stop.wait();

    // The teardown budget — the profile's `[daemon] shutdown_deadline_ms`, which the shipped unit's
    // `TimeoutStopSec=` is sized above. Resolved BEFORE the claim below because the losing arm needs
    // the same number: a loser that waited on a different budget than the winner runs on is a second
    // number to keep in step, and the unit was only ever sized against one.
    let deadline = profile.shutdown_deadline();

    // Claim the teardown — exactly once, however many triggers arrived (a typed `shutdown` and a
    // `systemctl stop` genuinely can land together). One claim site today, so this cannot lose; it
    // is here so a future second stop route cannot quietly become a second teardown, i.e. a second
    // cancel sweep against a book that is already closing. `vike_ops::stop`'s tests prove the claim
    // is exactly-once under concurrency.
    if !stop.begin_teardown() {
        // ⚠ It must WAIT here, and it must not return early. Returning from `main` TERMINATES THE
        // PROCESS — so a guard whose job is to stop a second teardown would instead be killing the
        // FIRST one at an arbitrary instruction: a cancel sweep abandoned halfway, with some of the
        // book cancelled and the rest still resting at the venue. That is worse than the duplicate
        // it prevents. Bounded by the same deadline the winner's own teardown runs under, because a
        // wedged winner must not hold the process open past `TimeoutStopSec=`, where SIGKILL is
        // waiting regardless.
        tracing::warn!("teardown already claimed — waiting for it, not running it twice");
        if !stop.await_teardown(deadline) {
            tracing::error!(
                ?deadline,
                "the running teardown outran its own budget — exiting anyway, because SIGKILL is \
                 already due; resting orders may not all have been cancelled"
            );
        }
        return ExitCode::SUCCESS;
    }

    // Bounded, graceful teardown — the EXACT vike-app window-close primitive. There are no venue
    // feeds to fan out on a paper mount, so `tasks` is empty and `shutdown_and_join` is the
    // sequential tail, hard-capped by the profile's deadline so a wedged join can never hang exit.
    // Raise the summary thread's stop flag HERE — as early as possible, so it has the whole
    // teardown to notice — but do NOT join it here. The join is a BOUNDED TASK below.
    //
    // ⚠ It used to be `summary_stop.store(true); let _ = summary_handle.join();` on this line, and
    // the claim that made that safe — "it sleeps in <=100 ms slices, so it is bounded by that" —
    // was FALSE. The same thread body also runs `AlertMount::on_snapshot` INLINE, whose production
    // sink is a `WebhookSink<UreqTransport>` built with `timeout_global(10s)`
    // (`crates/vike-alerting/src/delivery.rs`'s `UreqTransport::new`). A stop landing while an
    // alert is being delivered therefore paid up to 10 s per sink, on a join with NO timeout,
    // entirely OUTSIDE `run_with_deadline` — against a `TimeoutStopSec=10` in both shipped units.
    // One in-flight alert webhook was enough to lose the SIGKILL race, and
    // `the_default_shutdown_deadline_fits_inside_the_units_stop_timeout` was comparing two numbers
    // while excluding the step that blew the sum.
    summary_stop.store(true, Ordering::Relaxed);
    // Stop the observe publisher: its poll thread ends and every subscriber mailbox closes, so the
    // per-connection writer threads wake and exit. The accept-loop thread is DETACHED (like
    // vike-datahub) and reaped at process exit — a client parked in a full-buffer `write_all` is that
    // connection's own thread, isolated from this teardown.
    if let Some(p) = &observe {
        p.shutdown();
    }
    // In the LIVE arm, quiesce the live-event forwarder BEFORE the core shutdown (load-bearing
    // ordering — a live user-data pump can otherwise wedge the core join; see `vike_run::node`'s
    // forwarder teardown-safety note), and fan the venue-feed teardown out as a bounded task (mirrors
    // vike-app's `feed_tasks`: `|mut f| Box::new(move || f.shutdown())`). The PAPER arm has NO feeds,
    // so `tasks` is empty and `shutdown_and_join` is the sole sequential tail — byte-identical to the
    // pre-live daemon.
    // The reconcile driver (LIVE + `VIKE_RECONCILE=1` only; `None` on the paper mount AND on any live
    // run with the flag unset) rides the SEQUENTIAL tail below, not a parallel task.
    //
    // ONE bounded task PER venue feed (split-plane I10): `run_with_deadline` fans the tasks out as
    // parallel threads and joins them ALL before the sequential tail, so every mounted venue's
    // feed quiesces inside the ONE `[daemon] shutdown_deadline_ms` budget and a wedged venue
    // cannot serialize its siblings. A single-mount daemon builds exactly one task — the pre-I10
    // shape. `post_feeds` (the recorder's bounded flush) moves into the sequential TAIL, which
    // starts only after every feed task has joined — the same feeds → recorder → core order as
    // before, now stated by `run_with_deadline`'s own task/tail contract instead of by one
    // closure's statement order.
    //
    // `mut` is unconditional now: the summary-thread join below is pushed on every build, feature
    // or not. (It used to be needed only by the Telegram push and carried a `cfg_attr` allow.)
    let (mut tasks, post_feeds, recon_driver): (
        Vec<Box<dyn FnOnce() + Send + 'static>>,
        PostFeeds,
        _,
    ) = match live_extras {
        Some(LiveTeardown { feeds, forwarder_stop, post_feeds, recon_driver }) => {
            forwarder_stop.store(true, Ordering::Relaxed);
            let tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = feeds
                .into_iter()
                .map(|mut f| Box::new(move || f.shutdown()) as Box<dyn FnOnce() + Send + 'static>)
                .collect();
            (tasks, post_feeds, recon_driver)
        }
        None => (Vec::new(), None, None),
    };
    // Stop+join the Telegram poller as a BOUNDED task: its thread may be parked in a `getUpdates`
    // long-poll for up to `LONG_POLL_SECS`, so putting it in the sequential tail could add that to
    // every shutdown. As a task it is hard-capped by the same deadline everything else is. Absent
    // the mount this is a no-op (byte-identical to the pre-Telegram teardown), and absent the
    // `telegram` FEATURE the arm is not compiled at all (there is no `telegram` binding to consume).
    #[cfg(feature = "telegram")]
    if let Some(t) = telegram {
        tasks.push(Box::new(move || t.shutdown()));
    }
    // Join the summary thread as a BOUNDED task, for exactly the reason the Telegram poller is one:
    // it can be parked in a blocking network call it did not choose the length of. Its flag was
    // raised above, so in the ordinary case this task completes immediately; when an alert webhook
    // is mid-POST it is hard-capped by the same deadline as everything else instead of adding its
    // sink's 10 s to a 10 s `TimeoutStopSec=`. As a task it is also COUNTED, so a stop it does
    // delay is reported as `1 task(s) still in flight` rather than as the `0` that made this class
    // of straggler undiagnosable.
    tasks.push(Box::new(move || {
        let _ = summary_handle.join();
    }));
    let join_core = move || {
        // feeds → recorder → core: the recorder's bounded flush runs FIRST in this tail, which
        // `run_with_deadline` starts only after every feed task above has joined — so no in-flight
        // tick is dropped, exactly as when the flush rode inside the one feed task.
        if let Some(f) = post_feeds {
            f();
        }
        // Stop the reconcile driver BEFORE the core join (vike-app's teardown order): it holds only a
        // WEAK core-ingest sender, so it can never wedge the core join, but joining it here bounds its
        // `vt-core-recon` thread to the daemon's lifetime. `None` on the paper mount AND on any live run
        // with `VIKE_RECONCILE` unset — the `if let` is then skipped, byte-identical to the old tail.
        if let Some(driver) = recon_driver {
            driver.shutdown();
        }
        handle.shutdown_and_join();
    };
    let teardown_started = std::time::Instant::now();
    let outcome = run_with_deadline(tasks, Box::new(join_core), deadline);
    let teardown_took = teardown_started.elapsed();
    // Release any loser of the claim above (`vike_ops::stop`'s module doc). Announced on BOTH
    // outcomes: a hard-capped teardown has finished WAITING, which is the only thing a loser can act
    // on, and leaving it parked past this point would add one bound to another.
    stop.finish_teardown();
    match outcome {
        ShutdownOutcome::Graceful => {
            tracing::info!(
                elapsed_ms = teardown_took.as_millis() as u64,
                "vike-tradehub shut down gracefully"
            );
            ExitCode::SUCCESS
        }
        // The orchestration thread died without signalling — a panic in the teardown itself. It
        // returns at once rather than at the deadline, so it must NOT be reported as a hard cap:
        // that wording named a budget nothing had spent. Still exit 0 (an operator stop is not a
        // crash), but say plainly that the teardown did not complete.
        ShutdownOutcome::Aborted { stage } => {
            tracing::error!(
                elapsed_ms = teardown_took.as_millis() as u64,
                stage = ?stage,
                "vike-tradehub teardown ABORTED after {teardown_took:?} — the shutdown \
                 orchestration panicked while {}; flushes past that point did NOT run",
                stage.describe()
            );
            ExitCode::SUCCESS
        }
        ShutdownOutcome::HardCapped { still_running, stage } => {
            // A deliberate stop that outran its deadline — surface it, but still exit 0 so a systemd
            // `Restart=on-failure` unit does not treat an operator stop as a crash.
            //
            // ⚠ The message names the STAGE and the ELAPSED time, not just a count and the budget.
            // It used to read "hard-capped at the 10s deadline; 0 task(s) still in flight", which is
            // self-contradictory: `still_running` counts only the PARALLEL tasks, so it is 0 for the
            // whole of the tail. A live stop printed exactly that and sent an operator hunting a
            // straggler that did not exist, while the real holder was the tail's core join
            // (the alpaca+ctrader live rehearsal (PR #1407), Finding C). Printing `{deadline:?}`
            // was the other half: it echoed the CONFIGURED budget and so could not even show that
            // the time had been spent.
            tracing::warn!(
                elapsed_ms = teardown_took.as_millis() as u64,
                deadline_ms = deadline.as_millis() as u64,
                still_running,
                stage = ?stage,
                "vike-tradehub shutdown hard-capped after {teardown_took:?} (deadline {deadline:?}): {} \
                 — {still_running} parallel task(s) still in flight",
                stage.describe()
            );
            ExitCode::SUCCESS
        }
    }
}

/// Say what [`vike_ops::stop::install_handlers`] did, through the subscriber this binary owns.
///
/// It is a separate step from the install because the install happens FIRST — at the very top of
/// [`main`], before the settings load that decides where logs go — and a library that wrote to a
/// caller's stderr on its own initiative could not be used by a binary whose stdout is a protocol.
/// So the outcome travels as DATA and is reported here, exactly like `settings_warning_lines`.
fn log_handler_outcome(outcome: &stop::HandlerOutcome) {
    match outcome {
        // ⚠ The line names what THIS platform actually installed. `deploy/vike-tradehub.service`
        // tells an operator to grep the log for the unix wording as proof the build is new enough,
        // so the two must not be one string that is half true on each platform — and B10 gave
        // Windows a real arm while leaving this message claiming a signal that does not exist there.
        #[cfg(not(windows))]
        stop::HandlerOutcome::Installed => tracing::info!(
            "SIGTERM/SIGINT will stop this daemon gracefully — the teardown runs, so \
             `cancel_orders_on_shutdown` is honoured on a `systemctl stop` as well as on an \
             interactive one"
        ),
        // Windows: console control events, which reach only a daemon that HAS a console. A detached
        // background run is stopped through the stop file armed in `main` instead — this line says
        // which of the two you have, because the answer decides how an operator stops the box.
        #[cfg(windows)]
        stop::HandlerOutcome::Installed => tracing::info!(
            "Ctrl-C / Ctrl-Break / console close will stop this daemon gracefully — the teardown \
             runs, so `cancel_orders_on_shutdown` is honoured. A DETACHED run has no console to \
             deliver those: it stops through the stop file (docs/ops/tradehub-windows.md)"
        ),
        // Neither POSIX signals nor a Windows console. The truth about the platform, not a
        // degradation — said out loud so nobody believes a service-manager stop is graceful.
        stop::HandlerOutcome::Unsupported => tracing::info!(
            "no signal or console stop on this platform — stop with the stdio `shutdown` word"
        ),
        // The one case an operator MUST see: the daemon trades, but a stop is back to abandoning
        // the resting book at the venue and nothing else would say so.
        stop::HandlerOutcome::Failed(e) => tracing::error!(
            error = %e,
            "could NOT install the signal handler — a SIGTERM will run NO teardown, so resting \
             orders stay live at the venue; close the book yourself before stopping \
             (docs/ops/kill-switches.md section C)"
        ),
    }
}

/// Hand-rolled tiny arg parser (no `clap` — PR-9 adds no dependency). Accepts `--config value` and
/// `--config=value`; `--config` is required; `-h`/`--help` short-circuits to [`Parsed::Help`] and
/// `-V`/`--version` to [`Parsed::Version`], both of which are a SUCCESS (see those variants' docs,
/// and `main`'s arms for why stdout is right here).
/// `--profile`/`--profile=value` is OPTIONAL — the RunProfile risk-budget file (Task 2); absent, the
/// daemon still checks `VIKE_RUN_PROFILE` ([`vike_core::resolve_profile`]'s env fallback), and absent
/// BOTH the risk budget is untouched (today's behavior).
///
/// ⚠ Takes the argv TAIL — `argv[0]` is ALREADY STRIPPED, by the shim and by the `vike` dispatcher
/// alike. The old wrapper here did its own `skip(1)`, which was correct for the standalone shim
/// (full process argv) and ate the first REAL argument through the dispatcher: the v0.1.16 image
/// smoke ran `vike-tradehub --version` through the symlink and was answered with the usage error.
fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    parse_args_from(argv.iter().cloned())
}

/// **THE RULE, and this daemon's one addition to it.** A valued flag must be GIVEN a value: a
/// token beginning with `--` is a FLAG and never a value, and a value that is BLANK is no value at
/// all. The same rule `vike_backfill::cli::flag_value` spells for the backfill bins; this crate
/// cannot depend on that one (nothing may — it pulls every bridge crate), so the spelling is
/// repeated rather than shared.
///
/// It is `--`, not a bare `-`: a negative number is a real value in this workspace's parsers, so a
/// `starts_with('-')` test would turn every signed field into a usage error. Neither flag here
/// takes a number, but the predicate is the workspace's and does not fork per binary. A path that
/// genuinely begins with `--` is reachable as `./--weird.toml`.
///
/// **The BLANK clause is this daemon's, and it is what makes the two spellings of one flag agree.**
/// `--config=` used to be ACCEPTED with an empty config path — the inline branch took
/// `split_once`'s right half verbatim and no arm checked it — while `--config` with no value was
/// refused. A `systemd` `ExecStart` interpolating an unset shell variable produces exactly the
/// accepting form (`--config=$VIKE_CONFIG` with the variable unset, or `--config "$VIKE_CONFIG"`
/// with quotes), so the daemon started, satisfied its own required-flag check, and then failed
/// opening `""` — a file-open error instead of "you gave no --config". Nothing legitimately passes
/// an empty or whitespace path to either flag: `--config` names the profile TOML that decides the
/// venue, the token and the mount shape, and `--profile` names a `RunProfile` risk-budget file
/// whose ABSENCE is already spelled by omitting the flag (it then falls through to
/// `VIKE_RUN_PROFILE`). So both spellings of both flags now refuse it.
fn flag_value(flag: &str, value: Option<String>) -> Result<String, String> {
    match value {
        None => Err(format!("{flag} requires a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} requires a value, but the next argument is another flag ({v})"))
        }
        Some(v) if v.trim().is_empty() => Err(format!(
            "{flag} requires a non-empty value (an unset shell variable in a systemd ExecStart= \
             line produces exactly `{flag}=`)"
        )),
        Some(v) => Ok(v),
    }
}

/// The whole of the daemon's argument surface, over an already-`argv[0]`-stripped stream. PURE — no
/// environment, no filesystem — so the LIVE daemon's command line is drivable from a test without a
/// process. [`parse_args`] is the thin process-argv wrapper; the SOURCE of argv is the only thing it
/// decides.
///
/// Both spellings of both flags resolve through [`flag_value`], which is what stops them
/// disagreeing: `--config`, `--config=` and `--config ""` are now the same refusal.
fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut config_path: Option<String> = None;
    let mut profile_path: Option<String> = None;
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            // `inline.or_else(|| it.next())` rather than a match: the INLINE half is `Some("")` for
            // `--config=`, which must reach `flag_value`'s blank clause rather than falling through
            // to the next argument as if no value had been written at all.
            "--config" => {
                config_path = Some(flag_value("--config", inline.or_else(|| it.next()))?);
            }
            "--profile" => {
                profile_path = Some(flag_value("--profile", inline.or_else(|| it.next()))?);
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let config_path = config_path.ok_or("missing required --config <profile.toml>")?;
    Ok(Parsed::Args(Args { config_path, profile_path }))
}

/// The daemon's reconcile env: the REAL process env (the `VIKE_RECONCILE` idiom — NOT the `.env` creds
/// map; a shell-exported flag is invisible to `load_workspace_dotenv()`, see
/// [`vike_ops::reconcile_config`]'s module doc) with the daemon's QUARANTINE-FIRST default folded
/// in. Both the `VIKE_RECONCILE=1` master gate ([`vike_ops::reconcile_config::reconcile_enabled`])
/// and every `VIKE_RECONCILE_*` knob ([`vike_ops::reconcile_config::build_recon_config`]) are read
/// off this one map, exactly as `vike-app`'s `App::new` builds `std::env::vars().collect()`.
fn daemon_recon_env() -> HashMap<String, String> {
    with_quarantine_first_default(process_env().clone())
}

/// Fold the daemon's QUARANTINE-FIRST `VIKE_RECONCILE_POLICY` default into a reconcile env map: default
/// it to `quarantine` ONLY when the operator has not set it (an explicit value — `hybrid`/`synthesize`/
/// … — is honored verbatim). A fresh live daemon must auto-fold NOTHING, because `hybrid`
/// auto-applies `PositionDrift` — rewriting position size and booking realized PnL at the venue's
/// price — before anyone has confirmed that venue's position report is complete (CLAUDE.md's
/// `VIKE_RECONCILE` rollout rule). ⚠ The rule used to be justified by "an incomplete open-order
/// fetch under `hybrid` auto-cancels `OrphanLocalOrder`s"; that was false (the kind resolves to
/// zero events under every policy — `crates/vike-exec/tests/recon/recon_policy_pin.rs`), so the DEFAULT
/// is unchanged but its reason is. Split out as a PURE function so the default is unit-testable
/// without mutating the
/// process-global env (`std::env::set_var` is unsound from parallel test threads).
fn with_quarantine_first_default(mut env: HashMap<String, String>) -> HashMap<String, String> {
    env.entry("VIKE_RECONCILE_POLICY".to_string()).or_insert_with(|| "quarantine".to_string());
    env
}

/// What a LIVE mount hands back: the core, its teardown handles, `build_node`'s own arming record,
/// and the B11 live-account lock claims.
///
/// ⚠ The fourth element is HELD, never read — its LIFETIME is the claim (see
/// `vike_ops::live_lock`). `main` binds it for the whole session, so a second live process on any
/// of these accounts is refused for exactly as long as this one can trade. Returning it rather than
/// storing it in [`LiveTeardown`] is deliberate: the teardown struct is DESTRUCTURED at the top of
/// the shutdown path, which would release every claim while the core is still joining and its
/// cancel sweep is still running orders.
type LiveMount = (
    CoreHandle,
    LiveTeardown,
    std::collections::HashSet<String>,
    Vec<vike_ops::live_lock::LiveLock>,
);

/// The LIVE mount (`VIKE_TRADEHUB_LIVE=1`): stand up the twelve-venue [`vike_run::build_node`] core
/// with the resolved strategy mounts folded in (split-plane I10: one for the historical
/// single-mount profile, N for a `[[mounts]]` one), then wire each DISTINCT mounted venue's live
/// market feed onto it. Returns the live
/// [`CoreHandle`] plus the [`LiveTeardown`] handles. HARD-errors (never a silent paper fallback) on any
/// venue/symbol the daemon has not wired for live — the daemon's own allow-list (safety gates #4/#5).
/// The PER-VENUE credential gate (#2) and each venue's own second gate (#3, e.g. `HYPERLIQUID_MAINNET`)
/// still apply INSIDE `build_node`, so a credential-less venue mounts paper even here (no real orders).
///
/// `risk_profile` is the resolved `--profile`/`VIKE_RUN_PROFILE` [`vike_core::RunProfile`]'s `[risk]`
/// table (RunProfile wiring — closing the live gap): threaded straight into [`NodeConfig::risk_profile`],
/// which `build_node` applies uniformly to every one of the twelve venues' `RiskLimits` via
/// `vike_mount::make_engine`. `None` (no `--profile`/`VIKE_RUN_PROFILE`, today's only path before this
/// wiring) leaves every venue's `RiskLimits` byte-identical to before — this is the SAME budget
/// `resolve_paper_risk_limits` already arms on the PAPER mount above, now also reaching the LIVE one.
/// The caller (this file's `main`) resolves this through
/// [`vike_core::RunProfile::risk_for_live_venue_mount`] rather than reading `.risk` directly, so by
/// the time it reaches this function it is guaranteed to have come from a `mode = "live"` profile —
/// `make_engine`'s hardcoded `GridSource::VenueFetched` never contradicts the profile it came from.
///
/// `policy` is this MACHINE's `<vike home>/policy.toml`, resolved once by [`resolve_settings`] at the
/// top of [`main`] and projected here onto [`vike_run::MountPolicy`] — the subset a venue mount
/// applies (settings-unification Phase 6c). A DIFFERENT authority from `risk_profile`: per-machine
/// and admin-owned, with no env and no CLI layer at all.
///
/// ⚠ Its `venues` field is the per-venue ARMING CEILING and its default is `paper` EVERYWHERE, so
/// `Policy::default()` (no file) mounts an ALL-PAPER daemon regardless of the credential store —
/// see this module's doc. `market_slippage` is the other binding field, consumed by the hyperliquid
/// arm (this daemon's primary live venue, and the only roster venue with no native market order, so
/// its every market intent and every tripped stop-MARKET is priced at that band); for that one,
/// no file leaves the arm on hyperliquid's own compiled-in literal, byte-identically.
///
/// `flags` is the resolved [`vike_config::Flags`] — this arm consumes `reconcile`,
/// `tradehub_record` and `oco_cancel_sibling_on_dead_exit`, each still overridden by its own
/// variable inside the loader. They arrive as a parameter rather than being read here so that ONE
/// load decides them for the whole process.
///
/// `mounts` is the resolved mount SET (split-plane I10: N strategies / N venues, one row for the
/// historical single-mount profile) — each entry carries its strategy (the A-S maker by default,
/// or whatever that profile row's `[strategy]` table named), its A-S lowering `cfg` (this
/// function's FEED wiring reads it: `cfg.token_id`/`cfg.interval` name what to subscribe, and the
/// Polymarket arm's `TickBarSynthesizer` window is `cfg.interval_ms`) and its strategy-free `spec`
/// projection ([`vike_run::build_live_multi_strategy_core`] mounts on those). Per entry the two
/// agree by construction — the caller derives each `spec` from its `cfg`.
///
/// The FIRST entry is the PRIMARY mount: its `cfg` supplies the per-venue account seed
/// (`NodeConfig::seed_cash` / `CoreConfig::seed_cash`), exactly as the single-mount daemon always
/// did. Each DISTINCT venue's feed arm is wired exactly once, however many mounts share the venue
/// (subscriptions dedup per series inside the arm); the teardown handle carries one [`LiveFeeds`]
/// entry per venue.
fn live_mount(
    mounts: Vec<ResolvedMount>,
    risk_profile: Option<vike_exec::ProfileRisk>,
    profile: Option<vike_core::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    state_dir: &std::path::Path,
) -> Result<LiveMount, String> {
    // The credentials map — the daemon binary owns this I/O (the light client crates must not).
    // Absent per-venue creds keep that venue PAPER even with the gate on (safety gate #2), and that
    // is unchanged by WHICH store supplied them: see [`workspace_credentials`].
    let vars = workspace_credentials();
    live_mount_with(mounts, risk_profile, profile, policy, flags, vars, state_dir, &ProdFeedCtors)
}

/// [`live_mount`] with its two impurities as PARAMETERS — the credential map (production:
/// [`workspace_credentials`], the store the binary owns) and the feed-construction seam
/// (production: [`ProdFeedCtors`], whose defaults are the arms' own inline expressions). The
/// split exists for the deterministic data-only splice test
/// (`src/feed_splice_seam_tests.rs`): a scripted constructor plus a fake-key map is what lets a
/// CI test drive the REAL mount path — plan gate, withhold, `build_node`, feed wiring — with no
/// store, no network and no weekday market.
///
/// The third return is `build_node`'s own `live_venues` record — the per-venue EXEC ARMING STATE
/// (`vike_mount::make_engine` inserts a venue exactly when it constructs a real exec client). It is
/// what the seam test asserts "the data-only declaration kept exec paper" against, and it is also
/// what `main` RENDERS: the ready banner's `mode` and every `WireMountRow::live` are derived from
/// it, so no paper-vs-live report can disagree with the mount that produced it.
///
/// ⚠ [`live_mount`] used to DISCARD it, on the reasoning that "the arming disclosures inside
/// [`wire_venue_feeds`] already read it". Those disclosures are `tracing` lines; the ready banner is
/// the string `docs/ops/tradehub-the CI box.md` and `deploy/vike-tradehub-project.service` both call the
/// authority on paper-vs-live, and it was being built from the PROFILE instead. MEASURED on the CI box,
/// one startup: nine venues in this set, one venue in that banner. Do not re-narrow the return.
// ⚠ EIGHT arguments, one over clippy's default, and the shape is the point rather than an
// oversight: this is the daemon's live composition seam and every parameter is an INJECTED
// IMPURITY — the resolved mount set, the operator budget, the run profile, the machine policy, the
// flags, the credential map, the state directory the B11 sentinels are claimed in, and the feed
// constructors. That list is what lets `src/feed_splice_seam_tests.rs` drive the REAL mount path
// with no store, no network and no real keys; bundling them into a struct would hide exactly the
// substitutions those tests exist to make. The `state_dir` argument (the eighth) is the one that
// crossed the threshold, and it cannot be resolved here: only the boot walk knows it.
#[allow(clippy::too_many_arguments)]
fn live_mount_with(
    mounts: Vec<ResolvedMount>,
    risk_profile: Option<vike_exec::ProfileRisk>,
    // The SAME run profile `risk_profile` was derived from, carried WHOLE so its `[guards]` and
    // `[sinks]` tables can reach the `CoreConfig` below. Threading it stopped at `[risk]` before:
    // every guard an operator wrote was parsed, validated, announced as ignored — and dropped.
    profile: Option<vike_core::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    mut vars: HashMap<String, String>,
    // Where the B11 live-account sentinels are claimed (`<state_dir>/LIVE-<venue>.lock`) — the
    // daemon's own `booted.state_dir`, a PARAMETER because the claim is made here (after the
    // `data_only` withhold, before `build_node`) and this function resolves no paths of its own.
    state_dir: &std::path::Path,
    make: &dyn FeedCtors,
) -> Result<LiveMount, String> {
    // The RECONCILE master gate — `reconcile` in `<project>/settings/flags.toml`, still overridden by
    // `VIKE_RECONCILE` (env > file > default, resolved once in [`resolve_settings`]). OFF (the
    // default) ⇒ `node_cfg.recon_enabled` is `false` (byte-identical to the old hardcoded value) and
    // the `spawn_recon` mount below is SKIPPED entirely: no `ReconConfig`, no driver, no
    // `vt-core-recon` thread.
    //
    // ⚠ Only the MASTER gate moved. The rest of the `VIKE_RECONCILE_*` family — cadences, lookbacks,
    // the policy name, the balance tolerances, AND the two sibling FLAGS
    // (`reconcile_generate_missing`, `reconcile_balance`) — is still built from `recon_env` by
    // `reconcile_config::build_recon_config` below, which reads the whole family from ONE map. Those
    // two flags are recorded as unconsumed in `vike_config::CONSUMPTION` rather than half-wired here:
    // a family whose parts disagree about a single reconcile pass would be worse than one that is
    // uniformly environment-driven. `daemon_recon_env` still carries this daemon's QUARANTINE-FIRST
    // policy default.
    let recon_env = daemon_recon_env();
    let recon_enabled = flags.reconcile;

    // Safety gate #3 (feed side), resolved ONCE here rather than inside `venue_feed_plan`'s
    // `"hyperliquid"` arm: `feeds.rs` is a LIBRARY module, and `crates/vike-ops/src/settings.rs`'s
    // settings-registry gate refuses a library reading process env its caller cannot see, override,
    // or even know about — `env::var` is a `Layer::Binary` read, correct here in the binary, wrong
    // there. Both this read and `vike-mount`'s `hl_env` fold through the ONE converged rule
    // (`vike_bridge_core::mainnet::mainnet_for`) plus hyperliquid's switch row, so the two can never
    // disagree on how `HYPERLIQUID_MAINNET` parses.
    let hl_process_mainnet = process_env().get("HYPERLIQUID_MAINNET").cloned();
    let hyperliquid_mainnet = vike_bridge_core::mainnet::mainnet_for(
        "hyperliquid",
        hl_process_mainnet.as_deref(),
        vars.get("HYPERLIQUID_MAINNET").map(String::as_str),
    );

    // Safety gates #4/#5 — the daemon's venue+symbol ALLOW-LIST, resolved per MOUNT and BEFORE
    // `vars` is moved into the NodeConfig (split-plane I10: every mount must pass its own venue's
    // gate, and the venue set collects ONE plan per DISTINCT venue, in mount order — the plan is
    // venue+environment-derived, so mounts sharing a venue share its plan). Any unwired venue is a
    // HARD ERROR (never a silent paper fallback). The dispatch itself lives in `venue_feed_plan`
    // (`feeds.rs`), where `live_wired_venues_pin.rs` scans its arms.
    let mut venue_plans: Vec<(String, VenuePlan)> = Vec::new();
    for m in &mounts {
        let plan = venue_feed_plan(&m.cfg, &vars, hyperliquid_mainnet)?;
        if !venue_plans.iter().any(|(v, _)| v == &m.cfg.venue) {
            venue_plans.push((m.cfg.venue.clone(), plan));
        }
    }
    // The DATA-PLANE-ONLY declarations (the `data_only` profile key, validated to the
    // credentialed-data venue set at load): withhold each declared venue's credentials from the
    // map `build_node`'s exec arms will read — AFTER the plan loop above, which is the ordering
    // the seam rests on (each credentialed-data plan CARRIES its resolved config, so the FEED
    // keeps the credentials exec loses; see [`withhold_exec_credentials`]). An undeclared mount
    // leaves `vars` untouched, byte-identically. The startup line is the declaration's own
    // disclosure — the arming banner inside `wire_venue_feeds` then announces the resulting
    // paper exec per venue ([`data_only_arming`]).
    let data_only: std::collections::HashSet<String> = mounts
        .iter()
        .filter(|m| m.row.data_only_effective())
        .map(|m| m.cfg.venue.clone())
        .collect();
    for venue in &data_only {
        let withheld = withhold_exec_credentials(&mut vars, venue);
        tracing::warn!(
            venue = %venue,
            keys_withheld = withheld,
            "DATA-ONLY mount declared for {venue} (`data_only = true` in the profile): its \
             credentials are WITHHELD from the exec mount, so exec stays on the PAPER book by \
             declaration while the credentialed market feed authenticates from the same store"
        );
    }
    // SAFETY GATE #6 — ONE live process per venue ACCOUNT (split-plane B11, the Danger-2
    // tripwire): claim `<state_dir>/LIVE-<venue>.lock` for every venue this mount is about to ARM,
    // BEFORE `build_node` constructs a single exec client. Two properties, and the daemon had
    // NEITHER of them until this call site existed:
    //
    // 1. THE SET. The claims used to be made in `main` over `mount_venues` — the RUN PROFILE's
    //    venues. That is a different question: the profile says which `(venue, symbol)` pairs carry
    //    a STRATEGY, while `build_node` arms an exec client for every `vike_run::WIRED_MARKETS`
    //    venue the credential store answers for and the ceiling permits. MEASURED on the CI box, one
    //    startup of the shipped daemon, two lines apart — `live_venues={"hyperliquid","deribit",
    //    "okx","bybit","alpaca","aster","binance","ig","oanda"}` beside `mode":"LIVE (venue=bybit)"`
    //    — nine live authenticated sessions behind ONE lock. `vike_run::armed_live_venues` is the
    //    pre-mount probe that answers the arming question instead, and `build_node`'s own
    //    `refuse_unarmed_live_venues` backstop refuses the node if anything arms outside it.
    // 2. THE MAP IT READS. It is THIS `vars` — the map AFTER the `data_only` withhold above — and
    //    that ordering is load-bearing: a declared data-only venue has had its exec credentials
    //    taken away, so it will not arm, and claiming its account lock would refuse a legitimate
    //    second process for a venue this daemon deliberately leaves on paper. `main` could not have
    //    read this map at all; the withhold happens here.
    //
    // The claims are RETURNED, not dropped: `main` binds them for the whole session (the OS
    // releases them on death, so there is no stale-lock sweep). Wiring only — the mechanism, the
    // refusal message and the tests live in `vike_ops::live_lock`.
    let mount_policy = vike_run::MountPolicy::from(policy);
    let mut live_account_locks: Vec<vike_ops::live_lock::LiveLock> = Vec::new();
    // ⚠ A ROUTE KEY per ACCOUNT, not a venue id: `binance` for a venue's default account — so no
    // deployed sentinel filename moves — and `binance#ALT` for a second one. `LiveLock::acquire`'s
    // own doc has required exactly this since the route-key split landed, because keying on the
    // canonical venue would make ONE process mounting two accounts of one exchange refuse its own
    // second mount.
    // ⚠ The probe reads the SETTINGS, not this profile's mounts, and that is correct rather than an
    // omission: a second account ARMS on its `policy.accounts.<venue>.<LABEL>` line plus its own
    // credentials, and `make_engine_accounts` mounts it whether or not a `[[mounts]]` row names it.
    // So the set of sentinels to claim is the same set whatever this profile mounts — which is what
    // lets the claim be made HERE, before `build_node` has assembled anything.
    for route_key in vike_run::armed_live_venues(&vars, &mount_policy) {
        match vike_ops::live_lock::LiveLock::acquire(state_dir, &route_key) {
            Ok(l) => live_account_locks.push(l),
            Err(e) => return Err(format!("refusing LIVE mount: {e}")),
        }
    }

    // ...and the one CROSS-mount consistency gate, likewise before any core spawns: two polymarket
    // mounts on ONE token must agree on the interval, because they will share one `MakerSink`
    // whose bar synth runs at exactly one window (see `LiveFeeds::Polymarket`).
    #[cfg(feature = "polymarket")]
    check_poly_token_intervals(&mounts)?;
    // ...and its ctrader twin (split-plane I9): every ctrader mount shares one data socket and one
    // bar SYNTHESIZER window, so rows disagreeing on the interval are refused before any core
    // spawns — see `check_ctrader_intervals`.
    check_ctrader_intervals(&mounts.iter().map(|m| &m.cfg).collect::<Vec<_>>())?;

    // Opt-in PIT-`SymbolProperties` recorder (`VIKE_RECORD_PROPERTIES=1`), constructed HERE — the
    // DataFusion-edge cut moved construction out of vike-mount into the binaries.
    // `PropertiesRecorder::open_from_env` names the concrete DataFusion store backend, which only a
    // `record-feeds`/`materialize` build carries (both enable `vike-data/hist-datafusion`); a
    // DEFAULT (DataFusion-free) daemon build cannot open the store, so it passes `None` —
    // `VIKE_RECORD_PROPERTIES` is inert there (recording needs a recording feature built in),
    // byte-identical to the recorder's own disabled path.
    #[cfg(any(feature = "record-feeds", feature = "materialize"))]
    let properties_rec = vike_data::PropertiesRecorder::open_from_env(&tick_store_root());
    #[cfg(not(any(feature = "record-feeds", feature = "materialize")))]
    let properties_rec: Option<Arc<vike_data::PropertiesRecorder>> = None;

    // The live core config carries the SAFETY KNOBS a live daemon must be capped with (NOT the paper
    // mount's minimal `CoreConfig::default()`). Field names/values mirror vike-app's fat-build
    // CoreConfig: a 30s submit-ack backstop, a 25% equity-drawdown liquidate-only latch, and the
    // margin-call watchdog (inert at 1× until leverage is raised). `recon_enabled` is the
    // `VIKE_RECONCILE=1` gate resolved above (unset ⇒ `false`, byte-identical to the pre-reconcile
    // daemon: `build_node` builds no reconnect-trigger channel). `strategy`/`extra_mounts` are
    // left unset here — `build_live_multi_strategy_core` folds the resolved mounts into them
    // before `build_node` consumes the config.
    //
    // The PRIMARY (first) mount's `seed_cash` supplies the per-venue account seed, exactly as the
    // single-mount daemon always passed its one mount's — `build_node` applies `NodeConfig::
    // seed_cash` uniformly to every extra venue engine and `core_config.seed_cash` to the primary,
    // and a per-ROW seed would need a per-venue seed table `build_node` does not take (stated
    // rather than silently summed: `validate_for_live`'s seed refusal runs per row either way).
    let primary_seed = mounts[0].cfg.seed_cash;
    // THE ARMING ROWS, computed HERE and nowhere else in this function. The window is narrow and
    // both edges are load-bearing: AFTER `withhold_exec_credentials` above (so a `data_only`
    // venue's exec keys are already gone from `vars`, and the rows say paper for it, which is what
    // this daemon actually does) and BEFORE `vars`/`mount_policy` move into `NodeConfig` below
    // (after which neither exists to read).
    //
    // This is why `vike_run::journal_venue_mounts` takes ROWS rather than the map. A signature
    // taking `vars` would let the call sit anywhere downstream and be wrong by a comment; taking
    // rows makes the caller pick this moment, and there is only one.
    let arming = vike_run::venue_arming(&vars, &mount_policy.venues);

    let mut node_cfg = NodeConfig {
        vars,
        properties_rec,
        seed_cash: primary_seed,
        recon_enabled,
        risk_profile,
        // The machine's ceilings, projected onto what a venue mount applies. No `policy.toml` ⇒
        // `Policy::default()` ⇒ `MountPolicy::default()` ⇒ every venue keeps its compiled-in
        // literal, byte-identical to this daemon before Phase 6c.
        //
        // ⚠ The SAME value the live-account lock claims above were computed from — one projection,
        // so the set that was locked and the set the arms resolve cannot disagree by construction.
        policy: mount_policy,
        core_config: vike_core::CoreConfig {
            seed_cash: primary_seed,
            // ⚠ These three are the daemon's DEFAULTS, not its answer: `profile`'s `[guards]` table
            // overwrites whichever of them it names, via `apply_guards_and_sinks` below. Before that
            // call existed they were the only values that could ever apply, and an operator's
            // configured guards were parsed, warned about and discarded.
            submit_ack_timeout: Some(Duration::from_secs(30)),
            max_drawdown: Some(0.25),
            margin_call: Some(vike_exec::MarginCallConfig::default()),
            // Honor the same opt-in write-ahead journal env the paper mount does (off by default).
            journal: vike_core::journal_config_from_env(),
            // OPT-IN OCO SIBLING-CANCEL ON A DEAD EXIT (off by default) — `oco_cancel_sibling_on_
            // dead_exit` in `<project>/settings/flags.toml`, still overridden by
            // `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`. When a released bracket exit terminates
            // UNFILLED, the default KEEPS the surviving OCO sibling so the position retains
            // whatever protection it still has; on, the sibling is canceled and the book is left
            // flat. Off ⇒ byte-identical to this daemon before the flag reached it.
            //
            // ⚠ vike-app was this flag's ONLY reader in the tree, so the behaviour existed in the
            // GUI and was UNREACHABLE on the server that runs unattended — the one deployment where
            // "leave the book flat when protection dies" is most likely to be what an operator
            // wants. Both mounts read it now (the paper arm via `PaperMountOpts`), so the rehearsal
            // and the live run cannot disagree about it either.
            oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,
            // OPT-IN SHUTDOWN POLICY (off by default) — `cancel_orders_on_shutdown` in
            // `<project>/settings/flags.toml`, still overridden by
            // `VIKE_CANCEL_ORDERS_ON_SHUTDOWN`. OFF (the default, and the behaviour that has always
            // shipped) leaves every resting order LIVE AT THE VENUE when the daemon stops, with
            // nothing left running to manage it; ON cancels the book during teardown, inside the
            // `[daemon] shutdown_deadline_ms` budget. It never flattens — positions survive either
            // way.
            //
            // ⚠ Reachable only on a stop that actually TEARS DOWN: an interactive
            // `shutdown`/`quit`/Ctrl-D. Under `deploy/vike-tradehub.service` stdin is `/dev/null`
            // and SIGTERM has no handler, so `systemctl stop` kills the process before any of this
            // runs — see `docs/ops/kill-switches.md` §D, which says so to the operator.
            cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,
            // Runtime strategy mount/unmount (split-plane B5): resolve a wire `MountStrategy`'s
            // `[strategy]`-vocabulary spec through the daemon's own profile machinery
            // (`crate::mount_factory` — same refusals as a profile load). Absent this,
            // the core refuses every runtime mount. Grafted through the I10 rebase: the
            // relocated multi-mount NodeConfig must answer MountStrategy exactly as B5's did.
            strategy_factory: Some(crate::mount_factory::strategy_factory()),
            // Durable strategy state + runtime-mount topology (B5 residual closed): arms the
            // per-mount `<mount_id>.json` sidecars AND the `runtime_mounts.json` topology record
            // the resurrect below replays. `None` (no project, no `$VIKE_STATE_ROOT`) disarms
            // both — byte-identical to this daemon before the residual closed.
            state_dir: strategy_state_dir(),
            ..vike_core::CoreConfig::default()
        },
    };

    // ⚠ THE `[guards]`/`[sinks]` WIRING. Applied HERE — after the daemon's own defaults, before
    // `build_node` — because the profile is the auditable authority and the literals above are the
    // fallback. Five guards (`submit_ack_timeout_ms`, `submit_ack_confirm_grace_ms`,
    // `max_drawdown`, `conditionals_on_ticks`, `margin_call`) plus `sinks.equity_sample_ms` map
    // 1:1 onto `CoreConfig` fields that already existed; the converters already returned the right
    // types. Nothing called them. From #816 (which wired `[risk]`) until now, every one of those
    // keys was parsed, VALIDATED, reported as ignored by a `warn!` and then dropped — a settings
    // key that nothing reads, which this workspace deleted `Policy::max_total_exposure` for.
    //
    // The return's `unwired` is the set that is still unwired, which is DISCLOSED rather than
    // warned about generically: an operator who set `initial_trading_state = "halted"` must be told
    // that exact key did not arm, not handed a sentence about the whole section.
    //
    // ⚠ …and `confirm_grace` is the OTHER half of that return: the stage-1/stage-2 pairing evaluated
    // on the FINAL numbers — this daemon's own `submit_ack_timeout: Some(30s)` literal above, or
    // whatever the profile put in its place, against the grace the profile did or did not name. A
    // profile that RAISES the timeout and says nothing about the grace walks the untouched grace
    // under HARD LOWER BOUND (a) in silence, and nothing below this line could ever notice: an
    // undersized grace is not an error, it is a narrower margin, and it surfaces as a phantom
    // OrderRejected against an order the venue actually holds. It WARNS rather than refusing (see
    // `vike_core::ConfirmGraceHazard`, which argues that against
    // `docs/decisions/0013-degrade-vs-refuse.md`), and the sentence is rendered by the type so this
    // daemon and `vike-app` cannot drift about the arithmetic.
    if let Some(p) = &profile {
        let vike_core::GuardsReport { unwired, confirm_grace } =
            p.apply_guards_and_sinks(&mut node_cfg.core_config);
        if !unwired.is_empty() {
            tracing::warn!(
                "run profile keys {unwired:?} are SET and reach no CoreConfig — they are parsed \
                 and validated but arm nothing in this daemon (see \
                 `vike_core::RunProfile::apply_guards_and_sinks` for why each one is still \
                 unwired); every other [guards]/[sinks] key IS applied"
            );
        }
        if let Some(hazard) = confirm_grace {
            tracing::warn!("{hazard}");
        }
    }

    // Fold the resolved strategies into `core_config.strategy` + `extra_mounts`, then build the
    // 12-venue node. Per-venue exec is credential-gated inside `build_node` (absent creds ⇒ that
    // venue is paper), and `spawn_core_multi` wires applied-fill capture on each mount's own venue
    // engine. `recon_clients`/`recon_trigger` are captured (the reference mount needs them).
    //
    // ⚠ `build_live_multi_strategy_core` with ONE mount IS `build_live_strategy_core` (first mount
    // → `strategy`, an empty rest → `extra_mounts`), so the single-mount daemon builds the same
    // node it always did. `cfgs` is cloned out first — the feed wiring below still needs each
    // mount's A-S lowering after the specs move into the core.
    let cfgs: Vec<MakerMountConfig> = mounts.iter().map(|m| m.cfg.clone()).collect();
    let vike_run::Node { handle, recon_clients, recon_trigger, live_venues, forwarder_stop } =
        vike_run::build_live_multi_strategy_core(
            mounts
                .into_iter()
                .map(|m| vike_run::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
                .collect(),
            node_cfg,
        )
        .map_err(|e| format!("build_live_multi_strategy_core: {e}"))?
        .node;

    // ...and RECORD what each venue was ASKED to be and what it BECAME. The `venue_mounted`
    // channel's writer, called HERE and not from `main` for exactly the reason the lock claim
    // above gives: `vars` is the POST-withhold map. A `data_only` venue's exec credentials are
    // gone from it by now, so journalling from `main` — where `live_venues` is conveniently in
    // scope — would predict against the PRE-withhold map and record a venue this daemon
    // deliberately keeps on paper as one that was refused for a credential reason. Same trap the
    // sentinels had to move in here to avoid.
    //
    // Placed AFTER the node is built because the whole value of the record is the pairing: the
    // prediction (`vike_mount::venue_arming`, which the arming screen renders) against the OUTCOME
    // (`live_venues`). Before this line the second half does not exist.
    //
    // Failures WARN and continue. A journal that cannot be written must never stop a daemon that
    // has already mounted its venues — the ledger is evidence, not a gate.
    for r in vike_run::journal_venue_mounts(
        Some(state_dir),
        &arming,
        &live_venues,
        env!("CARGO_PKG_VERSION"),
        vike_model::now_ms(),
    ) {
        if let Err(e) = r {
            tracing::warn!(error = %e, "venue mount record not journalled");
        }
    }

    // Resurrect RUNTIME strategy mounts (B5 residual closed): replay the topology sidecar through
    // the SAME lossless command lane a wire MountStrategy is lowered into — AFTER the core
    // spawned, BEFORE `wire_venue_feeds` below arms a single feed. The ingest lane is FIFO, so
    // every resurrected mount (and its `<mount_id>.json` state load, part of the mount arm) folds
    // ahead of the first market message — `resurrect_runtime_mounts`'s ordering contract. Nothing
    // here can fail the mount: a stale record skips with a warn, a corrupt file reads empty.
    if let Some(dir) = strategy_state_dir() {
        let outcome =
            crate::mount_factory::resurrect_runtime_mounts(&dir, |c| handle.send_command(c));
        if outcome.sent + outcome.skipped > 0 {
            tracing::info!(
                sent = outcome.sent,
                skipped = outcome.skipped,
                "runtime-mount resurrect replayed the topology sidecar"
            );
        }
    }

    // Carry the reconcile ingredients across the feed build ONLY when `VIKE_RECONCILE=1` — the
    // `spawn_recon` mount runs AFTER the feeds so a feed-subscribe error returns BEFORE any
    // `vt-core-recon` thread is spawned (no orphaned driver on the error path). OFF (the default): the
    // handles are moved into the tuple and DROPPED right here — matching the pre-reconcile daemon's
    // `..`-destructure drop timing exactly (`recon_trigger` is already `None`, since `build_node` gates
    // the channel on the same flag) — so no ingredients survive to mount a driver. This is the ONLY
    // unconditional new binding, and its OFF-path effect is a byte-identical early drop.
    let recon_ingredients = recon_enabled.then_some((recon_clients, recon_trigger));

    // Opt-in live-feed recording (the `record-feeds` FEATURE + the `tradehub_record` flag, from
    // `flags.toml` or `VIKE_TRADEHUB_RECORD`): a `RecorderSink` teed into the feed so every
    // quote/trade/book is persisted for offline replay. `post_feeds` runs the recorder's bounded
    // flush AFTER `feeds.shutdown()` (in-flight ticks drained) and BEFORE the core join — the
    // vike-app feeds → recorder → core teardown order. Off ⇒ `None` + an identity `wrap`
    // (byte-identical to no recording).
    #[cfg(feature = "record-feeds")]
    let (recorder_sink, post_feeds): (Option<Arc<vike_data::RecorderSink>>, PostFeeds) =
        match open_tradehub_recorder(flags.tradehub_record) {
            Some((sink, handle)) => (Some(sink), Some(Box::new(move || handle.shutdown()))),
            None => (None, None),
        };
    #[cfg(not(feature = "record-feeds"))]
    let post_feeds: PostFeeds = None;

    // Tee the venue's base sink into the recorder when recording is on (identity otherwise), so the
    // SAME quote/trade/book stream drives both the core and the writer actor.
    let wrap = |base: Arc<dyn LiveDataSink>| -> Arc<dyn LiveDataSink> {
        #[cfg(feature = "record-feeds")]
        if let Some(r) = &recorder_sink {
            return Arc::new(vike_data::TeeSink(vec![
                base,
                Arc::clone(r) as Arc<dyn LiveDataSink>,
            ]));
        }
        base
    };

    // Wire the venue's live market feed onto the core's ingest lanes.
    // Wire each mounted venue's live market feed onto the core's ingest lanes — ONE feed arm per
    // distinct venue, however many mounts share it (subscriptions dedup per series INSIDE the
    // arm). The arms live in `wire_venue_feeds`, directly above `live_mount`, where
    // `cex_feed_wiring_pin.rs` scans them.
    let mut feeds: Vec<LiveFeeds> = Vec::with_capacity(venue_plans.len());
    for (venue, plan) in &venue_plans {
        let venue_cfgs: Vec<&MakerMountConfig> =
            cfgs.iter().filter(|c| &c.venue == venue).collect();
        feeds.push(wire_venue_feeds(
            plan,
            &venue_cfgs,
            &handle,
            &live_venues,
            &arming,
            &data_only,
            &wrap,
            make,
        )?);
    }

    // Reconcile driver mount (VIKE_RECONCILE=1) — the reference is `vike-app`'s `App::new` recon_driver
    // block. Placed AFTER the feeds are up so a feed-subscribe error above returns before this ever
    // spawns a thread. `recon_ingredients` is `Some` only when `recon_enabled` (the OFF path already
    // dropped the handles above) ⇒ this whole mount is byte-identically absent when `VIKE_RECONCILE` is
    // unset. `spawn_recon` owns its own `vt-core-recon` thread and respects the single-writer rule (it
    // only blocking-fetches REST reports and enqueues `Command::ReconcileReports` for the fold thread —
    // see that module's doc); it ADOPTS the pre-built `recon_trigger` pair so a venue reconnect poke
    // reaches this same driver.
    let recon_driver = match recon_ingredients {
        None => None,
        // Enabled but every venue is paper (no creds ⇒ no `ReconClient`): inert, no thread — mirrors
        // vike-app's `recon_clients.is_empty()` arm. The (empty) clients + `recon_trigger` drop here.
        Some((clients, _trigger)) if clients.is_empty() => {
            tracing::info!(
                "VIKE_RECONCILE=1 but no live venue ReconClients (all paper); reconcile inert"
            );
            None
        }
        Some((clients, recon_trigger)) => {
            // The per-venue feed-status health map, from the feeds THIS mount actually built
            // ([`LiveFeeds::recon_feed_statuses`]). `build_node` builds no market feeds of its own
            // (they stay with the caller — see `vike_run::node`'s module doc), so the mounted venue's
            // own `Feeds::status` handle is the only one that exists, and it gates ONLY that venue:
            // every other reconciled venue is absent from the map, reads `Healthy`, and is never
            // health-blocked — the exec-only-venue shape (CLAUDE.md's per-venue health gate).
            //
            // ⚠ The map was UNCONDITIONALLY EMPTY before the CEX arm, and empty is still the right
            // answer for hyperliquid/polymarket — the gate can only ever SUPPRESS a pass, and a
            // wrongly-suppressed pass can stay suppressed, so a row is added per venue on evidence,
            // never by sweeping up whatever handles are in scope.
            let feed_statuses = recon_feed_statuses_of(&feeds);
            let recon_cfg = reconcile_config::build_recon_config(&recon_env, feed_statuses);
            tracing::warn!(
                venues = clients.len(),
                policy = ?recon_cfg.policy.default,
                "VIKE_RECONCILE=1 — mounting the reconciliation engine (quarantine-first unless \
                 VIKE_RECONCILE_POLICY is set); a restarted live daemon re-adopts open venue \
                 orders/positions instead of a blind fresh core"
            );
            Some(vike_core::spawn_recon(&handle, clients, recon_cfg, recon_trigger))
        }
    };

    Ok((
        handle,
        LiveTeardown { feeds, forwarder_stop, post_feeds, recon_driver },
        live_venues,
        live_account_locks,
    ))
}

/// Resolve the tick-recording store root for the LIVE mount — `$VIKE_TICK_STORE`, else an
/// already-populated `<exe_dir>/market_data/ticks`, else **`<project>/market_data/ticks`**, else
/// `<exe_dir>/market_data/ticks`.
///
/// The LADDER lives in `crates/vike-model/src/tick_store_path.rs`'s `resolve_tick_store_root`, not
/// here: `crates/vike-app/src/main.rs`'s `tick_store_root` is the same resolution for the desktop
/// app, the two were byte-identical copies, and two sides that must not disagree get a shared home
/// BELOW both rather than a copy in each. vike-data reads no path env itself, so the READS stay in
/// this binary — the variable, the boot's settings directory, and this executable's own directory.
///
/// ⚠ The project rung is [`SETTINGS_DIR`] — **the boot's own walk** — and NOT [`state_dir`]: that
/// one is relocated by `$VIKE_STATE_ROOT`, which moves this daemon's STATE tree and has never moved
/// a data store. It is also not a second `project_data_dir(&cwd)` call, which would be
/// `$VIKE_SETTINGS_DIR`-BLIND — the same trap [`alerts_path`] and the HALT sentinel each record.
///
/// ⚠ **The shipped unit's `ReadWritePaths=` does not grant this path under either default.**
/// `deploy/vike-tradehub.service` grants `<project>/settings/state` and nothing else, so under
/// `ProtectSystem=strict` a store open at `<project>/market_data/ticks` fails with `EROFS` exactly as one
/// at `<exe_dir>/market_data/ticks` did. The difference is that the project path is one line an operator
/// can legitimately grant (`deploy/vike-recorder.service` already grants its own `market_data/`), while the
/// exe directory is the one path that unit argues twice must never be writable. Recording is
/// best-effort either way: an unwritable root disables recording, never the mount.
///
/// Consulted only by the feature-gated recording paths (the PIT-properties recorder, the
/// `record-feeds` `RecorderSink`, the `materialize` store open) — all of which need the DataFusion
/// backend those features carry, so the fn is gated with them.
///
/// Memoized so the resolution is announced ONCE rather than once per recording path.
#[cfg(any(feature = "record-feeds", feature = "materialize"))]
fn tick_store_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let exe_dir =
            std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let resolved = vike_model::tick_store_path::resolve_tick_store_root(
            process_env().get("VIKE_TICK_STORE").map(String::as_str),
            SETTINGS_DIR.get().and_then(Option::as_ref).map(|p| p.as_path()),
            exe_dir.as_deref(),
        );
        // Which root, and by which rung. A store does not merge, so an answer that moved presents
        // as an empty result set weeks later rather than as an error now — one line at startup is
        // what turns that into a diagnosis.
        tracing::info!(
            store = %resolved.root.display(),
            rung = resolved.rung.as_str(),
            "tick store root resolved: {}",
            resolved.rung.why()
        );
        resolved.into_path()
    })
    .clone()
}

/// Best-effort live-feed recorder (opt-in: the `record-feeds` feature + the resolved `tradehub_record`
/// flag). Open the DataFusion+Parquet tick store at [`tick_store_root`] and spawn the `RecorderSink`
/// writer actor — teed into the live feed (see [`live_mount`]) so every quote/trade/book is persisted
/// for offline replay (the daemon twin of vike-app's `open_tick_recorder`). `None` when the gate is
/// off (byte-identical to no recording) OR on any open/spawn failure (logged, then the feed runs
/// unrecorded — recording must NEVER fail the mount). ⚠ A chatty feed writes a LOT; watch disk usage.
///
/// `enabled` is a PARAMETER — `flags.tradehub_record` from `flags.toml`, still overridden by
/// `VIKE_TRADEHUB_RECORD` inside the loader — rather than the `std::env::var` this used to perform,
/// so one resolution decides it and a file can turn it on.
#[cfg(feature = "record-feeds")]
fn open_tradehub_recorder(
    enabled: bool,
) -> Option<(Arc<vike_data::RecorderSink>, vike_data::RecorderHandle)> {
    if !enabled {
        return None;
    }
    let root = tick_store_root();
    let store = match vike_data::DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                ?root,
                "recorder store open failed: {e} — the live feed runs UNRECORDED"
            );
            return None;
        }
    };
    match vike_data::RecorderSink::spawn(Arc::new(store), vike_data::RecorderConfig::default()) {
        Ok(pair) => {
            tracing::warn!(
                ?root,
                "live-feed recording is ON (flags.toml `tradehub_record`, or VIKE_TRADEHUB_RECORD=1) \
                 — watch disk usage"
            );
            Some(pair)
        }
        Err(e) => {
            tracing::error!("recorder writer spawn failed: {e} — the live feed runs UNRECORDED");
            None
        }
    }
}

/// Spawn the off-path journal materializer (unified-journaling #2) IFF the `materialize` feature is
/// built AND the WAL is enabled (`journal_config_from_env()` is `Some` — the SAME
/// journal enablement env, so this adds NO new toggle). It tail-follows the WAL and materializes each
/// fill/order into the Tier-2 exec-log store series (`kind=exec_fill` / `kind=exec_order`) that
/// `vike-report` reads (`fills_from_store`) — the live tearsheet's durable sink. The store is the
/// DataFusion+Parquet hist store at [`tick_store_root`] (the same root the recorder uses);
/// best-effort — an open failure logs and returns `None`, leaving the WAL written (a later cold
/// `tearsheet --journal DIR` still reads it directly). `None` also when the WAL is off, which is the
/// byte-identical default. The returned handle drains once more on Drop.
#[cfg(feature = "materialize")]
fn maybe_spawn_materializer() -> Option<vike_ops::journal_mat::MaterializerHandle> {
    let dir = vike_core::journal_config_from_env().map(|jc| jc.dir)?;
    let root = tick_store_root();
    match vike_data::DataFusionHist::open(&root) {
        Ok(store) => {
            tracing::info!(
                ?dir,
                ?root,
                "journal materializer ON — WAL fills/orders → Tier-2 exec log (the vike-report sink)"
            );
            vike_ops::journal_mat::maybe_spawn(
                Some(dir),
                Arc::new(store),
                vike_ops::journal_mat::MaterializerConfig::default(),
            )
        }
        Err(e) => {
            tracing::warn!(
                ?root,
                "journal materializer: hist store open failed: {e} — skipping (WAL still written; \
                 read it later with `tearsheet --journal DIR`)"
            );
            None
        }
    }
}

/// Default (feature absent) build: no materializer, so the daemon stays DataFusion-free and
/// byte-identical to the pre-materializer daemon.
#[cfg(not(feature = "materialize"))]
fn maybe_spawn_materializer() -> Option<vike_ops::journal_mat::MaterializerHandle> {
    None
}

/// Wall-clock milliseconds since the UNIX epoch — the `now_ms` the pure alert evaluator takes (it
/// owns no clock by design). A pre-1970 clock is clamped to 0 rather than panicking; a wrong wall
/// clock must never take the daemon down.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The alerting rule file: `$VIKE_ALERTS` (see [`ALERTS_ENV`]) if set and non-blank, else
/// `<state_dir>/alerts.json`.
///
/// The ENV half is resolved HERE, in the binary — settings STEP 2 deleted the twin read that used
/// to sit in `persist::path()` inside a LIBRARY (exactly the shape the settings registry exists to
/// push out of libraries), so this is now the workspace's only `$VIKE_ALERTS` read and the daemon
/// hands the result to `persist::load_path`. The library still states the BASENAME
/// (`vike_ops::alerting::persist::ALERTS_FILE`), so the two sides cannot disagree about it.
///
/// `None` when there is no override AND no state directory resolves — there is no file to read
/// then, and [`maybe_mount_alerts`] says so rather than reading somewhere else.
fn alerts_path() -> Option<PathBuf> {
    alerts_path_in(process_env().get(ALERTS_ENV).map(String::as_str), state_dir().as_deref())
}

/// [`alerts_path`]'s pure core, so the precedence is testable without touching the process
/// environment: the override when set and non-blank, else `<state_dir>/alerts.json`.
///
/// A blank override falls THROUGH rather than resolving to `""` — an empty systemd
/// `Environment=VIKE_ALERTS=` line would otherwise point the loader at the working directory, the
/// same class of bug `vike_model::state_path`'s own blank-value guard exists for.
fn alerts_path_in(override_path: Option<&str>, state_dir: Option<&Path>) -> Option<PathBuf> {
    match override_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => Some(state_dir?.join(vike_ops::alerting::persist::ALERTS_FILE)),
    }
}

/// The project's STATE directory — `<project>/settings/state` — or `None` when no project sits
/// above the working directory.
///
/// The env-reading half of `vike_model::state_path` (which is pure), living in the BINARY exactly
/// as the settings-registry rule wants. [`STATE_ROOT_ENV`] names the directory outright and wins;
/// there is no third location.
///
/// ⚠ **The rung below the override is [`SETTINGS_STATE_DIR`] — the BOOT's walk — and it used to be
/// a walk of its own.** `project_state_dir(&cwd)` is `$VIKE_SETTINGS_DIR`-BLIND, so this daemon's
/// log home, `alerts.json` and telegram ledger hung off a project the settings and credentials had
/// not necessarily come from. `deploy/vike-tradehub.service` sets the override AND
/// `WorkingDirectory=` to the same tree, so the two agreed on the CI box by coincidence of the working
/// directory rather than because the override was honoured — which is precisely the dependency the
/// unit's own comment says the variable removes.
fn state_dir() -> Option<PathBuf> {
    if let Some(explicit) =
        process_env().get(STATE_ROOT_ENV).cloned().filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(explicit));
    }
    SETTINGS_STATE_DIR.get().cloned().flatten()
}

/// The daemon's STRATEGY-STATE directory — `<state root>/strategy-state`, handed to
/// `vike_core::CoreConfig::state_dir` by BOTH mount arms (split-plane B5, residual closed). Two
/// families of files live under it, both written by the core's own arms and read back at the
/// next boot: the per-mount `<mount_id>.json` durable-state sidecars
/// (`vike_core::strategy_state`) and the runtime-mount TOPOLOGY sidecar
/// (`vike_core::mount_topology` — what [`resurrect_runtime_mounts`] replays;
/// `crate::mount_factory`'s `resurrect_runtime_mounts` documents the ordering contract
/// both arms obey).
///
/// The SUBDIRECTORY is vike-app's own project-rung spelling (`state_dir_path` in that binary
/// joins `strategy-state` under ITS state root) so the two binaries shelve strategy state the
/// same way; hanging it off [`state_dir`] means `$VIKE_STATE_ROOT` relocates it together with
/// every other file this daemon writes (alerts, the telegram ledger, logs). `None` — no project,
/// no override — keeps `CoreConfig::state_dir` at `None`: no sidecar is ever written or read,
/// byte-identical to this daemon before B5's residual closed. Deliberately NOT
/// `config.state_dir`: that key is vike-app's sidecar knob (its `CONSUMPTION` row names that
/// binary), and this daemon's state tree is uniformly `$VIKE_STATE_ROOT`-rooted.
fn strategy_state_dir() -> Option<PathBuf> {
    Some(state_dir()?.join("strategy-state"))
}

/// The DEFAULT log directory — `<state root>/logs`, under [`state_dir`] so the daemon's rolling
/// trace file sits with every other file the program writes and no human edits.
///
/// Handed to `vike_log::init` as `LogConfig::project_dir`, which is the layer BELOW `$VIKE_LOG_DIR`
/// (still the operator's override) and below anything a config file names, and ABOVE vike-log's
/// `<exe_dir>/logs` last resort. `None` — no state root, no project — keeps that last resort, which
/// is what a binary run from a directory with no project above it gets.
///
/// The env read stays here in the binary (via [`state_dir`]); `vike_model::state_path` is pure and
/// vike-log depends on no crate at all, so neither can resolve this itself.
fn log_dir() -> Option<PathBuf> {
    Some(state_dir()?.join(vike_model::state_path::LOGS_SUBDIR))
}

/// Build the headless alerting mount, or `None` (the DEFAULT-OFF path).
///
/// The rules FILE is the gate — no separate env flag, the same absent-config-is-the-gate idiom the
/// venues use for credentials. `None` (no file, an unparseable file, zero rules, or every rule
/// disabled) means nothing is constructed at all: no `AlertEngine`, no `LogSink`, and no
/// `WebhookSink`. Delivery targets come from the engine's own pure
/// `webhook_configs_from_env` over THIS binary's credential map ([`workspace_credentials`]) —
/// never a second config path invented here, and never a store read performed inside the alerting
/// library. It is still passed as a THUNK, not a value, so on the OFF path the credential store is
/// not even opened and no Telegram token is ever loaded.
///
/// The thunk used to be `vike_ops::alerting::webhook_configs_from_workspace_env`, a library
/// function that opened the workspace `.env` itself — the class
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Moving the
/// read here is also a small correctness gain: this daemon resolves the credential store the same
/// way every other consumer does, so alert webhook targets are found where the venue credentials
/// already live.
///
/// **The OFF path is LOGGED, and that is deliberate.** Every other consequence of a mis-resolved
/// state directory announces itself — panes reset, a layout list comes back empty — but alerting's
/// steady state IS silence, so "no alerts arrived" is indistinguishable from "the rules file was
/// never found". One line naming the path the daemon actually consulted closes that, for every
/// cause at once: a mistyped `$VIKE_ALERTS`, a unit started from the wrong `WorkingDirectory`, a
/// file that parsed but holds no ENABLED rule. `info!` rather than `warn!` because alerting is
/// off-by-default and unconfigured is the normal state for most nodes — a warn on every daemon
/// that never wanted alerts is a warn nobody reads.
fn maybe_mount_alerts() -> Option<AlertMount> {
    let Some(path) = alerts_path() else {
        tracing::info!(
            "alerting: no rules file — ${ALERTS_ENV} is unset and no project settings directory \
             resolves above the working directory, so no alerts can fire"
        );
        return None;
    };
    let Some(mount) = alerts::maybe_mount(alerts::load_rules(&path), || {
        vike_ops::alerting::webhook_configs_from_env(&workspace_credentials())
    }) else {
        tracing::info!(
            ?path,
            "alerting: no enabled rules at this path, so no alerts can fire (a missing, \
             unparseable, empty or all-disabled rules file all land here)"
        );
        return None;
    };
    tracing::warn!(
        ?path,
        rules = mount.rule_count(),
        enabled = mount.enabled_count(),
        "alerting engine mounted (off-fold, driven by the snapshot summary tick): Price / Drawdown \
         / ReconAlert rules can fire here; Fill / OrderRejected / Indicator / Feed / \
         FillRateBreaker / PolymarketResolution / SeriesStale rules load but have no source in the \
         daemon yet"
    );
    Some(mount)
}

/// Start the authenticated node server (observe always; order-control gated) + snapshot publisher IFF
/// an ADDRESS was configured. Returns the [`PublisherHandle`] so the bounded teardown can stop it.
///
/// Both gates arrive as PARAMETERS, resolved once by [`resolve_settings`] at the top of [`main`]:
/// `addr` is `config.tradehub_addr` (`config.toml`, still overridden by `VIKE_TRADEHUB_ADDR`) and
/// `control_enabled` is `flags.tradehub_control` (`flags.toml`, still overridden by
/// `VIKE_TRADEHUB_CONTROL`). They used to be `std::env::var` reads RIGHT HERE, which is why the file
/// layer did nothing at all: a validated `tradehub_addr = "127.0.0.1:7979"` in `config.toml` left
/// nothing listening and printed no line saying so, while `vike-cli config show` reported the file as
/// its origin. Taking them as arguments also puts the decision to open a remote order-write surface
/// at ONE call site, where a diff can see it.
///
/// - `addr = None` (or blank) ⇒ `None`: the daemon is byte-identical to the pure-stdio PR-9 daemon.
/// - Addr set but no `VIKE_TRADEHUB_OBSERVE_KEY` in the credential store ⇒ logged + `None` (the
///   absent-credential-is-the-gate convention: no key, no server), and the daemon keeps trading
///   headless. The keys are read HERE — the daemon binary owns that I/O via
///   [`workspace_credentials`]; the light `vike-tradehub-client` crate must not.
/// - A bind failure is likewise logged and non-fatal (the daemon keeps trading).
/// - A NON-LOOPBACK address ⇒ logged + `None` unless `allow_public_bind` — see below.
///
/// ## ⚠ The non-loopback refusal (`allow_public_bind` = `flags.tradehub_allow_public_bind`)
///
/// `crate::server`'s handshake is PLAINTEXT and authenticates the CONNECTION, not each
/// frame — so on a reachable network it hands out an offline cracking target for the node key and,
/// after `AuthOk`, an on-path attacker can inject a `Command`. The design answer is a loopback
/// listener plus an SSH tunnel (`ssh -L 7879:localhost:7879 the CI box`), the same way `vike-datahub` is
/// reached; `server::DEFAULT_ADDR` is loopback for that reason. Nothing ENFORCED it: `check_addr`
/// only requires a `:`, so `tradehub_addr = "0.0.0.0:7879"` — which is simply what one types for a
/// server — published an order-write surface with no warning.
///
/// So a non-loopback bind now needs a SECOND, differently-named opt-in
/// (`flags.tradehub_allow_public_bind`, or `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1`). It is a flag
/// rather than a refusal without an escape hatch because reaching a node from a trusted LAN or
/// from inside a WireGuard/VPN interface is legitimate, and a guard people cannot turn off is a
/// guard they route around. It is a SEPARATE flag rather than an inference from the address
/// because the mistake this catches is *typing an address*, and no address can be its own consent.
/// ON, the bind proceeds behind a `warn!` naming what is now exposed — never silently.
///
/// The publisher reads ONLY the arc-swap snapshot cell (`CoreHandle::snapshot_cell`), never the core
/// fold, and the accept loop runs on a detached thread. ORDER CONTROL is double-gated: `Scope::Control`
/// is offered only when `control_enabled` — off, the control key is zeroed AND no `CommandSink` is
/// passed to `serve`, so a `Control` auth / `Command` is refused two ways.
// The REQ-2 advertisement is the 8th parameter. Grouping them into a struct would HIDE the
// property this signature exists for: every input that decides whether a remote order-write
// surface opens is spelled at the one call site, visible in a diff (see the call in `main`).
#[allow(clippy::too_many_arguments)]
fn start_observe_server(
    handle: &CoreHandle,
    identity: vike_tradehub_client::wire::WireNodeIdentity,
    mounts: Vec<vike_tradehub_client::wire::WireMountRow>,
    addr: Option<&str>,
    control_enabled: bool,
    allow_public_bind: bool,
    // The `SettingsShow`/`SetSetting` source (REQ-7): the boot-resolved settings directory, this
    // binary's ONE startup env sweep and the hot-apply seam, built by the CALLER — it owns all
    // three facts (the settings-registry rule), and this function only hands the handle on.
    settings_source: server::SettingsShowSource,
    // The REQ-2 advertisement (`config.datahub_advertise_addr`), likewise caller-owned: this
    // function stamps it into `Welcome` and decides nothing about it.
    datahub_advertise: Option<&str>,
) -> Option<PublisherHandle> {
    let addr = match addr.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => a.to_string(),
        None => return None,
    };
    // Resolve ONCE, exactly as `TcpListener::bind` will, and classify before anything else is built
    // — no key read, no publisher spawned, no socket opened on a refusal. An address that resolves
    // to nothing is left to `bind` to reject with its own message (below), as it always was.
    let resolved: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match server::bind_decision(&resolved, allow_public_bind) {
        server::BindDecision::Proceed => {}
        server::BindDecision::ProceedExposed(exposed) => {
            tracing::warn!(
                %addr, %exposed, control = control_enabled,
                "node server binding a NON-LOOPBACK address (flags.toml \
                 `tradehub_allow_public_bind`, or VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1) — the node \
                 handshake is PLAINTEXT and authenticates the connection, not each frame, so \
                 anyone who can reach this address can collect the nonce+mac for offline cracking \
                 and, once a session is up, inject frames. Put a tunnel or a VPN in front of it"
            );
        }
        server::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "node-server address is NOT loopback — observe server NOT started. This surface's \
                 handshake is plaintext and (with control on) places REAL orders, so it is meant \
                 to be reached over an SSH tunnel: keep `tradehub_addr` on 127.0.0.1 and run \
                 `ssh -L 7879:localhost:7879 <host>`. If this host genuinely must listen on a \
                 trusted network, set `tradehub_allow_public_bind = true` in flags.toml (or \
                 VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1). The daemon keeps trading headless"
            );
            return None;
        }
    }
    // `auth::from_vars`, not `NodeKeys::from_vars`: the TYPE moved down to
    // `vike_datahub_client::node_auth` when 0025 gave the datahub the same handshake, so the
    // tradehub-NAMED constructor is a free function in this service's binding module. Same two key
    // names, same trimming, same credential-is-the-gate `None`.
    let keys = match vike_tradehub_client::auth::from_vars(&workspace_credentials()) {
        Some(k) if k.has(Scope::Observe) => k,
        _ => {
            tracing::error!(
                %addr,
                "a node-server address is configured but there is no VIKE_TRADEHUB_OBSERVE_KEY in the \
                 credential store — observe server NOT started (absent credential is the gate); \
                 daemon keeps trading headless"
            );
            return None;
        }
    };
    // Belt-and-suspenders: when control is OFF, ZERO the control key so it is never even loaded into
    // the server — a `Control` auth then cannot verify regardless of what the `.env` held.
    let keys = if control_enabled {
        keys
    } else {
        NodeKeys::new(keys.key_for(Scope::Observe).to_vec(), Vec::new())
    };
    // The command sink is WITHHELD unless control is enabled — the second, independent gate, so even
    // a mis-set control key can never reach the core without the master flag.
    let commands = control_enabled.then(|| handle.command_sink());
    if control_enabled {
        tracing::warn!(
            "order-control channel ENABLED (flags.toml `tradehub_control`, or VIKE_TRADEHUB_CONTROL=1) \
             — a Control-authenticated peer may place/cancel REAL orders"
        );
    }
    let limits = resolve_control_limits();
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "observe server bind failed; daemon keeps trading headless");
            return None;
        }
    };
    // `spawn_with_mounts` (split-plane I10): the per-mount rows travel as publisher process-static
    // data, exactly like the identity block.
    //
    // ⚠ A SINGLE-MOUNT daemon now passes ONE ROW rather than none. It used to pass none and let the
    // server's `StrategyStatus` arm derive the row from the identity block, and that derivation is
    // where `WireMountRow::live` picked up `WireNodeIdentity::live` — the process-wide
    // `flags.tradehub_live` gate — as its answer to a per-VENUE question. The row `main` now builds
    // carries the identity's own `strategy` and `params` strings, so the wire answer is
    // byte-identical apart from `live`, which is taken from the mount's arming record instead. The
    // server's fallback arm STAYS: it still serves an identity-only publisher (`publish::spawn`).
    let publisher = publish::spawn_with_mounts(handle.snapshot_cell(), Some(identity), mounts);
    let server_publisher = publisher.clone();
    // The REQ-2 datahub advertisement (`config.toml`'s `datahub_advertise_addr`, or
    // VIKE_DATAHUB_ADVERTISE_ADDR): set ⇒ every Welcome carries `datahub=<addr>` and a client
    // with no explicit `datahub_addr` of its own dials the datahub there. Blank-trimmed like
    // `addr` above; unset advertises nothing (the pre-REQ-2 Welcome). ⚠ The REQ-7 settings source
    // is NOT built here any more — the caller owns it (see this function's parameter), which is
    // why only the advertisement is normalized at this line.
    let datahub_advertise =
        datahub_advertise.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    let advertise_log = datahub_advertise.clone();
    std::thread::Builder::new()
        .name("vt-tradehub-node-accept".into())
        .spawn(move || {
            if let Err(e) = server::serve(
                listener,
                server_publisher,
                keys,
                commands,
                limits,
                Some(settings_source),
                datahub_advertise,
            ) {
                tracing::error!(error = %e, "node server accept loop exited");
            }
        })
        .expect("spawn node accept thread");
    tracing::info!(
        %addr,
        control = control_enabled,
        datahub_advertise = advertise_log.as_deref().unwrap_or("(none)"),
        "tradehub node server listening (authenticated; observe always, control gated by \
         flags.toml `tradehub_control` / VIKE_TRADEHUB_CONTROL)"
    );
    Some(publisher)
}

/// The deployment's per-order notional CEILING — `max_notional_per_order` in
/// `<vike home>/policy.toml` — resolved exactly once by [`resolve_settings`] at the top of [`main`].
///
/// A process-wide `OnceLock` rather than a threaded parameter because it is a property of the
/// MACHINE, not of a server or a channel, and both control surfaces must see the identical value.
/// It replaces an `env::var` that was re-read on every call to [`resolve_control_limits`], so this
/// is less global state than before, not more.
static POLICY_MAX_NOTIONAL: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();

/// **`<project>/settings/state` as the ONE boot walk resolved it** — `vike_boot::Booted::state_dir`,
/// stored by [`resolve_settings`] so [`state_dir`] never has to walk for it again.
///
/// It is a `OnceLock` for the same reason [`POLICY_MAX_NOTIONAL`] is: the state root is a property
/// of the PROCESS, and its four consumers ([`log_dir`], [`alerts_path`], [`telegram_ledger_paths`]
/// and — through them — everything they write) sit in four unrelated places, none of which is
/// reachable from `main`'s locals.
///
/// ⚠ **`None` inside the cell is a legitimate answer** (no project above the working directory);
/// an UNSET cell means [`resolve_settings`] has not run, which cannot happen after `main`'s first
/// statement and which [`state_dir`] treats as "no state root" rather than silently walking — a
/// fallback walk is the exact defect this static removes.
static SETTINGS_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// **`<project>/settings` as the ONE boot walk resolved it** — `vike_boot::Booted::settings_dir`,
/// stored by [`resolve_settings`] so [`tick_store_root`] never has to walk for it again.
///
/// A SEPARATE cell from [`SETTINGS_STATE_DIR`] rather than that one's parent, for the reason
/// vike-app keeps the same two apart: the `state/` derivation belongs to `vike-boot`, which already
/// performs it, and re-deriving one from the other in the opposite direction is how two answers
/// start. It is also NOT [`state_dir`] — `$VIKE_STATE_ROOT` relocates the state tree and has never
/// moved a data store.
///
/// ⚠ Gated with the two recording features because [`tick_store_root`] is its only consumer and is
/// gated with them; a DEFAULT daemon build compiles neither, and an ungated static here would be
/// dead code under `-D warnings`.
///
/// `None` inside the cell is a legitimate answer (no project above the working directory); an UNSET
/// cell means [`resolve_settings`] has not run, which cannot happen after `main`'s first statement.
#[cfg(any(feature = "record-feeds", feature = "materialize"))]
static SETTINGS_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// The REAL process environment, swept ONCE at startup by [`resolve_settings`] — where the settings
/// directory (`VIKE_SETTINGS_DIR`) is named, when a deployment names it. See
/// [`workspace_credentials`].
///
/// It must be the REAL process env and not the credential map: a systemd unit's `Environment=` /
/// `EnvironmentFile=` line is the only channel an unattended daemon has, and a store cannot name
/// where it itself lives.
static PROCESS_ENV: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// The credential store, read ONCE per process. See [`workspace_credentials`] for why once.
static CREDENTIALS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// **The credential store: `<project>/settings/secrets.env`.**
///
/// The one place this daemon asks "what credentials do I have" — the live mount, the node server's
/// auth keys, the alert webhooks, and (under its feature) the Telegram control channel all come
/// through here.
///
/// An unreadable store returns an EMPTY map with a `tracing::error!` — every venue stays paper and
/// the daemon fails loudly at the venue, instead of signing orders with whichever credentials
/// happened to load. `crates/vike-bridge-core/tests/credential_chain_roots.rs` gates it.
///
/// ⚠ **The read is memoized, and that is a correctness property, not a micro-optimization.** Four
/// call sites reach this function and up to four of them run on one start; each used to perform a
/// COMPLETE, independent load — `resolve_project` → `read_to_string` → `parse_dotenv` — of a
/// plaintext file holding live venue API secrets. Two consequences, both observed on a clean
/// install:
///
/// 1. The store's PERMISSION finding is emitted by `try_load_workspace_secrets_at` on every
///    invocation (deliberately: "no caller of either wrapper can forget to surface it"). So a 0644
///    store logged the identical `WARN … readable beyond its owner …` line TWICE per start —
///    measured, on a paper daemon with a node address and the Telegram channel up. A warning that
///    repeats reads as two findings, and the operator goes looking for the second file.
/// 2. Every extra call re-read the secrets off disk and materialised a second copy of every venue
///    key in the process. Reading a credential file once is simply the smaller surface.
///
/// The two static `OnceLock`s answer different questions and neither subsumes the other:
/// [`PROCESS_ENV`] caches the `std::env::vars()` SWEEP (which is what names `VIKE_SETTINGS_DIR`, so
/// it must be the real process env), while this one caches the resulting STORE READ. Only the first
/// existed; that is why the sweep happened once and the file read happened four times.
///
/// The clone is per call and deliberate: `NodeConfig` owns its `vars`, and the Telegram channel
/// takes this function as a `fn() -> HashMap<..>` pointer, so an owned map is the shape every caller
/// already wants. What is saved is the I/O and the log line, not the allocation.
fn workspace_credentials() -> HashMap<String, String> {
    CREDENTIALS
        .get_or_init(|| {
            vike_bridge_core::credentials::load_workspace_secrets_from_env(process_env())
        })
        .clone()
}

/// [`PROCESS_ENV`], borrowed — the ONE sweep this binary owns, handed to the pure library functions
/// that take configuration as a parameter (`vike_config::boot_lines`, the credential loader above).
///
/// `get_or_init` rather than `get().expect(..)`: [`resolve_settings`] fills it at startup, but a
/// helper that PANICS when called before it would make the ordering of two startup steps a crash
/// risk rather than a detail.
fn process_env() -> &'static HashMap<String, String> {
    PROCESS_ENV.get_or_init(|| std::env::vars().collect())
}

/// Startup step 0 — before the LOG SUBSCRIBER, the profile, or any mount: refuse a stale
/// environment, then load the whole of `<project>/settings/`.
///
/// 1. **Refuse.** `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` is no longer read (Phase 5 of the
///    settings-unification design — `docs/superpowers/specs/2026-08-04-settings-unification-design.md`).
///    An operator who set it in a unit file believes a ceiling is armed on this node; a build that
///    quietly ignored it would accept remote orders of ANY size while they believed otherwise.
///    That is strictly worse than either keeping the variable or refusing to boot, so the daemon
///    refuses, and `vike_config::refuse_removed_env` names the file and key that replace it.
/// 2. **Resolve.** Load the settings directory. A missing file is NOT an error — it is the
///    permissive default, byte-identical to a pre-Phase-5 daemon with nothing set. A file that
///    exists and is broken IS an error: a deployment that wrote a ceiling and typo'd the key must
///    not silently run without one.
///
/// The BINARY owns the environment read, per the settings-registry rule; `vike_config` never
/// touches `std::env` itself.
///
/// **RETURNS the whole [`vike_config::Settings`], not just the policy**, and that is the shape the
/// wiring needed: this daemon reads `config.log_dir`, `config.tradehub_addr`,
/// `config.datahub_advertise_addr`,
/// `preferences.log_level`, `preferences.log_file_level` and five `flags.*` fields out of it, on top
/// of the policy the live mount projects onto `vike_run::MountPolicy`. **Exactly one load DECIDES
/// what this daemon does**, and every consumer above reads that one value.
///
/// ⚠ It is no longer the only `vike_config::load` in the process, and the distinction is the whole
/// point of the sentence. `vike_config::boot_lines` re-loads through `vike_config::provenance`,
/// because a row's ORIGIN cannot be recovered from a merged `Settings` — that read is a DISCLOSURE
/// and its result reaches nothing but the log. What must never appear is a second load whose value
/// is CONSUMED: two answers to "what is the ceiling" is the class of bug the ceilings are
/// file-only to avoid. The disclosure read is safe because it is given this function's own
/// `settings_dir` and this binary's own [`PROCESS_ENV`] sweep, so it can only differ by re-reading
/// the same files microseconds later — and a file edited inside that window is the one case where
/// the log showing the NEWER value is the useful answer.
///
/// **…and the settings DIRECTORY is returned beside the settings**, for that reason: the startup
/// disclosure in [`main`] describes the directory that was actually loaded from rather than walking
/// for one of its own. Two walks that could answer with two different projects is precisely the
/// failure the disclosure exists to surface (the CI box: an unrelated directory, no policy, no
/// credentials, every venue silently paper), so the disclosure must not be able to reproduce it.
///
/// ⚠ **It logs NOTHING, on purpose.** It runs before `vike_log::init` (the log destination and both
/// levels are among the settings it resolves), so a `tracing` call here would go to no subscriber at
/// all. Everything it would have said is emitted by [`main`] the moment the subscriber exists —
/// `settings_warning_lines` for the loader's own resolutions, and `vike_config::boot_lines` for what
/// was resolved and from where.
fn resolve_settings(
    env: &HashMap<String, String>,
    cwd: Option<&Path>,
) -> Result<vike_boot::Booted, String> {
    let vars: HashMap<String, String> = env.clone();
    // The SAME sweep serves the credential chain (see [`PROCESS_ENV`] / [`workspace_credentials`]),
    // so the settings directory is resolved once, here, at the root — before anything mounts a
    // venue. It is `set` BEFORE the boot below because [`workspace_credentials`], which the boot
    // calls, reads it.
    let _ = PROCESS_ENV.set(vars.clone());

    // ⚠ THE ORDER BELOW IS `vike-boot`'s, not this file's, and four other composition roots run the
    // same one. It used to be written out here — refuse, arm-check, walk, load — and in four other
    // `main`s besides, which is what made the CI box's failure expensive: the walk happening in five
    // places is five places to fix and five chances for two of them to disagree.
    vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        settings: vike_boot::SettingsLoad::Load,
        // ...and the credential file may not ARM REAL MONEY. `secrets.env` is plaintext and parsed
        // last-wins, so appending ONE line to it — with no read access at all — would otherwise be
        // enough to flip this daemon onto a live venue. `vike-boot` runs the refusal at step 0,
        // before the settings load and a long way before every mount, precisely because this
        // process signs real orders. It does NOT change which sources arm
        // (`vike_bridge_core::mainnet`'s converged rule still reads both, deliberately); it makes an
        // arming credential file STOP the process rather than run it. See `vike_config::arming`.
        //
        // The LOADER is this daemon's own ([`workspace_credentials`], memoized), passed as a
        // function: `vike-boot` owns WHEN the store is opened and this binary owns HOW, which is
        // what keeps that crate free of `vike_bridge_core`'s transport stack.
        credentials: vike_boot::Credentials::LoadWith(&workspace_credentials),
        log_home: vike_boot::LogHome::Elsewhere(
            "this daemon's log home hangs off `$VIKE_STATE_ROOT` when set — see [`state_dir`] and \
             [`log_dir`] — which relocates the whole STATE tree (alerts.json included) \
             independently of where the settings were read from. `<settings>/state/logs` is only \
             its fallback, and it is `Booted::state_dir` (this boot's OWN walk) that supplies it \
             — see [`SETTINGS_STATE_DIR`].",
        ),
        disclosure: vike_boot::Disclosure::Render,
    })
    .inspect(|booted| {
        let _ = POLICY_MAX_NOTIONAL.set(booted.settings.policy.max_notional_per_order);
        // The rung UNDER `$VIKE_STATE_ROOT`, taken from the boot rather than walked for again —
        // see [`state_dir`]. Set here, inside the one function that runs before anything reads it.
        let _ = SETTINGS_STATE_DIR.set(booted.state_dir.clone());
        // ...and the settings directory itself, which is what the DATA store's project rung is
        // taken beside — see [`SETTINGS_DIR`]. Same one walk, so the tape cannot land in a
        // different project than the credentials that signed the orders it records.
        #[cfg(any(feature = "record-feeds", feature = "materialize"))]
        let _ = SETTINGS_DIR.set(booted.settings_dir.clone());
    })
}

/// Every non-fatal resolution the loader made, formatted for the daemon's log — the PURE half of
/// [`resolve_settings`]'s surfacing step.
///
/// It exists as a function rather than an inline `for` loop for one reason: "the binary that loaded
/// the settings LOGS the warnings, never swallows them" is a real property with a real failure mode
/// (a clamped preference silently taking effect while the operator believes their number is in
/// force), and a property worth stating is worth gating. `vike_config` deliberately returns these as
/// DATA — it depends on serde + toml + vike-model and NOT on `tracing`, because a library that
/// writes to stderr on its own initiative cannot be used by a binary whose stdout is a protocol,
/// which this daemon's is. So the obligation to emit them is the binary's, and
/// `a_clamp_warning_is_surfaced_not_swallowed` below is what holds it.
fn settings_warning_lines(settings: &vike_config::Settings) -> Vec<String> {
    settings.warnings.iter().map(|w| format!("settings: {w}")).collect()
}

/// The server-edge control limits (PR-12 defense-in-depth), resolved ONCE at startup and fixed for
/// the process lifetime. Deliberate behavior change from the old in-library per-connection re-read:
/// limits no longer hot-reload per accepted connection (never a documented feature); a changed
/// limit lands on daemon restart.
///
/// The two halves come from DIFFERENT authorities, which is the Phase-5 point:
/// - the NOTIONAL ceiling from [`POLICY_MAX_NOTIONAL`] (the policy file — no env layer at all;
///   `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` was removed because a ceiling a stale unit file can raise
///   is not a ceiling), and
/// - the command RATE still from `VIKE_TRADEHUB_CONTROL_RATE`, read HERE in the binary (audit F13,
///   the settings-registry rule) — a throughput knob, not a risk ceiling: raising it cannot place a
///   larger order.
///
/// The resolution itself stays the pure, unit-tested [`server::ControlLimitsConfig::from_policy`].
///
/// ONE config, every control SURFACE: the TCP server ([`start_observe_server`]) and — under the
/// `telegram` feature — the Telegram channel (`maybe_start_telegram`) both build their own token
/// bucket from this same resolved value, exactly as each TCP connection does.
fn resolve_control_limits() -> server::ControlLimitsConfig {
    server::ControlLimitsConfig::from_policy(
        POLICY_MAX_NOTIONAL.get().copied().flatten(),
        process_env().get("VIKE_TRADEHUB_CONTROL_RATE").map(String::as_str),
    )
}

/// The at-most-once Telegram `update_id` ledger's basename inside the project state directory.
///
/// **`.ledger`, not the `.log` it used to be.** The file is not a log and never was — it holds one
/// integer per line and its only reader takes the max. The old suffix invited exactly the reading
/// that produced the defect this move fixes ("it's a log, best-effort is fine"), and
/// `<settings>/state/` is a SHARED directory, so a name that says what the file IS earns its keep —
/// the same reasoning `crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `TOKEN_FILE` records
/// for `ctrader_token.json`.
#[cfg(feature = "telegram")]
const TELEGRAM_LEDGER_FILE: &str = "telegram_updates.ledger";

/// The PRE-MOVE location's basename, read once at open so an existing install's mark migrates
/// instead of replaying: `<exe_dir>/telegram_updates.log`.
#[cfg(feature = "telegram")]
const LEGACY_TELEGRAM_LEDGER_FILE: &str = "telegram_updates.log";

/// Where the at-most-once ledger lives: `<state_dir>/telegram_updates.ledger`, plus the legacy
/// `<exe_dir>` copy to migrate from. `None` when no state directory resolves — see
/// `crate::telegram::maybe_spawn`, which refuses to arm the channel rather than inventing a
/// second location.
///
/// ⚠ **It used to be `<exe_dir>/telegram_updates.log`, and that was a production defect.**
/// `deploy/vike-tradehub.service` runs `ProtectSystem=strict` with `ExecStart=<project>/bin/…`, so
/// the executable's directory is READ-ONLY — every append failed, and the writes were best-effort,
/// so it failed SILENTLY. What degraded was the at-most-once record of a remote order-origination
/// path. The cure is not to make the exe directory writable (a daemon that can rewrite its own
/// binary is a worse problem); it is to put program-written state where program-written state
/// goes — `crates/vike-model/src/state_path.rs`'s `project_state_dir`, the one root
/// `<state_dir>/alerts.json` already resolves to, and the one the unit grants `ReadWritePaths=` for.
///
/// Deliberately NOT its own env knob: `$VIKE_STATE_ROOT` and `$VIKE_SETTINGS_DIR` already relocate
/// it, and the settings registry is better off without a third row that only names a path.
#[cfg(feature = "telegram")]
fn telegram_ledger_paths() -> Option<crate::telegram::LedgerPaths> {
    Some(crate::telegram::LedgerPaths {
        path: state_dir()?.join(TELEGRAM_LEDGER_FILE),
        legacy: legacy_telegram_ledger_path(),
    })
}

/// The pre-move ledger path, `<exe_dir>/telegram_updates.log` — READ at open to carry an existing
/// install's mark forward, never written. `None` when the executable's directory is unknowable,
/// which simply means there is nothing to migrate.
#[cfg(feature = "telegram")]
fn legacy_telegram_ledger_path() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|d| d.join(LEGACY_TELEGRAM_LEDGER_FILE))
}

/// Mount the TELEGRAM control channel, or return `None` having constructed NOTHING.
///
/// FOUR simultaneous gates (see [`crate::telegram`]'s module doc): the `tradehub_control` and
/// `telegram_control` FLAGS (`flags.toml`, each still overridden by `VIKE_TRADEHUB_CONTROL` /
/// `VIKE_TELEGRAM_CONTROL`), plus a `VIKE_TELEGRAM_BOT_TOKEN` and a non-empty
/// `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` (both from the credential store). The two flags arrive as
/// PARAMETERS — resolved once by [`resolve_settings`], not re-read here — and are still evaluated by
/// the pure `telegram::control_gates_open`, which keeps its own exact-`"1"` grammar; a resolved
/// `false` is passed as `None` (absent), which that grammar already means "closed". The credential
/// map, [`workspace_credentials`], is handed over as a FUNCTION (the
/// `maybe_mount_alerts` idiom) — so on the OFF path no credential store is opened at all, the bot
/// token never enters this process, no `ureq` agent is built, and no thread is spawned. That makes
/// the default daemon byte-identical to the pre-Telegram one.
///
/// The channel gets its OWN `ControlLimits` bucket over the SAME [`resolve_control_limits`] config
/// the TCP server uses, and a `CommandSink` clone — the same handle `start_observe_server` threads
/// into `serve`. Nothing here touches the core fold: commands go through the non-blocking ingest
/// lane and reads come off the lossy arc-swap snapshot cell. (The sink/cell clones are made before
/// the gate is consulted purely because the deps builder closure captures them; on the closed path
/// the closure is dropped un-called, and an in-process channel handle nobody can send through
/// grants nothing — the gate is on whether a REMOTE surface exists at all.)
///
/// A FIFTH, compile-time gate sits above all four: the crate's off-by-default `telegram` feature.
/// Without it this function, its ledger-path helpers, its env-name constant and the whole
/// `crate::telegram` module are absent from the binary — the four runtime gates protect a
/// path that exists, the feature makes the path not exist.
///
/// And one PRECONDITION sits below all five, checked inside `maybe_spawn` after the gates: the
/// at-most-once ledger ([`telegram_ledger_paths`]) must be readable and appendable. It is not a
/// gate in the same sense — an operator does not choose it — but it fails the same way, loudly and
/// closed: no channel, one `error!` naming the cause, and a daemon that keeps trading headless.
#[cfg(feature = "telegram")]
fn maybe_start_telegram(
    handle: &CoreHandle,
    tradehub_control: bool,
    telegram_control: bool,
) -> Option<crate::telegram::TelegramHandle> {
    let limits = resolve_control_limits();
    let sink = handle.command_sink();
    let cell = handle.snapshot_cell();
    crate::telegram::maybe_spawn(
        as_gate(tradehub_control),
        as_gate(telegram_control),
        workspace_credentials,
        telegram_ledger_paths(),
        move |cfg| {
            Box::new(crate::telegram::ProdTelegramDeps::new(cfg, limits, sink, cell))
                as Box<dyn crate::telegram::TelegramDeps + Send>
        },
    )
}

/// Print the `ready` banner (synchronously, so it precedes everything the thread prints), then run
/// the stdin control channel on its OWN thread with a handle on the shared stop flag.
///
/// It is a detached thread on purpose. Under systemd stdin is `/dev/null`, so the loop hits EOF at
/// once, logs, and ends; on a TTY it blocks in a read that nothing can interrupt, and the process
/// exit after the teardown is what reaps it. Either way it must never be joined — joining a thread
/// parked in a terminal read is a hang, and this is a SHUTDOWN path.
///
/// It takes the core's cloneable capabilities rather than the [`CoreHandle`] itself, because the
/// handle is MOVED into the teardown below (`shutdown_and_join` consumes it) and a `'static` thread
/// cannot borrow it: [`vike_core::CommandSink`] for the command lane and the arc-swap snapshot cell
/// for `status`. Both are exactly what the network control server already holds.
fn spawn_stdin_control(handle: &CoreHandle, token: String, mode: String, stop: Arc<AtomicBool>) {
    println!(
        "{}",
        serde_json::json!({
            "kind": "ready",
            // PAPER vs LIVE (venue=<v>) — an operator always knows the mode from the ready banner.
            "mode": mode,
            "control": "newline-JSON vike_exec::Command on stdin; words: shutdown|quit|exit|status|help"
        })
    );
    let _ = std::io::stdout().flush();

    let sink = handle.command_sink();
    let cell = handle.snapshot_cell();
    std::thread::Builder::new()
        .name("vt-tradehub-stdio".into())
        .spawn(move || {
            let is_tty = std::io::stdin().is_terminal();
            control_loop(
                std::io::stdin().lock(),
                is_tty,
                &stop,
                // LOSSLESS, exactly as `CoreHandle::send_command` was here before: an operator's
                // typed order must not be dropped because the ingest lane was momentarily full.
                |cmd| sink.send_blocking(cmd),
                || summary_line(&cell.load_full(), &token),
            );
        })
        .expect("spawn vt-tradehub-stdio thread");
}

/// The stdin control channel's whole behaviour, as a function of its LINES, one boolean, and two
/// callbacks — so the rule below can be tested instead of trusted.
///
/// Reads newline-delimited input: a bare control word (`shutdown`/`quit`/`exit`/`status`/`help`) is
/// the interactive convenience; anything else is parsed as a JSON [`vike_exec::Command`] and lowered
/// through `send`. Command-decode errors go to STDERR so STDOUT stays protocol-only.
///
/// ⚠ **The rule a reader gets wrong: EOF is a stop only on a TTY.** Under systemd stdin is
/// `/dev/null` and reads EOF the instant the daemon starts, so treating EOF as a stop would exit the
/// daemon at startup, every start, on every box. That is also precisely why the stdio channel could
/// never be the systemd stop path, and therefore why `vike_ops::stop` exists.
fn control_loop(
    reader: impl BufRead,
    is_tty: bool,
    stop: &AtomicBool,
    mut send: impl FnMut(Command),
    mut status: impl FnMut() -> String,
) {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("stdin read error: {e}");
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed {
            "shutdown" | "quit" | "exit" => {
                stop::request_stop(stop);
                return;
            }
            "help" => {
                println!(
                    "{}",
                    serde_json::json!({
                        "kind": "help",
                        "commands": "newline-JSON vike_exec::Command (e.g. {\"Order\":{\"Submit\":{…}}}); words: shutdown|quit|exit|status|help"
                    })
                );
                let _ = std::io::stdout().flush();
            }
            "status" => {
                println!("{}", status());
                let _ = std::io::stdout().flush();
            }
            _ => match serde_json::from_str::<Command>(trimmed) {
                Ok(cmd) => {
                    send(cmd);
                    println!("{}", serde_json::json!({ "kind": "ack" }));
                    let _ = std::io::stdout().flush();
                }
                Err(e) => {
                    // STDERR, never STDOUT: keep stdout the clean protocol surface.
                    eprintln!("vike-tradehub: not a valid JSON vike_exec::Command: {e}");
                }
            },
        }
    }
    if is_tty {
        // Ctrl-D from a human at a terminal is an explicit stop.
        stop::request_stop(stop);
    } else {
        // ⚠ On Windows this line lands on precisely the run that CANNOT be Ctrl-C'd. A non-tty
        // stdin is the background shape, and a background process has no console for a control
        // event to arrive from — so naming Ctrl-C here would send an operator to the one stop route
        // this run does not have. Name the file instead; it is the only one that works detached.
        #[cfg(windows)]
        tracing::info!(
            stop_file = %vike_ops::stop::STOP_FILE_NAME,
            "stdin control channel closed (non-tty) — the daemon keeps trading headless. Stop it \
             with Ctrl-C if this run HAS a console, or by creating the stop file named above in \
             <project>/settings/state if it does not (docs/ops/tradehub-windows.md)"
        );
        #[cfg(not(windows))]
        tracing::info!(
            "stdin control channel closed (non-tty) — the daemon keeps trading headless; stop via SIGTERM"
        );
    }
}

/// One-line JSON summary of the live snapshot for the STDOUT protocol surface. Pure over the
/// snapshot — a LOSSY read, never a fold. `token` selects the mount symbol's net position.
///
/// ⚠ **Every number here is CROSS-VENUE — the whole account, not one engine.** That is a
/// correctness property, not a preference: four of the seven used to read the PRIMARY engine
/// (`CoreSnapshot::build` binds `let acc = &engine.account`, and `vike_run::WIRED_MARKETS` lists
/// binance first, so the primary on a the CI box CEX node is the binance PAPER engine, which has traded
/// nothing) while `orders`/`working` iterated every engine and `equity` was
/// `Portfolio::equity_total`. Measured on the CI box: the bybit mount took ten maker fills and moved the
/// balance, `equity` tracked them to eight decimal places, and the same line printed
/// `fees: 0.0, realized_pnl: 0.0, net_pos: 0.0, positions: 0` — a line that reads like an account
/// summary while four of its fields describe a different venue than the two that move. So the
/// scope rule is: **a field on this line may not be narrower than the equity fields'.**
///
/// Deliberately still one FLAT object rather than gaining a per-mount breakdown: this is a
/// fixed-arity metric row a `jq`/alerting consumer reads positionally, and a nested
/// variable-length array would make every parser's shape depend on how many mounts happen to be
/// up. Per-mount truth is not lost — it rides in `CoreSnapshot::mounts`, which the alerting engine
/// on this same thread already reads.
///
/// ## ⚠ There is no `equity` key, on purpose — a wallet and a book are not addable
///
/// This line used to carry one `equity`, and it was `Portfolio::equity_total`: the `py_sum` of
/// every venue block's equity. A block's equity means two different things depending on its
/// `BalanceMode` (`ExecutionEngine::mode_equity`) — a `Delta` block is `seed + own cash flow +
/// realized + unrealized`, this daemon's own book-keeping, while an `Authoritative` block is
/// `venue wallet + unrealized`, the venue's number for the WHOLE ACCOUNT the credentials open.
/// Summing the two produces a figure that is neither.
///
/// Measured on the CI box 2026-08-17: `VIKE_RECONCILE=1` made bybit authoritative, and this line's
/// `equity` jumped from ~10000 to **62647.10600813** — the shared bybit demo account's 53647 USDT
/// `walletBalance` (settlements observed on AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT, none of them
/// traded by this daemon) plus nine paper mounts' 1000 seed each. Nothing had gained 52 thousand
/// dollars; the report had added a wallet it does not own to seed cash that does not exist.
///
/// So the field is SPLIT, and each half names its own provenance in its own key —
/// [`vike_core::Portfolio::equity_book_total`] / [`vike_core::Portfolio::equity_wallet_total`],
/// which partition exactly the sum `equity_total` takes:
///
/// - **`equity_book`** — the mount's own accounting, summed over the `Delta` blocks. Seeds this
///   daemon chose, moved by fills this daemon booked.
/// - **`equity_wallet`** — the venue-attested observation, summed over the `Authoritative` blocks.
///   Whole-account wallets, adopted verbatim from `ReconClient::fetch_balance` / a live
///   `AccountState` push.
/// - **`wallet_venues`** — comma-joined venue ids behind `equity_wallet` (empty string when
///   none), so the reader can see WHOSE wallet was quoted without opening a log. Still fixed
///   arity: always present, always a string.
///
/// ## ⚠ `equity_book` is the WHOLE book, not the mounted set — hence `equity_book_mounted`
///
/// `build_node` seeds EVERY default-build venue engine with the primary mount's `seed_cash`, so a
/// daemon running two mounts still carries ten `Delta` blocks and `equity_book` sums all ten. The
/// I10 live rehearsal (`docs/ops/i10-rehearsal-2026-08-19.md`) measured exactly that: two mounts
/// seeded at 10k each, `equity_book` printing **100000.0**, and the note observing that "an
/// operator eyeballing the summary should know the number is not 'the two mounts' seeds'".
///
/// The figure is CORRECT and stays. What it lacked was the scoped companion an operator is
/// actually reading for, so the same treatment the `equity` split got is applied again — report
/// both, conflate neither, name the provenance in the key:
///
/// - **`equity_book_mounted`** — `equity_book` restricted to the venues this daemon has MOUNTED
///   (`CoreSnapshot::mounts`' `MountRowKind::Mount` rows), same `py_sum` law, same registration
///   order. On the rehearsal's profile this reads 20000.0 beside `equity_book`'s 100000.0.
/// - **`mounted_venues`** — comma-joined venues it was scoped to (empty when the core has mounted
///   nothing, which is also when the figure is `0.0` — the name list is how the reader tells
///   "scoped to nothing" from "nothing to scope"). Fixed arity, like `wallet_venues`.
///
/// A mounted venue that has flipped `Authoritative` is NAMED but contributes nothing to the book
/// figure — its equity is in `equity_wallet`, exactly as `equity_book`'s own partition demands.
/// `crate::summary::mounted_book_equity` is the implementation and carries the full
/// argument for why this is a NEW pair of keys rather than a narrowed `equity_book` (the
/// partition property, and the rule against silently changing a number a consumer already reads).
///
/// **Renaming rather than keeping `equity` as the sum is the deliberate half of this change.** A
/// consumer keyed on `.equity` now reads `null` and breaks loudly instead of silently reading a
/// number that means nothing; the only two consumers in this repo are this file's own tests and
/// `tests/sigterm_stop.rs`'s `"kind":"summary"` substring match, and `vike_tradehub_client` reads
/// the SEPARATE TCP observe wire (`WireSnapshot`), not stdout. On a pure-paper node
/// `equity_wallet` is `0.0` with an empty `wallet_venues`; on a fully-live node `equity_book` is
/// `0.0`. Either way a reader can tell WHICH quantity is in front of them from the key alone.
///
/// ⚠ **The daemon cannot decide this for the operator.** Adopting the venue balance is exactly
/// right on a DEDICATED account and is why `CoreThread::reconcile_reports` does it; it is wrong
/// on a SHARED one, and no venue API field distinguishes the two — the same
/// `crates/bridges/bybit/src/recon_client.rs` scopes `fetch_position_status_reports`/
/// `fetch_fill_reports` to the mount's symbol and `fetch_balance` to the whole account.
/// Reporting both and conflating neither is the honest
/// answer available to code; a dedicated sub-account is the operator's lever.
///
/// `net_pos` is `Portfolio::net_position` — the SIGNED sum of `token`'s legs across every venue,
/// not the primary's leg. Cross-venue netting is the right meaning here because that is what
/// `equity` beside it already does, and because a hedged basis pair genuinely IS flat; ⚠ it nets
/// on the symbol STRING, so it only nets legs that are the same instrument — two venues spelling
/// one underlying differently stay two rows, and a shared spelling across venues with different
/// contract multipliers sums CONTRACTS, not coins.
fn summary_line(snap: &CoreSnapshot, token: &str) -> String {
    let working = snap.orders.iter().filter(|o| !o.status.is_terminal()).count();
    let net_pos = snap.portfolio.net_position(token);
    // The MOUNTED-SET scoping (I10 rehearsal follow-up) — `crate::summary`, which owns
    // the argument for why this is a NEW pair of keys rather than a narrowed `equity_book`.
    let mounted = crate::summary::mounted_book_equity(snap);
    // `fault` is `Option<String>` — render it as an explicit JSON null / string.
    let fault = match &snap.fault {
        Some(f) => serde_json::Value::String(f.clone()),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "kind": "summary",
        "seq": snap.seq,
        "trading_state": format!("{:?}", snap.trading_state),
        "orders": snap.orders.len(),
        "working": working,
        "positions": snap.portfolio.position_count(),
        "net_pos": net_pos,
        "realized_pnl": snap.portfolio.realized_pnl_total(),
        "fees": snap.portfolio.fees_paid_total(),
        "equity_book": snap.portfolio.equity_book_total(),
        "equity_book_mounted": mounted.total,
        "mounted_venues": mounted.venues.join(","),
        "equity_wallet": snap.portfolio.equity_wallet_total(),
        "wallet_venues": snap.portfolio.wallet_venues().join(","),
        "fault": fault
    })
    .to_string()
}

// The DETERMINISTIC venue-feed splice test — its own file so `tests/venue_feed_splice_smoke.rs`'s
// module doc has one place to point at, and because it is a whole scripted venue double rather
// than one more case for the inline module below.
// ⚠ `#[path]` because this module sits BESIDE `tradehub_cli.rs` rather than under it. It resolved
// implicitly while this body was `src/main.rs` — a crate root — and a non-root module looks in a
// subdirectory named after itself.
#[path = "feed_splice_seam_tests.rs"]
#[cfg(test)]
mod feed_splice_seam_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------------
    // The ready banner (`ready_mode_line`) — the string `docs/ops/tradehub-the CI box.md` and
    // `deploy/vike-tradehub-project.service` both call the ONE authority on paper-vs-live.
    // ---------------------------------------------------------------------------------------------

    /// A `live_venues` ARMING RECORD, spelled the way `vike_run::build_node` hands one over.
    fn armed(venues: &[&str]) -> std::collections::HashSet<String> {
        venues.iter().map(|v| (*v).to_string()).collect()
    }

    /// **THE DEFECT, reproduced from the measurement that found it.**
    ///
    /// The record below is verbatim what the CI box's shipped daemon logged on one startup, and the
    /// banner it printed two lines later was `LIVE (venue=bybit)` — the single venue its profile
    /// mounted. Nine venues held live authenticated exec sessions and the operator's one authority
    /// on paper-vs-live named one of them.
    ///
    /// The expectation is written out LONGHAND rather than derived from the input, so the test
    /// cannot agree with a renderer that has the same bug (the declaration-pinning failure this
    /// repo has been bitten by three times).
    #[test]
    fn the_banner_names_every_armed_venue_not_the_one_the_profile_mounts() {
        let record = armed(&[
            "hyperliquid",
            "deribit",
            "okx",
            "bybit",
            "alpaca",
            "aster",
            "binance",
            "ig",
            "oanda",
        ]);
        assert_eq!(
            ready_mode_line(true, &record),
            "LIVE (venue=alpaca+aster+binance+bybit+deribit+hyperliquid+ig+oanda+okx)"
        );
        // ...and the shape the defect actually printed is now impossible from this record. Spelled
        // separately because a renderer that named only the FIRST armed venue would satisfy no
        // equality above but would still be the same class of lie.
        assert_ne!(ready_mode_line(true, &record), "LIVE (venue=bybit)");
    }

    /// Sorted, so an operator diffing two startups of ONE binary never sees a reordering and reads
    /// it as a change: `HashSet` iteration order is not a function of the contents alone.
    ///
    /// Two records built by inserting the same venues in OPPOSITE orders must render identically —
    /// and must render in the order written here, which is neither insertion order.
    #[test]
    fn the_banner_sorts_the_arming_record_rather_than_iterating_it() {
        let forwards = armed(&["okx", "binance", "aster"]);
        let backwards = armed(&["aster", "binance", "okx"]);
        assert_eq!(ready_mode_line(true, &forwards), "LIVE (venue=aster+binance+okx)");
        assert_eq!(ready_mode_line(true, &forwards), ready_mode_line(true, &backwards));
    }

    /// The LIVE GATE ON with NOTHING ARMED — the empty-credential-store shape every
    /// `venue_feed_splice_smoke` case runs the shipped binary in, and the shape a fresh deployment
    /// has before its first key is added.
    ///
    /// It must name the sentinel, and it must NOT collapse to `PAPER`: the gate being on is an
    /// operator-visible fact independent of what armed (live feeds, real prices, the B11
    /// live-account locks held, and one credential appearing arms real exec on the next start).
    #[test]
    fn an_armed_gate_with_nothing_armed_reads_none_and_not_paper() {
        let mode = ready_mode_line(true, &armed(&[]));
        assert_eq!(mode, "LIVE (venue=none)");
        assert_ne!(mode, "PAPER", "the live gate is ON — collapsing to PAPER would hide the arm");
        // Not a truncated `LIVE (venue=)`, which reads as a broken line rather than a statement.
        assert!(!mode.ends_with("venue=)"), "the empty set must render a WORD: {mode}");
    }

    /// The PAPER arm is byte-identical to the pre-fix daemon — the string, and nothing else.
    ///
    /// Asserted against a NON-EMPTY record too: a paper mount builds no live client by any path, so
    /// this pairing cannot occur, and the test exists to pin that the renderer answers from the GATE
    /// on that arm rather than falling through to the venue rendering if it ever did.
    #[test]
    fn the_paper_arm_is_untouched() {
        assert_eq!(ready_mode_line(false, &armed(&[])), "PAPER");
        assert_eq!(ready_mode_line(false, &armed(&["binance", "bybit"])), "PAPER");
    }

    /// The empty-set sentinel sits in a field whose every other value is a venue id, so it must not
    /// be capable of being one.
    ///
    /// `vike_model::VENUES` is DERIVED from the `crates/bridges/*` tree (its own roster test walks
    /// the directory), so a future bridge crate named `none` reddens here rather than silently
    /// making the banner ambiguous between "nothing armed" and "the `none` venue armed".
    #[test]
    fn banner_sentinel_is_not_a_venue_id() {
        assert!(
            !vike_model::VENUES.contains(&NO_VENUE_ARMED),
            "`{NO_VENUE_ARMED}` is now a venue id — the ready banner's empty-set sentinel must be \
             renamed to something the roster cannot contain"
        );
    }

    /// A seed per mount, venue-addressed the way `run` builds them.
    fn seeds(venues: &[&str]) -> Vec<WireMountSeed> {
        venues
            .iter()
            .map(|v| WireMountSeed {
                strategy: "buy_hold".to_string(),
                params: format!("venue={v} symbol=X interval=1m :: size=1"),
                venue: (*v).to_string(),
            })
            .collect()
    }

    /// The `StrategyStatus` row's `live` is a PER-VENUE fact, and the two ways it used to
    /// over-claim are both asserted here rather than described.
    ///
    /// One armed venue and two unarmed mounts in one daemon: the row set must SPLIT. Under the old
    /// `live: flags.tradehub_live` all three read `true`, which is a claim that three mounts place
    /// real orders when one does.
    #[test]
    fn a_mount_row_is_live_only_when_its_own_venue_armed() {
        let rows = wire_mount_rows(seeds(&["bybit", "okx", "oanda"]), &armed(&["bybit"]));
        let by_venue: Vec<bool> = rows.iter().map(|r| r.live).collect();
        assert_eq!(
            by_venue,
            vec![true, false, false],
            "only the armed venue's mount trades live; rows: {rows:?}"
        );
    }

    /// The `data_only = true` mount — the case the profile loader guarantees is REACHABLE, because
    /// it refuses that key unless the live gate is on.
    ///
    /// `withhold_exec_credentials` strips the venue's keys before `build_node`, so the venue never
    /// enters the arming record even though its FEED is credentialed and live. The row must read
    /// paper: its orders go to the paper book.
    #[test]
    fn a_data_only_mounts_row_reads_paper_even_though_its_feed_is_credentialed() {
        let rows = wire_mount_rows(seeds(&["oanda"]), &armed(&[]));
        assert!(!rows[0].live, "a withheld venue mounts paper exec, and the row must say so");
        // ...and the row is otherwise untouched — this fix changes ONE field.
        assert_eq!(rows[0].strategy, "buy_hold");
        assert_eq!(rows[0].params, "venue=oanda symbol=X interval=1m :: size=1");
    }

    /// A PAPER daemon hands over an empty record, so every row reads paper — byte-identical to the
    /// pre-fix answer on that arm, where `flags.tradehub_live` was `false` for the same rows.
    #[test]
    fn every_row_of_a_paper_daemon_reads_paper() {
        let rows = wire_mount_rows(seeds(&["bybit", "oanda"]), &armed(&[]));
        assert!(rows.iter().all(|r| !r.live), "rows: {rows:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // The exec badge is per ACCOUNT
    // ---------------------------------------------------------------------------------------------

    /// One arming row, through the same type `vike_run::venue_arming` produces.
    fn arming_row(venue: &'static str, label: Option<&str>) -> vike_config::VenueArming {
        vike_config::VenueArming {
            venue,
            label: match label {
                None => vike_model::account_keys::AccountLabel::Default,
                Some(l) => vike_model::account_keys::AccountLabel::parse(l).expect("a legal label"),
            },
            ceiling: vike_config::VenueMode::Live,
            effective: vike_config::VenueMode::Live,
            block: vike_config::ArmingBlock::None,
        }
    }

    /// **THE DEFECT.** The default `bybit` account has no credentials and mounts paper; a LABELLED
    /// `bybit` account armed real exec in this same process. The venue-keyed badge answered
    /// `live_venues.contains("bybit")` — false — and the daemon announced `exec = PAPER` beside a
    /// live authenticated bybit session.
    ///
    /// ⚠ The armed set is spelled through `VenueArming::route_key`, NOT as a `"bybit#ALT"` literal:
    /// a fixture that hand-writes the key it is about to look up passes with the renderer broken,
    /// which is the seeded-through-the-function-under-test failure this program has hit before.
    #[test]
    fn a_venue_whose_labelled_account_armed_does_not_announce_paper() {
        let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
        let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();

        let others = other_live_accounts("bybit", &rows, &live);
        assert_eq!(others, vec![rows[1].route_key()], "the ALT account's route key must be found");

        let announced =
            with_other_live_accounts(cex_arming(CexVenue::Bybit, false, false), "bybit", others);
        assert_eq!(announced.exec, EXEC_OTHER_ACCOUNT_LIVE);
        assert_ne!(announced.exec, EXEC_PAPER, "the badge must not read paper for a live venue");
        assert!(
            !announced.exec.starts_with(EXEC_PAPER),
            "…and must not answer an `exec=PAPER` grep either: {}",
            announced.exec
        );
        // The remedy still arms THIS mount, and now says what the paper verdict is about.
        let remedy = announced.remedy.expect("a paper mount still carries its remedy");
        assert!(remedy.contains("ALREADY LIVE"), "{remedy}");
        assert!(remedy.contains(&rows[1].route_key()), "the live account must be NAMED: {remedy}");
        assert!(remedy.contains("BYBIT_DEMO_API_KEY"), "the original remedy survives: {remedy}");
    }

    /// Both accounts armed: the badge still answers an `exec=LIVE` grep, and says there is more
    /// than one book behind it.
    #[test]
    fn a_venue_with_two_armed_accounts_says_so_and_still_reads_live() {
        let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
        let live: std::collections::HashSet<String> =
            rows.iter().map(vike_config::VenueArming::route_key).collect();
        let announced = with_other_live_accounts(
            cex_arming(CexVenue::Bybit, true, true),
            "bybit",
            other_live_accounts("bybit", &rows, &live),
        );
        assert_eq!(announced.exec, EXEC_LIVE_MULTI_ACCOUNT);
        assert!(announced.exec.starts_with(EXEC_LIVE), "an `exec=LIVE` grep must still match");
        assert_eq!(announced.other_live, vec![rows[1].route_key()]);
        assert!(announced.remedy.is_none(), "a live mount carries no remedy");
    }

    /// **A single-account box is BYTE-IDENTICAL.** Every row is a default-account row, so
    /// `other_live_accounts` is empty for every venue and `with_other_live_accounts` returns its
    /// input field for field — asserted against the UNWRAPPED producer, both armed and not.
    #[test]
    fn a_single_account_box_announces_exactly_what_it_did_before() {
        let rows: Vec<vike_config::VenueArming> =
            ["binance", "bybit", "okx", "aster"].into_iter().map(|v| arming_row(v, None)).collect();
        for armed_venues in [vec![], vec!["bybit"], vec!["binance", "bybit", "okx", "aster"]] {
            let live: std::collections::HashSet<String> =
                armed_venues.iter().map(|v| (*v).to_string()).collect();
            for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
                let slug = venue.slug();
                assert!(
                    other_live_accounts(slug, &rows, &live).is_empty(),
                    "{slug} has no second account on this box"
                );
                for mainnet in [false, true] {
                    let exec_live = live.contains(slug);
                    let bare = cex_arming(venue, mainnet, exec_live);
                    let wrapped = with_other_live_accounts(
                        cex_arming(venue, mainnet, exec_live),
                        slug,
                        other_live_accounts(slug, &rows, &live),
                    );
                    assert_eq!(bare, wrapped, "{slug} mainnet={mainnet} must be untouched");
                    assert_eq!(
                        wrapped.exec,
                        if exec_live { EXEC_LIVE } else { EXEC_PAPER },
                        "…and reads exactly the two badges it always read"
                    );
                }
            }
        }
        // The credentialed-data producers take the same trip.
        for (bare, slug) in [
            (alpaca_arming(false), "alpaca"),
            (ctrader_arming(true), "ctrader"),
            (oanda_arming(false), "oanda"),
            (deribit_arming(true), "deribit"),
            (ig_arming(false), "ig"),
        ] {
            let exec = bare.exec;
            let wrapped = with_other_live_accounts(bare, slug, Vec::new());
            assert_eq!(wrapped.exec, exec, "{slug} must keep its badge with no second account");
            assert!(wrapped.other_live.is_empty());
        }
    }

    /// The route key of ANOTHER venue's labelled account never leaks into this venue's answer, and
    /// a labelled account that did NOT arm is not reported as live.
    #[test]
    fn the_answer_is_scoped_to_the_venue_and_to_what_actually_armed() {
        let rows = vec![
            arming_row("bybit", Some("ALT")),
            arming_row("okx", Some("ALT")),
            arming_row("bybit", Some("HEDGE")),
        ];
        // Only okx#ALT armed.
        let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();
        assert!(
            other_live_accounts("bybit", &rows, &live).is_empty(),
            "okx's armed account must not appear under bybit"
        );
        assert_eq!(other_live_accounts("okx", &rows, &live), vec![rows[1].route_key()]);
    }

    /// `exec_badge` is total over its two inputs, and the four strings are distinct — so no state
    /// can be mistaken for another by a grep.
    #[test]
    fn the_four_exec_badges_are_distinct() {
        let all = [
            exec_badge(false, false),
            exec_badge(false, true),
            exec_badge(true, false),
            exec_badge(true, true),
        ];
        let mut dedup = all.to_vec();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), all.len(), "the four badges must be distinct: {all:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // The CEX (binance/bybit/okx) live arm.
    // ---------------------------------------------------------------------------------------------

    /// A `MakerMountConfig` for `venue` on the symbol `build_node` actually mounts it on — the same
    /// lowering `DaemonProfile::to_mount_config` produces for a CEX profile.
    fn cex_cfg(venue: CexVenue) -> MakerMountConfig {
        let symbol = wired_symbol_for(venue.slug()).expect("build_node mounts this venue");
        MakerMountConfig::crypto(venue.slug(), symbol, 0.5, 0.001)
    }

    /// **The slug table, pinned against an authority that is NOT `slug()` itself.**
    ///
    /// ⚠ This is the missing gate, and its absence was MEASURED: on the CI box, rewriting
    /// `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE vike-tradehub suite GREEN.
    /// Every other slug assertion here — `every_cex_venue_slug_names_a_wired_market`,
    /// `cex_plan_accepts_the_wired_symbol`, the recon feed-status key — reads its expectation FROM
    /// `slug()` and compares it against a set that contains all three strings, so no PERMUTATION of
    /// them can fail: the declaration-pinning failure mode this repo has been bitten by three times.
    ///
    /// The authority is each bridge crate's OWN public venue constant — the string that crate
    /// stamps on the ticks its pump emits and that its exec path signs under. That makes this a
    /// genuine cross-check rather than a hand copy of the match arms: `CexVenue::Binance` MUST be
    /// the venue `vike_binance` is, because `CexVenue::Binance`'s feed arm calls
    /// `vike_binance::market_data::spawn_binance_market_data` and nothing else.
    ///
    /// The literal half is asserted too, because the bridge constant is itself mutable — the two
    /// authorities would have to be changed together and in agreement to slip a wrong slug through.
    /// The `match` is EXHAUSTIVE, so a fourth CEX venue is a compile error here rather than a row
    /// somebody forgets.
    #[test]
    fn cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string() {
        for venue in CexVenue::ALL {
            let (bridge_const, literal) = match venue {
                CexVenue::Binance => (vike_binance::spot::VENUE, "binance"),
                CexVenue::Bybit => (vike_bybit::perp::VENUE, "bybit"),
                CexVenue::Okx => (vike_okx::perp::VENUE, "okx"),
                // Homed in `urls` (the feed-plane module) rather than an exec module — aster's
                // canonical id lives beside its host table so BOTH planes can name it
                // (`crates/bridges/aster/src/urls.rs`'s `VENUE`).
                CexVenue::Aster => (vike_aster::urls::VENUE, "aster"),
            };
            assert_eq!(
                venue.slug(),
                bridge_const,
                "{venue:?}'s slug must be the venue string its OWN bridge crate declares — the \
                 feed arm calls that crate's pump, while `make_engine` keys the ExecutionClient, \
                 the credential prefix and the {{VENUE}}_MAINNET switch on this slug, so a \
                 disagreement mounts one venue's book against another venue's account"
            );
            assert_eq!(
                venue.slug(),
                literal,
                "{venue:?}'s slug is pinned VERBATIM here as well as against the bridge constant, \
                 so that changing both in step is still a deliberate two-place edit"
            );
        }
    }

    /// Every CEX venue's slug must be a venue `build_node` actually mounts an engine for, and the
    /// slug must be the string that table keys on. Cheap, but it is the join between the FEED half
    /// (this file) and the EXEC half (`vike_run::WIRED_MARKETS`): a typo'd slug would give a feed
    /// with no engine behind it, and every order would vanish at `accepts_symbol`.
    ///
    /// ⚠ This one CANNOT see a permuted slug — every string it checks against contains all three.
    /// `cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string` above is that gate.
    #[test]
    fn every_cex_venue_slug_names_a_wired_market() {
        for venue in CexVenue::ALL {
            let slug = venue.slug();
            assert!(
                wired_symbol_for(slug).is_some(),
                "{slug} has a live feed arm but `build_node` mounts no engine for it"
            );
            assert!(
                crate::config::LIVE_WIRED_VENUES.contains(&slug),
                "{slug} has a feed arm but is not advertised in LIVE_WIRED_VENUES"
            );
        }
    }

    /// The plan gate ACCEPTS the wired pair for each of the three venues, and CARRIES the resolved
    /// `{VENUE}_MAINNET` verdict through to the feed block.
    ///
    /// The verdict has to travel in the plan: `vars` is moved into the `NodeConfig` before the feed
    /// block runs, so the announcement cannot re-read the flag for itself — the same reason
    /// `VenuePlan::Hyperliquid` carries its resolved `Network`. A plan that dropped it would leave
    /// the mount unable to say which network it is about to trade on.
    #[test]
    fn cex_plan_accepts_the_wired_symbol_and_carries_the_network() {
        for venue in CexVenue::ALL {
            let cfg = cex_cfg(venue);
            for mainnet in [false, true] {
                match cex_plan(venue, &cfg, mainnet) {
                    Ok(VenuePlan::Cex { venue: v, mainnet: m }) => {
                        assert_eq!(v, venue, "the plan must name the venue it was asked about");
                        assert_eq!(
                            m,
                            mainnet,
                            "{} must carry the resolved mainnet verdict to the announcement — \
                             `vars` is gone by then",
                            venue.slug()
                        );
                    }
                    other => {
                        panic!("{} on its own wired symbol must plan, got {other:?}", venue.slug())
                    }
                }
            }
        }
    }

    /// **A foreign symbol is REFUSED, not silently mounted.**
    ///
    /// `vike_mount::make_engine` wires no `extra_symbols`, so
    /// `vike_exec::ExecutionEngine::accepts_symbol` is plain equality: a mount on the wrong symbol
    /// keeps its feed, keeps quoting, and has every order AND every fill dropped with no log line
    /// anywhere. That is the failure this gate converts into a startup error, so the error must name
    /// it.
    #[test]
    fn cex_plan_refuses_a_foreign_symbol() {
        for venue in CexVenue::ALL {
            let mut cfg = cex_cfg(venue);
            let wired = cfg.token_id.clone();
            cfg.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
            let err = cex_plan(venue, &cfg, false).expect_err("a foreign symbol must be refused");
            assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
            assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
        }
    }

    /// **A non-positive or non-finite tick size is REFUSED.**
    ///
    /// `cfg.tick_size` is handed straight to `spawn_*_market_data`, where it sizes the `L2Book`
    /// price grid — a plain `f64` parameter with no validation on the venue side. A `0.0` (the
    /// value a profile that never set `tick_size` would produce if the default ever changed) or a
    /// NaN builds a degenerate grid that raises no error and yields no usable top-of-book, so the
    /// maker simply never quotes. It is also the maker's OWN grid, so one check covers both.
    #[test]
    fn cex_plan_refuses_an_unusable_tick_size() {
        for bad in [0.0, -0.5, f64::NAN, f64::INFINITY] {
            let mut cfg = cex_cfg(CexVenue::Okx);
            cfg.tick_size = bad;
            let err = match cex_plan(CexVenue::Okx, &cfg, false) {
                Ok(plan) => panic!(
                    "tick_size {bad} must be refused BEFORE a book is built on it, but the gate \
                     planned {plan:?}"
                ),
                Err(e) => e,
            };
            assert!(
                err.contains("tick_size"),
                "the refusal must name the field an operator has to fix: {err}"
            );
        }
        // ...and a good one still passes, so the guard is not simply always-on.
        let mut cfg = cex_cfg(CexVenue::Okx);
        cfg.tick_size = 0.1;
        assert!(cex_plan(CexVenue::Okx, &cfg, false).is_ok(), "a real okx tick size must pass");
    }

    /// A `LiveDataSink` that discards everything — enough to construct a real venue `Feeds`, which
    /// connects nothing until `subscribe_*` is called, so the tests below build genuine feed objects
    /// without touching the network.
    struct NullSink;

    impl LiveDataSink for NullSink {
        fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_model::Bar>) {}
        fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
        fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
        fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
        fn quote(&self, _v: &str, _s: &str, _q: vike_model::QuoteTick) {}
        fn trade(&self, _v: &str, _s: &str, _t: vike_model::TradeTick) {}
        fn book(&self, _v: &str, _s: &str, _b: Arc<vike_model::L2Book>) {}
    }

    fn cex_bars_for(venue: CexVenue) -> CexBars {
        let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
        match venue {
            CexVenue::Binance => {
                CexBars::Binance(vike_binance::market_feed::Feeds::new(sink, || {}))
            }
            CexVenue::Bybit => CexBars::Bybit(vike_bybit::market_feed::Feeds::new(sink, || {})),
            CexVenue::Okx => CexBars::Okx(vike_okx::market_feed::Feeds::new(sink, || {})),
            // The same `with_env(.., Live)` construction as `wire_venue_feeds`' aster arm (a
            // `Feeds` connects nothing until `subscribe_*`, so this stays network-free).
            CexVenue::Aster => CexBars::Aster(vike_aster::market_feed::Feeds::with_env(
                sink,
                || {},
                vike_bridge_core::Environment::Live,
            )),
        }
    }

    /// **The feed-status health map carries EXACTLY the mounted CEX venue's own row.**
    ///
    /// Two halves, and both are load-bearing in opposite directions:
    ///
    /// - a row MUST exist, or that venue's reconcile pass reads a blanket `Healthy` and keeps
    ///   reconciling against a feed that is known to be down;
    /// - and no OTHER venue may appear, because the health gate can only ever SUPPRESS a pass — a
    ///   spurious row silently stops an unrelated venue from reconciling at all, and a suppressed
    ///   pass can stay suppressed (`reconcile_config::health_from_feed_status`'s doc).
    ///
    /// The keys must also be the exact strings `ReconManager::should_reconcile` looks up, which is
    /// what ties `CexBars::slug` to `CexVenue::slug` and to `vike_run::WIRED_MARKETS`.
    #[test]
    fn the_cex_health_map_carries_only_the_mounted_venues_own_row() {
        for venue in CexVenue::ALL {
            let bars = cex_bars_for(venue);
            assert_eq!(bars.slug(), venue.slug(), "CexBars::slug must match CexVenue::slug");

            let feeds = LiveFeeds::Cex { ticks: None, bars };
            let map = feeds.recon_feed_statuses();
            assert_eq!(
                map.keys().collect::<Vec<_>>(),
                vec![venue.slug()],
                "exactly one row — this venue's own — must be health-gated, got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            // The handle is live: it is the same `Arc` the feed will publish its status through, so
            // the gate reads a real string rather than a detached copy.
            let status = Arc::clone(map.get(venue.slug()).expect("its own row"));
            *status.lock().expect("status mutex") = "disconnected".to_string();
            assert_eq!(
                reconcile_config::health_from_feed_status(
                    &feeds.recon_feed_statuses()[venue.slug()].lock().expect("status").clone()
                ),
                vike_core::ReconHealth::Degraded,
                "a disconnected feed must degrade THIS venue's reconcile health"
            );
        }
    }

    // ---------------------------------------------------------------------------------------------
    // The mount ANNOUNCEMENT (`cex_arming`) — the decisions the feed block makes about what to tell
    // the operator. `live_mount` itself cannot be called from a test (it is a `main.rs` function
    // that spawns a core, opens two sockets and reads the credential store), which is why these
    // decisions were extracted; before that, NOTHING exercised the block and both defects below
    // shipped inside it.
    // ---------------------------------------------------------------------------------------------

    /// **The LIVE announcement must name the NETWORK.** "EXEC IS LIVE" is a materially different
    /// statement on demo than on mainnet, and both states are reachable from the same profile by one
    /// environment variable. The old line said neither.
    #[test]
    fn a_live_cex_mount_announces_which_network_it_trades_on() {
        for venue in CexVenue::ALL {
            let demo = cex_arming(venue, false, true);
            assert_eq!(demo.exec, "LIVE");
            // Aster spells its non-mainnet tier the way its OWN credential naming does
            // (`ASTER_TESTNET_*` — `load_aster_credentials`), so the log matches the keys the
            // operator actually provisioned; the flag venues keep their `DEMO` spelling.
            let expected = if venue == CexVenue::Aster { "TESTNET" } else { "DEMO" };
            assert_eq!(demo.network, expected, "{} non-mainnet ⇒ {expected}", venue.slug());
            assert_eq!(demo.remedy, None, "a live mount has nothing to remedy");

            let main = cex_arming(venue, true, true);
            assert_eq!(main.exec, "LIVE");
            assert_eq!(
                main.network,
                "MAINNET",
                "{} mainnet with live creds is REAL MONEY and the mount must say so",
                venue.slug()
            );
            assert_ne!(
                demo.network, main.network,
                "the two networks must be DISTINGUISHABLE in the log — this is the whole point"
            );
        }
    }

    /// **The PAPER remedy must be ACTIONABLE IN THE STATE IT IS PRINTED IN.**
    ///
    /// ⚠ The defect this pins: the old warning advised `{VENUE}_DEMO_API_KEY` /
    /// `{VENUE}_DEMO_API_SECRET` unconditionally. `vike_mount::make_engine` chooses the credential
    /// TIER from the mainnet flag before it looks anything up — `if mainnet {
    /// load_credentials_from(venue, Environment::Live, vars) } else { … Demo … }` — so in the
    /// `{VENUE}_MAINNET=1` + no-LIVE-keys state, which is exactly the state an operator hits while
    /// going live, the advice names a tier that is never consulted. Following it produces the
    /// identical PAPER mount and the identical warning, with nothing to distinguish the second
    /// attempt from the first. An instruction that cannot work is worse than no instruction.
    #[test]
    fn the_paper_remedy_names_the_tier_make_engine_will_actually_read() {
        for venue in CexVenue::ALL {
            if venue == CexVenue::Aster {
                // Aster has no `{VENUE}_MAINNET` flag and no `_API_KEY` shape, so this test's
                // whole flag-tier vocabulary does not apply — its remedy has its own gate below
                // (`the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard`).
                continue;
            }
            let upper = venue.slug().to_uppercase();

            // DEMO state: the demo tier IS what `make_engine` reads, so advise it.
            let demo = cex_arming(venue, false, false);
            assert_eq!(demo.exec, "PAPER");
            assert_eq!(demo.network, "DEMO");
            let demo_remedy = demo.remedy.expect("a paper mount must say how to arm it");
            assert!(
                demo_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
                "unset flag ⇒ the DEMO tier is the one that arms exec: {demo_remedy}"
            );
            assert!(
                !demo_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
                "and it must not send an operator to add real-money keys: {demo_remedy}"
            );

            // MAINNET state: the demo tier is NOT consulted. Advising it is the bug.
            let main = cex_arming(venue, true, false);
            assert_eq!(main.exec, "PAPER");
            assert_eq!(main.network, "MAINNET");
            let main_remedy = main.remedy.expect("a paper mount must say how to arm it");
            assert!(
                main_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
                "{upper}_MAINNET=1 ⇒ `make_engine` loads the LIVE tier, so that is the tier to \
                 name: {main_remedy}"
            );
            assert!(
                !main_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
                "⚠ THE DEFECT: advising the DEMO tier while {upper}_MAINNET=1 is advice that arms \
                 NOTHING — `make_engine` never consults it in this state: {main_remedy}"
            );
            assert!(
                main_remedy.contains(&format!("UNSET {upper}_MAINNET")),
                "the other way out — dropping back to demo — must be named too, because an \
                 operator who has no live keys yet wants that one: {main_remedy}"
            );

            // The two states must not print the same sentence: the whole failure was one string
            // serving both.
            assert_ne!(
                demo_remedy,
                main_remedy,
                "{} prints the SAME remedy in both states, which is the defect",
                venue.slug()
            );
        }
    }

    /// **Aster's PAPER remedy must speak ITS credential model, not the flag venues'.**
    ///
    /// The generic remedy vocabulary is wrong for aster three separate ways, and each wrong word
    /// sends an operator hunting a value that does not exist: there is no `ASTER_DEMO_*` tier
    /// (the non-mainnet spelling is `TESTNET`), there is no `_API_KEY`/`_API_SECRET` shape (the
    /// venue discontinued HMAC keys — the credential is an agent-wallet `_USER`/`_PRIVATE_KEY`
    /// pair, `vike_aster::signing::load_aster_credentials`), and there is no `ASTER_MAINNET` flag
    /// to set or unset. The one hazard the remedy MUST carry instead: `make_engine` resolves the
    /// LIVE tier FIRST, so provisioning `ASTER_LIVE_*` arms REAL-MONEY MAINNET exec — the
    /// credential tier IS the network choice.
    ///
    /// Only the `mainnet = false` paper state is asserted because it is the only reachable one:
    /// aster's mainnet verdict is "LIVE creds present" ([`cex_mainnet_enabled`]), and present LIVE
    /// creds make exec LIVE — `remedy = None`.
    #[test]
    fn the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard() {
        let r = cex_arming(CexVenue::Aster, false, false).remedy.expect("paper");
        assert!(
            r.contains("ASTER_TESTNET_USER") && r.contains("ASTER_TESTNET_PRIVATE_KEY"),
            "the SAFE tier to advise is testnet, in the agent-wallet key shape: {r}"
        );
        assert!(
            r.contains("ASTER_LIVE_USER") && r.contains("REAL-MONEY MAINNET"),
            "…and it must say what the OTHER tier arms, because LIVE-first resolution makes \
             adding those keys a real-money decision: {r}"
        );
        assert!(
            !r.contains("_API_KEY") && !r.contains("_API_SECRET"),
            "aster has no HMAC key shape; naming one sends the operator after a value that does \
             not exist: {r}"
        );
        // `"DEMO"` and not an `ASTER_DEMO*` key spelling, deliberately twice over: it is the
        // STRONGER ban (no DEMO-tier vocabulary at all, not merely no one key), and a whole-literal
        // `ASTER_`-prefixed spelling here would read as an env key to the settings-registry
        // scanner's literal sweep (`vike_ops::scan::find_map_lookups`), demanding a registry row
        // for fixture data — the exact #1114 shape.
        assert!(
            !r.contains("DEMO") && !r.contains("UNSET ASTER_MAINNET"),
            "no DEMO tier and no MAINNET flag exist for aster — the flag venues' vocabulary is \
             exactly the unreachable advice this remedy exists to avoid: {r}"
        );
    }

    /// OKX v5 signs every request with a passphrase; binance and bybit use none (and aster's
    /// agent-wallet model has no passphrase concept at all). The remedy must
    /// name the keys that venue actually needs — no more (a key that venue has no use for sends an
    /// operator hunting a value that does not exist) and no fewer (an OKX mount with key+secret and
    /// no passphrase loads credentials, mounts LIVE, and then fails every signed call).
    #[test]
    fn the_remedy_names_the_passphrase_only_where_the_venue_signs_with_one() {
        for mainnet in [false, true] {
            let okx = cex_arming(CexVenue::Okx, mainnet, false).remedy.expect("paper");
            assert!(okx.contains("_API_PASSPHRASE"), "okx v5 requires a passphrase: {okx}");
            for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Aster] {
                let r = cex_arming(venue, mainnet, false).remedy.expect("paper");
                assert!(
                    !r.contains("_API_PASSPHRASE"),
                    "{} uses no passphrase; naming one sends the operator after a value that does \
                     not exist: {r}",
                    venue.slug()
                );
            }
        }
    }

    /// **Every CEX venue drives BOTH maker verbs, bybit included.**
    ///
    /// ⚠ The defect this pins: the mount used to log `quote_lane = "on_order_book"` for bybit and
    /// `"on_quote_tick"` for the other two, on the claim that "bybit publishes no L1 quote lane at
    /// all". That claim was FALSE. `spawn_bybit_market_data`'s `MdEvent::BookUpdated` arm sends
    /// `ticks.book(..)` and then `quote_from_book(&book, symbol)` — a real `QuoteUpdate` on the same
    /// core tick lane binance and okx use. The `venue_caps` row it cited (`live_data.quotes = false`)
    /// describes the `DataClient::subscribe_quotes` seam, which this pump does not go through.
    ///
    /// A false capability claim in a runtime log FIELD is the same class as a false `LIVE_CAPABLE`
    /// row: an operator reads it as a measurement of the running system and debugs the wrong lane.
    #[test]
    fn every_cex_venue_announces_both_requote_lanes() {
        for venue in CexVenue::ALL {
            let arming = cex_arming(venue, false, true);
            assert!(
                arming.requote_lanes.contains("on_quote_tick"),
                "{} publishes a QuoteUpdate on the core tick lane, so it drives on_quote_tick: {}",
                venue.slug(),
                arming.requote_lanes
            );
            assert!(
                arming.requote_lanes.contains("on_order_book"),
                "{} publishes a BookUpdate too, so it drives on_order_book: {}",
                venue.slug(),
                arming.requote_lanes
            );
            // ⚠ The lanes are venue-INDEPENDENT. A per-venue lane string is exactly the shape the
            // false claim took, so equality across the roster is asserted rather than left implicit.
            assert_eq!(
                arming.requote_lanes,
                cex_arming(CexVenue::Binance, false, true).requote_lanes,
                "{} must announce the SAME lanes as binance — all three publish quote, trade and \
                 book",
                venue.slug()
            );
        }
    }

    /// The quote's PROVENANCE is still per-venue, and still worth logging — it is what an operator
    /// checks when the quote lane is silent. bybit's is derived, so it is the one that can go quiet
    /// while the book lane stays live (`quote_from_book` returns `None` on a one-sided book).
    #[test]
    fn the_quote_source_distinguishes_derived_from_native() {
        let bybit = cex_arming(CexVenue::Bybit, false, true).quote_source;
        assert!(
            bybit.contains("DERIVED") && bybit.contains("quote_from_book"),
            "bybit's quote is folded out of orderbook.50 by the pump: {bybit}"
        );
        for venue in [CexVenue::Binance, CexVenue::Okx, CexVenue::Aster] {
            let src = cex_arming(venue, false, true).quote_source;
            assert!(
                src.contains("native"),
                "{} decodes a native top-of-book channel: {src}",
                venue.slug()
            );
            assert_ne!(src, bybit, "provenance must still distinguish the venues");
        }
        // The fork and its template are the SAME channel on different host families — the field
        // must still say which one a silent quote lane should be debugged against.
        assert_ne!(
            cex_arming(CexVenue::Aster, false, true).quote_source,
            cex_arming(CexVenue::Binance, false, true).quote_source,
            "aster's row must be distinguishable from binance's"
        );
    }

    /// The hyperliquid and polymarket arms stay on the EMPTY map — byte-identically to before the
    /// CEX arm existed. Pinned rather than assumed: the tempting "just collect every status handle
    /// in scope" change would silently start suppressing reconcile passes on venues nobody assessed,
    /// and it would look like a tidy-up in review.
    #[test]
    fn the_non_cex_arms_stay_on_the_empty_health_map() {
        let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
        let hl = LiveFeeds::Hyperliquid(vike_hyperliquid::market_feed::Feeds::new(sink, || {}));
        assert!(
            hl.recon_feed_statuses().is_empty(),
            "hyperliquid keeps the exec-only-venue shape (every venue reads Healthy)"
        );
    }

    /// **The credential store is read ONCE per process, however many callers ask for it.**
    ///
    /// A clean install found this as a doubled log line: with a 0644 store, the daemon printed the
    /// identical `readable beyond its owner` WARN twice per start, because
    /// `try_load_workspace_secrets_at` surfaces the store's permission finding on EVERY invocation
    /// (deliberately — "no caller can forget to surface it") and four call sites each performed a
    /// complete, independent load. A warning that repeats reads as two findings.
    ///
    /// The doubled line was the symptom; the read was the defect. This asserts the fix at the
    /// property, not at the log: after any call, [`CREDENTIALS`] is populated, so a later caller
    /// takes the cached map instead of re-opening a plaintext file full of live venue secrets. Drop
    /// the memoization and this fails — the `OnceLock` stays empty while the map still comes back.
    ///
    /// ⚠ It reads the REAL store for this working directory, which under `cargo test` is
    /// `crates/vike-tradehub/` — a crate directory with no `settings/` in it, so the load resolves
    /// `Source::None` and an empty map. Nothing here asserts the CONTENTS, precisely so the test says
    /// the same thing on a developer box, on CI, and on a production checkout.
    #[test]
    fn the_credential_store_is_read_once_however_many_callers_ask() {
        let first = workspace_credentials();
        assert!(
            CREDENTIALS.get().is_some(),
            "the store read must be memoized — an unmemoized `workspace_credentials` re-opens the \
             credential file, and re-emits its permission warning, once per caller"
        );
        let second = workspace_credentials();
        assert_eq!(first, second, "two callers must see the same credentials");
        assert!(
            std::ptr::eq(CREDENTIALS.get().unwrap(), CREDENTIALS.get().unwrap()),
            "one stored map, cloned per caller — not one load per caller"
        );
    }

    #[test]
    fn summary_line_is_valid_json_with_expected_keys() {
        let snap = CoreSnapshot::empty("polymarket", "TOK");
        let line = summary_line(&snap, "TOK");
        let v: serde_json::Value =
            serde_json::from_str(&line).expect("the summary line must be valid JSON");
        assert_eq!(v["kind"], "summary");
        assert_eq!(v["orders"], 0);
        assert_eq!(v["working"], 0);
        assert_eq!(v["positions"], 0);
        assert_eq!(v["net_pos"], 0.0);
        assert!(v["fault"].is_null(), "no fault on a fresh snapshot");
        assert!(v["trading_state"].is_string());
    }

    /// The FIELD SET of the summary line is a STDOUT PROTOCOL surface — pinned verbatim, as a set,
    /// so the scope fix below cannot quietly add or drop a key that something downstream parses.
    /// A key added on purpose is one edited row here; a key added by accident is a red test.
    ///
    /// ⚠ The set CHANGED, deliberately, on 2026-08-17: the single `equity` was replaced by
    /// `equity_book`/`equity_wallet`/`wallet_venues`. `equity` was `Portfolio::equity_total`, the
    /// sum of the daemon's own book-keeping and a venue's whole-account wallet — see
    /// [`summary_line`]'s doc for the 62647.10600813 measured on the CI box. Dropping the NAME rather
    /// than redefining it is the point: a consumer keyed on `.equity` gets `null` and breaks
    /// loudly instead of silently reading a figure that means nothing.
    ///
    /// ⚠ …and AGAIN on 2026-08-19 (the I10 rehearsal follow-up): `equity_book_mounted` +
    /// `mounted_venues` were ADDED. Additive on purpose — every existing key keeps its name, its
    /// meaning and its VALUE, so no `jq`/alerting consumer reads a number that changed under it;
    /// the mounted-set scoping arrives as its own labelled pair instead
    /// (`crate::summary::mounted_book_equity` carries the argument).
    #[test]
    fn the_summary_line_field_set_is_exactly_these_fifteen_keys() {
        let line = summary_line(&CoreSnapshot::empty("bybit", "BTCUSDT"), "BTCUSDT");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        let mut keys: Vec<&str> =
            v.as_object().expect("a JSON object").keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "equity_book",
                "equity_book_mounted",
                "equity_wallet",
                "fault",
                "fees",
                "kind",
                "mounted_venues",
                "net_pos",
                "orders",
                "positions",
                "realized_pnl",
                "seq",
                "trading_state",
                "wallet_venues",
                "working",
            ],
            "the summary line's field set is a protocol surface — changing it is a deliberate edit"
        );
        assert!(
            !v.as_object().expect("a JSON object").contains_key("equity"),
            "there must be no bare `equity` key: a venue wallet and a book-kept equity are \
             different quantities, and the name that used to carry their sum is retired rather \
             than redefined so a stale consumer fails loudly"
        );
    }

    /// One venue block, spelled out so the scope test below reads as data rather than a builder.
    fn venue_block(
        venue: &str,
        realized: f64,
        fees: f64,
        positions: Vec<vike_core::PositionView>,
    ) -> vike_core::VenueBlock {
        vike_core::VenueBlock {
            venue: venue.to_string(),
            balance: 0.0,
            realized_pnl: realized,
            fees_paid: fees,
            funding_paid: 0.0,
            balance_mode: vike_exec::BalanceMode::Delta,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: vike_exec::TradingState::Active,
            multipliers: Default::default(),
            multiplier_default: 1.0,
            positions,
        }
    }

    fn position(venue: &str, symbol: &str, size: f64) -> vike_core::PositionView {
        vike_core::PositionView {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            position_side: "BOTH".to_string(),
            size,
            avg_px: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    /// ⚠ **The the CI box shape, and the bug this test exists for.** `vike_run::WIRED_MARKETS` lists
    /// binance first, so on a CEX node the PRIMARY engine is the binance PAPER engine — which has
    /// traded nothing — while the mount that actually trades is bybit, a NON-primary engine.
    /// `CoreSnapshot::build` binds `let acc = &engine.account` (the primary) into the scalar
    /// `Portfolio::realized_pnl`/`fees_paid` and `CoreSnapshot::positions`, so a summary line that
    /// read those four fields reported binance's silence: measured on the CI box as
    /// `fees: 0.0, realized_pnl: 0.0, net_pos: 0.0, positions: 0` on the very minute the bybit mount
    /// booked ten maker fills — while `equity` in the SAME line tracked those fills to eight decimal
    /// places, which is how we know the engine saw everything and only the REPORT was wrong.
    ///
    /// So this snapshot is built the way `build` builds one on the CI box: primary venue block EMPTY and
    /// the primary-mirroring scalars left at 0.0, every traded number living in the SECOND venue
    /// block. Every assertion below fails on the pre-fix code.
    #[test]
    fn the_summary_reports_a_non_primary_venues_fills_not_the_untraded_primarys_silence() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![
            venue_block("binance", 0.0, 0.0, vec![]),
            venue_block("bybit", 12.5, 0.75, vec![position("bybit", "BTCUSDT", -0.25)]),
        ];
        // The primary-mirroring scalars stay exactly as `build` leaves them for an untraded
        // primary — the point is that the line must NOT be reading them.
        assert_eq!(snap.portfolio.realized_pnl, 0.0);
        assert_eq!(snap.portfolio.fees_paid, 0.0);
        assert!(snap.positions.is_empty());

        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(
            v["realized_pnl"], 12.5,
            "realized_pnl must be the CROSS-VENUE total; the primary engine traded nothing"
        );
        assert_eq!(
            v["fees"], 0.75,
            "fees must be the CROSS-VENUE total; the primary engine paid none"
        );
        assert_eq!(
            v["positions"], 1,
            "positions must count every venue's rows, not just the primary's"
        );
        assert_eq!(
            v["net_pos"], -0.25,
            "net_pos must net the mount symbol across every venue, not read the primary's leg"
        );
    }

    /// The other half of "cross-venue": the four widened fields must SUM, not merely find the one
    /// venue that happens to be non-empty. A per-venue-block fix that returned the first non-zero
    /// row would pass the test above and fail this one.
    #[test]
    fn the_summary_totals_every_venue_rather_than_picking_one() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![
            venue_block("binance", 4.0, 0.25, vec![position("binance", "BTCUSDT", 2.0)]),
            venue_block("bybit", -1.5, 0.75, vec![position("bybit", "BTCUSDT", -0.5)]),
            venue_block("okx", 0.5, 0.5, vec![position("okx", "ETHUSDT", 3.0)]),
        ];
        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(v["realized_pnl"], 3.0, "4.0 + -1.5 + 0.5");
        assert_eq!(v["fees"], 1.5, "0.25 + 0.75 + 0.5");
        assert_eq!(v["positions"], 3, "one row per venue, ETHUSDT included");
        assert_eq!(
            v["net_pos"], 1.5,
            "BTCUSDT nets +2.0 against -0.5 across venues; the ETHUSDT leg is a different symbol"
        );
    }

    /// ⚠ **The the CI box shape of 2026-08-17, and the bug this test exists for.** `VIKE_RECONCILE=1`
    /// made bybit authoritative; `CoreThread::reconcile_reports` adopted the account's USDT
    /// `walletBalance` — 53647, from a SHARED demo account carrying settlements on
    /// AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT that this daemon never traded — while nine paper mounts
    /// still contributed 1000 seed each. The headline `equity` jumped from ~10000 to
    /// **62647.10600813**: a venue wallet the daemon does not own, plus paper seed cash, added
    /// together and printed as one number.
    ///
    /// The mount's OWN accounting on bybit is the 0.75 of realized PnL and the quarter-coin
    /// position beside it — four orders of magnitude away from the wallet. So the report must
    /// distinguish the two, and a reader must be able to tell WHICH is which from the key alone.
    /// Every assertion below fails on the pre-fix line, which carried neither key.
    #[test]
    fn an_adopted_venue_wallet_is_never_added_to_book_kept_paper_seed() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut venues = vec![vike_core::VenueBlock {
            equity: 1_000.0,
            ..venue_block("binance", 0.0, 0.0, vec![])
        }];
        for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
            venues.push(vike_core::VenueBlock {
                equity: 1_000.0,
                ..venue_block(v, 0.0, 0.0, vec![])
            });
        }
        // The one live mount: its cash is the venue's whole-account wallet, adopted verbatim.
        venues.push(vike_core::VenueBlock {
            balance: 53_647.10600813,
            balance_mode: vike_exec::BalanceMode::Authoritative,
            equity: 53_647.10600813,
            ..venue_block("bybit", 0.75, 0.25, vec![position("bybit", "BTCUSDT", -0.25)])
        });
        snap.portfolio.venues = venues;
        // The conflated figure the old line printed, kept here as the thing NOT to report.
        snap.portfolio.equity_total =
            vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));
        assert_eq!(snap.portfolio.equity_total, 62_647.10600813, "the measured the CI box headline");

        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");

        assert_eq!(
            v["equity_book"], 9_000.0,
            "the book-kept half is the nine paper mounts' seed and nothing else"
        );
        assert_eq!(
            v["equity_wallet"], 53_647.10600813,
            "the venue-attested half is bybit's whole-account wallet, reported as its own quantity"
        );
        assert_eq!(
            v["wallet_venues"], "bybit",
            "and the line NAMES whose wallet it quoted, so the reader need not open a log"
        );
        // The whole point: no field on this line is the sum of the two.
        for (key, val) in v.as_object().expect("a JSON object") {
            if let Some(f) = val.as_f64() {
                assert_ne!(
                    f, 62_647.10600813,
                    "`{key}` is the conflated total — a venue wallet and a book-kept equity are \
                     different quantities and no field may add them"
                );
            }
        }
        // The mount's own accounting is still reported, unwidened and unswallowed by the wallet.
        assert_eq!(v["realized_pnl"], 0.75);
        assert_eq!(v["net_pos"], -0.25);
    }

    /// The other half: on a node where NOTHING has ever attested a balance, the wallet fields must
    /// read empty rather than mirroring the book — otherwise a paper daemon reports its seed cash
    /// twice, once under each name, and the split says nothing.
    #[test]
    fn a_pure_paper_node_reports_a_zero_wallet_and_names_no_venue() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![
            vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
            vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
        ];
        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(v["equity_book"], 3_500.0);
        assert_eq!(v["equity_wallet"], 0.0, "no venue has attested a balance");
        assert_eq!(v["wallet_venues"], "", "so there is no wallet to name");
    }

    /// A `MountRowKind::Mount` row for `venue`/`symbol` — the snapshot half of "what this daemon
    /// runs", which is what the mounted-set scoping reads.
    fn mount_row(venue: &str, symbol: &str) -> vike_core::MountView {
        vike_core::MountView {
            kind: vike_core::MountRowKind::Mount,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: "1m".to_string(),
            ready: true,
            position: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            notional: 0.0,
            budget: None,
            latched: false,
        }
    }

    /// ⚠ **The I10 REHEARSAL shape, and the observation this pair of keys exists for**
    /// (`docs/ops/i10-rehearsal-2026-08-19.md`): two mounts seeded at 10k, ten default-build venue
    /// engines each carrying that same seed, and a summary line reading `equity_book: 100000.0`.
    ///
    /// Both halves are asserted, because the fix is that BOTH are reported: `equity_book` keeps
    /// its whole-book value (a consumer that already reads it sees no change, and the
    /// book/wallet partition still holds), while `equity_book_mounted` answers the question the
    /// operator was actually asking — 20000.0 — and `mounted_venues` names the two it scoped to.
    #[test]
    fn the_summary_scopes_a_mounted_set_figure_beside_the_whole_book() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = [
            "binance",
            "bybit",
            "okx",
            "hyperliquid",
            "aster",
            "deribit",
            "alpaca",
            "ctrader",
            "ig",
            "oanda",
        ]
        .into_iter()
        .map(|v| vike_core::VenueBlock { equity: 10_000.0, ..venue_block(v, 0.0, 0.0, vec![]) })
        .collect();
        snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];

        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(
            v["equity_book"], 100_000.0,
            "the whole-book figure is CORRECT and unchanged — ten engines seeded at 10k each"
        );
        assert_eq!(
            v["equity_book_mounted"], 20_000.0,
            "…and the scoped companion is the two MOUNTED venues' seeds, which is the number the \
             rehearsal's operator was reaching for"
        );
        assert_eq!(
            v["mounted_venues"], "binance,bybit",
            "the line NAMES what it scoped to, so the reader need not open a profile"
        );
    }

    /// The RESIDUAL row is not a mount. `CoreThread::mount_views` appends one
    /// `MountRowKind::Residual` row (venue: the empty string) whenever any mount exists, so a
    /// scoping that filtered on "has a venue row" rather than on KIND would silently widen the
    /// set the day a residual row carried a venue — and would name an empty venue today.
    #[test]
    fn the_residual_row_is_not_treated_as_a_mount() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![
            vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
            vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
        ];
        snap.mounts = vec![
            mount_row("binance", "BTCUSDT"),
            vike_core::MountView { kind: vike_core::MountRowKind::Residual, ..mount_row("", "") },
        ];
        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(v["equity_book_mounted"], 1_000.0, "only the binance mount is scoped in");
        assert_eq!(v["mounted_venues"], "binance", "the residual row names no venue");
    }

    /// A core that has mounted NOTHING scopes to nothing: `0.0` with an EMPTY name list, which is
    /// how a reader tells "scoped to nothing" from "nothing to scope". `equity_book` still
    /// reports the seed, so no capital goes unreported by the line as a whole.
    #[test]
    fn a_mountless_snapshot_scopes_to_zero_and_names_nobody() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![vike_core::VenueBlock {
            equity: 1_000.0,
            ..venue_block("binance", 0.0, 0.0, vec![])
        }];
        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(v["equity_book"], 1_000.0, "the whole book is still reported");
        assert_eq!(v["equity_book_mounted"], 0.0);
        assert_eq!(v["mounted_venues"], "");
    }

    /// A MOUNTED venue that has flipped `Authoritative` (reconcile adopted its wallet) is NAMED
    /// but contributes NOTHING to the mounted BOOK figure — its equity lives in `equity_wallet`.
    /// The scoped figure obeys the same partition `equity_book` does; a mounted-set number that
    /// quietly pulled an adopted wallet back into a "book" key would re-commit the exact
    /// conflation the 62647.10600813 split exists to prevent.
    #[test]
    fn a_mounted_venue_on_an_adopted_wallet_is_named_but_not_booked() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![
            vike_core::VenueBlock { equity: 10_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
            vike_core::VenueBlock {
                balance: 53_647.10600813,
                balance_mode: vike_exec::BalanceMode::Authoritative,
                equity: 53_647.10600813,
                ..venue_block("bybit", 0.0, 0.0, vec![])
            },
        ];
        snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];
        let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
            .expect("the summary line must be valid JSON");
        assert_eq!(
            v["equity_book_mounted"], 10_000.0,
            "bybit's adopted wallet is NOT book-kept equity, mounted or otherwise"
        );
        assert_eq!(v["mounted_venues"], "binance,bybit", "but it IS a mount, and is named as one");
        assert_eq!(v["equity_wallet"], 53_647.10600813, "its equity is reported under the wallet");
        assert_eq!(v["wallet_venues"], "bybit");
    }

    // -- the stop path: what the stdio control channel does, and what it deliberately does not ----

    /// Drive [`control_loop`] over a fixed script and report `(stop raised?, commands accepted)`.
    fn drive(input: &str, is_tty: bool) -> (bool, usize) {
        let stop = AtomicBool::new(false);
        let mut sent = 0usize;
        control_loop(
            std::io::Cursor::new(input.as_bytes()),
            is_tty,
            &stop,
            |_cmd| sent += 1,
            || "{}".to_string(),
        );
        (stop.load(Ordering::SeqCst), sent)
    }

    /// ⚠ **The regression this daemon cannot afford.** Under systemd stdin is `/dev/null`, which
    /// reads EOF the instant the daemon starts. If EOF meant "stop", the daemon would exit at
    /// startup on every box, every start — so a non-tty EOF must leave the flag DOWN and the daemon
    /// trading headless until a signal arrives. `tests/sigterm_stop.rs` proves the same property
    /// against the real process; this pins the rule it rests on.
    #[test]
    fn a_non_tty_eof_does_not_stop_the_daemon() {
        assert_eq!(
            drive("", false),
            (false, 0),
            "a non-tty EOF must NOT raise the stop flag — systemd wires stdin to /dev/null, so this \
             would exit at startup on every service box"
        );
    }

    /// …and the mirror image, so the rule above is not bought by ignoring EOF entirely: Ctrl-D from
    /// a human at a terminal IS an explicit stop.
    #[test]
    fn a_tty_eof_stops_the_daemon() {
        assert_eq!(drive("", true), (true, 0), "Ctrl-D on a TTY is a stop");
    }

    /// Every stop word raises the flag on either channel — the word is explicit, so whether the
    /// channel is a terminal has nothing to add.
    #[test]
    fn every_stop_word_raises_the_flag_on_either_channel() {
        for word in ["shutdown", "quit", "exit"] {
            for is_tty in [true, false] {
                assert!(
                    drive(&format!("{word}\n"), is_tty).0,
                    "`{word}` must stop the daemon (is_tty={is_tty})"
                );
            }
        }
    }

    /// A JSON command is lowered and the channel keeps running — a stop is a WORD, never a side
    /// effect of having been sent something.
    #[test]
    fn a_command_is_lowered_and_does_not_stop_the_daemon() {
        let req = vike_model::OrderRequest {
            client_order_id: "stdio-1".to_string(),
            venue: "polymarket".to_string(),
            symbol: "TOK".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "limit".to_string(),
            price: Some(0.4),
            ..Default::default()
        };
        let json =
            serde_json::to_string(&Command::Order(vike_exec::OrderIntent::Submit(Box::new(req))))
                .expect("serialize an operator command");
        assert_eq!(
            drive(&format!("{json}\n"), false),
            (false, 1),
            "the command must reach the core lane, and must not be read as a stop"
        );
    }

    /// Garbage on the control channel is reported, never obeyed: a typo must not stop a daemon that
    /// is holding a live book, and it must not be mistaken for a command either.
    #[test]
    fn junk_neither_stops_the_daemon_nor_reaches_the_core() {
        assert_eq!(drive("\n   \nnot json\nhalt\n", false), (false, 0));
    }

    /// EVERY shipped unit that starts this daemon, as `(repo-relative path, contents)`.
    ///
    /// ⚠ There are TWO, and for a while only the first was read. `deploy/vike-tradehub-project.service`
    /// is the ONE-PROJECT-FOLDER unit the CI box actually runs; it carries its own `TimeoutStopSec=`
    /// hand copy, its comment claimed the test below checked it, and nothing did. A second ungated
    /// copy of a value whose entire purpose is that it cannot drift is exactly the defect the pair
    /// of tests below exists to prevent, so the fix is to read both rather than to soften the
    /// comment. `include_str!` needs a literal path, which is why this is a table and not a glob —
    /// a THIRD unit must be added here by hand, and that is the intended cost.
    const SHIPPED_UNITS: [(&str, &str); 2] = [
        ("deploy/vike-tradehub.service", include_str!("../../../deploy/vike-tradehub.service")),
        (
            "deploy/vike-tradehub-project.service",
            include_str!("../../../deploy/vike-tradehub-project.service"),
        ),
    ];

    /// `TimeoutStopSec=` from a unit's text, skipping COMMENTED lines — both units discuss the
    /// directive in prose right above setting it, so a naive prefix match on an untrimmed line
    /// would read the commentary and pass on a unit that never sets the directive at all.
    fn stop_timeout_secs(path: &str, unit: &str) -> u64 {
        unit.lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix("TimeoutStopSec="))
            .unwrap_or_else(|| panic!("`{path}` must set TimeoutStopSec= explicitly"))
            .trim()
            .parse()
            .unwrap_or_else(|e| {
                panic!("`{path}`: TimeoutStopSec= is a plain number of seconds: {e}")
            })
    }

    /// The teardown budget must fit inside EVERY shipped unit's `TimeoutStopSec=`, or systemd
    /// SIGKILLs the daemon mid-teardown and the graceful stop buys nothing. Both numbers are READ
    /// (the profile default from `DaemonSettings`, the timeout from each unit) rather than restated,
    /// so the two cannot drift apart in a later edit.
    #[test]
    fn the_default_shutdown_deadline_fits_inside_the_units_stop_timeout() {
        let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
        for (path, unit) in SHIPPED_UNITS {
            let timeout = stop_timeout_secs(path, unit);
            // ⚠ ONE step of this daemon's stop is still OUTSIDE `run_with_deadline`: stopping the
            // observe publisher, which closes mailboxes and does not join the detached accept loop,
            // so it is bounded by `publish::POLL_INTERVAL`. STRICT `<` is what leaves room for it.
            //
            // ⚠ This comment used to name TWO such steps and call both "sub-second by
            // construction". That was FALSE of the second one: the summary-thread join sat here
            // with no timeout while the same thread delivers alerts INLINE through a
            // `WebhookSink<UreqTransport>` whose `timeout_global` is 10 s — so a single in-flight
            // alert could blow a `TimeoutStopSec=10` on its own, and this assertion was comparing
            // two numbers while excluding the term that broke the sum. The fix was to move that
            // join INTO `tasks`, where the deadline below actually covers it; the assertion is
            // unchanged, but it is now true. (The sibling recorder's version of this test made the
            // same class of mistake with a 12 s feed-stop prefix and still claimed the flush was
            // safe from SIGKILL. Say what is compared, so the next reader can check the claim
            // rather than inherit it.)
            assert!(
                deadline < timeout * 1_000,
                "the default [daemon] shutdown_deadline_ms ({deadline}) must be strictly under \
                 `{path}`'s TimeoutStopSec={timeout}s — otherwise SIGKILL wins the race and the \
                 teardown is cut in half. This bounds the hard-capped teardown, which now INCLUDES \
                 the summary-thread join; the publisher stop that precedes it is bounded by one \
                 poll interval and rides the difference."
            );
        }
    }

    /// The loser of a teardown claim waits on the SAME budget the winner's teardown runs under.
    ///
    /// It is one number by construction — `deadline` is resolved once, above the claim, and passed
    /// to both `StopSignal::await_teardown` and `run_with_deadline`. This asserts the property that
    /// makes that safe: whichever thread ends the process, the stop still fits inside the unit's
    /// `TimeoutStopSec=`, because both arms are bounded by the same profile deadline. A loser
    /// waiting on a LARGER bound would let a second stop route hold the process open past SIGKILL;
    /// one waiting on a smaller bound would exit while the winner was still cancelling.
    #[test]
    fn a_losing_claim_waits_within_the_same_stop_timeout_the_teardown_does() {
        let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
        for (path, unit) in SHIPPED_UNITS {
            let timeout = stop_timeout_secs(path, unit);
            // The wait the losing arm performs is `stop.await_teardown(deadline)` — the same value.
            assert!(
                deadline < timeout * 1_000,
                "a loser parked for the winner's budget ({deadline} ms) must still be released \
                 before `{path}`'s TimeoutStopSec={timeout}s, or a second stop route turns a \
                 graceful stop into a SIGKILL"
            );
        }
    }

    // -- alerting: where the rules file is looked for --------------------------------------------

    /// The rules file resolves in exactly TWO places and nowhere else: `$VIKE_ALERTS` when it
    /// names one, else `<state_dir>/alerts.json`. With NEITHER there is no path at all — which is
    /// what makes [`maybe_mount_alerts`] log the OFF line instead of quietly reading a file
    /// somewhere the operator was never told about.
    ///
    /// The third assert is the one that bites: a resolver that reached for a directory of its own
    /// — beside the executable, the working directory, a home — would answer `Some` there.
    #[test]
    fn the_alerts_file_resolves_only_from_the_override_or_the_state_directory() {
        let state = Path::new("/tmp/vike-state");
        assert_eq!(
            alerts_path_in(None, Some(state)),
            Some(state.join("alerts.json")),
            "no override ⇒ the state directory, joined with the library's own basename"
        );
        assert_eq!(
            alerts_path_in(Some("/etc/vike/rules.json"), Some(state)),
            Some(PathBuf::from("/etc/vike/rules.json")),
            "an explicit $VIKE_ALERTS names the file outright"
        );
        assert_eq!(
            alerts_path_in(None, None),
            None,
            "no override and no state directory ⇒ NO path, never one of this resolver's own \
             invention"
        );
        // A blank override is an unset one in every shell that produced it.
        for blank in [Some(""), Some("   ")] {
            assert_eq!(alerts_path_in(blank, Some(state)), Some(state.join("alerts.json")));
            assert_eq!(alerts_path_in(blank, None), None, "and blank cannot conjure one either");
        }
    }

    // -- settings / policy (settings-unification Phase 6c) ---------------------------------------

    /// THE no-file property, at this daemon's own edge: with no `policy.toml` on the machine the
    /// loader yields `Policy::default()`, whose venue-facing projection is `MountPolicy::default()`.
    /// Driven through the REAL loader (`load(None, &{})` is exactly "no home directory, no project
    /// file, no environment") rather than asserting the default struct, so a default that stopped
    /// being the no-file answer would fail here.
    ///
    /// ⚠ This test's NAME used to be `..._leaves_every_venue_on_its_own_literal`, and that is no
    /// longer what the default means. Every SCALAR field is still `None` — each venue arm keeps its
    /// compiled-in literal — but `venues` defaults to `paper` for every venue, so a daemon with no
    /// `policy.toml` mounts ALL PAPER. Asserted here rather than left to the equality, because the
    /// equality would hold just as well if the default flipped to `live` on both sides.
    #[test]
    fn an_absent_policy_file_is_the_mount_default_and_arms_no_venue() {
        let settings = vike_config::load(None, &HashMap::new()).unwrap();
        let mount = vike_run::MountPolicy::from(&settings.policy);
        assert_eq!(mount, vike_run::MountPolicy::default());
        assert_eq!(mount.market_slippage, None, "no file ⇒ each venue keeps its own literal");
        for venue in vike_model::VENUES {
            assert_eq!(
                mount.venue_mode(venue),
                vike_config::VenueMode::Paper,
                "{venue}: no policy.toml must arm NOTHING — credential presence is no longer a gate \
                 that can act alone"
            );
        }
        assert!(
            settings.warnings.is_empty(),
            "a defaults-only load resolves nothing: {settings:?}"
        );
    }

    /// …and a policy that DOES name the band reaches the projection the live mount threads into
    /// `make_engine`. Pure: the file→`Policy` half is `vike-config`'s own `tests/load.rs`; this pins
    /// the daemon's hand-off, which is the half that did not exist before Phase 6c.
    #[test]
    fn a_configured_band_reaches_the_live_mounts_projection() {
        let policy =
            vike_config::Policy { market_slippage: Some(0.002), ..vike_config::Policy::default() };
        assert_eq!(vike_run::MountPolicy::from(&policy).market_slippage, Some(0.002));
    }

    /// A loader warning is SURFACED, never swallowed. `main` emits exactly what
    /// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
    /// daemon logs it — the failure this guards is the loader resolving something the operator did
    /// not write while they believe their own file is in force.
    ///
    /// ⚠ The warning is HAND-STUFFED, which it deliberately was not before. This drove
    /// `Settings::clamp_to_policy` — `policy.rate.max_utilization` over
    /// `preferences.rate_utilization` — and that clamp was removed with both of its fields: a
    /// ceiling that bounded a value nothing read. `Settings::warnings` has no producer today (its
    /// doc argues why the CHANNEL is kept regardless), so there is nothing left to build one
    /// through; what is still gated is this daemon's own half — the `settings: ` prefix and the
    /// verbatim text reaching the log.
    #[test]
    fn a_loader_warning_is_surfaced_not_swallowed() {
        let mut settings = vike_config::Settings::default();
        settings.warnings.push("preferences.something was resolved to 0.5".to_string());

        let lines = settings_warning_lines(&settings);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("settings: "), "{}", lines[0]);
        assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
        assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
    }

    /// The quiet path stays quiet: nothing to resolve ⇒ nothing emitted, so a warn line in a
    /// daemon's log always means something actually happened.
    #[test]
    fn a_clean_load_emits_no_settings_lines() {
        assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
    }

    // -- reconcile-on-restart gate (VIKE_RECONCILE) ---------------------------------------------

    /// The reconcile master gate reads the FLAGS FILE, and the environment still wins over it.
    ///
    /// Replaces a test that asserted `vike_ops::reconcile_config::reconcile_enabled`'s grammar — a
    /// function this daemon no longer calls, and one vike-ops tests itself. What is worth pinning
    /// HERE is the property the wiring introduced: `live_mount` takes `flags.reconcile`, so a
    /// `flags.toml` this daemon can actually be deployed with turns reconciliation on, and an
    /// operator's exported variable still overrides it for one run.
    ///
    /// Driven through the REAL `vike_config::load` over a throwaway settings directory rather than
    /// a hand-built `Flags`, because the thing being asserted is the LOADER's precedence, and a
    /// hand-built value would assert nothing about it. The env key comes from `vike_config`'s own
    /// constant so a rename cannot leave this test passing against a name nothing reads.
    #[test]
    fn the_reconcile_gate_reads_the_flags_file_and_the_env_still_wins() {
        let dir = tempfile::tempdir().expect("temp settings dir");
        std::fs::write(dir.path().join("flags.toml"), "reconcile = true\n").unwrap();

        // OFF by default — no file, no variable: byte-identical to the pre-reconcile daemon.
        let bare = vike_config::load(None, &HashMap::new()).unwrap();
        assert!(!bare.flags.reconcile, "unset ⇒ OFF (no mount, no thread)");

        // The FILE arms it. This is the half that did nothing before the wiring.
        let from_file = vike_config::load(Some(dir.path()), &HashMap::new()).unwrap();
        assert!(from_file.flags.reconcile, "flags.toml must arm the gate");

        // …and the environment still outranks the file, in BOTH directions.
        let off = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "0".to_string())]);
        let overridden = vike_config::load(Some(dir.path()), &off).unwrap();
        assert!(!overridden.flags.reconcile, "env must override a file `true`");

        let on = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "1".to_string())]);
        assert!(vike_config::load(None, &on).unwrap().flags.reconcile);
    }

    /// QUARANTINE-FIRST: with `VIKE_RECONCILE_POLICY` unset the daemon folds in `quarantine`, so
    /// even a local-origin divergence (`MissingFill`) is HELD — a fresh live daemon auto-folds
    /// NOTHING (CLAUDE.md's rule: `hybrid` auto-applies `PositionDrift` on an incomplete position
    /// fetch, rewriting position size and booking realized PnL at the venue's price). Contrast the
    /// `hybrid` reference default, which synthesizes it below.
    #[test]
    fn daemon_reconcile_policy_defaults_to_quarantine() {
        use vike_exec::recon::{DivergenceKind, ReconMode};
        let env = with_quarantine_first_default(HashMap::new());
        let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
        assert_eq!(cfg.policy.default, ReconMode::Quarantine);
        assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    }

    /// An operator who sets `VIKE_RECONCILE_POLICY=hybrid` is honored verbatim — the quarantine-first
    /// default only fills an UNSET value.
    #[test]
    fn daemon_reconcile_policy_honors_explicit_override() {
        use vike_exec::recon::{DivergenceKind, ReconMode};
        let env = with_quarantine_first_default(HashMap::from([(
            "VIKE_RECONCILE_POLICY".to_string(),
            "hybrid".to_string(),
        )]));
        let cfg = reconcile_config::build_recon_config(&env, HashMap::new());
        // hybrid auto-synthesizes a local-origin MissingFill (the quarantine default holds it).
        assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    }

    // ---------------------------------------------------------------------------------------------
    // The CREDENTIALED-DATA arms (alpaca/ctrader, split-plane I9): the plan gates, the credential
    // threading, the interval constraints, and the arming disclosures.
    //
    // ⚠ Every credential key below is BUILT with `format!` fragments, never spelled as a whole
    // `ALPACA_`/`CTRADER_`-prefixed literal: the settings-registry literal sweep
    // (`vike_ops::scan::find_map_lookups`) reads a whole env-shaped literal as a read sighting —
    // the #1114 shape the aster remedy test above already documents.
    // ---------------------------------------------------------------------------------------------

    /// The SANDBOX trio, exactly as `vike_alpaca::load_alpaca_config_from(Demo, …)` looks it up.
    fn alpaca_vars() -> HashMap<String, String> {
        let tier = vike_alpaca::alpaca_tier(vike_bridge_core::Environment::Demo);
        ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"]
            .iter()
            .map(|k| (format!("ALPACA_{tier}_{k}"), format!("test-{k}")))
            .collect()
    }

    /// The app pair + DEMO token pair, exactly as `CtraderConfig::from_vars(Demo, …)` looks them up.
    fn ctrader_vars() -> HashMap<String, String> {
        let tier = "DEMO";
        let mut vars: HashMap<String, String> = ["CLIENT_ID", "CLIENT_SECRET"]
            .iter()
            .map(|k| (format!("CTRADER_{k}"), format!("app-{k}")))
            .collect();
        vars.insert(format!("CTRADER_{tier}_ACCESS_TOKEN"), "tok".to_string());
        vars.insert(format!("CTRADER_{tier}_REFRESH_TOKEN"), "refresh".to_string());
        vars
    }

    fn alpaca_cfg() -> MakerMountConfig {
        let symbol = wired_symbol_for("alpaca").expect("build_node mounts alpaca");
        let mut cfg = MakerMountConfig::crypto("alpaca", symbol, 0.01, 1.0);
        cfg.interval = "1m".to_string();
        cfg.interval_ms = 60_000;
        cfg
    }

    fn ctrader_cfg() -> MakerMountConfig {
        let symbol = wired_symbol_for("ctrader").expect("build_node mounts ctrader");
        let mut cfg = MakerMountConfig::crypto("ctrader", symbol, 0.00001, 1_000.0);
        cfg.interval = "1m".to_string();
        cfg.interval_ms = 60_000;
        cfg
    }

    /// Every credentialed-data slug is joined to the exec plane the same way the CEX slugs are:
    /// an engine row in `vike_run::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
    #[test]
    fn the_credentialed_data_venues_name_wired_markets_and_live_wired_rows() {
        for slug in ["alpaca", "ctrader", "oanda"] {
            assert!(
                wired_symbol_for(slug).is_some(),
                "{slug} has a live feed arm but `build_node` mounts no engine for it"
            );
            assert!(
                crate::config::LIVE_WIRED_VENUES.contains(&slug),
                "{slug} has a feed arm but is not advertised in LIVE_WIRED_VENUES"
            );
        }
    }

    /// **The alpaca plan CARRIES the resolved SANDBOX config** — the same reason
    /// `VenuePlan::Cex` carries its mainnet verdict: `vars` is moved into the `NodeConfig` before
    /// the feed block runs, so the DATA credentials must travel in the plan. The threading is
    /// exec's own loader over exec's own tier, so the two planes cannot resolve differently.
    #[test]
    fn alpaca_plan_accepts_the_wired_pair_and_carries_the_sandbox_config() {
        match alpaca_plan(&alpaca_cfg(), &alpaca_vars()) {
            Ok(VenuePlan::Alpaca(config)) => {
                assert_eq!(config.account_id, "test-ACCOUNT_ID", "the pinned account travels");
                assert!(
                    config.hosts.data_ws.contains("sandbox"),
                    "Demo tier ⇒ SANDBOX data hosts (the exec side's own tier): {}",
                    config.hosts.data_ws
                );
            }
            other => panic!("the wired alpaca pair with the trio present must plan, got {other:?}"),
        }
    }

    /// **Absent credentials REFUSE the alpaca mount** — the documented divergence from the exec
    /// gate (which degrades to paper): there is no keyless alpaca stream, and a live mount without
    /// a feed quotes into the void. The refusal must name the exact trio and the store.
    #[test]
    fn alpaca_plan_refuses_absent_credentials_naming_the_sandbox_trio() {
        let err = alpaca_plan(&alpaca_cfg(), &HashMap::new())
            .expect_err("no credentials must refuse the live mount, never mount feed-less");
        for needle in ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCOUNT_ID", "SANDBOX", "secrets.env"] {
            assert!(err.contains(needle), "the refusal must name {needle}: {err}");
        }
        // A partial trio is the same refusal — the loader is all-or-nothing.
        let mut partial = alpaca_vars();
        partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
        assert!(alpaca_plan(&alpaca_cfg(), &partial).is_err(), "a partial trio must refuse too");
    }

    /// The alpaca-shaped per-mount refusals: a foreign symbol (the silent-drop hazard, same as
    /// `cex_plan`) and a non-1m interval (the WS serves 1m bars only, and bars drive the live
    /// watchdogs — a `5m` mount would run with both dead).
    #[test]
    fn alpaca_plan_refuses_a_foreign_symbol_and_a_non_1m_interval() {
        let mut foreign = alpaca_cfg();
        let wired = foreign.token_id.clone();
        foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = alpaca_plan(&foreign, &alpaca_vars()).expect_err("foreign symbol");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

        let mut five = alpaca_cfg();
        five.interval = "5m".to_string();
        five.interval_ms = 300_000;
        let err = alpaca_plan(&five, &alpaca_vars()).expect_err("non-1m interval");
        assert!(err.contains("1m"), "the refusal must name the one servable interval: {err}");

        let mut bad_tick = alpaca_cfg();
        bad_tick.tick_size = 0.0;
        assert!(
            alpaca_plan(&bad_tick, &alpaca_vars())
                .expect_err("degenerate tick")
                .contains("tick_size"),
            "the tick refusal names the field"
        );
    }

    /// **The ctrader plan CARRIES the resolved DEMO config** (same vars-lifetime argument as
    /// alpaca's), and the Demo tier resolves the demo HOST — the exec side's own pin.
    #[test]
    fn ctrader_plan_accepts_the_wired_pair_and_carries_the_demo_config() {
        match ctrader_plan(&ctrader_cfg(), &ctrader_vars()) {
            Ok(VenuePlan::Ctrader(config)) => {
                assert_eq!(
                    config.host, "demo.ctraderapi.com",
                    "Demo tier ⇒ the demo protobuf host, never live"
                );
                assert_eq!(config.port, vike_ctrader::config::CTRADER_PORT);
                assert_eq!(
                    config.account_id, None,
                    "no account id in the store ⇒ discovery at connect (never a live default — \
                     `conn.rs`'s NEVER-default-to-live rule)"
                );
            }
            other => {
                panic!("the wired ctrader pair with the tokens present must plan, got {other:?}")
            }
        }
    }

    /// **Absent credentials REFUSE the ctrader mount**, naming the app pair, the DEMO token pair
    /// and the store — same divergence-from-exec argument as alpaca's refusal.
    #[test]
    fn ctrader_plan_refuses_absent_credentials_naming_the_token_set() {
        let err = ctrader_plan(&ctrader_cfg(), &HashMap::new())
            .expect_err("no credentials must refuse the live mount, never mount feed-less");
        for needle in
            ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCESS_TOKEN", "_REFRESH_TOKEN", "secrets.env"]
        {
            assert!(err.contains(needle), "the refusal must name {needle}: {err}");
        }
        // The app pair alone (no token pair) is the same refusal — the loader is all-or-nothing.
        let mut app_only = ctrader_vars();
        app_only.retain(|k, _| !k.contains("TOKEN"));
        assert!(
            ctrader_plan(&ctrader_cfg(), &app_only).is_err(),
            "an app registration without an OAuth grant must refuse too"
        );
    }

    /// The ctrader foreign-symbol refusal — the same silent-drop hazard as every other venue.
    #[test]
    fn ctrader_plan_refuses_a_foreign_symbol() {
        let mut foreign = ctrader_cfg();
        let wired = foreign.token_id.clone();
        foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = ctrader_plan(&foreign, &ctrader_vars()).expect_err("foreign symbol");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
    }

    /// **Two ctrader mounts must agree on ONE synth interval** — they share one data socket and
    /// one `MakerSink` bar synthesizer, so a second interval would silently never fire (the
    /// polymarket same-token constraint, worn by the venue that synthesizes its bars).
    #[test]
    fn ctrader_mounts_must_agree_on_one_synth_interval() {
        let a = ctrader_cfg();
        let mut b = ctrader_cfg();
        b.interval = "5m".to_string();
        b.interval_ms = 300_000;
        let err = check_ctrader_intervals(&[&a, &b]).expect_err("two windows, one synth");
        assert!(
            err.contains("mounts[0]") && err.contains("mounts[1]"),
            "the refusal names both offending rows: {err}"
        );
        // Agreement passes, and other venues' rows never trip it.
        let c = ctrader_cfg();
        assert!(check_ctrader_intervals(&[&a, &c]).is_ok(), "one shared window is fine");
        let hl = MakerMountConfig::crypto("hyperliquid", "BTC", 0.5, 0.001);
        assert!(
            check_ctrader_intervals(&[&a, &hl]).is_ok(),
            "a non-ctrader row at any interval is not this gate's business"
        );
    }

    /// **The arming disclosures state each venue's fixed network and its quote-only requote lane.**
    /// Neither venue has a mainnet flag — alpaca is SANDBOX-pinned, ctrader DEMO-pinned, both by
    /// `make_engine`'s own tier resolution — and neither serves a book lane, so claiming the CEX
    /// `on_order_book` verb here would be the false-lanes claim `CexVenue::quote_source` records.
    #[test]
    fn alpaca_and_ctrader_arming_disclose_fixed_network_and_quote_only_requote_lane() {
        let alpaca = alpaca_arming(true);
        assert_eq!(alpaca.exec, "LIVE");
        assert_eq!(alpaca.network, "SANDBOX", "alpaca's own tier word, not DEMO");
        assert_eq!(alpaca.requote_lanes, "on_quote_tick", "no book lane exists on this venue");
        assert_eq!(alpaca.remedy, None, "a live mount has nothing to remedy");

        let ctrader = ctrader_arming(true);
        assert_eq!(ctrader.exec, "LIVE");
        assert_eq!(ctrader.network, "DEMO");
        assert_eq!(ctrader.requote_lanes, "on_quote_tick", "no trade/book lane on this venue");
        assert_eq!(ctrader.remedy, None);
    }

    /// **The two PAPER remedies name their own (different) causes.** Alpaca's paper state should
    /// be unreachable (the plan refused absent creds; `spawn` is infallible) — its remedy says
    /// "report a bug", never "add a key". Ctrader's IS reachable — a failed SYNCHRONOUS exec
    /// connect demotes to paper while the later data connect succeeds — and its remedy is a
    /// RESTART, not a key. Neither may advise credentials: the plan gate already proved them
    /// present, so key advice here would be unreachable-advice, the defect class
    /// `CexArming::remedy` documents.
    #[test]
    fn the_credentialed_data_paper_remedies_name_their_actual_causes() {
        let alpaca = alpaca_arming(false).remedy.expect("paper must carry a remedy");
        assert!(
            alpaca.contains("bug"),
            "alpaca paper with resolved creds is a gate disagreement to report: {alpaca}"
        );
        let ctrader = ctrader_arming(false).remedy.expect("paper must carry a remedy");
        assert!(
            ctrader.contains("restart") || ctrader.contains("Restart"),
            "ctrader paper means the exec handshake failed; the remedy is a retry: {ctrader}"
        );
        assert!(
            ctrader.contains("SYNCHRONOUSLY"),
            "…and it must say WHY exec can be paper while this very feed is live: {ctrader}"
        );
    }

    /// **The data-only disclosure replaces ONLY the remedy** ([`data_only_arming`]): the venue
    /// facts — network, requote lanes, quote source — stay whatever that venue's own
    /// `*_arming(false)` says (never a second copy that can drift), while the remedy names the
    /// DECLARATION, the mechanism, and the way back — and stops claiming a bug or a restart, the
    /// two ordinary paper causes that are false on the declared path.
    #[test]
    fn the_data_only_disclosure_names_the_declaration_and_keeps_the_venue_facts() {
        for (venue, base) in [
            ("alpaca", alpaca_arming(false)),
            ("ctrader", ctrader_arming(false)),
            ("oanda", oanda_arming(false)),
            ("ig", ig_arming(false)),
        ] {
            let plain = match venue {
                "alpaca" => alpaca_arming(false),
                "ctrader" => ctrader_arming(false),
                "oanda" => oanda_arming(false),
                _ => ig_arming(false),
            };
            let armed = data_only_arming(base, venue);
            assert_eq!(armed.exec, "PAPER", "{venue}: the declared state IS paper");
            assert_eq!(armed.network, plain.network, "{venue}: network is the venue's own fact");
            assert_eq!(armed.requote_lanes, plain.requote_lanes, "{venue}: lanes untouched");
            assert_eq!(armed.quote_source, plain.quote_source, "{venue}: source untouched");
            let remedy = armed.remedy.expect("a declared data-only mount still discloses WHY");
            for needle in ["data_only = true", "WITHHELD", "BY DECLARATION", venue] {
                assert!(remedy.contains(needle), "{venue}: must carry {needle:?}: {remedy}");
            }
            // The two ORDINARY paper causes, each false on the declared path: the
            // alpaca/oanda/ig gate-disagreement claim and ctrader's handshake-retry advice.
            // (The remedy MAY say "not a bug" — that is the correction, not the claim.)
            for false_claim in ["bug to report", "restart", "Restart"] {
                assert!(
                    !remedy.contains(false_claim),
                    "{venue}: the declared path must not claim {false_claim:?}: {remedy}"
                );
            }
        }
    }

    /// **The withhold is the venue's whole `{VENUE}_` key family and nothing else**
    /// ([`withhold_exec_credentials`]): every prefixed key goes (exec cannot resolve any tier),
    /// every foreign key stays (another venue's mount is untouched), and the count the
    /// disclosure logs is the count removed. Every key is spelled through its venue's own naming
    /// authority (`vike_oanda::oanda_env_var_names`, `vike_ig::ig_env_var_names` — the
    /// `oanda_vars` idiom above) so a key-grid rename reddens here rather than silently testing
    /// dead names — and neither a hardcoded `vars.get("…")` literal (which the settings-registry
    /// scanner resolves into a demand for a false `SETTINGS` row) nor a
    /// `vike_model::credential_keys` builder call (which enrols the whole crate as a
    /// generated-key composition site) appears here.
    #[test]
    fn withhold_exec_credentials_strips_the_venue_prefix_and_nothing_else() {
        let (key_k, acct_k) = vike_oanda::oanda_env_var_names(vike_bridge_core::Environment::Demo);
        // A FOREIGN venue's key (ig, through its own naming authority) and a non-venue-prefixed
        // key: both must survive an oanda withhold untouched.
        let (foreign, _, _) = vike_ig::ig_env_var_names(vike_bridge_core::Environment::Demo);
        let unrelated = "OPERATOR_NOTE".to_string();
        let mut vars = HashMap::from([
            (key_k.clone(), "tok".to_string()),
            (acct_k.clone(), "acct".to_string()),
            (foreign.clone(), "other-venue".to_string()),
            (unrelated.clone(), "kept".to_string()),
        ]);
        let withheld = withhold_exec_credentials(&mut vars, "oanda");
        assert_eq!(withheld, 2, "both oanda keys and only the oanda keys");
        assert!(!vars.contains_key(&key_k) && !vars.contains_key(&acct_k));
        assert!(
            vike_oanda::load_oanda_config_from(vike_bridge_core::Environment::Demo, &vars)
                .is_none(),
            "the loader make_engine's oanda arm calls must now resolve ABSENCE — that absence IS \
             the paper gate the declaration rides"
        );
        assert_eq!(vars.get(&foreign).map(String::as_str), Some("other-venue"));
        assert_eq!(vars.get(&unrelated).map(String::as_str), Some("kept"));
    }

    /// **The credentialed-data venues contribute NO reconcile feed-status row** — the DECISION
    /// documented on `LiveFeeds::recon_feed_statuses`: neither client exposes a status handle, and
    /// the exec plane already classifies both as interval-only, never-health-blocked venues.
    /// Constructed for real on the alpaca side (`AlpacaDataClient::new` is network-free — lazy
    /// connections); ctrader's arm is the same literal `HashMap::new()` but its client cannot be
    /// built without a live protobuf handshake, so its half rests on the same match arm this test
    /// pins the shape of.
    #[test]
    fn the_alpaca_feed_contributes_no_recon_health_row() {
        let config = vike_alpaca::AlpacaConfig {
            client_id: "cid".to_string(),
            client_secret: "sec".to_string(),
            account_id: "acct".to_string(),
            env: vike_bridge_core::Environment::Demo,
            hosts: vike_alpaca::hosts_for(vike_bridge_core::Environment::Demo),
        };
        let client = vike_alpaca::AlpacaDataClient::new(config, Arc::new(NullSink), || {});
        let feeds = LiveFeeds::Alpaca(client);
        assert!(
            feeds.recon_feed_statuses().is_empty(),
            "no status handle exists on this seam — an invented row could only suppress passes"
        );
    }

    // ── OANDA: the third credentialed-data arm, and the only one with no socket at all ────────

    /// The practice-tier token + account pair, exactly as `load_oanda_config_from(Demo, …)` looks
    /// them up (`vike_oanda::oanda_env_var_names` is the naming authority; spelled through it so
    /// a rename of the key grid reddens here rather than silently testing dead names).
    fn oanda_vars() -> HashMap<String, String> {
        let (key_k, acct_k) = vike_oanda::oanda_env_var_names(vike_bridge_core::Environment::Demo);
        HashMap::from([
            (key_k, "tok-abc-123".to_string()),
            (acct_k, "101-004-1234567-001".to_string()),
        ])
    }

    fn oanda_cfg() -> MakerMountConfig {
        let symbol = wired_symbol_for("oanda").expect("build_node mounts oanda");
        let mut cfg = MakerMountConfig::crypto("oanda", symbol, 0.00001, 1_000.0);
        cfg.interval = "1m".to_string();
        cfg.interval_ms = 60_000;
        cfg
    }

    /// **The oanda plan CARRIES the resolved PRACTICE session** (the same vars-lifetime argument
    /// as alpaca's and ctrader's), and the Demo tier resolves the fxPractice hosts on BOTH bases
    /// — the REST one the candle poll fetches from and the STREAM one the pricing stream dials.
    /// Both travel in the one config, so the two feed lanes cannot end up on different networks.
    #[test]
    fn oanda_plan_accepts_the_wired_pair_and_carries_the_practice_session() {
        match oanda_plan(&oanda_cfg(), &oanda_vars()) {
            Ok(VenuePlan::Oanda(config)) => {
                assert_eq!(config.account_id, "101-004-1234567-001", "the account travels");
                assert!(
                    config.rest_base.contains("fxpractice")
                        && config.stream_base.contains("fxpractice"),
                    "Demo tier ⇒ fxPractice on BOTH bases (the exec side's own pin): {} / {}",
                    config.rest_base,
                    config.stream_base
                );
                assert!(
                    !format!("{config:?}").contains("tok-abc-123"),
                    "the plan is `Debug`ged by the allow-list refusals — the bearer token must \
                     never be in that output"
                );
            }
            other => panic!("the wired oanda pair with the pair present must plan, got {other:?}"),
        }
    }

    /// **Absent credentials REFUSE the oanda mount** — the alpaca divergence-from-exec argument,
    /// doubled: BOTH of this venue's feed lanes are Bearer-authed, so there is no keyless half to
    /// fall back to either. The refusal must name the exact pair and the store.
    #[test]
    fn oanda_plan_refuses_absent_credentials_naming_the_token_and_account() {
        let err = oanda_plan(&oanda_cfg(), &HashMap::new())
            .expect_err("no credentials must refuse the live mount, never mount feed-less");
        for needle in ["_API_KEY", "_ACCOUNT_ID", "DEMO", "secrets.env"] {
            assert!(err.contains(needle), "the refusal must name {needle}: {err}");
        }
        // A token with no account id is the same refusal — the loader is all-or-nothing, and a
        // half-configured store is exactly how an operator arrives here.
        let mut partial = oanda_vars();
        partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
        assert!(oanda_plan(&oanda_cfg(), &partial).is_err(), "a lone token must refuse too");
    }

    /// The oanda-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
    /// shares), a degenerate tick size, and — the one that is this venue's own — an interval the
    /// candles endpoint has no GRANULARITY code for. The last is DERIVED from
    /// `vike_oanda::granularity` in both directions here: a mappable interval must pass and an
    /// unmappable one must fail, so the gate cannot drift from the table it reads.
    #[test]
    fn oanda_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
        let mut foreign = oanda_cfg();
        let wired = foreign.token_id.clone();
        foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = oanda_plan(&foreign, &oanda_vars()).expect_err("foreign symbol");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

        let mut bad_tick = oanda_cfg();
        bad_tick.tick_size = 0.0;
        assert!(
            oanda_plan(&bad_tick, &oanda_vars())
                .expect_err("degenerate tick")
                .contains("tick_size"),
            "the tick refusal names the field"
        );

        // The venue's own refusal, both directions against the one table.
        let mut unservable = oanda_cfg();
        unservable.interval = "7m".to_string();
        unservable.interval_ms = 420_000;
        assert!(
            vike_oanda::granularity("7m").is_none(),
            "the fixture interval must genuinely be unmappable, or this proves nothing"
        );
        let err = oanda_plan(&unservable, &oanda_vars()).expect_err("unservable interval");
        assert!(err.contains("7m"), "the refusal must name the interval asked for: {err}");
        assert!(
            err.contains("granularity"),
            "…and the table that decided it, so the operator can look up what IS servable: {err}"
        );

        // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
        // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
        let mut four_hour = oanda_cfg();
        four_hour.interval = "4h".to_string();
        four_hour.interval_ms = 14_400_000;
        assert!(
            vike_oanda::granularity("4h").is_some()
                && oanda_plan(&four_hour, &oanda_vars()).is_ok(),
            "oanda serves many granularities; only an UNMAPPABLE interval may be refused"
        );
    }

    /// **The oanda arming discloses the venue's own network word and its quote-only requote
    /// lane.** There is no `OANDA_MAINNET` flag — `make_engine` resolves the Demo tier
    /// unconditionally and `oanda_hosts` maps it to fxPractice — and the venue publishes neither a
    /// book lane nor a trade tape, so claiming the CEX `on_order_book` verb would be the
    /// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
    /// `OandaExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials
    /// can only be the two gates disagreeing over one map.
    #[test]
    fn oanda_arming_discloses_the_practice_network_and_a_quote_only_requote_lane() {
        let live = oanda_arming(true);
        assert_eq!(live.exec, "LIVE");
        assert_eq!(live.network, "PRACTICE", "the venue's own word for its non-live environment");
        assert_eq!(live.requote_lanes, "on_quote_tick", "no book lane and no trade tape here");
        assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

        let paper = oanda_arming(false).remedy.expect("paper must carry a remedy");
        assert!(
            paper.contains("bug"),
            "oanda paper with resolved creds is a gate disagreement to report: {paper}"
        );
        assert!(
            !paper.to_lowercase().contains("add "),
            "…and it must NOT advise adding a key: `oanda_plan` already proved the pair present, \
             so key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
        );
    }

    /// **The oanda feed contributes NO reconcile health row either — and this one is a genuine
    /// DECISION rather than an absence.** Unlike `AlpacaDataClient`/`CtraderData`, this client DOES
    /// expose a `status` handle of exactly the shape the CEX row keys on, so the row is withheld on
    /// its merits: one last-writer-wins string is shared by the quote reader and every candle
    /// poller, and a transient poll failure would read `Degraded` and suppress a reconcile pass
    /// the bar lane has nothing to do with. Constructible for real here — `Feeds::new` is
    /// network-free (threads are spawned per subscription, and this feed has none).
    #[test]
    fn the_oanda_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
        let feeds = vike_oanda::market_feed::Feeds::new(Arc::new(NullSink), || {});
        assert!(
            !feeds.status.lock().expect("status").is_empty(),
            "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
        );
        let live = LiveFeeds::Oanda(Box::new(feeds));
        assert!(
            live.recon_feed_statuses().is_empty(),
            "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' oanda \
             paragraph for what would earn the row"
        );
    }

    // ── DERIBIT: the widest feed arm, and the one with NO credential gate ─────────────────────

    fn deribit_cfg() -> MakerMountConfig {
        let symbol = wired_symbol_for("deribit").expect("build_node mounts deribit");
        let mut cfg = MakerMountConfig::crypto("deribit", symbol, 0.5, 10.0);
        cfg.interval = "1m".to_string();
        cfg.interval_ms = 60_000;
        cfg
    }

    /// deribit is joined to the exec plane the same way every other live-wired slug is: an engine
    /// row in `vike_run::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
    #[test]
    fn deribit_names_a_wired_market_and_a_live_wired_row() {
        assert!(
            wired_symbol_for("deribit").is_some(),
            "deribit has a live feed arm but `build_node` mounts no engine for it"
        );
        assert!(
            crate::config::LIVE_WIRED_VENUES.contains(&"deribit"),
            "deribit has a feed arm but is not advertised in LIVE_WIRED_VENUES"
        );
    }

    /// **The deribit plan takes NO credentials and refuses none** — the property that separates
    /// this arm from its three credentialed-data neighbours, asserted in the only way that cannot
    /// rot: the plan function takes no vars map at all, and `venue_feed_plan` reaches it with an
    /// EMPTY one and still succeeds. Its feed is keyless public MAINNET, so absent keys are the
    /// ordinary unconfigured state and leave a working feed over a paper book.
    #[test]
    fn deribit_plan_needs_no_credentials_and_mounts_on_an_empty_store() {
        match deribit_plan(&deribit_cfg()) {
            Ok(VenuePlan::Deribit) => {}
            other => panic!(
                "the wired deribit pair must plan with no credential map in sight, got {other:?}"
            ),
        }
        // …and the venue really is one `venue_feed_plan` reaches with an empty store, which is the
        // end-to-end statement of the same fact (the three credentialed arms cannot do this).
        assert!(
            // `hyperliquid_mainnet: false` is inert here — this call exercises the deribit arm,
            // which never reads it.
            venue_feed_plan(&deribit_cfg(), &HashMap::new(), false).is_ok(),
            "an empty credential map must still plan a deribit live mount — a keyless feed has no \
             refusal to make, and inventing one would be the failure this arm exists not to copy"
        );
    }

    /// The deribit-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
    /// shares), a degenerate tick size, and — this venue's own — an interval the chart channel has
    /// no RESOLUTION code for. The last is DERIVED from `vike_deribit::data::resolution_code` in
    /// both directions here: a mappable interval must pass and an unmappable one must fail, so the
    /// gate cannot drift from the table it reads.
    #[test]
    fn deribit_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
        let mut foreign = deribit_cfg();
        let wired = foreign.token_id.clone();
        foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = deribit_plan(&foreign).expect_err("foreign symbol");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

        let mut bad_tick = deribit_cfg();
        bad_tick.tick_size = 0.0;
        assert!(
            deribit_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
            "the tick refusal names the field"
        );

        // The venue's own refusal, both directions against the one table. `4h` is not an arbitrary
        // fixture: the deribit resolution enum genuinely skips it, which is exactly the kind of
        // gap that makes a hand-written list of intervals unsafe here.
        let mut unservable = deribit_cfg();
        unservable.interval = "4h".to_string();
        unservable.interval_ms = 14_400_000;
        assert!(
            vike_deribit::data::resolution_code("4h").is_err(),
            "the fixture interval must genuinely be unmappable, or this proves nothing"
        );
        let err = deribit_plan(&unservable).expect_err("unservable interval");
        assert!(err.contains("4h"), "the refusal must name the interval asked for: {err}");
        assert!(
            err.contains("resolution_code"),
            "…and the table that decided it, so the operator can look up what IS servable: {err}"
        );

        // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
        // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
        let mut two_hour = deribit_cfg();
        two_hour.interval = "2h".to_string();
        two_hour.interval_ms = 7_200_000;
        assert!(
            vike_deribit::data::resolution_code("2h").is_ok() && deribit_plan(&two_hour).is_ok(),
            "deribit serves many resolutions; only an UNMAPPABLE interval may be refused"
        );
    }

    /// **The deribit arming discloses the TESTNET exec network, the MAINNET quote source, and the
    /// full requote pair.** All three are facts rather than style: `make_engine` loads the DEMO
    /// key tier and the bridge's authed sockets are hardcoded testnet while every public read is
    /// hardcoded mainnet, and `venue_caps::DERIBIT` declares `book: true`, which this arm really
    /// subscribes. The PAPER remedy is the CEX "add the keys" one — reachable, unlike the
    /// credentialed arms' "report a bug" — and must NOT invent a `{VENUE}_MAINNET` flag, which
    /// this venue does not have.
    #[test]
    fn deribit_arming_discloses_testnet_exec_over_mainnet_prices() {
        let live = deribit_arming(true);
        assert_eq!(live.exec, "LIVE");
        assert_eq!(live.network, "TESTNET", "every authed deribit socket is hardcoded testnet");
        assert_eq!(
            live.requote_lanes, "on_quote_tick + on_order_book",
            "this venue declares `book: true` and the arm subscribes the lossless book lane"
        );
        assert!(
            live.quote_source.contains("MAINNET"),
            "the feed reads a DIFFERENT network from exec — a disclosure that hid that would be a \
             half-truth: {}",
            live.quote_source
        );
        assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

        // The caps row is the authority for the lane claim above, read rather than restated.
        let caps = vike_model::venue_caps::caps_for("deribit");
        assert!(caps.live_data.book, "the `on_order_book` half of the claim comes from this row");
        assert!(!caps.live_data.depth, "…and the conflating DOM lane stays unwired");

        let paper = deribit_arming(false).remedy.expect("paper must carry a remedy");
        assert!(
            paper.contains("_API_KEY") && paper.contains("_API_SECRET"),
            "the keyless-feed venue's remedy IS reachable and must name the exec keys: {paper}"
        );
        assert!(
            !paper.contains("MAINNET=1"),
            "deribit is SWITCHLESS (`mainnet_switch_for`) — advising a flag that is read nowhere \
             is the unreachable-advice defect `CexArming::remedy` documents: {paper}"
        );
        assert!(
            vike_bridge_core::mainnet::mainnet_switch_for("deribit").is_none(),
            "…and that claim is read from the switch table, not asserted from memory"
        );
    }

    /// **The deribit feed contributes NO reconcile health row** — the oanda decision, one lane
    /// count worse. This client exposes a `status` handle of the CEX row's shape, but four lanes
    /// share it last-writer-wins, and the book lane writes an error string on every deliberate
    /// resync (a chain gap IS a session fault here), so a healthy feed would publish
    /// `Degraded`-reading text as ordinary operation and suppress passes. Constructible for real —
    /// `Feeds::new` is network-free.
    #[test]
    fn the_deribit_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
        let feeds = vike_deribit::market_feed::Feeds::new(Arc::new(NullSink), || {});
        assert!(
            !feeds.status.lock().expect("status").is_empty(),
            "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
        );
        let live = LiveFeeds::Deribit(Box::new(feeds));
        assert!(
            live.recon_feed_statuses().is_empty(),
            "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' deribit \
             paragraph for what would earn the row"
        );
    }

    // ── IG: the narrowest feed arm — two verbs, and the absence is structural ─────────────────

    /// The DEMO trio, exactly as `load_ig_config_from(Demo, …)` looks it up
    /// (`vike_ig::ig_env_var_names` is the naming authority; spelled through it so a rename of the
    /// key grid reddens here rather than silently testing dead names).
    fn ig_vars() -> HashMap<String, String> {
        let (key_k, id_k, pw_k) = vike_ig::ig_env_var_names(vike_bridge_core::Environment::Demo);
        HashMap::from([
            (key_k, "key-xyz".to_string()),
            (id_k, "myuser".to_string()),
            (pw_k, "s3cr3t".to_string()),
        ])
    }

    fn ig_cfg() -> MakerMountConfig {
        let symbol = wired_symbol_for("ig").expect("build_node mounts ig");
        let mut cfg = MakerMountConfig::crypto("ig", symbol, 0.1, 1.0);
        cfg.interval = "1m".to_string();
        cfg.interval_ms = 60_000;
        cfg
    }

    /// ig is joined to the exec plane the same way every other live-wired slug is: an engine row
    /// in `vike_run::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
    #[test]
    fn ig_names_a_wired_market_and_a_live_wired_row() {
        assert!(
            wired_symbol_for("ig").is_some(),
            "ig has a live feed arm but `build_node` mounts no engine for it"
        );
        assert!(
            crate::config::LIVE_WIRED_VENUES.contains(&"ig"),
            "ig has a feed arm but is not advertised in LIVE_WIRED_VENUES"
        );
    }

    /// **The ig plan CARRIES the resolved DEMO login** (the same vars-lifetime argument as
    /// alpaca's, ctrader's and oanda's — `vars` is moved into the `NodeConfig` before the feed
    /// block runs), and the Demo tier resolves the DEMO dealing gateway, which is the base every
    /// per-subscription `IgSession::login` will use.
    #[test]
    fn ig_plan_accepts_the_wired_pair_and_carries_the_demo_session() {
        match ig_plan(&ig_cfg(), &ig_vars()) {
            Ok(VenuePlan::Ig(config)) => {
                assert_eq!(
                    config.rest_base,
                    vike_ig::ig_rest_base(vike_bridge_core::Environment::Demo),
                    "the Demo tier must resolve the DEMO gateway, read from the venue's own \
                     resolver rather than restated here"
                );
                let dbg = format!("{config:?}");
                for secret in ["key-xyz", "myuser", "s3cr3t"] {
                    assert!(
                        !dbg.contains(secret),
                        "the plan is `Debug`ged by the allow-list refusals — no IG secret may be \
                         in that output, and {secret} was: {dbg}"
                    );
                }
            }
            other => panic!("the wired ig pair with the trio present must plan, got {other:?}"),
        }
    }

    /// **Absent credentials REFUSE the ig mount** — the alpaca divergence-from-exec argument:
    /// every Lightstreamer subscription opens its own IG login, so there is no keyless half to
    /// fall back to. The refusal must name the exact trio and the store. A PARTIAL trio refuses
    /// too — the loader is all-or-nothing, and a half-filled store is exactly how an operator
    /// arrives here.
    #[test]
    fn ig_plan_refuses_absent_credentials_naming_the_whole_trio() {
        let err = ig_plan(&ig_cfg(), &HashMap::new())
            .expect_err("no credentials must refuse the live mount, never mount feed-less");
        for needle in ["_API_KEY", "_IDENTIFIER", "_PASSWORD", "DEMO", "secrets.env"] {
            assert!(err.contains(needle), "the refusal must name {needle}: {err}");
        }
        let mut partial = ig_vars();
        partial.retain(|k, _| !k.ends_with("_PASSWORD"));
        assert!(ig_plan(&ig_cfg(), &partial).is_err(), "a trio missing its password must refuse");
    }

    /// The ig-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
    /// shares), a degenerate tick size, and — this venue's own — an interval Lightstreamer has no
    /// chart SCALE for. The last is DERIVED from `vike_ig::market_data::ig_scale` in both
    /// directions here: a streamable interval must pass and an unstreamable one must fail, so the
    /// gate cannot drift from the table it reads.
    #[test]
    fn ig_plan_refuses_a_foreign_symbol_and_an_unstreamable_interval() {
        let mut foreign = ig_cfg();
        let wired = foreign.token_id.clone();
        foreign.token_id = format!("{wired}.NOT.THE.MOUNTED.ONE");
        let err = ig_plan(&foreign, &ig_vars()).expect_err("foreign symbol");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the epic build_node mounts: {err}");

        let mut bad_tick = ig_cfg();
        bad_tick.tick_size = 0.0;
        assert!(
            ig_plan(&bad_tick, &ig_vars()).expect_err("degenerate tick").contains("tick_size"),
            "the tick refusal names the field"
        );

        // The venue's own refusal, both directions against the one table. `15m` is a width the
        // crate's REST history ladder serves and the STREAMING set does not, which is precisely
        // why the gate reads `ig_scale` rather than "does IG have candles at all".
        let mut unstreamable = ig_cfg();
        unstreamable.interval = "15m".to_string();
        unstreamable.interval_ms = 900_000;
        assert!(
            vike_ig::market_data::ig_scale("15m").is_none(),
            "the fixture interval must genuinely be unstreamable, or this proves nothing"
        );
        let err = ig_plan(&unstreamable, &ig_vars()).expect_err("unstreamable interval");
        assert!(err.contains("15m"), "the refusal must name the interval asked for: {err}");
        assert!(
            err.contains("ig_scale"),
            "…and the table that decided it, so the operator can look up what IS streamed: {err}"
        );

        // …and a NON-1m interval the venue DOES stream must pass — this is not alpaca, whose WS
        // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
        let mut five_minute = ig_cfg();
        five_minute.interval = "5m".to_string();
        five_minute.interval_ms = 300_000;
        assert!(
            vike_ig::market_data::ig_scale("5m").is_some()
                && ig_plan(&five_minute, &ig_vars()).is_ok(),
            "ig streams several scales; only an UNSTREAMABLE interval may be refused"
        );
    }

    /// **The ig arming discloses the DEMO gateway and a QUOTE-ONLY requote lane, and the second
    /// half is structural.** `venue_caps::IG` declares no trade tape and no book because a DEALER
    /// venue publishes neither — the same fact that keeps IG DEFERRED in
    /// `market_data_conformance.rs` — so claiming the CEX `on_order_book` verb would be the
    /// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
    /// `IgExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials can
    /// only be the two gates disagreeing over one map.
    #[test]
    fn ig_arming_discloses_the_demo_gateway_and_a_quote_only_requote_lane() {
        let live = ig_arming(true);
        assert_eq!(live.exec, "LIVE");
        assert_eq!(live.network, "DEMO", "the tier and the gateway are the same word here");
        assert_eq!(live.requote_lanes, "on_quote_tick", "no trade tape and no ladder on a dealer");
        assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

        // The caps row is the authority for the lane claim above, read rather than restated — and
        // it is what makes the two absences structural rather than unwired.
        let caps = vike_model::venue_caps::caps_for("ig");
        assert!(caps.live_data.quotes && caps.live_data.bars, "the two verbs this arm subscribes");
        assert!(
            !caps.live_data.trades && !caps.live_data.book && !caps.live_data.depth,
            "…and the three IG structurally cannot serve; widening the arm past this row would \
             turn a venue fact into a mount error"
        );

        let paper = ig_arming(false).remedy.expect("paper must carry a remedy");
        assert!(
            paper.contains("bug"),
            "ig paper with resolved creds is a gate disagreement to report: {paper}"
        );
        assert!(
            !paper.to_lowercase().contains("add "),
            "…and it must NOT advise adding a key: `ig_plan` already proved the trio present, so \
             key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
        );
    }

    /// **The ig feed contributes NO reconcile health row** — the oanda decision once more, and the
    /// closed-market case makes it sharpest: a status handle of the CEX row's shape exists, shared
    /// last-writer-wins across subscription threads that each hold their own IG session, and FX
    /// closes every weekend, so a gate keyed on it would suppress passes on quiet rather than on
    /// failure. Constructible for real — `Feeds::new` is network-free (threads are spawned per
    /// subscription, and this feed has none).
    #[test]
    fn the_ig_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
        let config = vike_ig::load_ig_config_from(vike_bridge_core::Environment::Demo, &ig_vars())
            .expect("the fixture trio resolves");
        let feeds = vike_ig::market_feed::Feeds::new(Arc::new(NullSink), || {}, config);
        assert!(
            !feeds.status.lock().expect("status").is_empty(),
            "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
        );
        let live = LiveFeeds::Ig(feeds);
        assert!(
            live.recon_feed_statuses().is_empty(),
            "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' ig \
             paragraph for what would earn the row"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // `parse_args_from` — the LIVE DAEMON's whole argument surface.
    //
    // It had no test at all until this section, because `parse_args` read `std::env::args()` and so
    // could not be driven. Everything below is the SAME parser the shipped daemon runs; only the
    // argv SOURCE is injected.
    // ---------------------------------------------------------------------------------------------

    /// An argv stream as `parse_args_from` takes it — already `argv[0]`-stripped.
    fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
    /// expects a run rather than help or a version.
    fn run_args(v: &[&str]) -> Args {
        match parse_args_from(argv(v)) {
            Ok(Parsed::Args(a)) => a,
            other => panic!("expected a run from {v:?}, got {other:?}"),
        }
    }

    /// **`--config` is REQUIRED and it is the whole daemon**: it names the profile TOML that decides
    /// the venue, the token and the A-S mount shape. Absent, the parse must fail rather than start a
    /// daemon on defaults.
    #[test]
    fn config_is_required_and_its_absence_is_named() {
        let e = parse_args_from(argv(&[])).expect_err("a bare invocation must not start a daemon");
        assert!(e.contains("--config"), "the error names the missing flag: {e}");
        let profile_only =
            parse_args_from(argv(&["--profile", "run.toml"])).expect_err("--profile is not enough");
        assert!(profile_only.contains("--config"), "{profile_only}");
    }

    /// Both flags, in BOTH spellings each — `--flag value` and `--flag=value`. The `=` form is what a
    /// systemd `ExecStart=` line tends to carry, and this is the only parser in this sweep that
    /// accepts it.
    #[test]
    fn both_flags_parse_in_both_spellings() {
        for spelling in [
            &["--config", "d.toml", "--profile", "r.toml"][..],
            &["--config=d.toml", "--profile=r.toml"][..],
            &["--config=d.toml", "--profile", "r.toml"][..],
            &["--config", "d.toml", "--profile=r.toml"][..],
        ] {
            let a = run_args(spelling);
            assert_eq!(a.config_path, "d.toml", "{spelling:?}");
            assert_eq!(a.profile_path.as_deref(), Some("r.toml"), "{spelling:?}");
        }
        // …and `--profile` is genuinely OPTIONAL: absent it falls through to $VIKE_RUN_PROFILE.
        assert_eq!(run_args(&["--config", "d.toml"]).profile_path, None);
    }

    /// A value containing `=` survives BOTH spellings — `split_once` takes the FIRST `=` only, so a
    /// path like `/etc/vike/a=b.toml` is not truncated either way. Worth pinning because it is the
    /// one place the `=` spelling could silently corrupt an operator's path.
    #[test]
    fn a_value_containing_an_equals_sign_is_not_truncated() {
        assert_eq!(run_args(&["--config", "/etc/a=b.toml"]).config_path, "/etc/a=b.toml");
        assert_eq!(run_args(&["--config=/etc/a=b.toml"]).config_path, "/etc/a=b.toml");
    }

    /// **`--help` and `--version` are SUCCESSES, not errors** — the regression this file's `Parsed`
    /// enum exists to prevent (a non-zero `--help` breaks `set -e`, packaging smoke tests and every
    /// wrapper that checks a status). Both spellings of each, and `--help` reached AFTER other
    /// arguments, which is how a person actually asks for it.
    #[test]
    fn help_and_version_are_outcomes_not_errors() {
        for flag in ["-h", "--help"] {
            assert!(matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Help)), "{flag}");
        }
        for flag in ["-V", "--version"] {
            assert!(matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Version)), "{flag}");
        }
        // Asked for after a valid --config: help still wins, and `--config` being satisfied is not
        // what makes it work — the arm returns before the required-flag check.
        assert!(matches!(
            parse_args_from(argv(&["--config", "d.toml", "--help"])),
            Ok(Parsed::Help)
        ));
        assert!(matches!(parse_args_from(argv(&["--help", "--config"])), Ok(Parsed::Help)));
        // `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box, so it stays an
        // unknown argument here rather than silently printing a version.
        assert!(parse_args_from(argv(&["-v"])).is_err(), "-v must not be --version");
    }

    /// A typo'd flag is REJECTED and NAMED, not ignored — including the `=` spelling of one, which
    /// is the form a `systemd` unit line takes.
    #[test]
    fn an_unknown_argument_is_rejected_and_named() {
        for bad in ["--conf", "--config-path", "--Config", "-c", "d.toml"] {
            let e = parse_args_from(argv(&[bad, "d.toml"])).expect_err("a typo must not run");
            assert!(e.contains(bad), "the error names the offending argument: {e}");
        }
        let inline = parse_args_from(argv(&["--conf=d.toml"])).expect_err("the = spelling too");
        assert!(inline.contains("--conf"), "{inline}");
    }

    /// A trailing valued flag is an ERROR rather than a silently-defaulted one — the hole every
    /// `vike-backfill` bin has and this daemon does not, because both arms `ok_or` instead of
    /// letting `it.next()`'s `None` fall through.
    #[test]
    fn a_trailing_valued_flag_is_an_error_in_both_arms() {
        for flag in ["--config", "--profile"] {
            let e = parse_args_from(argv(&[flag])).expect_err("a trailing valued flag");
            assert!(e.contains(flag), "{e}");
        }
        // …and one with a value already given still errors on the trailing one.
        let e = parse_args_from(argv(&["--config", "d.toml", "--profile"])).expect_err("trailing");
        assert!(e.contains("--profile"), "{e}");
    }

    /// **A FINDING, now refused — and the point is that the two SPELLINGS agree.** `--config=` with
    /// nothing after the `=` used to be ACCEPTED and to yield an EMPTY config path: the inline
    /// branch took `split_once`'s right half verbatim and no arm checked it for emptiness. A
    /// `systemd` unit whose `ExecStart` interpolates an unset shell variable produces exactly that
    /// line. The daemon then failed opening `""`, so it did not trade on a default — but the
    /// required-flag check it was supposed to trip had already passed, and the diagnostic an
    /// operator got was a file-open error rather than "you gave no --config". The space spelling
    /// did NOT have the hole, so the same flag behaved differently depending on how it was written.
    ///
    /// All four ways of writing "no value" are now the same refusal, and each error names the flag:
    /// no value at all, an empty inline value, an empty quoted argument (`--config ""`, the OTHER
    /// shape an unset `$VIKE_CONFIG` takes), and a whitespace-only one.
    #[test]
    fn an_empty_value_is_refused_in_both_spellings_exactly_like_a_missing_one() {
        for line in
            [&["--config"][..], &["--config="][..], &["--config", ""][..], &["--config", "   "][..]]
        {
            let e =
                parse_args_from(argv(line)).expect_err("an empty config path is no config path");
            assert!(e.contains("--config"), "the error names the flag for {line:?}: {e}");
        }
        for line in [
            &["--config", "d.toml", "--profile"][..],
            &["--config", "d.toml", "--profile="][..],
            &["--config", "d.toml", "--profile", ""][..],
        ] {
            let e = parse_args_from(argv(line)).expect_err("…and the optional flag the same way");
            assert!(e.contains("--profile"), "{line:?}: {e}");
        }
        // OMITTING `--profile` is still how you say "no run profile" — it falls through to
        // $VIKE_RUN_PROFILE. Refusing an EMPTY value must not have made the flag mandatory.
        assert_eq!(run_args(&["--config", "d.toml"]).profile_path, None);
    }

    /// **A FINDING, pinned.** A repeated flag takes the LAST value, silently — so a unit file that
    /// gained a second `--config` line (a merge, an override drop-in) runs the daemon on the second
    /// profile with nothing said about the first. Pinned because the direction is what an operator
    /// appending an override depends on, and because "silently" is the part worth knowing.
    #[test]
    fn a_repeated_flag_takes_the_last_value_silently() {
        assert_eq!(run_args(&["--config", "a.toml", "--config", "b.toml"]).config_path, "b.toml");
        assert_eq!(run_args(&["--config=a.toml", "--config=b.toml"]).config_path, "b.toml");
    }

    /// **A FINDING, now refused — and the diagnostic is about the right token.** In the SPACE
    /// spelling a valued flag used to consume whatever followed, including another flag:
    /// `--config --profile r.toml` yielded the config path `"--profile"` and then died on `r.toml`
    /// as an unknown argument, so the message named a token the operator had written correctly.
    /// The TRAILING case had nothing left to trip over and parsed CLEANLY with a config path of
    /// `--profile`, after which the daemon failed opening a file by that name.
    ///
    /// Both now name the unfed flag AND the flag that would have been eaten, and neither mentions
    /// the innocent trailing value.
    #[test]
    fn a_swallowed_flag_is_refused_and_the_diagnostic_names_both_flags() {
        let e = parse_args_from(argv(&["--config", "--profile", "r.toml"]))
            .expect_err("a flag is not a config path");
        assert!(e.contains("--config") && e.contains("--profile"), "both are named: {e}");
        assert!(!e.contains("r.toml"), "…and the message is no longer about the value: {e}");
        // The trailing case, which used to parse cleanly.
        let tail =
            parse_args_from(argv(&["--config", "--profile"])).expect_err("no longer a config path");
        assert!(tail.contains("--config") && tail.contains("--profile"), "{tail}");
        // The `=` spelling of the same mistake is refused too, so neither form has a hole.
        let inline = parse_args_from(argv(&["--config=--profile"])).expect_err("inline too");
        assert!(inline.contains("--config") && inline.contains("--profile"), "{inline}");
    }
}
