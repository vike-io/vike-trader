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
//! every one of those reads seeing the SAME map the boot consumed, rather than five independent
//! `std::env::vars()` sweeps that could each answer differently mid-process.
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
//! it through [`vike_mount::build_paper_strategy_core_with`] / [`vike_mount::build_live_strategy_core`].
//! So the profile that was backtested is the profile that trades.
//!
//! ABSENT `[strategy]`, the mounted strategy is the Avellaneda–Stoikov `vike_mm::SpreadMaker` built
//! from the profile's own maker fields by [`vike_mount::build_maker`] — the same function the
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
//! with zero GUI code: it mounts the resolved strategy on the PRODUCTION live core
//! ([`vike_core::spawn_core`]) with the paper exchange as the `ExecutionClient` — so there is ZERO
//! real-money / credential / geo risk and NO live feed (a maker only quotes once a feed drives it).
//!
//! The `tradehub_live` FLAG (`<project>/settings/flags.toml`, still overridden by
//! `VIKE_TRADEHUB_LIVE`) instead mounts the SAME strategy on the REAL wired-market [`vike_mount::build_node`] core
//! with per-venue credential-gated exec AND wires the venue's own
//! LIVE feed. OFF (the default) is byte-identical to the pure-paper daemon. The path is defended by
//! FIVE gates: (1) the `tradehub_live` master gate; (2) per-venue creds in the workspace `.env`
//! (absent creds keep that venue paper even with the gate on); (3) each venue's own network gate —
//! for binance/bybit/okx/hyperliquid the ceiling IS the network (decision 0095: `live` means
//! mainnet, anything below it demo/testnet); (4) the daemon's own venue ALLOW-LIST
//! ([`live_mount`] hard-errors on any unwired venue — never a silent paper fallback); (5) the
//! venue+symbol validation against build_node's own `WIRED_MARKETS` table
//! ([`crate::config::DaemonProfile::validate_for_live`]). The live-wired venue set is
//! [`crate::config::LIVE_WIRED_VENUES`] — pinned equal to `venue_feed_plan`'s arms by
//! `tests/daemon/live_wired_venues_pin.rs`, so it is not restated here (a previous sentence here
//! named the set and was two venues stale within a month). Each venue quotes in its own price
//! domain: Polymarket via [`MakerMountConfig::outcome_token`] (the \[0,1\] outcome-token domain), every
//! other venue via [`MakerMountConfig::crypto`] (`vike_model::PriceDomain::Unbounded` +
//! RawLocal/ConstantTau) — the $-scale domain that lifts the old A-S `[tick, 1−tick]` wall clamp
//! (`vike_mm::avellaneda` §2.2) which used to pin a $64k quote to `None`.
//!
//! ## Reconcile-on-restart — ON by default for a LIVE mount, QUARANTINE-FIRST (S2)
//! So a restarted LIVE daemon re-adopts open venue orders/positions instead of mounting a blind fresh
//! core, [`live_mount`] mounts the reconciliation engine ([`vike_core::spawn_recon`]) exactly as
//! `vike-app` did while the desktop mounted venues — same [`crate::reconcile_config`] env parser,
//! same `node.recon_clients`/`node.recon_trigger`, same per-venue `ReconConfig`. This daemon is
//! the only reconciling root now.
//!
//! ⚠ **The gate FLIPPED on 2026-09-06, and the flip is the whole point of this section.** It used to
//! be off unless an operator exported `VIKE_RECONCILE=1`, which meant a live daemon that restarted
//! and never asked the venue what it held was trading against a BELIEF. The decision is
//! [`crate::reconcile_config::reconcile_gate`] — called once here (and once in `vike-app`, until
//! the desktop lost its local core, so the two binaries could not answer differently) — and it
//! takes three inputs: the resolved `flags.reconcile`
//! (force-on), the resolved `flags.reconcile_off` (the refusal, which wins), and the LENGTH of
//! [`vike_mount::armed_live_venues`] — this mount's own pre-mount probe of how many venue ACCOUNTS it
//! is about to authenticate as. Non-zero ⇒ reconcile. A PAPER mount is unchanged and byte-identical:
//! the probe answers zero, `recon_enabled` stays `false`, `build_node` builds no reconnect-trigger
//! channel, no `ReconConfig` is built, `spawn_recon` is never called, and no `vt-core-recon` thread
//! is spawned.
//!
//! The POLICY is **`quarantine`** (holds every divergence for operator confirm; auto-folds NOTHING)
//! unless the operator sets `VIKE_RECONCILE_POLICY` — folded in by
//! [`crate::reconcile_config::quarantine_first_default`], and the pairing is load-bearing rather
//! than a preference: under `hybrid` a default-on daemon would auto-apply `PositionDrift` at the
//! first pass after every restart, rewriting position size and booking realized PnL at the venue's
//! price before anyone has confirmed that venue's report is complete.
//! `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` is the verdict, and
//! `docs/ops/reconcile-on-restart.md` is the operator's side of it.
//!
//! (The `quarantine` default used to be justified by "an incomplete open-order fetch under `hybrid`
//! auto-cancels `OrphanLocalOrder`s"; that was false — the kind resolves to zero events under every
//! policy. The default stands on the `PositionDrift` ground instead. See
//! `crates/vike-exec/tests/recon/recon_policy_pin.rs`.) Health gate: the
//! daemon runs no market feeds `build_node` exposes ([`vike_mount::build_node`] leaves feeds to the
//! caller), so reconcile mounts with an EMPTY feed-status map — every reconciled venue reads
//! [`vike_core::ReconHealth::Healthy`] and is never health-blocked (the exec-only-venue shape; a pass
//! against a briefly-down venue fails soft, whereas a wrongly-suppressed pass can stay suppressed).
//!
//! ## Optional observe + control server (PR-11/12/13)
//! When `config.toml`'s `tradehub_addr` (or `VIKE_TRADEHUB_ADDR`) is set, the daemon also starts an authenticated node
//! server (`crate::server` + `publish`) so a laptop GUI (`vike-desktop`) can WATCH this
//! live node's snapshots over an SSH tunnel — and, when the `tradehub_control` FLAG is on and a
//! `VIKE_TRADEHUB_CONTROL_KEY` is set, TRADE it (place/cancel orders over a `Scope::Write`
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
//! `vike_mount::MountPolicy` and threads it into every `vike_mount::make_engine` call `build_node`
//! makes, and so into every venue's mount.
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
//! least one ENABLED rule, the daemon mounts `vike_alerting`'s `AlertEngine`
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
//! 0. [`vike_ops::stop::install_handlers`] — the FIRST statement of `main`, before argv and before
//!    the settings load. Until it returns, SIGTERM carries the OS default disposition, so every
//!    later step of this list is a window in which a service stop runs no teardown at all.
//! 1. [`vike_log::init`] (the daemon HOLDS the returned guards for the whole process), then the
//!    STARTUP DISCLOSURE — `vike_buildinfo::version_line` (which commit this binary is),
//!    `vike_config::boot_lines` (the settings directory that answered, each file present or absent,
//!    the resolved ceilings, and whether a credential store sits beside them) — and
//!    `log_handler_outcome`, reporting what step 0 did. All four are the first moment there is a
//!    subscriber to say anything through, which is why none of them can happen earlier.
//! 2. Parse the `--config` [`DaemonProfile`](crate::config::DaemonProfile) → a
//!    [`vike_mount::MakerMountConfig`].
//! 3. [`vike_mount::build_paper_maker_core`] → the live [`vike_core::CoreHandle`].
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
// Used by `alerts_path`, `state_dir`, `strategy_state_dir`, `log_dir` and the legacy telegram
// ledger path. (It named `tick_store_root` too, until that function went with `record-feeds`.)
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::reconcile_config;
use vike_core::{CoreHandle, CoreSnapshot};
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_ops::stop::{self, StopSignal};
// LIVE path only: the live feed is wired through a `LiveDataSink` onto the core lanes. `DataClient`
// itself is no longer named here — Task 3 moved every site that named it directly (`FeedCtors`,
// `LiveFeeds`'s `Oanda`/`Deribit` variants) into `feeds.rs`.
use crate::alerts::{self, AlertMount};
use crate::config::resolve_paper_risk_limits;
use vike_data::LiveDataSink;
use vike_exec::Command;
use vike_mount::{MakerMountConfig, NodeConfig};
// `LiveFeeds`/`CexBars`/`CexTicks`/`FeedCtors`/`ProdFeedCtors`/`PostFeeds`/`LiveTeardown` used to
// be private to this file; all moved to the library (`feeds.rs`) for the SAME reason `CexVenue`/
// `VenuePlan` did — `venue_feed_plan`/`wire_venue_feeds`/`recon_feed_statuses_of` name them in
// their own signatures, and a library cannot name a type owned by the binary that depends on it.
// `check_poly_token_intervals` is imported separately below: it exists in the library only under
// the `polymarket` feature, so an unconditional import here would fail an OFF build.
#[cfg(feature = "polymarket")]
use crate::feeds::check_poly_token_intervals;
use crate::feeds::{
    FeedCtors, LiveFeeds, LiveTeardown, PostFeeds, ProdFeedCtors, recon_feed_statuses_of,
    venue_feed_plan, wire_venue_feeds,
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
#[cfg(test)]
use crate::CexVenue;
use crate::venue_arming::withhold_exec_credentials;
#[cfg(test)]
use crate::venue_arming::{
    EXEC_LIVE, EXEC_LIVE_MULTI_ACCOUNT, EXEC_OTHER_ACCOUNT_LIVE, EXEC_PAPER, alpaca_arming,
    cex_arming, ctrader_arming, data_only_arming, deribit_arming, exec_badge, ig_arming,
    oanda_arming, other_live_accounts, with_other_live_accounts,
};
use crate::venue_plan::check_ctrader_intervals;
#[cfg(test)]
use crate::venue_plan::{
    alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan, wired_symbol_for,
};
use crate::{ResolvedMount, VenuePlan};
use vike_tradehub_client::{NodeKeys, Scope};

const USAGE: &str = "\
usage: vike-tradehub [--profile <run_profile.toml>]

  --config PATH   RETIRED (0086): accepted for ONE release and IGNORED. The daemon profile comes
                  from the ACTIVE daemon-profile ROW; `vike-cli config bootstrap-daemon` creates
                  one on a box that has none.
  --profile PATH  the operator-budget RunProfile TOML ([risk] ceilings); absent, $VIKE_RUN_PROFILE
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

/// The alerting rule-file override, read HERE rather than in the library so the env read stays in
/// the binary — the settings-registry rule. Since settings STEP 2 this is the workspace's ONLY
/// `$VIKE_ALERTS` read (`vike_alerting::persist` no longer reads it at all). Unset ⇒
/// `<project>/settings/state/alerts.json` (see [`alerts_path`]). Absent file ⇒ no rules ⇒ no
/// engine (see [`maybe_mount_alerts`]).
const ALERTS_ENV: &str = "VIKE_ALERTS";

/// The STATE-ROOT override — the one root for every file the program writes and no human edits.
/// Unset, it is `<project>/settings/state` (`vike_model::state_path`). Read here in the BINARY,
/// which is the correct shape; the resolver itself is pure.
///
/// ⚠ `_ROOT`, not `_DIR`: `VIKE_STATE_DIR` was `vike-app`'s strategy-state SIDECAR directory and
/// meant something else entirely — and it is a REMOVED variable now, refused at startup — see
/// `vike_model::state_path`'s module doc.
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

/// What the ready banner leads with when the credential store EXISTS and could not be OPENED.
///
/// ⚠ **It is a PREFIX, never a replacement, and both halves are load-bearing.** The suffix keeps
/// the existing `PAPER` / `LIVE (venue=…)` grammar verbatim, so every operator grep, every runbook
/// and `crates/vike-tradehub/tests/sigterm_stop.rs`'s exact-string check keep working — the banner
/// still answers paper-vs-live, which is its job. The prefix is what the banner did NOT say: an
/// empty credential map reaches the mount as *no credentials*, the live gate drops every venue to
/// paper, nothing fails, and the line an operator greps came back `LIVE (venue=none)` — which
/// `deploy/vike-tradehub.service`'s own header documents as a legitimate answer ("gate on,
/// nothing armed"). This is the one fact that tells those two boxes apart at the place the operator
/// is already looking.
///
/// UPPERCASE and leading because it is competing with a line that reads as normal. The reason and
/// the repair are NOT in here — a banner is one line and this one is already the widest field in
/// it; they are in the `tracing::error!` [`credential_store_health`] drives, and, for the commonest
/// cause, in `vike_secrets::DbErrorKind::ReadOnlyRollback`'s own message.
const STORE_UNREADABLE_BANNER: &str = "CREDENTIAL STORE UNREADABLE";

/// The ready banner's `"mode"` — the string `docs/ops/tradehub-the CI box.md` and
/// `deploy/vike-tradehub.service` both name as the ONE authority on paper-vs-live, and which
/// operators grep for (`grep '"kind":"ready"'`).
///
/// ⚠ **It is a function of the ARMING RECORD, and the profile's mount set is not a parameter.**
/// That signature is the fix. The string used to be built from the daemon's `mount_venues` — the
/// DISTINCT venues the profile mounts a strategy on — which answers a different question and is
/// neither a subset nor a superset of what armed: `vike_mount::build_node` calls
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
/// `venues` is `vike_mount::build_node`'s own `live_venues` — a venue is in it exactly when
/// `make_engine_with_legs` constructed a REAL `ExecutionClient` for it, so the set excludes a venue
/// with no credentials, a `data_only = true` venue whose keys [`withhold_exec_credentials`] took
/// away, a ctrader/ibkr venue whose synchronous connect failed and demoted it, and polymarket's
/// recon-only fallback. Sorted before rendering: a `HashSet` iterates in an order that changes
/// between runs of the same binary, and an operator diffing two startups must not read a
/// reordering as a change.
///
/// ⚠ **`health` is the THIRD state, and it is not paper-vs-live.** An unreadable credential store
/// produces the same EMPTY map an unconfigured box produces, so the mount arms nothing and this
/// function would otherwise render `LIVE (venue=none)` — the exact string a correctly-unarmed box
/// prints. [`STORE_UNREADABLE_BANNER`] carries that argument; the rendering is a PREFIX so the
/// paper-vs-live half stays byte-identical in both arms.
fn ready_mode_line(
    live: bool,
    venues: &std::collections::HashSet<String>,
    health: &vike_bridge_core::credentials::StoreHealth,
) -> String {
    let mode = if live {
        let mut armed: Vec<&str> = venues.iter().map(String::as_str).collect();
        armed.sort_unstable();
        let named = if armed.is_empty() { NO_VENUE_ARMED.to_string() } else { armed.join("+") };
        format!("LIVE (venue={named})")
    } else {
        // ⚠ UNCHANGED, and deliberately not routed through the `venue=` rendering above. A PAPER
        // daemon builds no live client by any path, so there is no set to name and nothing an
        // operator's existing `grep PAPER` should have to learn.
        "PAPER".to_string()
    };
    if health.is_readable() {
        // The ABSENT store is this arm — `StoreHealth::Readable` covers `Source::None`, because an
        // empty map from a box with no store is a real measurement of a real answer. So an
        // unconfigured daemon's banner is byte-for-byte what it was before this parameter existed.
        return mode;
    }
    // ...and the PAPER arm is prefixed too, deliberately. The gate being off does not make an
    // unreadable store harmless: the same store supplies the alert webhook targets and (under its
    // feature) the Telegram control channel, so a paper daemon with this fault is silently running
    // without the surfaces that would have told anyone about it.
    format!("{STORE_UNREADABLE_BANNER} — {mode}")
}

/// Complete the per-mount `StrategyStatus` rows from the mount's ARMING RECORD.
///
/// `WireMountRow::live` is documented as "true iff this MOUNT trades LIVE" — a per-VENUE question.
/// It used to be filled with `flags.tradehub_live`, a per-PROCESS boolean resolved before any venue
/// was mounted and never revised, so it over-claimed in two reachable ways at once:
///
/// - a mount whose venue has NO credentials reported `live: true` while `vike_mount::make_engine`
///   had put it on the paper exchange (`vike_mount::build_node`'s whole credential gate), and
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
            live: venues.contains(&s.venue),
            strategy: s.strategy,
            params: s.params,
            // The addressing key and the typed params are the LIVE overlay's, not the boot block's:
            // `server.rs`'s `Request::StrategyStatus` arm writes them from the core's own published
            // mount rows, because a boot-time copy of either would be stale the moment the first
            // `UpdateParams` lands — the defect the read exists to fix. Left empty here so there is
            // exactly one source rather than two that can disagree.
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            typed_params: None,
            // ⚠ Filled from the SEED, unlike the three fields above it. Those are left empty
            // because the server's live overlay owns them and a boot-time copy would go stale; a
            // mount's PRODUCT cannot go stale, because a mount that changed product would be a
            // different mount. So this is the one row field the boot block is the right source for.
            asset_class: s.asset_class,
        })
        .collect()
}

/// One mount's wire row, MINUS the fact that does not exist yet.
///
/// `main` must capture the per-mount strings BEFORE the mount (both arms MOVE `resolved`) and can
/// only fill in `live` AFTER it (that answer is `build_node`'s arming record, which the mount
/// produces). The seam is a struct rather than a tuple so neither half can be silently reordered,
/// and `venue` is carried explicitly because it is the KEY the arming record is queried with.
///
/// ⚠ This used to add "`WireMountRow` itself deliberately holds no addressing field", which was
/// true when written and stopped being true on 2026-09-07: that row now carries
/// `venue`/`symbol`/`interval`, and its own doc retires the deferral by name. The BOOT block still
/// leaves them empty, for a different and narrower reason — `wire_mount_rows`'s comment on the
/// empty fields is the authority: the addressing key is the LIVE overlay's, so a boot-time copy
/// would be a second source that goes stale at the first `UpdateParams`.
struct WireMountSeed {
    strategy: String,
    params: String,
    /// The mount's venue id — looked up in `build_node`'s `live_venues` to decide `live`, then
    /// dropped. NOT published FROM HERE: `WireMountRow`'s addressing key is filled by the server's
    /// `Request::StrategyStatus` overlay from the LIVE core, never by this boot-time seed. (It read
    /// "a `WireMountRow` carries no addressing fields yet" until that row grew them.)
    venue: String,
    /// WHAT PRODUCT this mount trades, as `vike_model::AssetClass`'s stored word.
    ///
    /// Taken from the profile row, which is where 0061 Phase 5 put it. ⚠ `Option` because
    /// `crate::config::MountCfg`'s field is one: a ROW-backed mount always has a class (the column
    /// is `NOT NULL` and `profile_rows` refuses a row without it), and a TOML-backed mount may not
    /// have been migrated yet. That absence is HONEST and is not the same as a node too old to
    /// carry the field at all — `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS` is what tells
    /// the two apart, and it is advertised beside this.
    ///
    /// ⚠ Unlike the addressing key above, this is a BOOT-TIME fact and is filled HERE rather than
    /// by the server's live overlay. A mount's product does not change while it runs — if it did,
    /// it would be a different mount — so there is no staleness for the overlay to fix.
    asset_class: Option<String>,
}

/// One mount's SELF-ADDRESSED params line — the `[[mounts]]` rendering of
/// `DaemonProfile::effective_params`, prefixed with the mount's own venue/symbol/interval so that
/// N rows of `size=2`-style strings are not indistinguishable.
///
/// ⚠ **This prefix is the OLD route to a mount's addressing key, and the reason given for it here
/// is retired.** It used to read "because a `WireMountRow` deliberately carries no structured
/// addressing fields yet"; that row grew `venue`/`symbol`/`interval` on 2026-09-07 and its own doc
/// says string-parsing this prefix "stops being anybody's answer". The prefix stays because this
/// `params` STRING is still what an operator reads and what a pre-`FEATURE_STRATEGY_PARAMS` node
/// can offer — a client that wants to ADDRESS a row reads the structured fields instead.
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
/// Settings STEP 2 PR 1, Task 2) — independent of the DAEMON profile (venue/token_id/A-S mount
/// shape), which since 0086 comes from the ACTIVE daemon-profile ROW alone and is no longer named on
/// argv at all. `None` here (no `--profile` flag) falls through to
/// [`vike_core::resolve_profile`]'s `VIKE_RUN_PROFILE` env fallback; both absent ⇒ `Ok(None)` ⇒ the
/// daemon's risk budget is byte-identical to today.
///
/// ⚠ `config_path` is the RETIRED `--config` value, kept ONLY so [`run`] can warn that it is
/// ignored — never read as a profile source. See [`USAGE`]'s own line on the flag; `vike-cli`'s
/// `config bootstrap-daemon` (`crates/vike-cli/src/cmd/config_profile_bootstrap.rs`) is the writer
/// that replaces it on a box with no active daemon-profile row.
///
/// `Debug` on both this and [`Parsed`] so a parse that was supposed to FAIL can report what it
/// produced instead (`Result::expect_err` requires it) — the same reason
/// `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    config_path: Option<String>,
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
        // …and the ONE target the global file level may not silence. `crate::audit::FILE_PIN` is
        // declared beside the `tracing` call it protects; this is the composition root that arms
        // it, because which records a BINARY may not lose is a binary's decision. Without it
        // `deploy/vike-tradehub.service`'s `Environment=VIKE_LOG_FILE_LEVEL=warn` swallowed every
        // accepted control command on the live box — see that constant's doc for the measurement.
        file_target_pins: vec![(
            crate::audit::FILE_PIN.0.to_string(),
            crate::audit::FILE_PIN.1.to_string(),
        )],
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

    // Decision 0095: before any mount or feed, so every Polymarket connection uses the operator's
    // egress rows (`declare_polymarket_egress`).
    #[cfg(feature = "polymarket")]
    declare_polymarket_egress(booted.settings_dir.as_deref());

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
    // The LIVE mount below takes the venue-facing projection (`vike_mount::MountPolicy`) from this
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

    // ⚠ WHICH PROFILE IS LIVE — since decision 0086 ("settings live only in the database")
    // verdict 1, the ACTIVE ROW IS THE ONLY SOURCE: no binary reads a profile TOML any more, so a
    // `--config` argument (RETIRED — see `Args`'s doc on `config_path`) decides nothing and a box
    // with no active daemon-profile row has no profile to fall back to. The read half needs NO unit
    // change and never did: 0057's EROFS section measures that `ProtectSystem=strict` leaves reads
    // untouched, and this daemon already opens this same store read-only at mount for the account
    // table.
    // ⚠ Tracked separately from `store_profiles` itself: a store that EXISTS and cannot be read is
    // an ERROR, never "no profiles" — the same distinction the credential store draws
    // (`StoreHealth::Unreadable`) — and the refusal below must say WHICH one this box hit, because
    // only one of them has a cure this binary can name. `bootstrap-daemon` writes a NEW row; it
    // cannot repair a store that will not open, and telling an operator to run it for THAT fault
    // would send them to the wrong tool.
    let mut profile_store_unreadable: Option<String> = None;
    let store_profiles = match booted.settings_dir.as_deref() {
        Some(dir) => {
            match vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir)) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("the settings store's profile rows could not be read: {e}");
                    profile_store_unreadable = Some(e.to_string());
                    vike_secrets::profile_store::Profiles::none()
                }
            }
        }
        None => vike_secrets::profile_store::Profiles::none(),
    };
    // The RETIRED flag's one-release warning (the `RETIRED_PROFILE_FLAG` idiom
    // `vike-datahub`'s `datahub_cli` carries for its own renamed flag) — said whenever the argument
    // was given at all, regardless of whether an active row exists, because the value is never
    // consulted either way.
    if let Some(given) = &args.config_path {
        tracing::warn!(
            "--config {given} is RETIRED (0086) and IGNORED — the daemon profile comes from the \
             ACTIVE daemon-profile row, never from a file. Drop this argument from the unit's \
             ExecStart= line; a future release refuses it outright."
        );
    }
    let active_daemon = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Daemon)
        .map(|p| p.row.name.clone());
    let profile = match active_daemon.as_deref().and_then(|n| store_profiles.by_name(n)) {
        // The row won. Its body goes back through `DaemonProfile::from_toml_str` — the EXISTING
        // parser and every refusal it carries — rather than through a second validator, which is
        // 0057's *What is LOST* requirement stated in its own words.
        Some(stored) => {
            tracing::info!("daemon profile: `{}` (the active row)", stored.row.name);
            match crate::profile_rows::rows_to_daemon_profile(stored) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("bad profile row `{}`: {e}", stored.row.name);
                    eprintln!("vike-tradehub: bad profile row `{}`: {e}", stored.row.name);
                    return ExitCode::FAILURE;
                }
            }
        }
        // ⚠ NO FILE FALLBACK, by 0086 verdict 1 — a profile TOML is never read by this binary,
        // however it was invoked. This is a NARROWING of a promise this crate's spawn tests used to
        // prove for the CREDENTIAL half of this same store: an unreadable store no longer leaves
        // this daemon running on paper, because the mount config lives in the same store now and
        // there is no second rung left to read it from. That promise still holds for credentials
        // alone (an unreadable store still yields an EMPTY credential map, never a refusal, by the
        // arm below) — it is the PROFILE side that changed.
        None => {
            let msg = match &profile_store_unreadable {
                // The store EXISTS and will not open — `bootstrap-daemon` cannot cure this; it
                // writes through the very open this daemon just failed. Name the credential store's
                // own repair instead, since it is the same file.
                Some(e) => format!(
                    "the settings store exists but its profile rows could not be read ({e}), so \
                     this binary cannot learn what to mount. THE DAEMON CANNOT REPAIR THIS ITSELF \
                     — it opens the store read-only and its settings directory is read-only in its \
                     own mount namespace, so a restart meets the identical state. This is the SAME \
                     store credentials live in — repair it from an OPERATOR SHELL, where the \
                     directory is writable: run any `vike-cli secrets` command (`vike-cli secrets \
                     list` is enough — opening the store read-write is what replays a rollback \
                     journal a killed writer left behind), then restart. `vike-cli config \
                     bootstrap-daemon` writes a NEW row through this same store and cannot repair \
                     an unopenable one."
                ),
                // No store, or a store with profile tables and no active row — an ordinary
                // unconfigured box, cured by writing the first row.
                None => {
                    "no ACTIVE daemon-profile row in the settings store, and this binary reads \
                          no profile file any more (0086). Create one with `vike-cli config \
                          bootstrap-daemon <name> --venue <venue> --asset-class <class> --symbol \
                          <sym>` (or --token-id on polymarket) — it also activates it — then \
                          restart."
                        .to_string()
                }
            };
            tracing::error!("{msg}");
            eprintln!("vike-tradehub: {msg}");
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
        // construction site, which is exactly `vike_mount::build_maker(&cfg)`: the very function
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
    //
    // ⚠ THIS WAS `resolved[0]`, AND THE INDEX WAS THE WHOLE DECLARATION. 0057's `tradehub.toml`
    // verdict is that the primary must become EXPLICIT before mounts become rows, because a TABLE
    // HAS NO INHERENT ORDER and reproducing "the first one" from rows would carry an accident
    // forward as a requirement. `DaemonProfile::primary_mount` is the one resolution; a profile
    // that declares nothing still answers index 0, so every profile that has ever shipped mounts
    // byte-identically to before this line changed.
    let primary = profile.primary_mount();
    let cfg = resolved[primary.index()].cfg.clone();
    // Say WHICH row it is and whether anybody chose it. A declared primary that nobody can see is
    // the same defect as an undeclared one, and the `word()` half is what tells an operator reading
    // a startup log that the daemon picked row 0 because nothing said otherwise.
    tracing::info!(
        index = primary.index(),
        how = primary.word(),
        venue = %resolved[primary.index()].cfg.venue,
        symbol = %resolved[primary.index()].cfg.token_id,
        "the PRIMARY mount — the daemon's singular identity (summary token, mode line, seed policy)"
    );
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
    // ONLY for its `[risk]` table (`run_profile.mode` is ignored here — the
    // `DaemonProfile` above already fully owns that shape). Resolved from the REAL process env (the
    // `VIKE_RECONCILE` idiom), never the `.env` creds map. `Ok(None)` (no explicit path AND no env
    // var) is the untouched default; a resolved-but-broken profile is a loud startup failure, never
    // a silent fall-through to the hardcoded risk defaults.
    // The WAL's three variables with `config.journal_dir` folded in, resolved ONCE for the whole
    // process: the paper and live mounts' `CoreConfig::journal` and the journal rung's disclosure
    // below all read it, and two resolutions could name two directories. See [`journal_vars`].
    // (It named "the off-path materializer below" as the second reader until 2026-09-28; the
    // materializer went with the `materialize` feature on 2026-09-22.)
    let journal_vars = journal_vars(settings.config.journal_dir.as_deref());
    let run_profile_vars: HashMap<String, String> = process_env().clone();
    // ⚠ **THE RUN PROFILE'S ROW RUNG — and this block was a DECLARED PARTIAL until it landed.**
    // It used to warn that *"this binary does not read run-profile bodies from rows yet (0057
    // Phase 2)"* and then resolve from `--profile`/`VIKE_RUN_PROFILE` regardless, with `None`
    // hardcoded where the row goes. It now takes the exact shape of the DAEMON-profile rung five
    // screens above: the owner's ruling (`vike_secrets::profile_store::select`) picks the winner,
    // the disclosure names everything it shadowed, and a winning row's BODY goes back through
    // `RunProfile::from_toml_str` — the EXISTING parser and every refusal it carries — rather than
    // through a second validator.
    let active_run = store_profiles
        .active(vike_secrets::profile_store::ProfileKind::Run)
        .map(|p| p.row.name.clone());
    let run_selection = crate::profile_rows::select_run_profile(
        active_run.as_deref(),
        args.profile_path.as_deref(),
        &run_profile_vars,
    );
    if run_selection.row_shadowed_something() {
        tracing::warn!("{}", crate::profile_rows::selection_line("run profile", &run_selection));
    } else {
        tracing::info!("{}", crate::profile_rows::selection_line("run profile", &run_selection));
    }
    let run_profile = match active_run.as_deref().and_then(|n| store_profiles.by_name(n)) {
        // The row won.
        //
        // ⚠ An `Err` here is a HARD startup failure and deliberately does NOT fall through to the
        // file rung. Falling through would make a corrupt row indistinguishable from an absent one
        // — and an absent one is what a live mount REFUSES on, so the fall-through would quietly
        // convert a refusal into a mount judging orders against the file's ceilings while the
        // operator believes the row is in force. Same disposition as the daemon-profile rung above.
        Some(stored) => match crate::profile_rows::rows_to_run_profile(stored) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("bad run profile row `{}`: {e}", stored.row.name);
                eprintln!("vike-tradehub: bad run profile row `{}`: {e}", stored.row.name);
                return ExitCode::FAILURE;
            }
        },
        // No row — byte-identical to every box that has not crossed, which is every box and every
        // CI lane.
        None => match vike_core::resolve_profile(
            args.profile_path.as_deref().map(Path::new),
            &run_profile_vars,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("bad run profile: {e}");
                eprintln!("vike-tradehub: bad run profile: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    // ⚠ THE JOURNAL RUNG'S OWN DISCLOSURE, emitted ONCE here rather than at either of the two
    // `CoreConfig` sites that call `journal_config_for` — both take this same `run_profile` and
    // this same `journal_vars`, so one line at the resolution point says it for the paper mount
    // and the live one alike, and says it before either is built. Silent otherwise: a resolved
    // profile makes the `VIKE_JOURNAL_DIR` / `config.journal_dir` rung unreachable, and a box that
    // completes the migration (activate the row, drop the shadowed `VIKE_RUN_PROFILE=` line) lands
    // in exactly that state. `crate::profile_rows::journal_rung_shadowed` carries the argument and
    // answers `None` on every box that cannot be affected.
    if let Some(line) =
        crate::profile_rows::journal_rung_shadowed(run_profile.as_ref(), &journal_vars)
    {
        tracing::warn!("{line}");
    }
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
    // `mode == Mode::Live`) and threads it straight into `vike_mount::make_engine` for every wired market
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
    // ⚠ THE ARMING OUTCOME, disclosed as ONE value rather than left to be inferred from three
    // separate lines. It is the value 0057's Phase 3 must preserve across a migration — the
    // OUTCOME, not the rows — and `crates/vike-tradehub/tests/daemon/profile_rows.rs` compares it
    // before and after for a the build runner pre-migration state. Emitting it here is what makes that
    // comparison a claim about production rather than about a test's own model.
    tracing::info!(
        outcome = ?crate::profile_rows::resolve_arming(live, run_profile.as_ref(), &profile),
        "the resolved ARMING OUTCOME — paper, live-refused, or live"
    );
    // The DISTINCT venues across the mount set, in mount order — one entry for a single-mount
    // profile. It is a LOG-LINE vec and nothing else: the two startup lines below render it as
    // "what did the operator ask for".
    //
    // ⚠ IT USED TO DRIVE THE B11 LOCK CLAIMS TOO, AND THAT WAS THE DEFECT. The lock is per venue
    // ACCOUNT and must cover what actually ARMS; this vec answers a different question, and the
    // measurement below is what it costs to confuse them. The claims moved to
    // [`live_mount_with`]'s safety-gate-#6 block, over `vike_mount::armed_live_venues`.
    //
    // ⚠ THIS IS THE PROFILE'S SET, AND IT IS NOT WHAT THE READY BANNER MAY NAME. The two answer
    // different questions and neither contains the other: the profile decides which (venue,
    // symbol) pairs this daemon MOUNTS A STRATEGY ON, while `vike_mount::build_node` calls
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
            // string opens with the mount's own venue/symbol/interval — the OLD route to a
            // mount's key, kept for the human-readable line now that the row struct carries the
            // structured fields; see `mounts_wire_params`); a single-mount daemon's one row
            // carries the identity block's own two strings — byte-identical to the row
            // `server.rs`'s `Request::StrategyStatus` arm used to DERIVE from the identity when
            // this daemon published no rows at all. Only that row's `live` changes, and it
            // changes from a lie to a fact.
            strategy: if multi { m.row.strategy_name().to_string() } else { strategy_name.clone() },
            params: if multi { mounts_wire_params(m) } else { strategy_params.clone() },
            venue: m.cfg.venue.clone(),
            asset_class: m.row.asset_class.clone(),
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
    // The third element is the mount's ARMING RECORD — `vike_mount::build_node`'s own `live_venues`
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
            "the live gate is ON (a `flags.tradehub_live` row, or VIKE_TRADEHUB_LIVE) — mounting the \
             LIVE build_node core; real orders MAY be placed on any venue whose creds are in the \
             credential store (each venue's network is its ceiling: `live` means mainnet)"
        );
        // `live_mount` threads this straight into `vike_mount::make_engine` for every wired market, which
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
            // `config.instance_origin` — the daemon's own origin claim, threaded to the core's
            // coid generator so a second deployment on this venue account is recognisable.
            settings.config.instance_origin.clone(),
            &journal_vars,
            // The `venue_setting` snapshot, read once, here: the mount hands each bridge its view,
            // and the feed wiring reads the mark-stream rows out of it (decision 0095).
            load_venue_settings_for(booted.settings_dir.as_deref()),
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
                       configures nothing here. Drop it, or arm the live gate (`vike-cli config set \
                       flags.tradehub_live true`, or VIKE_TRADEHUB_LIVE=1) for the data-only live \
                       mount";
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
            "mounting the PAPER strategy (headless daemon; no live feed — `vike-cli config set \
             flags.tradehub_live true`, or VIKE_TRADEHUB_LIVE=1, for the live build_node core)"
        );
        // ⚠ SAY OUT LOUD WHICH SENTINEL THIS MOUNT IS WATCHING.
        //
        // This line said the OPPOSITE until the paper mount was armed, and the reversal is the
        // point of the comment. It used to read "the HALT sentinel reaches NOTHING on a PAPER
        // mount", argued from a grep that returned nothing for `halt` anywhere under
        // `crates/vike-paper/src/`, and every word of it was true when it was written. It became
        // false in the commit that armed `crates/vike-mount/src/run.rs`'s `paper_client_for` — and a
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
        // precedence (`<project>/settings/state/HALT`, else `<exe_dir>/HALT`; a `VIKE_HALT_FILE`
        // override outranked both until decision 0099 retired it) that they cannot compute from the
        // unit file alone. `halt_path_from_env` is the same
        // memoized resolver the mount itself uses (`vike_bridge_core::halt`), so this cannot print a
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
        const PAPER_MOUNT_HALT_ADVISORY: &str = "HALT kill switch is ARMED on this PAPER mount: `touch` the sentinel below and every \
             order that OPENS risk is refused (a reduce_only submit still passes, so you can always \
             flatten). This mount builds no venue adapter, so nothing else here would name the file \
             — the halt_sentinel field is that name, and resolving it emits the usual \
             vike_bridge_core::halt arming report beside this line \
             (docs/ops/kill-switches.md section C).";
        tracing::warn!(
            halt_sentinel = %vike_bridge_core::halt::halt_path_from_env().display(),
            "{PAPER_MOUNT_HALT_ADVISORY}"
        );
        // Build + spawn the paper maker core (the PRODUCTION runtime) — the SAME mount the offline
        // tests use, plus the resolved operator risk budget (Task 2) armed
        // via `PaperMountOpts::risk_limits` (audit F12 collapsed the former `_with_risk_limits` twin
        // into the one options variant). `paper_risk_limits` is `RiskLimits::new()` absent a profile —
        // the `PaperMountOpts::default()` value — so this call is BYTE-IDENTICAL to
        // `vike_mount::build_paper_maker_core(&cfg)` in that case; see `resolve_paper_risk_limits`'s
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
        let opts = vike_mount::PaperMountOpts {
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
            // The WAL, resolved ONCE for this process — the same map and the same answer the LIVE
            // arm's `CoreConfig::journal` gets. Passed rather than left to the builder's own env
            // read because that read cannot see `config.journal_dir`, and a rehearsal that
            // journalled somewhere else (or nowhere) than the live mount would rehearse the wrong
            // thing. `journal_vars` keeps `VIKE_JOURNAL_DIR` winning over the file.
            // ⚠ From the RESOLVED run profile when there is one, never from a second read of
            // `VIKE_RUN_PROFILE` — see `crate::profile_rows::journal_config_for` for why the row
            // rung makes that distinction load-bearing.
            journal: crate::profile_rows::journal_config_for(run_profile.as_ref(), &journal_vars),
            ..Default::default()
        };
        let handle = if multi {
            // The `[[mounts]]` rehearsal (split-plane I10): N strategies on ONE paper core — one
            // engine per distinct venue, one single-symbol book per distinct `(venue, symbol)`
            // (`vike_mount::build_paper_multi_strategy_core_with`'s own doc carries the layout
            // argument). Same `opts` as the single-mount call, so a multi rehearsal arms the same
            // risk budget, halt sentinel and shutdown flags a single one would.
            vike_mount::build_paper_multi_strategy_core_with(
                resolved
                    .into_iter()
                    .map(|m| vike_mount::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
                    .collect(),
                opts,
            )
            .handle
        } else {
            let m0 = resolved.remove(0);
            vike_mount::build_paper_strategy_core_with(m0.strategy, &m0.spec, opts).handle
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
    // ⚠ ...and the THIRD state the banner has to be able to say: the credential store EXISTS and
    // would not OPEN. That produces the same EMPTY map an unconfigured box produces, so every
    // venue drops to paper, the mount's arming record is empty, NOTHING FAILS — `Restart=on-failure`
    // never fires and `OnFailure=vike-notify@` never pages — and this line used to print
    // `LIVE (venue=none)`, the exact string a correctly-unarmed box prints. See
    // [`credential_store_health`] for why this daemon announces the fault rather than refusing to
    // start, argued against `docs/decisions/0013-degrade-vs-refuse.md`.
    //
    // The read itself is the one `vike_boot::boot` already performed through
    // [`workspace_credentials`] at startup; this is its memoized verdict, not a second open.
    let store_health = credential_store_health();
    if let vike_bridge_core::credentials::StoreHealth::Unreadable(why) = store_health {
        // `SecretsError`'s own Display — a path and a reason, never file contents (its doc says
        // so), and for a killed writer's rollback journal it is
        // `vike_secrets::DbErrorKind::ReadOnlyRollback`'s message, which names the repair itself.
        tracing::error!(
            reason = %why,
            "THE CREDENTIAL STORE IS PRESENT AND UNREADABLE — this daemon is running with NO \
             credentials, so EVERY VENUE IS ON PAPER and no order can reach a venue. This is NOT \
             the absent-credential gate: an unreadable store and an unconfigured box must never \
             look the same to an operator. ⚠ THE DAEMON CANNOT REPAIR THIS ITSELF — it opens the \
             store read-only and its settings directory is read-only in its own mount namespace, \
             so a restart meets the identical state. Repair it from an OPERATOR SHELL, where the \
             directory is writable: run any `vike-cli secrets` command (`vike-cli secrets list` is \
             enough — opening the store read-write is what replays a rollback journal a killed \
             writer left behind), fix what the reason above names, then restart this daemon. The \
             ready banner below leads with the same finding"
        );
    }

    let mode = ready_mode_line(live, &live_venues, store_health);

    // ...and the SAME arming record decides each wire row's `live` — ONE value feeding both
    // reports, so the banner and the `StrategyStatus` verb cannot disagree about one startup.
    let wire_mounts = wire_mount_rows(wire_mount_seeds, &live_venues);

    // ⚠ TOMBSTONE — the journal MATERIALIZER (unified-journaling #2: WAL fills/orders copied
    // off-path into the `kind=exec_fill`/`kind=exec_order` store series) was spawned here, behind
    // the `materialize` feature, until #2093 deleted both on 2026-09-22 (docs/decisions/0084). The
    // WAL is still written, and `tearsheet --journal DIR` reads it directly. Six lines of its
    // comment outlived the code here, cut off mid-sentence, until 2026-09-28.

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
    // WHICH BOX this daemon is running on, resolved ONCE here and stamped into the identity block
    // below. ⚠ This is the ONE identity fact the client cannot derive: both listeners bind
    // loopback, so a thin client reaches them through an SSH tunnel and its own socket address is
    // always the tunnel mouth — every client on every box reads `127.0.0.1:7879` whichever daemon
    // it is attached to. `crate::self_address` argues the whole discovery: a configured
    // `tradehub_advertise_addr` wins, else the source address the kernel's own routing table names
    // for an off-box destination (a route LOOKUP — no packet leaves this machine and nothing is
    // contacted), else an empty string, which a client renders as "the daemon said nothing" and
    // never as an address.
    //
    // It is resolved into a local rather than inlined below because it is the one part of that
    // block that is not already-resolved — the literal keeps the wiring-only rule.
    let advertise_addr = crate::self_address::advertise_addr(
        settings.config.tradehub_advertise_addr.as_deref(),
        settings.config.tradehub_addr.as_deref(),
    );
    // WHO this daemon is, stamped into every published frame (split-plane B3) so an observer
    // holding several backends can label them and tell paper from live. A struct literal over
    // already-resolved locals — no new reads, no logic (the wiring-only rule).
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        // ⚠ 0086: the identity name is the ACTIVE DAEMON ROW's name — the same name a `bootstrap-
        // daemon`/`config activate daemon` call gave it — never a `--config` file's stem, which this
        // binary no longer reads at all. `"tradehub"` is the one compiled-in fallback, used only
        // when nothing named a row, which the earlier refusal already makes unreachable in practice
        // (there is no `profile` to have reached this line without an active row) — kept as the
        // honest answer for that impossible case rather than an `unwrap`.
        name: active_daemon.clone().unwrap_or_else(|| "tradehub".to_string()),
        strategy: strategy_name.clone(),
        params: strategy_params.clone(),
        live,
        build: vike_buildinfo::summary(),
        advertise_addr,
    };
    // Say out loud which box this daemon decided it is, and whether the operator told it or the
    // kernel did. An EMPTY answer is the one an operator has to be able to act on — it is what a
    // client renders as "nothing was said" — so it is logged as a WARNING naming the key that
    // fixes it, rather than passing in silence like a successful lookup.
    if identity.advertise_addr.is_empty() {
        tracing::warn!(
            "this daemon could not determine its own address (no route to name a source address \
             from, e.g. a loopback-only container) — an observing client will be told nothing \
             about which box this is. `vike-cli config set config.tradehub_advertise_addr <addr>` \
             (or VIKE_TRADEHUB_ADVERTISE_ADDR) to name it."
        );
    } else {
        tracing::info!(
            advertise_addr = %identity.advertise_addr,
            source = if settings.config.tradehub_advertise_addr.is_some() {
                "config.tradehub_advertise_addr"
            } else {
                "discovered from this box's routing table"
            },
            "reporting this box's address to observing clients"
        );
    }
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
        // The ACCOUNT-ADMIN declaration (`config.tradehub_account_admin`) and the boot's settings
        // directory: `start_observe_server` DECIDES from them, beside the bind decision, so the
        // capability and the bind classification cannot be taken from two different resolutions.
        settings.config.tradehub_account_admin.as_deref(),
        booted.settings_dir.as_deref(),
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

    // Bounded, graceful teardown — the EXACT window-close primitive the desktop uses. There are no
    // venue feeds to fan out on a paper mount, so `tasks` is empty and `shutdown_and_join` is the
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
    // ordering — a live user-data pump can otherwise wedge the core join; see `crates/vike-mount/src/node.rs`'s
    // forwarder teardown-safety note), and fan the venue-feed teardown out as a bounded task (mirrors
    // the desktop's `feed_tasks`: `|mut f| Box::new(move || f.shutdown())`). The PAPER arm has NO
    // feeds, so `tasks` is empty and `shutdown_and_join` is the sole sequential tail —
    // byte-identical to the pre-live daemon.
    // The reconcile driver (LIVE mounts only; `None` on the paper mount AND on any live run the
    // operator refused with `flags.reconcile_off`) rides the SEQUENTIAL tail below, not a parallel
    // task.
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
        // `vt-core-recon` thread to the daemon's lifetime. `None` on the paper mount AND on any live
        // run the operator refused — the `if let` is then skipped, byte-identical to the old tail.
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
/// `--config=value`; ⚠ `--config` is RETIRED (0086) and OPTIONAL — accepted for one release and
/// warned about, never required and never read as a profile source (see `Args`'s own doc on
/// `config_path`). `-h`/`--help` short-circuits to [`Parsed::Help`] and `-V`/`--version` to
/// [`Parsed::Version`], both of which are a SUCCESS (see those variants' docs, and `main`'s arms for
/// why stdout is right here).
/// `--profile`/`--profile=value` is OPTIONAL — the RunProfile risk-budget file (Task 2); absent, the
/// daemon still checks `VIKE_RUN_PROFILE` ([`vike_core::resolve_profile`]'s env fallback), and absent
/// BOTH the risk budget is untouched (today's behavior).
///
/// ⚠ Takes the argv TAIL — `argv[0]` is ALREADY STRIPPED, by the shim and by the `vike-backend`
/// dispatcher alike. The old wrapper here did its own `skip(1)`, which was correct for the
/// standalone shim (full process argv) and ate the first REAL argument through the dispatcher: the
/// v0.1.16 image smoke ran `vike-tradehub --version` through the symlink and was answered with the
/// usage error.
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
    // ⚠ NO LONGER REQUIRED (0086): the daemon profile comes from the active row, and `--config` is
    // a retired argument kept only for the one-release warning `run` emits when it is given. See
    // `Args`'s own doc on `config_path`.
    Ok(Parsed::Args(Args { config_path, profile_path }))
}

/// The daemon's reconcile env: the REAL process env (the `VIKE_RECONCILE` idiom — NOT the `.env` creds
/// map; a shell-exported flag is invisible to `load_workspace_dotenv()`, see
/// [`crate::reconcile_config`]'s module doc) with the QUARANTINE-FIRST default folded
/// in. Every `VIKE_RECONCILE_*` knob ([`crate::reconcile_config::build_recon_config`]) is read
/// off this one map, exactly as `vike-app`'s `App::new` built its own while the desktop reconciled.
///
/// ⚠ The fold itself LEFT this file on 2026-09-06 and now lives in
/// [`crate::reconcile_config::quarantine_first_default`] — not tidiness: the S2 default-on gate
/// applied to `vike-app`'s live mount too, and that root was building its `ReconConfig` off a raw
/// `std::env::vars()` sweep, i.e. under `hybrid`, the auto-folding policy. Two mounts turning
/// reconcile on by default under two different policies is the failure the move removes.
fn daemon_recon_env(flags: vike_config::Flags) -> HashMap<String, String> {
    daemon_recon_env_from(flags, process_env().clone())
}

/// [`daemon_recon_env`] over a CALLER-SUPPLIED process-env map — the pure half, split out so the
/// precedence below is a TEST rather than a sentence (`std::env::set_var` is an `unsafe fn` this
/// workspace forbids, so the only way to drive a "the environment already said `0`" case is to
/// hand the base map in).
///
/// ⚠ `or_insert`, so the PROCESS ENV WINS, and here that is a property of the BASE MAP rather than
/// of the tier: `process_env` is the real sweep, so a `VIKE_RECONCILE_BALANCE=0` in a unit file is
/// already in this map and nothing below replaces it. What lands is the value for a key nobody
/// exported — and it is the RESOLVED flag, which `vike_config::Flags` has already had the
/// environment applied over (`crates/vike-config/tests/flag_registry.rs`'s
/// `the_environment_overrides_the_row_for_every_flag`), so the two sources cannot disagree even in
/// principle. This is NOT the credential store: the `.env` map is never merged into this one, which
/// is why these two keys need no [`FoldTier`] decision.
fn daemon_recon_env_from(
    flags: vike_config::Flags,
    process_env: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = reconcile_config::quarantine_first_default(process_env);
    // ...and the two `VIKE_RECONCILE_*` FLAGS this family carries, folded in from the settings the
    // boot already resolved. `build_recon_config` reads the whole family out of ONE map, so filling
    // the map is the whole of the wiring — the seam that function's own module doc calls "purely a
    // TESTABILITY seam" is the seam a settings file reaches it through.
    for (name, resolved) in [
        (vike_config::flags::RECONCILE_BALANCE_ENV, flags.reconcile_balance),
        (vike_config::flags::RECONCILE_GENERATE_MISSING_ENV, flags.reconcile_generate_missing),
    ] {
        env.entry(name.to_string()).or_insert_with(|| flag_wire_value(resolved));
    }
    env
}

/// `"1"` / `"0"` — the exact grammar every one of these gates parses, and the one this daemon
/// writes when it folds a resolved flag into a map a library will read.
///
/// `"0"` rather than "leave the key out" for a `false`, deliberately: the two are equivalent to
/// every reader here (all of them compare against `"1"`), and writing the value makes the map an
/// honest record of what the boot decided rather than a record of what it decided to mention.
fn flag_wire_value(on: bool) -> String {
    if on { "1" } else { "0" }.to_string()
}

/// How the fold writes ONE key when `vars` ALREADY carries it — the credential store's own
/// `.env` being the only thing that can have put it there.
///
/// ⚠ **This has ONE variant, and it had two until decision 0095's review.** The second one,
/// `CredentialStoreFirst`, left a credential line standing and was reserved for the two Polymarket
/// arming gates, on the argument that [`vike_config::refuse_credential_file_arming`] refuses to
/// START on an arming line in that file. The argument had a hole: that refusal's value grammar is
/// "the text before `#`, trimmed, is exactly `1`", while the gate's reader
/// (`vike_polymarket::exec_plane::mount::poly_exec_enabled`) takes the FIRST TOKEN — so a store row
/// `POLY_EXEC=1 x` armed REAL-MONEY exec over `flags.poly_exec = false` without tripping it, and a
/// `POLY_EXEC=0` row silently vetoed `flags.poly_exec = true`. Both gates are `Resolved` now: the
/// flag is the SOLE source, and no credential row can arm or veto them.
/// `no_credential_store_line_survives_the_fold_for_any_key` holds that for every key.
///
/// The enum stays, single-variant, only because [`vike_config::CONSUMPTION`]'s six needles spell
/// `FoldTier::Resolved)` textually; collapsing the type re-keys those needles and is a change of its
/// own (decision 0095's prose sweep left it, as a code change rather than prose).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldTier {
    /// **OVERWRITE.** The resolved flag is written whatever `vars` held, so this key's value in
    /// the map IS `vike_config::Flags`' answer — file, with the environment applied over it.
    ///
    /// Leaving a store line standing would make `secrets.env` a THIRD source that outranks an
    /// exported variable, in no precedence model, invisible to `vike-cli config show` — and for
    /// [`vike_config::flags::ALLOW_WITHDRAW_KEYS_ENV`] and
    /// [`vike_config::flags::PREFLIGHT_SKIP_ENV`] that source would be widening a SAFETY OVERRIDE,
    /// while for the two Polymarket gates it would be arming (or vetoing) real-money exec from a
    /// plaintext credential file.
    Resolved,
}

/// **Declare Polymarket's egress into the bridge** — decision 0095: its proxy settings are
/// `venue.polymarket.*` rows in the settings database, and the bridge reads neither the environment
/// nor the store, so this root reads the rows ONCE and hands them over before the first Polymarket
/// client (the feed, the mount's geoblock probe and L2 handshake) exists.
///
/// Everything else — the precedence (the row, else the built-in default) and the ONE store-error
/// policy — lives in `vike_polymarket::declare_from_rows`, which the data server calls too: five
/// copies of this body once handled a store error two different ways. (A credential-map fallback
/// for the five legacy names rode here for one release; decision 0095's Task 7 deleted it, because
/// the boot now refuses a credential row under one of those names.)
#[cfg(feature = "polymarket")]
fn declare_polymarket_egress(settings_dir: Option<&std::path::Path>) {
    vike_polymarket::declare_from_rows(
        settings_dir.map(vike_secrets::venue_setting::load_venue_settings),
    );
}

/// Every key [`fold_flags_into_vars`] writes, with the resolved value and the TIER it is written
/// under — the table both the fold and its tests iterate, so neither can describe a set the other
/// does not have.
fn folded_flag_rows(flags: vike_config::Flags) -> [(&'static str, bool, FoldTier); 6] {
    use vike_config::flags as f;
    [
        // Polymarket's two mount gates. Both readers are MAP-ONLY since decision 0095
        // (`vike_polymarket::exec_plane::mount::poly_exec_enabled`,
        // `vike_polymarket::exec_plane::recon_client::poly_reconcile_enabled`): this fold is the only way
        // `flags.poly_exec` / `flags.poly_reconcile` reach them, and it OVERWRITES. ⚠ It used to
        // `or_insert` on the argument that a credential-store spelling "is refused at boot" — but that
        // refusal only catches a value whose text before `#` is exactly `1`, while the readers take the
        // first TOKEN, so `POLY_EXEC=1 x` armed real-money exec over a false flag and `POLY_EXEC=0`
        // vetoed a true one. The flag is the sole source now; the refusal stays as the loud half.
        (f::POLY_EXEC_ENV, flags.poly_exec, FoldTier::Resolved),
        (f::POLY_RECONCILE_ENV, flags.poly_reconcile, FoldTier::Resolved),
        // Hyperliquid's symbology universe — read inside
        // `vike_hyperliquid::instruments::HyperliquidInstruments::load_from_vars`, which had no
        // credential-store tier before that constructor existed.
        (f::HYPERLIQUID_HIP3_ENV, flags.hyperliquid_hip3, FoldTier::Resolved),
        // The PIT `SymbolProperties` recorder's gate. ⚠ This named "`open_from_vars` at the
        // construction site below" as its reader until 2026-09-28, and that call is not made: the
        // daemon has passed `properties_rec: None` since it stopped writing the store (#2093), so
        // nothing in this binary reads the value this row folds. `Resolved` is still the tier the
        // NEXT reader needs: `open_from_env` is a one-line wrapper that calls `open_from_vars` over
        // a snapshot of the PROCESS ENV, so a map handed to the map half has to carry the
        // environment's answer or the swap loses it in both directions.
        (f::RECORD_PROPERTIES_ENV, flags.record_properties, FoldTier::Resolved),
        // ⚠ The two SAFETY OVERRIDES, where `false` is the guarded state. They are folded like any
        // other flag, by the owner's ruling that every setting is editable from the UI including
        // the live gates — and what makes that safe is the tier, not the ruling: what lands here is
        // the RESOLVED flag and nothing else. The withdraw gate reads this map alone (decision 0095
        // retired its variable), so no credential line can arm it past its row; the preflight skip
        // still ORs the process environment in, and an exported `=0` disarms both sides of that OR.
        (f::ALLOW_WITHDRAW_KEYS_ENV, flags.allow_withdraw_keys, FoldTier::Resolved),
        (f::PREFLIGHT_SKIP_ENV, flags.preflight_skip, FoldTier::Resolved),
    ]
}

/// **The resolved [`vike_config::Flags`] that venue adapters read, folded into the map the mount
/// already threads them.** The whole of the wiring for six settings keys, in one place.
///
/// Every flag below gates code inside a venue adapter or a recorder, several frames under any
/// binary, and each one read its variable from the process environment (or from the credentials
/// map) with no way for `<project>/settings/flags.toml` to reach it. `vike_config::CONSUMPTION`
/// carried all six as written admissions for exactly that reason. The cure is not a parameter per
/// flag through `vike_mount::NodeConfig` and `vike_mount::make_engine`: those already take a
/// `&HashMap<String, String>` as the one channel a venue fact travels on, so the composition root's
/// job is to FILL that map, and this is where it fills it.
///
/// ⚠ **THE PRECONDITION, and every safety argument downstream rests on it: `flags` must be the
/// `vike_config::load`-resolved value over this process's OWN environment sweep.** That is what
/// makes "the environment wins" true of what lands in the map — `vike_config::load` applies the
/// env layer over the file layer (`crates/vike-config/tests/flag_registry.rs`'s
/// `the_environment_overrides_the_row_for_every_flag` proves it for every flag, registry-wide),
/// and [`resolve_settings`] hands it [`PROCESS_ENV`], the real sweep. A caller that resolved
/// `flags` from some OTHER map would be folding a value with no such property, and the readers
/// downstream would have no way to tell.
///
/// ⚠ **The write OVERWRITES, for every key.** An earlier spelling of this function used `or_insert`
/// for all six, which left a `VIKE_ALLOW_WITHDRAW_KEYS=1` line in `<project>/settings/secrets.env`
/// standing and let it be OR-ed in by `vike_mount::arming`'s withdraw gate — a third source,
/// outranking the environment, in no precedence model and invisible to `vike-cli config show`. Two
/// keys kept `or_insert` on purpose for a while (the Polymarket gates, whose credential-store
/// spelling is refused at boot), and that exception was closed because the refusal's value grammar
/// is narrower than the readers' — the whole history is on [`FoldTier`]. The properties are
/// `a_process_env_value_beats_a_file_value_for_every_wired_key` (the keys that keep an environment
/// layer) and `no_credential_store_line_survives_the_fold_for_any_key` (every key, hostile
/// spellings included).
///
/// ⚠ A key whose variable decision 0095 retired (`vike_config::FlagMeta::reads_env` is `false`:
/// the two Polymarket gates, `HYPERLIQUID_HIP3`, `VIKE_ALLOW_WITHDRAW_KEYS`) has NO environment
/// layer, so "the environment wins" above is a statement about the others; for those the settings
/// row is the whole of `flags`' answer, and what the precondition still buys is that no other
/// source stands in for it.
///
/// ⚠ A box configured entirely by `Environment=` lines is byte-identical to before this function
/// existed — which is the constraint `docs/decisions/0054-settings-move-into-one-database.md`
/// states, and the reason the CI box's live reconcile policy in a systemd drop-in keeps working.
///
/// ⚠ The keys the owner DEFERRED (`pm_resolve`, `hl_outcome`, `poly_auto_redeem`,
/// `poly_redeem_halt`, `poly_heartbeat`, `record_chains`) are deliberately absent:
/// their consumers exist but are mounted by no running binary, so folding them in would move a
/// value nothing would read and turn an honest admission into a false claim. (`record_dvol` was a
/// seventh and is no longer a key at all — `vike_config::DEAD_FLAG_KEYS`.)
pub(crate) fn fold_flags_into_vars(flags: vike_config::Flags, vars: &mut HashMap<String, String>) {
    for (name, resolved, FoldTier::Resolved) in folded_flag_rows(flags) {
        vars.insert(name.to_string(), flag_wire_value(resolved));
    }
}

/// **The write-ahead journal's three variables, with `config.journal_dir` folded in** — the map
/// `vike_core::journal_config_from` reads instead of the process environment.
///
/// `VIKE_JOURNAL_DIR` was read in a `vike-core` library several frames below this binary, which is
/// why `config.journal_dir` was a declared key nothing could reach. Resolving it HERE is the
/// settings-registry rule (libraries take configuration as parameters; only binaries read the
/// process environment), and it is resolved ONCE so the paper and live mounts'
/// `CoreConfig::journal` and the journal rung's startup disclosure cannot answer differently about
/// where the WAL is. (The second reader was "the materializer's tail-follow" until the
/// `materialize` feature was deleted on 2026-09-22.)
///
/// ⚠ **`or_insert`, so an `Environment=VIKE_JOURNAL_DIR=…` line beats `config.toml`.** The map
/// starts as the real process env; the file's value lands only where the variable is absent.
/// `VIKE_RUN_PROFILE` and `VIKE_JOURNAL_SNAPSHOT_EVERY` are carried through untouched — the profile
/// still short-circuits the dir knob entirely, exactly as it did.
fn journal_vars(journal_dir: Option<&std::path::Path>) -> HashMap<String, String> {
    journal_vars_from(journal_dir, process_env().clone())
}

/// [`journal_vars`] over a CALLER-SUPPLIED process-env map — the pure half, split out for the same
/// reason [`daemon_recon_env_from`] is: `std::env::set_var` is an `unsafe fn` this workspace
/// forbids, so "the variable was already exported" is a case only an injected base map can drive.
/// `the_process_env_beats_config_journal_dir` is the test.
fn journal_vars_from(
    journal_dir: Option<&std::path::Path>,
    process_env: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut vars = process_env;
    if let Some(dir) = journal_dir {
        // The CONSTANT, never the literal: `vike_config` owns this variable's spelling, and the
        // settings registry resolves a read through the indirection.
        vars.entry(vike_config::config::JOURNAL_DIR_ENV.to_string())
            .or_insert_with(|| dir.display().to_string());
    }
    vars
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

/// The LIVE mount (`VIKE_TRADEHUB_LIVE=1`): stand up the wired-market [`vike_mount::build_node`] core
/// with the resolved strategy mounts folded in (split-plane I10: one for the historical
/// single-mount profile, N for a `[[mounts]]` one), then wire each DISTINCT mounted venue's live
/// market feed onto it. Returns the live
/// [`CoreHandle`] plus the [`LiveTeardown`] handles. HARD-errors (never a silent paper fallback) on any
/// venue/symbol the daemon has not wired for live — the daemon's own allow-list (safety gates #4/#5).
/// The PER-VENUE credential gate (#2) and each venue's own network gate (#3, decision 0095: the
/// ceiling for binance/bybit/okx/hyperliquid) still apply INSIDE `build_node`, so a credential-less
/// venue mounts paper even here (no real orders).
///
/// `risk_profile` is the resolved `--profile`/`VIKE_RUN_PROFILE` [`vike_core::RunProfile`]'s `[risk]`
/// table (RunProfile wiring — closing the live gap): threaded straight into [`NodeConfig::risk_profile`],
/// which `build_node` applies uniformly to every wired venue's `RiskLimits` via
/// `vike_mount::make_engine`. `None` (no `--profile`/`VIKE_RUN_PROFILE`, today's only path before this
/// wiring) leaves every venue's `RiskLimits` byte-identical to before — this is the SAME budget
/// `resolve_paper_risk_limits` already arms on the PAPER mount above, now also reaching the LIVE one.
/// The caller (this file's `main`) resolves this through
/// [`vike_core::RunProfile::risk_for_live_venue_mount`] rather than reading `.risk` directly, so by
/// the time it reaches this function it is guaranteed to have come from a `mode = "live"` profile —
/// `make_engine`'s hardcoded `GridSource::VenueFetched` never contradicts the profile it came from.
///
/// `policy` is this MACHINE's `<vike home>/policy.toml`, resolved once by [`resolve_settings`] at the
/// top of [`main`] and projected here onto [`vike_mount::MountPolicy`] — the subset a venue mount
/// applies (settings-unification Phase 6c). A DIFFERENT authority from `risk_profile`: per-machine
/// and admin-owned, with no env and no CLI layer at all.
///
/// ⚠ Its `venues` field is the per-venue ARMING CEILING and its default is `paper` EVERYWHERE, so
/// `Policy::default()` (no file) mounts an ALL-PAPER daemon regardless of the credential store —
/// see this module's doc. `market_slippage` is the other binding field, consumed by the hyperliquid
/// bridge's mount as `MountRequest::market_slippage` (this daemon's primary live venue, and the
/// only roster venue with no native market order, so its every market intent and every tripped
/// stop-MARKET is priced at that band); for that one, no file leaves the mount on hyperliquid's own
/// compiled-in literal, byte-identically.
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
/// projection ([`vike_mount::build_live_multi_strategy_core`] mounts on those). Per entry the two
/// agree by construction — the caller derives each `spec` from its `cfg`.
///
/// The FIRST entry is the PRIMARY mount: its `cfg` supplies the per-venue account seed
/// (`NodeConfig::seed_cash` / `CoreConfig::seed_cash`), exactly as the single-mount daemon always
/// did. Each DISTINCT venue's feed arm is wired exactly once, however many mounts share the venue
/// (subscriptions dedup per series inside the arm); the teardown handle carries one [`LiveFeeds`]
/// entry per venue.
///
/// ⚠ The `allow` matches [`live_mount_with`]'s, which has carried one since it was written, and for
/// the same reason: every parameter here is a FACT THIS FUNCTION MAY NOT RESOLVE ITSELF — the
/// policy, the flags, the sentinel directory, the origin claim, the WAL map — and bundling them
/// into a struct to satisfy a count would hide the one property
/// `crates/vike-ops/tests/live_lock_claim_order_gate.rs` follows through this signature by INDEX.
#[allow(clippy::too_many_arguments)]
fn live_mount(
    mounts: Vec<ResolvedMount>,
    risk_profile: Option<vike_exec::ProfileRisk>,
    profile: Option<vike_core::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    state_dir: &std::path::Path,
    // `config.instance_origin` — see [`live_mount_with`]. LAST, and deliberately not beside
    // `flags`: `crates/vike-ops/tests/live_lock_claim_order_gate.rs` pins `state_dir`'s POSITION in
    // this signature (it is the live-account sentinel's directory, and the gate follows it by
    // index), so a new parameter goes after it or the pin measures the wrong argument.
    instance_origin: Option<vike_model::InstanceOrigin>,
    // The WAL's env map with `config.journal_dir` folded in — see [`journal_vars`]. A PARAMETER for
    // the same reason `instance_origin` is one: this function resolves no settings of its own, and
    // `main` resolves it ONCE so this core's `journal`, the paper mount's and the startup
    // disclosure name the same directory (the materializer's tail-follow was the second reader
    // until the `materialize` feature was deleted on 2026-09-22). AFTER `state_dir`, like every
    // other addition — see the note above.
    journal_vars: &HashMap<String, String>,
    // Every venue's `venue_setting` rows, read ONCE by `run` from the boot's settings directory
    // (`load_venue_settings_for`), carried on `MountPolicy::venue_settings` and handed to the feed
    // wiring, which reads the mark-stream rows. LAST, like every addition since the live-lock gate
    // pinned `state_dir`'s position.
    venue_settings: std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<LiveMount, String> {
    // The credentials map — the daemon binary owns this I/O (the light client crates must not).
    // Absent per-venue creds keep that venue PAPER even with the gate on (safety gate #2), and that
    // is unchanged by WHICH store supplied them: see [`workspace_credentials`].
    let vars = workspace_credentials();
    live_mount_with(
        mounts,
        risk_profile,
        profile,
        policy,
        flags,
        vars,
        state_dir,
        instance_origin,
        journal_vars,
        &ProdFeedCtors,
        venue_settings,
    )
}

/// The venue-settings snapshot for this process — the one read of the `venue_setting` table the
/// live mount sees. No settings directory, or a store that will not open, gives every venue an
/// empty view — each feed keeps its charter mark-stream default — and the error is said.
pub(crate) fn load_venue_settings_for(
    settings_dir: Option<&std::path::Path>,
) -> std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings> {
    let Some(dir) = settings_dir else { return std::collections::BTreeMap::new() };
    match vike_secrets::venue_setting::load_venue_settings(dir) {
        Ok(settings) => settings,
        Err(e) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; every venue takes an empty view (its \
                 built-in defaults)"
            );
            std::collections::BTreeMap::new()
        }
    }
}

/// The file spelling of the dead-man action → the core's own type.
///
/// An extension trait rather than a `From` impl because BOTH types are foreign here (the orphan
/// rule), and here rather than in `vike-config` because that crate sits below `vike-core` and must
/// not depend on it — this binary is the crate that depends on both, so the mapping is its job
/// (`vike_config::DeadManActionSetting`'s doc says the same from the other side). The `match` is
/// exhaustive on the file side, so a variant added there is a compile error here rather than a
/// spelling that silently maps to the other action.
trait DeadManActionToCore {
    fn to_core(self) -> vike_core::DeadManAction;
}

impl DeadManActionToCore for vike_config::DeadManActionSetting {
    fn to_core(self) -> vike_core::DeadManAction {
        match self {
            vike_config::DeadManActionSetting::CancelAllAndHalt => {
                vike_core::DeadManAction::CancelAllAndHalt
            }
            vike_config::DeadManActionSetting::CancelAll => vike_core::DeadManAction::CancelAll,
        }
    }
}

/// The dead-man switch the LIVE mount arms, folded from `policy.toml`'s two keys — the whole of
/// M4's wiring surface, as one pure function so the fold can be pinned without racing a timer.
///
/// `None` when the key is ABSENT (`Policy::deadman_timeout_ms` is `None`) and when the operator
/// wrote `deadman_timeout_ms = 0` (`Some(vike_config::DEADMAN_DISABLED_MS)`) — the two are OFF
/// alike, and only the WARNING beside this call ([`deadman_absent_warning`]) tells them apart.
/// `vike_core::CoreConfig::deadman` being `None` is what makes the core build no `DeadMan`, arm no
/// sweep timer and touch nothing on the fold — the switch's "off" is the ABSENCE of the config,
/// not a zero inside it (the core would clamp a zero timeout to 1 ms and trip on the first quiet
/// millisecond). Every other value arms it at exactly that many milliseconds; the file edge
/// (`vike_config::Policy::apply`) has already refused the sub-second and multi-day values, so
/// nothing is clamped here either. ⚠ For one morning `None` was unreachable from this function:
/// the key was a `u64` defaulting to 60 s, and this doc said "`None` exactly when the operator
/// wrote 0". The key's own doc (`vike_config::Policy::deadman_timeout_ms`) records why that was
/// reversed the same day — the switch observes SILENCE, not the connection, so an armed default
/// halted every session-bounded venue at every close.
///
/// `halt_file` is `vike_bridge_core::halt::halt_path_from_env()` — resolved by THIS binary, not by
/// `vike-core` (env reads stay in binaries, and `vike-core` deliberately carries no
/// `vike-bridge-core` edge). It is the SAME sentinel a manual `touch HALT` writes and every venue's
/// `ExecActor` submit boundary checks, so the automatic trip and the operator's hand reach one
/// file; the resolver is memoized, so the path the switch will write is the path the mount's own
/// halt report already printed.
///
/// ⚠ Called from `live_mount_with` ONLY. The daemon's `paper_mount` arm does not read these keys,
/// by ruling — see `vike_config::Policy::deadman_timeout_ms` for why a rehearsal that halts itself
/// over a quiet minute is not wanted. ⚠ That is the live GATE, not exec actually arming: the call
/// site runs before `build_node` decides per venue, so a live-gate run whose every exec is still
/// paper (no `[venues]` table, every venue capped `paper`, `data_only`) IS armed and a trip there
/// writes the process's real HALT sentinel. The key's doc states that shape and flags the
/// alternative (keying on `vike_mount::armed_live_venues`) as an owner decision not taken here.
/// `vike-app`'s live mount did not construct it either, nor did `crates/vike-run/src/bin/ibkr_mount.rs`
/// (deleted 2026-09-28); `docs/ops/kill-switches.md` carries those residuals.
fn deadman_config_from_policy(policy: &vike_config::Policy) -> Option<vike_core::DeadManConfig> {
    let timeout_ms = match policy.deadman_timeout_ms {
        None | Some(vike_config::DEADMAN_DISABLED_MS) => return None,
        Some(ms) => ms,
    };
    Some(vike_core::DeadManConfig {
        timeout: Duration::from_millis(timeout_ms),
        action: policy.deadman_action.to_core(),
        halt_file: Some(vike_bridge_core::halt::halt_path_from_env()),
    })
}

/// **Does THIS daemon subscribe a lane that can disclose a dead link for the venue?** — the third
/// fact the link dead-man's arming decision needs, and the one neither `policy.toml` nor
/// `vike_model::link_deadman_default` can answer.
///
/// ⚠ **Why it exists at all: the per-venue table answers a question about the ADAPTER, and this
/// daemon does not subscribe every lane an adapter has.** `link_deadman_default` says a venue's
/// bridge discloses a disconnect and cites the emitters; on binance/aster/bybit/okx there are two,
/// both on a BOOK socket, and this daemon subscribes exactly one of them — the HFT quote/trade/book
/// tick pump, never `subscribe_depth` (whose `l2_snapshot` verb is a default no-op in every sink
/// this daemon owns — [`crate::feeds`]'s `VenuePlan::Cex` arm says so in its own comment). So this
/// fold is what keeps the report honest in BOTH directions: without it the mount once logged
/// `LINK DEAD-MAN ARMED for binance` while nothing could reach the latch — a false claim about an
/// automatic stop, the exact class `docs/ops/kill-switches.md` opens by warning about — and a
/// venue whose only emitter this daemon stopped subscribing would silently go back to that state.
///
/// One arm per [`VenuePlan`] variant, each read from the feed wiring it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountLinkDisclosure {
    /// This mount subscribes a lane that hands `StreamStatus` to a sink the core reads, so a link
    /// death on it reaches `vike_core`'s latch. `lane` names the subscription for the startup line.
    Discloses { lane: &'static str },
    /// It does not — so however the table classifies the venue, the switch cannot fire for it in
    /// THIS daemon. `why` is told to the operator verbatim.
    Silent { why: &'static str },
}

/// The per-plan reading behind [`MountLinkDisclosure`]. Pure and total over [`VenuePlan`], so a new
/// variant has to answer the question before it compiles.
fn mount_link_disclosure(plan: &VenuePlan) -> MountLinkDisclosure {
    match plan {
        // ⚠ The CEX arm subscribes TWO lanes and exactly ONE of them discloses. The kline `Feeds`
        // lane does not and never has: it rides `vike_bridge_core::market_pump`, which carries no
        // link disclosure at all (`crates/bridges/binance/src/family/trades.rs`'s module doc states
        // it for the trade lane in the same words). The quote/trade/book PUMP does: it owns no
        // `LiveDataSink` — it takes a bare `vike_exec::TickSender` — and pushes a
        // `vike_model::FeedStatus` straight onto it, which is the same `Ingest::StreamStatus` arm a
        // sink-side disclosure reaches. That is the lane this arm rests on, and it is the venue's
        // BOOK socket rather than a side channel: the same `@depth`-class stream `depth_main`
        // reads, through the other seam.
        //
        // ⚠ **`subscribe_depth` is still NOT the answer here and must not be "added for
        // completeness".** It would open a SECOND socket to the SAME stream and maintain a SECOND
        // copy of the same book, to obtain a signal the pump already has — and deliver it into
        // `l2_snapshot`, a default no-op in every sink this daemon owns. See the pump's own
        // `disclose_link` for the argument.
        VenuePlan::Cex { .. } => MountLinkDisclosure::Discloses {
            lane: "the quote/trade/book tick pump (crates/bridges/binance/src/family/depth.rs's \
                   md_main and its bybit/okx twins), which discloses its transport state straight \
                   onto the core tick lane — one Disconnected per outage, one Live on the first \
                   frame back. NOT the kline lane, which discloses nothing, and NOT the DOM depth \
                   feed, which this daemon does not subscribe",
        },
        // The one arm that DOES: `subscribe_book` on every seated token, onto a `vike_mount::
        // MakerSink` whose `stream_status` forwards to its inner `vike_core::CoreLaneSink`.
        #[cfg(feature = "polymarket")]
        VenuePlan::Polymarket => MountLinkDisclosure::Discloses {
            lane: "subscribe_book on every seated token, onto a MakerSink that forwards \
                   stream_status to its inner CoreLaneSink",
        },
        // Both of these reach a `CoreLaneSink` directly and their feeds DO emit GapStart — so the
        // mount is not what stops them; `link_deadman_default` is (both are SessionBounded).
        VenuePlan::Oanda(_) => MountLinkDisclosure::Discloses {
            lane: "the pricing-stream + candles-poll lanes onto a CoreLaneSink",
        },
        VenuePlan::Ig(_) => MountLinkDisclosure::Discloses {
            lane: "the Lightstreamer quote/candle subscriptions onto a CoreLaneSink",
        },
        // The rest are `LinkDeadMan::Inert` in the table for the same underlying reason — their
        // bridges call `LiveDataSink::stream_status` nowhere — so the two facts agree and the
        // table's row is the one an operator is shown.
        VenuePlan::Hyperliquid(_)
        | VenuePlan::Alpaca(_)
        | VenuePlan::Ctrader(_)
        | VenuePlan::Deribit => MountLinkDisclosure::Silent {
            why: "this venue's bridge calls LiveDataSink::stream_status nowhere, so no lane this \
                  daemon could subscribe would disclose a dead link",
        },
    }
}

/// **The LINK dead-man the live mount arms** (M13) — the fold from `policy.toml`'s grace, the
/// per-venue table, the venues this daemon actually mounts AND the lanes it subscribed for each,
/// as one pure function so the whole decision can be pinned without racing a timer.
///
/// `None` — the switch NOT constructed — in exactly two cases, and they are different facts:
/// the operator wrote `link_deadman_grace_ms = 0` (`vike_config::Policy::
/// link_deadman_grace_ms_effective` resolves that to `None`, and an ABSENT key to the armed
/// default), or NOT ONE mounted venue both is armed by `vike_model::link_deadman_default` AND has
/// a disclosing lane in this mount ([`mount_link_disclosure`]). The second is the ordinary state
/// of an FX-only daemon; it was ALSO the state of a CEX-only one until the tick pumps grew a
/// disclosure (2026-09-06), which is the reach gap `docs/decisions/0038-the-dead-man-observes-the-
/// connection-not-silence.md` recorded and its "what would reopen this" clause named.
/// [`link_deadman_arming_report`] is what tells the operator which of the three happened, per
/// venue.
///
/// ⚠ **An empty venue set is never constructed as `Some`.** A `LinkDeadManConfig` whose `venues`
/// is empty would arm a timer, contribute a waker cadence and forfeit journal replay to watch
/// nothing — the "a mechanism exists" claim `docs/ops/kill-switches.md` opens by warning about,
/// wearing a config struct.
///
/// `action` is `deadman_action`, SHARED with the silence switch: what a trip DOES is the same
/// question for both, and a second key would be a second answer to it. `halt_file` is this
/// binary's own resolved sentinel, exactly as [`deadman_config_from_policy`] resolves it, so the
/// two switches and an operator's hand all reach ONE file.
///
/// ⚠ Called from `live_mount_with` ONLY — the daemon's `paper_mount` arm constructs neither
/// switch, by the same ruling and for the same reason (`vike_config::Policy::deadman_timeout_ms`
/// argues it once). ⚠ And that is the live GATE, not exec actually arming: a live-gate run whose
/// every exec is still paper carries this switch too, and a trip there writes the process's real
/// HALT sentinel — the shape the sibling's doc states and this one inherits unchanged.
fn link_deadman_config_from_policy(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, MountLinkDisclosure)],
) -> Option<vike_core::LinkDeadManConfig> {
    let grace_ms = policy.link_deadman_grace_ms_effective()?;
    let venues: std::collections::BTreeSet<String> = mounted_venues
        .iter()
        .filter(|(v, disclosure)| {
            // BOTH halves, and the second is not a formality: an armed venue whose lanes disclose
            // nothing here would put a name in this set that the latch can never hear about, which
            // is the "a mechanism exists" claim wearing a config struct.
            vike_model::link_deadman_default(v).is_on()
                && matches!(disclosure, MountLinkDisclosure::Discloses { .. })
        })
        .map(|(v, _)| v.clone())
        .collect();
    if venues.is_empty() {
        return None;
    }
    Some(vike_core::LinkDeadManConfig {
        grace: Duration::from_millis(grace_ms),
        action: policy.deadman_action.to_core(),
        venues,
        halt_file: Some(vike_bridge_core::halt::halt_path_from_env()),
    })
}

/// **One line per mounted venue**, saying whether the link dead-man armed there and why — returned
/// rather than logged so the DECISION is testable with no subscriber (the
/// [`deadman_absent_warning`] / `venue_arming_migration_message` idiom).
///
/// Why per venue and not one summary line: the FOUR ways a venue ends up unarmed are not the same
/// fact and an operator must be able to tell them apart. A venue can be off because its market has
/// SESSIONS (`vike_model::LinkDeadMan::SessionBounded` — the conservative default, flippable by an
/// observation), because its bridge discloses no disconnect at all
/// (`vike_model::LinkDeadMan::Inert` — a declared residual nothing can fix from `policy.toml`),
/// because THIS MOUNT subscribes no lane that would carry one ([`MountLinkDisclosure::Silent`] —
/// a fact about the daemon rather than the venue, and the only one of the four a code change here
/// could fix), or because the operator wrote `link_deadman_grace_ms = 0`. A summary saying "3 of 5
/// venues armed" hides all four behind a count.
///
/// ⚠ **The mount-lane case is checked BEFORE the armed line is printed, and that ordering is the
/// whole fix.** The first version of this function printed `LINK DEAD-MAN ARMED for binance …
/// cancels this venue's resting orders` on a daemon where nothing could ever reach the latch for
/// binance, because it consulted the venue table alone. A false promise about an automatic stop is
/// worse than no line at all.
fn link_deadman_arming_report(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, MountLinkDisclosure)],
) -> Vec<String> {
    let grace = policy.link_deadman_grace_ms_effective();
    // ⚠ The SAME class as [`deadman_absent_warning`] below, found by the sweep that fixed it: the
    // `(None, _, _)` arm told an operator to DELETE A LINE from a file nothing reads any more.
    // `vike_config::remedy` renders the write instruction; `vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`
    // is the value it has to name outright, because writing the default back is how one "deletes" a
    // row (there is no `config unset`).
    let grace_remedy = vike_config::SettingsFile::Policy.write_remedy("link_deadman_grace_ms");
    mounted_venues
        .iter()
        .map(|(venue, disclosure)| {
            let row = vike_model::link_deadman_default(venue);
            match (grace, row.off_reason(), disclosure) {
                (None, _, _) => format!(
                    "LINK DEAD-MAN off for {venue}: `link_deadman_grace_ms = 0` in {} turns it \
                     off for EVERY venue. {} to restore the default grace",
                    grace_remedy.holder(),
                    grace_remedy
                        .unset_clause(&vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS.to_string())
                ),
                (Some(_), Some(why), _) => format!(
                    "LINK DEAD-MAN off for {venue}: {why}. This venue has NO automatic stop for a \
                     dead link — `vike_model::link_deadman_default` is the row, and it says what \
                     would change it"
                ),
                (Some(_), None, MountLinkDisclosure::Silent { why }) => format!(
                    "LINK DEAD-MAN off for {venue}: the venue's bridge DOES report a dead link, \
                     but THIS DAEMON subscribes no lane that carries it — {why}. So there is no \
                     automatic stop for a dead {venue} link here, whatever the venue table says"
                ),
                (Some(ms), None, MountLinkDisclosure::Discloses { lane }) => format!(
                    "LINK DEAD-MAN ARMED for {venue} at {ms} ms over {lane}: a feed-reported \
                     disconnect lasting longer than that cancels this venue's resting orders \
                     (action: {})",
                    policy.deadman_action.as_str()
                ),
            }
        })
        .collect()
}

/// **The absent-key warning** — the message the live mount emits ONCE when `policy.toml` never
/// names `deadman_timeout_ms`, so that a switch which is off by omission is at least off in the
/// log. `None` for `Some(0)` (the operator decided, and wrote it) and for `Some(n)` (armed).
///
/// # Why a WARNING and not a default, and why not silence either
///
/// The switch this key arms observes SILENCE across the whole core's ingest, not the connection
/// (`vike_config::Policy::deadman_timeout_ms` carries the derivation and the morning's reversed
/// ruling), so a compiled-in default halts every session-bounded venue at every close — which is
/// why there is none. But a key nobody has heard of is not a decision: a 24/7 crypto mount that
/// WOULD want the switch gets the same silence as an FX mount that would be harmed by it, and the
/// daemon cannot tell them apart. So the mount says, once, that the switch is off, what it would
/// do, and the one line that arms it — and `deadman_timeout_ms = 0` is the spelling that says
/// "I read this and decided", which is the ONLY thing that silences it. The shape is
/// `crates/vike-mount/src/paper_fallback.rs`'s `venue_arming_migration_message`: name the file,
/// name the key, name what was refused (here: nothing is armed), and end with a paste-ready block
/// the operator CHOOSES rather than copies — the recommendation is
/// `vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS` and it is wrong for a venue that closes, so the
/// text says so beside the number.
///
/// Returns the message rather than logging it, so the decision is testable with no subscriber
/// (the `venue_arming_migration_message` idiom); [`warn_deadman_absent`] is the `Once` latch over
/// it.
///
/// ⚠ **The headline read `DEAD-MAN SWITCH IS OFF` and the body read "this live mount has NO
/// automatic stop".** Both became FALSE the day the CONNECTION-state dead-man shipped ARMED BY
/// DEFAULT (M13 — [`link_deadman_config_from_policy`] above): an operator reading the old text
/// would conclude they were unprotected, and would then either write a key that halts their FX
/// mount every Friday or trust a warning that no longer described their daemon. The message names
/// WHICH switch is off, says the other one is on, and points at the per-venue LINK DEAD-MAN lines
/// emitted beside it. `crates/vike-tradehub/tests/deadman_absent_warning.rs` asserts the retired
/// sentence cannot come back.
///
/// ⚠ **"The other one is on" is BRANCHED on, not asserted.** This function holds the very
/// [`vike_config::Policy`] that may carry `link_deadman_grace_ms = 0`, and the first rewrite
/// stated the sibling was armed unconditionally — telling the one operator who had turned BOTH
/// switches off that they had a protection they had explicitly removed. On that box the message
/// says so instead, and names the line that did it.
pub fn deadman_absent_warning(policy: &vike_config::Policy) -> Option<String> {
    if policy.deadman_timeout_ms.is_some() {
        return None;
    }
    // ⚠ **THE REMEDY NAMES THE ONE STORE THERE IS.** MEASURED in the live journal, the CI box, v0.1.33,
    // 2026-09-23T02:41:43Z, on the boot of a daemon that logged `binance: DEMO credentials present
    // → LIVE exec client (real demo orders)` a few lines later: this message told the operator to
    // add `deadman_timeout_ms` to `<project>/settings/policy.toml`, while the SAME boot, 23 lines
    // earlier, said `policy.toml ABSENT` and `the settings DATABASE answers for every key on this
    // box`. Following the instruction created a file nothing read, and the next restart printed the
    // identical warning. `docs/decisions/0086` removes the file this remedy used to sometimes name
    // rather than fixing the message per box: there is one store now, so `vike_config::remedy`
    // renders one arm unconditionally.
    let remedy = vike_config::SettingsFile::Policy.write_remedy("deadman_timeout_ms");
    // ⚠ BRANCH on the OTHER switch's effective state rather than asserting it. This function holds
    // the very `Policy` that may say `link_deadman_grace_ms = 0`, and on such a box the
    // unconditional sentence told the operator a switch was armed while every per-venue line below
    // said it was off for that exact reason.
    let sibling: String = match policy.link_deadman_grace_ms_effective() {
        Some(_) => "\
         ⚠ Do not read that as \"no automatic stop\": the CONNECTION-state dead-man is armed by \
         default and is a DIFFERENT key. It cancels a venue's resting orders when that venue's \
         own feed reports the link DISCONNECTED for longer than `link_deadman_grace_ms`, and it \
         stays quiet through a market that merely closed — which is why it may have an armed \
         default and this key may not.\n\n\
         ⚠ Armed by default is not armed EVERYWHERE, and this message will not guess for you: \
         where it arms is a per-venue question (every FX and equities venue is off because its \
         market has sessions, several venues disclose no disconnect at all, and a venue whose \
         only emitter is a lane this daemon does not subscribe is off too). The per-venue LINK \
         DEAD-MAN lines beside this message are the answer for THIS mount — read those, not this \
         paragraph. This key is the optional EXTRA for a 24/7 mount that also wants a silence \
         detector."
            .to_string(),
        None => {
            // ⚠ The SIBLING's way back is the same class as this message's own remedy, rendered
            // through the same `vike_config::remedy` so the two halves of one warning can never
            // disagree about how to write it.
            let grace = vike_config::SettingsFile::Policy.write_remedy("link_deadman_grace_ms");
            format!(
                "\
         ⚠ AND NEITHER IS THE OTHER ONE: you wrote `link_deadman_grace_ms = 0`, which turns the \
         CONNECTION-state dead-man off for EVERY venue. So this mount has NO automatic stop of \
         any kind — not for a dead socket and not for a quiet feed. {} to restore the \
         connection-state switch at its default grace; this key stays a separate decision.",
                grace.unset_clause(&vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS.to_string())
            )
        }
    };
    Some(format!(
        "THE SILENCE DEAD-MAN IS OFF: {}, so this live mount has no automatic stop for \"the feed \
         went QUIET and orders are still resting\". It is off by OMISSION, not by decision — \
         nothing arms it unless you write the key, and this line is the only place that says \
         so.\n\n\
         {sibling}\n\n\
         What THIS key would add: after that many milliseconds with no venue event, tick, bar, \
         quote, trade or book update on ANY venue or symbol this daemon follows, cancel every \
         resting order and (with the default `deadman_action`) engage HALT — `Halted` on every \
         engine plus the HALT sentinel file, which an operator must delete before the next \
         submit.\n\n\
         ⚠ It observes SILENCE, not the connection. On a venue whose market CLOSES (FX over the \
         weekend, equities every evening) or on a thin instrument in a quiet minute it trips with \
         no outage at all. Arm it on a 24/7 mount only.\n\n\
         To arm it for a venue that never closes, {} \
         (milliseconds; {} is the recommendation for a 24/7 venue, raise it for a thin one):\n\n\
         {}\n\n\
         To record that you decided AGAINST it, and silence this message:\n\n\
         {}\n",
        remedy.absent_clause(),
        remedy.write_clause(),
        vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS,
        remedy.write_line(&vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS.to_string()),
        remedy.write_line("0"),
    ))
}

/// The `Once` latch over [`deadman_absent_warning`]: `tracing::warn!` the message, at most once
/// per process, and only when there is one (an absent key). Same idiom, and the same reason, as
/// `crates/vike-mount/src/paper_fallback.rs`'s `venue_arming_migration` — the fact is
/// process-wide, and a paste-ready block repeated is a paste-ready block buried. Called from
/// `live_mount_with` beside the `deadman:` construction, and from nowhere else: the composition
/// root is where the policy is known to be a LIVE mount's, and the gate-off `paper_mount` arm
/// neither arms the switch nor warns about it.
///
/// ⚠ The latch sits INSIDE the `None` test, not around it, so a call that had nothing to say does
/// not consume the one chance to say it — the test that drives this counts events across a
/// `Some(n)`, a `Some(0)` and two `None`s in that order and expects exactly one.
pub fn warn_deadman_absent(policy: &vike_config::Policy) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if let Some(message) = deadman_absent_warning(policy) {
        ONCE.call_once(|| tracing::warn!("{message}"));
    }
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
/// the string `docs/ops/tradehub-the CI box.md` and `deploy/vike-tradehub.service` both call the
/// authority on paper-vs-live, and it was being built from the PROFILE instead. MEASURED on the CI box,
/// one startup: nine venues in this set, one venue in that banner. Do not re-narrow the return.
// ⚠ MORE arguments than clippy's default allows, and the shape is the point rather than an
// oversight: this is the daemon's live composition seam and every parameter is an INJECTED
// IMPURITY — the resolved mount set, the operator budget, the run profile, the machine policy, the
// flags, the credential map, the state directory the B11 sentinels are claimed in, this
// deployment's origin claim, and the feed constructors. That list is what lets
// `src/feed_splice_seam_tests.rs` drive the REAL mount path with no store, no network and no real
// keys; bundling them into a struct would hide exactly the substitutions those tests exist to
// make. `state_dir` was the argument that crossed clippy's threshold, and it cannot be resolved
// here: only the boot walk knows it.
// ⚠ A NEW parameter goes at the END, after `state_dir` — see that parameter's own note: the
// live-account sentinel gate follows it through this signature by INDEX. This comment used to
// carry a COUNT of the arguments, which is why it now does not.
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
    // `config.instance_origin` — this deployment's origin claim, stamped into every client order
    // id this core mints so a SECOND instance on the same venue account is recognisable on
    // reconcile rather than anonymous (`vike_model::instance_origin`). A PARAMETER because this
    // function resolves no settings of its own; `None` (the default) is byte-identical to before
    // the key existed.
    //
    // ⚠ AFTER `state_dir`, not beside `flags`, and that is a constraint rather than a preference:
    // `crates/vike-ops/tests/live_lock_claim_order_gate.rs` follows the sentinel directory through
    // this signature BY INDEX, so a parameter inserted ahead of `state_dir` makes that gate pin
    // the wrong argument. Add new ones at the end.
    instance_origin: Option<vike_model::InstanceOrigin>,
    // `config.journal_dir` folded over the process env — see [`journal_vars`] and [`live_mount`].
    journal_vars: &HashMap<String, String>,
    make: &dyn FeedCtors,
    // Every venue's `venue_setting` rows, read ONCE by `run` from the boot's settings directory
    // (`load_venue_settings_for`): carried on `MountPolicy::venue_settings`, and read by
    // `wire_venue_feeds` for the mark-stream rows — one snapshot for both. LAST, like every
    // addition since the live-lock gate pinned `state_dir`'s position.
    venue_settings: std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<LiveMount, String> {
    // The `VIKE_RECONCILE_*` FAMILY's env map — cadences, lookbacks, the policy name, the balance
    // tolerances, AND the two sibling FLAGS (`reconcile_generate_missing`, `reconcile_balance`),
    // all built from this one map by `reconcile_config::build_recon_config` below.
    //
    // ⚠ This paragraph used to say those two flags were "recorded as unconsumed in
    // `vike_config::CONSUMPTION` rather than half-wired here", and the very next line folds them —
    // a written claim that was false about the code it sat on, which is the exact defect class this
    // branch exists to remove. What is true is the argument BEHIND it: the family still moves as
    // ONE map, because `build_recon_config` reads the whole of it out of one map and nothing was
    // split off. `daemon_recon_env` folds in the QUARANTINE-FIRST policy default and then
    // `or_insert`s the two resolved flags over a base map that IS the process env — so an
    // `Environment=VIKE_RECONCILE_BALANCE=0` line is already present and is never replaced. See
    // [`daemon_recon_env_from`], whose precedence is pinned by
    // `the_process_env_beats_the_resolved_flag_in_the_reconcile_family`.
    //
    // ⚠ The MASTER GATE itself is decided further down, not here, and the move is deliberate: since
    // S2 the default is ON for a mount that arms a live venue account, and that fact is not known
    // until `vars` has been through the `data_only` withhold below and `vike_mount::armed_live_venues`
    // has been asked. Deciding it up here would either read a pre-withhold map (arming reconcile for
    // a venue this daemon deliberately leaves on paper) or force a second probe call that could
    // disagree with the one the lock claim uses.
    let recon_env = daemon_recon_env(flags);

    // ⚠ **FIRST, before anything reads `vars`.** The resolved flags have to be in the map before
    // `venue_feed_plan`, before `armed_live_venues` and a long way before `build_node`, because
    // every one of those asks the map a question a `flags.toml` line is now allowed to answer.
    // Folding later would give the earlier readers a different map from the later ones, which is
    // the "one flag, two places, disagreeing" failure `vike_config::flags`' module doc names.
    //
    // ⚠ It also runs BEFORE the `data_only` withhold below, which strips `vars` by `{VENUE}_`
    // prefix — so a folded key whose NAME started with an eligible venue's prefix would be counted
    // in that disclosure's `keys_withheld` as though the store had held one more credential. None
    // does: `no_folded_flag_key_collides_with_a_data_only_venue_prefix` holds that across both
    // tables, so a new folded key or a new `DATA_ONLY_VENUES` row cannot introduce one silently.
    fold_flags_into_vars(flags, &mut vars);

    // Safety gates #4/#5 — the daemon's venue+symbol ALLOW-LIST, resolved per MOUNT and BEFORE
    // `vars` is moved into the NodeConfig (split-plane I10: every mount must pass its own venue's
    // gate, and the venue set collects ONE plan per DISTINCT venue, in mount order — the plan is
    // venue+environment-derived, so mounts sharing a venue share its plan). Any unwired venue is a
    // HARD ERROR (never a silent paper fallback). The dispatch itself lives in `venue_feed_plan`
    // (`feeds.rs`), where `live_wired_venues_pin.rs` scans its arms. Safety gate #3 (feed side,
    // decision 0095) is now INSIDE that dispatch: `venue_feed_plan` takes the ceilings directly.
    let mut venue_plans: Vec<(String, VenuePlan)> = Vec::new();
    for m in &mounts {
        let plan = venue_feed_plan(&m.cfg, &vars, &policy.venues)?;
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
    //    a STRATEGY, while `build_node` arms an exec client for every `crate::wired_markets::WIRED_MARKETS`
    //    venue the credential store answers for and the ceiling permits. MEASURED on the CI box, one
    //    startup of the shipped daemon, two lines apart — `live_venues={"hyperliquid","deribit",
    //    "okx","bybit","alpaca","aster","binance","ig","oanda"}` beside `mode":"LIVE (venue=bybit)"`
    //    — nine live authenticated sessions behind ONE lock. `vike_mount::armed_live_venues` is the
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
    let mount_policy = vike_mount::MountPolicy {
        // ⚠ THE `account` TABLE IS READ HERE, in the composition root, and nowhere below it. The
        // mount needs it — dukascopy resolves WHICH LEGAL ENTITY an order reaches from an `account`
        // row's credential-key owner prefix (`crates/bridges/dukascopy/src/mount.rs`) — and so does
        // the arming projection, which must describe exactly what the mount will do. Until 2026-09-15
        // `vike-mount` opened the store itself, from a library file, at a directory taken from a
        // process global: the class `crates/vike-ops/tests/settings_registry.rs`'s
        // `CREDENTIAL_STORE_PIN` ratchets down, and invisible to it because neither account reader
        // was one of `CREDENTIAL_STORE_READERS`' keyed names. Both are keyed now, and this root —
        // already pinned, already the box's one credential reader — performs the read.
        //
        // ONE snapshot, carried on the policy object both seams already receive, so no caller can
        // hand the projection and the mount different tables. Errors are carried VERBATIM rather
        // than swallowed: a store that exists and will not open refuses a labelled account by
        // NAMING the failure, where a swallowed error used to be re-reported as a bad `account` row.
        accounts: vike_bridge_core::account_directory::AccountDirectory::read(
            vike_bridge_core::credentials::load_workspace_accounts_from_env(process_env()),
            vike_bridge_core::credentials::load_workspace_account_keys_from_env(process_env()),
        ),
        // The `venue_setting` snapshot `run` read — cloned rather than moved, so the parameter
        // stays usable after `mount_policy` moves into the `NodeConfig` below.
        venue_settings: venue_settings.clone(),
        ..vike_mount::MountPolicy::from(policy)
    };
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
    // ⚠ ONE call, TWO consumers. The armed set decides which sentinels to claim AND — since S2 —
    // whether this mount reconciles at all. Asking twice would let the lock claim and the reconcile
    // gate disagree about what this daemon is about to authenticate as, which is the one thing they
    // must never do.
    let armed_live = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &vars,
        &mount_policy,
    );
    for route_key in &armed_live {
        match vike_ops::live_lock::LiveLock::acquire(state_dir, route_key) {
            Ok(l) => live_account_locks.push(l),
            Err(e) => return Err(format!("refusing LIVE mount: {e}")),
        }
    }

    // THE RECONCILE MASTER GATE (S2) — ON by DEFAULT for a mount that arms a live venue account,
    // paired with `quarantine` so it folds nothing. One decision — in `vike-ops`, and called
    // identically by `vike-app`'s `App::new`, until the desktop lost its local core and the gate
    // moved here (2026-09-23) — so the GUI and the daemon could not differ about what a restart does.
    //
    // ⚠ THE CONSEQUENCE, stated where it happens: from here a live mount issues AUTHENTICATED READ
    // calls against every armed account at startup and then every `VIKE_RECONCILE_INTERVAL_MS`
    // (default 60 s), whether or not anybody asked. That is the price of not trading against a
    // belief, and it is bounded (a handful of REST reads per venue per minute, inside every wired
    // venue's published budget) — but aster has no testnet credentials in the store (its testnet
    // exists and is routed; only `ASTER_LIVE_*` is configured), so on a box that arms aster they are
    // MAINNET reads. `reconcile_gate`'s own doc carries the argument and the three ways to refuse.
    let recon_gate =
        reconcile_config::reconcile_gate(flags.reconcile, flags.reconcile_off, armed_live.len());
    // Disclosed at WARN whichever way it went, through the ONE shared emitter: "reconcile is on and
    // nobody asked" and "reconcile is off on a live box" are both things an operator must be able to
    // find in a journal without knowing which flag to grep for, and `vike-app` had to say it the same
    // way while it reconciled. See `reconcile_config::log_reconcile_gate`.
    reconcile_config::log_reconcile_gate(recon_gate, armed_live.len());
    let recon_enabled = recon_gate.enabled();

    // ...and the one CROSS-mount consistency gate, likewise before any core spawns: two polymarket
    // mounts on ONE token must agree on the interval, because they will share one `MakerSink`
    // whose bar synth runs at exactly one window (see `LiveFeeds::Polymarket`).
    #[cfg(feature = "polymarket")]
    check_poly_token_intervals(&mounts)?;
    // ...and its ctrader twin (split-plane I9): every ctrader mount shares one data socket and one
    // bar SYNTHESIZER window, so rows disagreeing on the interval are refused before any core
    // spawns — see `check_ctrader_intervals`.
    check_ctrader_intervals(&mounts.iter().map(|m| &m.cfg).collect::<Vec<_>>())?;

    // Opt-in PIT-`SymbolProperties` recorder (`VIKE_RECORD_PROPERTIES=1`). ⚠ ALWAYS `None` since
    // the daemon stopped writing the store (#2093, 2026-09-22, docs/decisions/0084):
    // `PropertiesRecorder` opens the concrete DataFusion store, which only a `record-feeds`/
    // `materialize` build carried (both enabled `vike-data/hist-datafusion`, and #2093 deleted
    // both); the recorder that survives runs inside the datahub, which opens its own store. So
    // nothing is constructed here, and `VIKE_RECORD_PROPERTIES` is inert in this binary whatever
    // sets it — the flag fold still writes it into `vars`, and no code downstream reads that key.
    //
    // ⚠ This comment described a construction HERE, through `PropertiesRecorder::open_from_vars`
    // over the folded `vars`, until 2026-09-28 — a call this function has not made since #2093.
    // What that call taught survives it, because it is why the key's fold tier is
    // [`FoldTier::Resolved`]: `open_from_env` is a one-line wrapper that calls `open_from_vars` over
    // `PropertiesRecorder::env_snapshot` — a map holding just this one variable, read from the
    // PROCESS ENV — so handing the map half the credential store's map instead REPLACES the
    // environment read rather than widening it, and an `or_insert` fold would let a stale
    // `secrets.env` line beat an exported value in BOTH directions. A root that constructs one again
    // inherits that argument; `a_process_env_value_beats_a_file_value_for_every_wired_key` is the
    // proof and `PropertiesRecorder::gate` the exact-`"1"` grammar.
    let properties_rec: Option<Arc<vike_data::PropertiesRecorder>> = None;

    // The live core config carries the SAFETY KNOBS a live daemon must be capped with (NOT the paper
    // mount's minimal `CoreConfig::default()`). Field names/values mirrored vike-app's fat-build
    // CoreConfig (the desktop builds none since #1610): a 30s submit-ack backstop, a 25%
    // equity-drawdown liquidate-only latch, and the margin-call watchdog (inert at 1× until
    // leverage is raised). `recon_enabled` is the
    // S2 reconcile gate resolved above (a PAPER mount ⇒ `false`, byte-identical to the pre-reconcile
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
    // This is why `vike_mount::journal_venue_mounts` takes ROWS rather than the map. A signature
    // taking `vars` would let the call sit anywhere downstream and be wrong by a comment; taking
    // rows makes the caller pick this moment, and there is only one.
    let arming = vike_mount::venue_arming(crate::registry::REGISTRY, &vars, &mount_policy);

    // THE DEAD-MAN'S ABSENT-KEY WARNING, once, beside the construction it describes (the
    // `deadman:` field of the literal below). Here and not in `deadman_config_from_policy`, because
    // this is the composition root where `policy` is known to be a LIVE mount's — the fold is pure
    // and the gate-off `paper_mount` arm never calls it, so a warning inside the fold would be a
    // warning nobody could count. Fires for an ABSENT key only: `deadman_timeout_ms = 0` is the
    // operator's recorded decision and gets no line. `warn_deadman_absent`'s doc argues the shape.
    warn_deadman_absent(policy);

    // THE LINK DEAD-MAN's venue set and its per-venue report (M13). The set is the DISTINCT venues
    // this daemon mounts, in mount order — `venue_plans` above already collected exactly that, and
    // it is the right set rather than a convenient one: a feed thread exists only for a mounted
    // venue, so a venue outside it can never disclose a `FeedStatus` and arming it would watch a
    // link nothing reports on. The report is emitted here, at INFO, one line per venue, because
    // "which venues does my automatic stop actually cover" is a startup question and the answer is
    // a join of a policy key, a per-venue table and this mount's own venue list — none of which an
    // operator can compute from the file alone.
    // ⚠ The PLAN travels with the venue, not just its name: whether the switch can fire here is a
    // question about the LANES this daemon subscribed, and `mount_link_disclosure` is the reading
    // of the feed wiring that answers it. Without it the mount announced an armed switch on every
    // CEX venue while nothing could reach the latch for any of them.
    let link_venues: Vec<(String, MountLinkDisclosure)> =
        venue_plans.iter().map(|(v, plan)| (v.clone(), mount_link_disclosure(plan))).collect();
    for line in link_deadman_arming_report(policy, &link_venues) {
        tracing::info!("{line}");
    }

    let mut node_cfg = NodeConfig {
        // THE VENUE REGISTRY this daemon names the bridges in (docs/decisions/0096, amended
        // 2026-09-29): `build_node` names none, and is handed it here.
        registry: crate::registry::REGISTRY,
        // THE WIRED MARKETS (docs/decisions/0098): which venues the node mounts, on which symbol,
        // in which orders — the same table `armed_live_venues` above claimed its locks from.
        markets: crate::wired_markets::WIRED_MARKETS,
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
            // The origin claim this daemon's client order ids carry — the parameter above,
            // straight through. Consumed at a FRESH coid session only (a restart resumes its
            // persisted one verbatim): `vike_core::CoreConfig::instance_origin` argues why.
            instance_origin,
            // ⚠ These three are the daemon's DEFAULTS, not its answer: `profile`'s `[guards]` table
            // overwrites whichever of them it names, via `apply_guards_and_sinks` below. Before that
            // call existed they were the only values that could ever apply, and an operator's
            // configured guards were parsed, warned about and discarded.
            submit_ack_timeout: Some(Duration::from_secs(30)),
            max_drawdown: Some(0.25),
            margin_call: Some(vike_exec::MarginCallConfig::default()),
            // THE DEAD-MAN SWITCH, from `policy.toml` and nothing else (M4). OPT-IN: armed only
            // when the operator wrote `deadman_timeout_ms = <n>`; an absent key and an explicit
            // `0` both leave this `None`, and `warn_deadman_absent` above is what tells the two
            // apart. (This comment read "Armed by default — sixty seconds of ingest silence
            // cancels the book" for one morning; the key's doc records the reversal.)
            // `deadman_config_from_policy` is the whole fold and argues each half;
            // `crates/vike-config/tests/policy_is_consumed.rs` names THIS line. ⚠ Not one of the
            // three `apply_guards_and_sinks` overwrites below: a run profile has no `[guards]`
            // field for it, deliberately — a profile may not lower a policy ceiling, and
            // `the_run_profile_cannot_touch_the_deadman` pins that.
            // ⚠ When written, armed by the live GATE, not by any venue arming: this literal is
            // built before `build_node` decides per venue, so an all-paper or `data_only` run
            // under the gate carries the switch too (the key's doc states the shape and the open
            // decision).
            deadman: deadman_config_from_policy(policy),
            // THE CONNECTION-STATE DEAD-MAN (M13) — ARMED BY DEFAULT, unlike the switch above, and
            // the asymmetry is the whole point of the re-ruling: this one observes what the BRIDGE
            // reports about the link, so a market that merely closed cannot trip it, so it may
            // have an armed default. `link_deadman_config_from_policy` folds three things — the
            // policy grace (absent ⇒ armed at `DEFAULT_LINK_DEADMAN_GRACE_MS`, `0` ⇒ off), the
            // per-venue table `vike_model::link_deadman_default`, and the venues this mount
            // actually has — and returns `None` when the grace is off OR no mounted venue is
            // armed, so an FX-only daemon builds nothing. The per-venue INFO lines above say which
            // it was. `crates/vike-config/tests/policy_is_consumed.rs` names THIS line.
            // ⚠ Not reachable from a run profile's `[guards]`, deliberately, exactly like the
            // sibling: a profile may not lower a policy ceiling.
            link_deadman: link_deadman_config_from_policy(policy, &link_venues),
            // Honor the same opt-in write-ahead journal the paper mount does (off by default).
            // ⚠ The RESOLVED run profile answers when there is one; otherwise the map this binary
            // resolved, which is what lets `config.journal_dir` in `<project>/settings/config.toml`
            // enable the WAL (no file could do that while this was a `vike-core` library env read,
            // and `VIKE_JOURNAL_DIR` still wins over the file inside [`journal_vars`]). The profile
            // rung is NOT a convenience: `journal_config_from` re-opens the file `VIKE_RUN_PROFILE`
            // names, so with a `run` ROW active and that variable still set the daemon would print
            // "which no longer decides anything" about a variable still deciding this sink. See
            // `crate::profile_rows::journal_config_for`.
            journal: crate::profile_rows::journal_config_for(profile.as_ref(), journal_vars),
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
    // `docs/decisions/0013-degrade-vs-refuse.md`), and the sentence is rendered by the type so no
    // caller can drift from the arithmetic — and this daemon is the only one that applies a run
    // profile's guards (it named "this daemon and `vike-app`" until 2026-09-28).
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
    // wired-market node. Per-venue exec is credential-gated inside `build_node` (absent creds ⇒ that
    // venue is paper), and `spawn_core_multi` wires applied-fill capture on each mount's own venue
    // engine. `recon_clients`/`recon_trigger` are captured (the reference mount needs them).
    //
    // ⚠ `build_live_multi_strategy_core` with ONE mount IS `build_live_strategy_core` (first mount
    // → `strategy`, an empty rest → `extra_mounts`), so the single-mount daemon builds the same
    // node it always did. `cfgs` is cloned out first — the feed wiring below still needs each
    // mount's A-S lowering after the specs move into the core.
    let cfgs: Vec<MakerMountConfig> = mounts.iter().map(|m| m.cfg.clone()).collect();
    let vike_mount::Node { handle, recon_clients, recon_trigger, live_venues, forwarder_stop } =
        vike_mount::build_live_multi_strategy_core(
            mounts
                .into_iter()
                .map(|m| vike_mount::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
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
    for r in vike_mount::journal_venue_mounts(
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

    // Carry the reconcile ingredients across the feed build ONLY when the S2 gate said yes — the
    // `spawn_recon` mount runs AFTER the feeds so a feed-subscribe error returns BEFORE any
    // `vt-core-recon` thread is spawned (no orphaned driver on the error path). OFF (a paper mount): the
    // handles are moved into the tuple and DROPPED right here — matching the pre-reconcile daemon's
    // `..`-destructure drop timing exactly (`recon_trigger` is already `None`, since `build_node` gates
    // the channel on the same flag) — so no ingredients survive to mount a driver. This is the ONLY
    // unconditional new binding, and its OFF-path effect is a byte-identical early drop.
    let recon_ingredients = recon_enabled.then_some((recon_clients, recon_trigger));

    // ⚠ The daemon no longer records its own feed (0084: the store has ONE writer plane, and the
    // recorder that survives runs inside the datahub with its watchdog, silence detection and
    // record profiles — none of which the removed `record-feeds` tee had). `post_feeds` stays as
    // the teardown slot so the sequential TAIL below keeps its shape; it is always `None` now.
    let post_feeds: PostFeeds = None;

    // Identity: nothing is teed off the venue's base sink any more.
    let wrap = |base: Arc<dyn LiveDataSink>| -> Arc<dyn LiveDataSink> { base };

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
            &venue_settings,
        )?);
    }

    // Reconcile driver mount — the reference was `vike-app`'s `App::new` recon_driver
    // block (gone with the desktop's local core). Placed AFTER the feeds are up so a
    // feed-subscribe error above returns before this ever spawns a thread. `recon_ingredients` is
    // `Some` only when `recon_enabled` (the OFF path already dropped the handles above) ⇒ this
    // whole mount is byte-identically absent on a PAPER mount and whenever the operator refused it.
    // `spawn_recon` owns its own `vt-core-recon` thread and respects the single-writer rule (it
    // only blocking-fetches REST reports and enqueues `Command::ReconcileReports` for the fold thread —
    // see that module's doc); it ADOPTS the pre-built `recon_trigger` pair so a venue reconnect poke
    // reaches this same driver.
    let recon_driver = match recon_ingredients {
        None => None,
        // Enabled but every venue is paper (no creds ⇒ no `ReconClient`): inert, no thread — mirrors
        // the `recon_clients.is_empty()` arm vike-app had. The (empty) clients + `recon_trigger`
        // drop here.
        Some((clients, _trigger)) if clients.is_empty() => {
            // ⚠ This arm is REACHABLE now in a way it was not before S2: the armed-live probe is
            // INTENT-based (`vike_mount::armed_live_venues`' own doc says so), so a venue whose
            // synchronous connect fails or whose factory declines a present-but-bad key probes live,
            // turns the default gate on, and then hands back no client. Saying so plainly beats a
            // silent no-op, because "reconcile is on" and "reconcile reached a venue" are different
            // facts and an operator reading the gate's own disclosure above has only the first.
            tracing::warn!(
                "reconcile is ON but no venue produced a ReconClient (every armed venue fell back \
                 to paper); the driver is inert this session and NOTHING is being reconciled"
            );
            None
        }
        Some((clients, recon_trigger)) => {
            // The per-venue feed-status health map, from the feeds THIS mount actually built
            // ([`LiveFeeds::recon_feed_statuses`]). `build_node` builds no market feeds of its own
            // (they stay with the caller — see `crates/vike-mount/src/node.rs`'s module doc), so the mounted venue's
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
                // LEGS, not venues: one row per venue ACCOUNT since the per-account producer
                // landed, and equal to the venue count on every box with no `[accounts]` table.
                accounts = clients.len(),
                policy = ?recon_cfg.policy.default,
                "mounting the reconciliation engine (quarantine-first unless VIKE_RECONCILE_POLICY \
                 is set); a restarted live daemon re-adopts open venue orders/positions instead of \
                 a blind fresh core. See the reconcile-gate line above for WHY it is on"
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
/// (`vike_alerting::persist::ALERTS_FILE`), so the two sides cannot disagree about it.
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
        None => Some(state_dir?.join(vike_alerting::persist::ALERTS_FILE)),
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
/// The SUBDIRECTORY is the spelling vike-app's project rung used (`state_dir_path` in that binary
/// joined `strategy-state` under ITS state root, until it went with the desktop's local core) so
/// the two binaries shelved strategy state the same way; hanging it off [`state_dir`] means
/// `$VIKE_STATE_ROOT` relocates it together with every other file this daemon writes (alerts, the
/// telegram ledger, logs). `None` — no project, no override — keeps `CoreConfig::state_dir` at
/// `None`: no sidecar is ever written or read, byte-identical to this daemon before B5's residual
/// closed. Deliberately NOT `config.state_dir`:
/// that key was the DESKTOP's sidecar knob, and this daemon's state tree is uniformly
/// `$VIKE_STATE_ROOT`-rooted. ⚠ That key no longer exists at all — the unread-settings sweep
/// deleted it once the desktop cut took its one reader, and `vike_config::REMOVED_ENV` refuses
/// `VIKE_STATE_DIR` at startup — so the distinction this paragraph draws is now permanent rather
/// than a choice this daemon re-makes.
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
/// The thunk used to be `vike_alerting::webhook_configs_from_workspace_env`, a library
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
        vike_alerting::webhook_configs_from_env(&workspace_credentials())
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
/// fold, and the accept loop runs on a detached thread. ORDER CONTROL is double-gated: `Scope::Write`
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
    // The ACCOUNT-ADMIN declaration (`config.tradehub_account_admin`), caller-owned like every
    // other settings fact here. This function DECIDES from it — see `account_admin_source` — and
    // the decision is deliberately taken beside the bind decision rather than in `main`, because
    // the two ask the same `resolved` addresses and a second resolution could answer differently.
    account_admin: Option<&str>,
    // The BOOT's settings directory, so the account store and the daemon's own credential read
    // resolve from ONE walk.
    settings_dir: Option<&std::path::Path>,
    // The REQ-2 advertisement (`config.datahub_advertise_addr`), likewise caller-owned: this
    // function stamps it into `Welcome` and decides nothing about it.
    datahub_advertise: Option<&str>,
) -> Option<PublisherHandle> {
    // An absent, blank or whitespace-only address means "no publisher" — the `?` returns None for
    // the whole function, as the explicit `None => return None` arm did before 1.97's
    // `question_mark` lint asked for it.
    let addr = addr.map(str::trim).filter(|a| !a.is_empty())?.to_string();
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
                "node server binding a NON-LOOPBACK address (a `flags.tradehub_allow_public_bind` \
                 row, or VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1) — the node \
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
                 trusted network, `vike-cli config set flags.tradehub_allow_public_bind true` (or \
                 VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1). The daemon keeps trading headless"
            );
            return None;
        }
    }
    // `auth::from_vars`, not `NodeKeys::from_vars`: the TYPE moved down to
    // `vike_node_proto::auth` when 0025 gave the datahub the same handshake, so the
    // tradehub-NAMED constructor is a free function in this service's binding module. Same two key
    // names, same trimming, same credential-is-the-gate `None`.
    // ⚠ THE NODE STORE, not `workspace_credentials()`. This daemon legitimately reads BOTH files —
    // it trades, so it needs the venue grid, and it serves, so it needs a node pair — but they are
    // different files for different reasons and it must not find one in the other. `node.env` first,
    // the credential store second while a box has not migrated, and the fallback SAYS SO.
    // The override comes out of the ONE sweep this binary owns, exactly as `workspace_credentials`
    // takes it — never a second `std::env::var`, which would be a `Layer::Library` row on a
    // may-only-shrink work-list and could answer differently from the boot.
    // ⚠ The probe is THIS SERVICE'S FAMILY, not all four platform names. `resolve_node_keys`
    // answers WHICH FILE, and this root then reads its own pair out of it — so with the wide
    // predicate a `node.env` holding only the DATAHUB pair (what `vike-cli datahub setup` writes)
    // answered `NodeFile` here, this daemon's tradehub pair in `secrets.env` resolved to nothing,
    // and the observe server declined to start blaming an absent credential that was sitting in a
    // file it had stopped reading. Decision 0051's "answers wholly" is scoped to the PAIR.
    let settings_override = process_env().get(vike_secrets::SETTINGS_DIR_ENV).map(String::as_str);
    let node_path = vike_secrets::workspace_node_path_from(settings_override);
    let settings_display = node_path
        .parent()
        .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
    let (node_store, node_key_source) = match vike_secrets::resolve_node_keys(
        settings_override,
        vike_model::credential_keys::is_tradehub_node_key,
    ) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(
                error = %e,
                %addr,
                "the node-key store is PRESENT but UNREADABLE — observe server NOT started. This is \
                 NOT the absent-credential gate: an unreadable file and a missing one must never \
                 look the same to an operator. Fix its permissions and restart; the daemon keeps \
                 trading headless"
            );
            return None;
        }
    };
    if node_key_source == vike_secrets::NodeKeySource::LegacyCredentialStore {
        tracing::warn!("{}", vike_secrets::legacy_node_key_notice(&settings_display));
    }
    // ⚠ The node store as a MAP, kept rather than consumed: the admin-key probe below reads the
    // same map, and a second `into_map` would be a second read of the same file with no guarantee
    // the two saw one state.
    let node_vars = node_store.secrets.into_map();
    let keys = match vike_tradehub_client::auth::from_vars(&node_vars) {
        Some(k) if k.has(Scope::Read) => k,
        _ => {
            tracing::error!(
                %addr,
                "a node-server address is configured but there is no VIKE_TRADEHUB_OBSERVE_KEY in \
                 the node-key store — observe server NOT started (absent credential is the gate); \
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
        NodeKeys::new(keys.key_for(Scope::Read).to_vec(), Vec::new())
    };
    // The command sink is WITHHELD unless control is enabled — the second, independent gate, so even
    // a mis-set control key can never reach the core without the master flag.
    let commands = control_enabled.then(|| handle.command_sink());
    // ⚠ **THE ACCOUNT-ADMIN CAPABILITY, decided HERE and nowhere else.** `None` — every box that
    // has not declared the barrier — means the server holds no account writer at all, advertises
    // no `account-verbs` capability, and is byte-identical to a binary without the verb. The three
    // questions it asks, and why the process cannot ask the confidentiality one for itself, are on
    // `account_admin_source`.
    //
    // ⚠ It reads the SAME `resolved` addresses `bind_decision` classified above, rather than
    // resolving the address a second time: two resolutions of one hostname can answer differently
    // (a DNS change between them), and the one thing this capability may not do is arm against a
    // bind that is not the one that happened.
    let accounts = account_admin_source(account_admin, &resolved, &node_vars, settings_dir);
    // ⚠ **THE ADMIN KEY IS ATTACHED ONLY WHEN THE CAPABILITY IS ARMED**, which is the key-ZEROING
    // gate above wearing the other polarity — and it is the stronger half of the same idea. The
    // control gate ZEROES a key it loaded; this one never loads one at all unless
    // `account_admin_source` said yes, so an `Admin` auth against an undeclared box cannot verify
    // REGARDLESS of what the node-key store holds. `from_vars` two lines up reads two names and
    // leaves `Scope::Account` empty, which is what makes that the default rather than a check.
    //
    // ⚠ **EXTEND the value the control gate already produced — never RE-READ the map.** This line
    // spelled `from_vars_with_admin(&node_vars)`, which is `from_vars` + `with_admin`, and
    // `from_vars` re-reads BOTH `OBSERVE_KEY_ENV` and `CONTROL_KEY_ENV`. That DISCARDED the zeroing
    // above rather than building on it, so arming the account capability on a control-DISABLED box
    // handed the process back the very control key that gate had emptied — and `run_handshake`'s
    // `scope == Scope::Write && !keys.has(Scope::Write)` refusal then admitted a Control peer to
    // `Request::Preview`, whose arm checks the scope and has NO sink gate. Measured 2026-09-17 by a
    // probe replaying both steps verbatim. `the_ordinary_node_key_read_leaves_the_admin_scope_absent`
    // cannot see it: it asserts only that `Scope::Account` is absent, never that Control stays absent.
    let keys = keys_for_account_capability(keys, accounts.is_some(), &node_vars);
    if control_enabled {
        tracing::warn!(
            "order-control channel ENABLED (a `flags.tradehub_control` row, or \
             VIKE_TRADEHUB_CONTROL=1) — a Control-authenticated peer may place/cancel REAL orders"
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
                accounts,
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
        "tradehub node server listening (authenticated; observe always, control gated by a \
         `flags.tradehub_control` row / VIKE_TRADEHUB_CONTROL)"
    );
    Some(publisher)
}

/// **The ONE site that decides whether this daemon can write credentials from the wire** —
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s three parts, asked
/// in order, returning `Some` only when all three answer yes.
///
/// It is a free function taking every input as a PARAMETER for [`start_observe_server`]'s reason
/// verbatim: *"so this call site is the one place that decides whether a remote order-write surface
/// opens, and so that decision is visible in a diff."* One rung sharper here — what this one opens
/// is a KEY-MATERIAL surface.
///
/// # The three questions
///
/// 1. **Has the operator DECLARED a barrier?** `config.tradehub_account_admin`, three-valued
///    (unset/`off` ⇒ `None` and nothing else happens; `loopback`; `contained`). ⚠ An UNRECOGNISED
///    value is OFF and is LOGGED — a typo'd `loopbak` must never arm a credential surface, and an
///    operator who typed one must be told rather than left believing the barrier is up.
/// 2. **If `loopback`, does the BIND agree?** [`server::bind_exposure`] over the already-resolved
///    addresses — the same classification [`server::bind_decision`] uses one step earlier. A
///    non-loopback bind under this value REFUSES the capability at `error`, naming the key and the
///    address. ⚠ **That refusal is UNWAIVABLE by `tradehub_allow_public_bind`**, which is the shape
///    copied from `vike_datahub_client::bind`'s `BindDecision::RefuseUnauthenticated`: the
///    public-bind flag is consent to publish an ORDER surface, and it is not consent to publish a
///    key-material one. An operator who genuinely wants the account verbs on a wide bind says
///    `contained`, which is an assertion about a barrier OUTSIDE the process rather than permission
///    for there to be none.
/// 3. **Is there an ADMIN KEY?** `VIKE_TRADEHUB_ADMIN_KEY` in the node-key store — the
///    credential-is-the-gate idiom, and the one thing a settings write cannot mint. A declaration
///    with no key arms nothing and says so.
///
/// # Why `contained` states rather than second-guesses
///
/// `docs/decisions/0026-containerisation-additive-backend-image.md` ruled that whether a container's
/// port is reachable is decided OUTSIDE the container, at `docker run -p`, and is invisible to the
/// process — so this daemon may not infer containment, and may not second-guess a declaration of it
/// either. What it can do, and does, is say out loud exactly what has been asserted, so the
/// assertion is auditable in a way an inferred one would not be.
/// **Attach the admin key when — and only when — the account capability is armed, EXTENDING the
/// keys the control gate already decided rather than re-reading the store.**
///
/// A free function so the composition can be DRIVEN by a test. `start_observe_server` binds a
/// socket, so the two steps this joins (the control gate's zeroing, then this attachment) had no
/// reachable seam between them, and a test that merely replayed them would pass no matter what the
/// daemon did — an assertion that cannot fail for its stated reason.
///
/// ⚠ **The bug this shape exists to prevent.** This was `from_vars_with_admin(&node_vars)`, which
/// is `from_vars` + `with_admin`, and `from_vars` re-reads BOTH `OBSERVE_KEY_ENV` and
/// `CONTROL_KEY_ENV` out of the same map. On a box with `flags.tradehub_control` OFF the caller has
/// already replaced `keys` with a control-EMPTY pair; re-reading DISCARDED that and handed the
/// process back the live control key, so `run_handshake`'s
/// `scope == Scope::Write && !keys.has(Scope::Write)` refusal stopped refusing and a Control
/// peer reached `Request::Preview` — whose arm checks the scope and has no sink gate behind it.
/// Measured 2026-09-17.
///
/// `keys` is therefore consumed and returned: the only way to obtain the result is to hand over the
/// value the gate produced, so a future edit cannot quietly source a fresh one.
fn keys_for_account_capability(
    keys: vike_tradehub_client::auth::NodeKeys,
    capability_armed: bool,
    node_vars: &HashMap<String, String>,
) -> vike_tradehub_client::auth::NodeKeys {
    if !capability_armed {
        return keys;
    }
    keys.with_admin(
        node_vars
            .get(vike_tradehub_client::auth::ADMIN_KEY_ENV)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.as_bytes().to_vec())
            .unwrap_or_default(),
    )
}

/// Can this PROCESS create a file in `dir`? The question `ls -ld` cannot answer.
///
/// Mode bits and ownership are the wrong instrument here: under `ProtectSystem=strict` the
/// directory's metadata is unchanged and the read-only-ness lives in this process's own mount
/// namespace, so the only honest probe is to try. Used by [`account_admin_source`] to refuse at
/// ARMING time rather than at the operator's first credential write.
///
/// ⚠ **It can never touch the credential store.** `create_new(true)` refuses to open an existing
/// path, so this cannot truncate or clobber anything — and the name is a fixed sentinel that no
/// store file uses. It is removed immediately; a leftover means the removal itself failed, which is
/// reported through the same `Err` rather than swallowed, because a directory that accepts a
/// creation and refuses a removal is not a directory this surface should call writable.
fn probe_writable(dir: &std::path::Path) -> std::io::Result<()> {
    let probe = dir.join(".vike-write-probe");
    std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)?;
    std::fs::remove_file(&probe)
}

fn account_admin_source(
    declaration: Option<&str>,
    resolved: &[std::net::SocketAddr],
    node_store: &HashMap<String, String>,
    settings_dir: Option<&std::path::Path>,
) -> Option<server::AccountAdminSource> {
    use server::{AccountBarrier, BindExposure};

    let raw = declaration.map(str::trim).filter(|d| !d.is_empty() && *d != "off")?;
    let Some(barrier) = AccountBarrier::parse(Some(raw)) else {
        tracing::error!(
            value = raw,
            "config.tradehub_account_admin is not a value this daemon recognises, so \
             account administration is NOT armed. It must be \"loopback\" (the listener is on \
             loopback and reached through a tunnel — CHECKED against the bind) or \"contained\" \
             (the barrier is outside this process, e.g. a container port published to 127.0.0.1 — \
             asserted, never verified). An unrecognised value is treated as OFF rather than \
             refusing to start: a typo must not arm a credential surface, and a daemon that will \
             not start is worse than one trading with this one capability down"
        );
        return None;
    };

    // ⚠ THE ONE ASSERTION THIS PROCESS CAN MAKE, and it is why the key has three values rather than
    // two. `contained` reaches no branch here at all: there is nothing about it to check.
    if barrier == AccountBarrier::Loopback
        && let BindExposure::Public(exposed) = server::bind_exposure(resolved)
    {
        tracing::error!(
            %exposed,
            "config.tradehub_account_admin = \"loopback\" DECLARES that this node server is \
             reachable only through a tunnel, and its bind is NOT loopback — account \
             administration is NOT armed. This wire is PLAINTEXT and authenticates the connection \
             rather than each frame, so a credential value on it is readable by anyone on the \
             path. ⚠ `tradehub_allow_public_bind` does NOT waive this: that flag is consent to \
             publish an ORDER surface, not a key-material one. Either put the listener back on \
             127.0.0.1 and reach it with `ssh -L`, or — if the barrier is genuinely outside this \
             process (a container port published to 127.0.0.1, a private interface) — declare that \
             instead with `tradehub_account_admin = \"contained\"`, which this daemon cannot \
             verify and will say so on every start"
        );
        return None;
    }

    // The KEY. Credential-is-the-gate, and the ONE thing a settings write cannot mint: a Control
    // peer that wrote this declaration into a config row still could not authenticate for the scope.
    //
    // ⚠ **THIS KEY HAS NO MINTING VERB YET, and the message below says so rather than naming one
    // that cannot produce it.** `vike-cli backend setup` mints the observe/control PAIR from the
    // CSPRNG and writes exactly the two names in `vike_model::credential_keys::PLATFORM_KEYS`;
    // `VIKE_TRADEHUB_ADMIN_KEY` is outside that table, and it is outside the venue grid too, so
    // `vike-cli secrets set` refuses it as a name nothing reads. On a box whose node keys are still
    // in `node.env` an operator can put it there by hand; on a MIGRATED box (which is both of this
    // project's) the row lives in the settings database's `node_key` table and there is no
    // sanctioned writer for that name at all. **So the capability is not armable by a supported
    // command today**, and that is the follow-up this surface owes: the name joins `PLATFORM_KEYS`
    // (a 4-entry table `crates/vike-cli/tests/node_cli.rs` asserts BY INDEX against the client
    // crate's own constants), `platform_key_service` learns to route it, and `backend setup` mints
    // a third. None of that is guessed at here — it is a gated change of its own, and shipping a
    // half of it would file a node key under the wrong service, which is exactly what
    // `docs/decisions/0051` exists to prevent.
    let keys = vike_tradehub_client::auth::from_vars_with_admin(node_store);
    if !keys.as_ref().is_some_and(|k| k.has(Scope::Account)) {
        tracing::error!(
            barrier = barrier.as_str(),
            "config.tradehub_account_admin declares a barrier, but there is no {} in this \
             box's node-key store — account administration is NOT armed, and an absent credential \
             is the gate. ⚠ There is no verb that mints this key yet: `vike-cli backend setup` \
             mints the observe/control PAIR and this name is outside that table, and \
             `vike-cli secrets set` refuses it as a name nothing reads. It has to reach the \
             node-key store beside that pair by whatever put the pair there. Until it does, this \
             daemon stays exactly as it is — the capability absent, which is the safe direction",
            vike_tradehub_client::auth::ADMIN_KEY_ENV
        );
        return None;
    }

    // The SETTINGS DIRECTORY, from the BOOT's own answer — never a second walk. A daemon whose boot
    // resolved no project has no store to administer, and resolving one here would be the
    // `_from`-less resolver's failure wearing a new surface.
    let Some(settings_dir) = settings_dir else {
        tracing::error!(
            "config.tradehub_account_admin declares a barrier, but this daemon resolved NO \
             settings directory at boot (no project above its working directory) — there is no \
             store to administer, so account administration is NOT armed. Set $VIKE_SETTINGS_DIR, \
             or run the daemon from its project root"
        );
        return None;
    };

    // ⚠ **THE SANDBOX — the wall that made every write verb on this surface fail at the FIRST
    // write with a bare SQLite string, on the shipped unit, measured 2026-09-17.**
    //
    // `deploy/vike-tradehub.service` runs `ProtectSystem=strict` with ONE grant,
    // `ReadWritePaths=<project>/settings/state`. The settings database is
    // `<project>/settings/db/vike.db` (`vike_secrets::dotenv::db_path_in`) — OUTSIDE it. So inside
    // the daemon's own mount namespace the credential store is READ-ONLY, while `ls -ld` from a
    // shell shows the directory writable: the fact is only visible from inside. Reads are
    // unaffected, which is why arming looked fine and only a write discovered it.
    //
    // 0065 §3c itself leans on that read-only-ness as a SAFETY argument for the settings surface.
    // It is the same fact, and for THIS surface it is a wall rather than a guarantee — so it is
    // probed here, at arming time, in front of the operator who just declared the barrier, instead
    // of surfacing as an unactionable error on their first `SetCredential`.
    //
    // ⚠ **The shipped unit now GRANTS it** — `deploy/vike-tradehub.service` carries a second
    // `ReadWritePaths=` naming `<settings>/db`, on the owner's ruling of 2026-09-18 that the daemon
    // has to be able to write its own database. So on a CURRENT install this probe passes and
    // nothing below fires. It stays because the probe is cheap, because an install predating that
    // ruling still has one grant, and because the failure it catches is otherwise invisible: the
    // read-only-ness lives in this process's mount namespace, so `ls -ld` from a shell shows a
    // writable directory and every message that sends an operator there sends them nowhere.
    //
    // The grant is the sub-directory and not `settings/`: granting the root would hand this daemon
    // write access to the `policy.toml` that caps it. The unit's own block argues why `db` is safe
    // TODAY (its three settings tables are 0057 Phase 1 MIRRORS — the files still win) and names the
    // two records that expire the argument. Read it there; it is not restated here.
    let db = vike_secrets::db_path_in(settings_dir);
    if let Some(db_dir) = db.parent()
        && db_dir.is_dir()
        && let Err(e) = probe_writable(db_dir)
    {
        tracing::error!(
            error = %e,
            dir = %db_dir.display(),
            "config.tradehub_account_admin declares a barrier and the key is present, but \
             this daemon CANNOT WRITE the settings database's directory — account administration \
             is NOT armed, because every verb on that surface would fail at its first write. On \
             the shipped unit this is the sandbox, not the filesystem: ProtectSystem=strict grants \
             `settings/state` alone, so `settings/db` is read-only INSIDE this process even though \
             it looks writable from a shell. The cure is the drop-in shipped beside the unit — \
             `deploy/vike-tradehub-account-admin.conf`, which grants `settings/db` and nothing \
             else — installed with `systemctl edit vike-tradehub` and a restart. Do NOT widen the \
             grant to `settings/`: that hands this daemon write access to the settings database \
             that caps it"
        );
        return None;
    }

    match barrier {
        AccountBarrier::Loopback => tracing::warn!(
            "ACCOUNT ADMINISTRATION ARMED (config.tradehub_account_admin = \"loopback\") — \
             an ADMIN-authenticated peer may add, rename, deactivate and REMOVE accounts on this \
             box, and may WRITE CREDENTIAL VALUES into its store. The bind was checked and is \
             loopback; this wire is plaintext, so its confidentiality is the tunnel's"
        ),
        // ⚠ Says exactly what has been ASSERTED rather than what has been checked, because nothing
        // here was checked. 0026's refusal to infer containment is what makes this the honest line
        // and a `/.dockerenv` probe the dishonest one.
        AccountBarrier::Contained => tracing::warn!(
            "ACCOUNT ADMINISTRATION ARMED (config.tradehub_account_admin = \"contained\") — \
             an ADMIN-authenticated peer may add, rename, deactivate and REMOVE accounts on this \
             box, and may WRITE CREDENTIAL VALUES into its store. ⚠ THE BARRIER IS ASSERTED, NOT \
             VERIFIED: this daemon has NOT checked its bind, and cannot see what is in front of \
             it. You have declared that something outside this process makes this listener \
             unreachable. If that is not true, every credential written over this wire crosses it \
             in PLAINTEXT"
        ),
    }
    Some(server::AccountAdminSource { settings_dir: settings_dir.to_path_buf(), barrier })
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

/// The REAL process environment, swept ONCE at startup by [`resolve_settings`] — where the settings
/// directory (`VIKE_SETTINGS_DIR`) is named, when a deployment names it. See
/// [`workspace_credentials`].
///
/// It must be the REAL process env and not the credential map: a systemd unit's `Environment=` /
/// `EnvironmentFile=` line is the only channel an unattended daemon has, and a store cannot name
/// where it itself lives.
static PROCESS_ENV: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// The credential store, read ONCE per process — **the map AND the verdict about the store it came
/// from**. See [`workspace_credentials`] for why once, and [`workspace_credentials_checked`] for
/// why both.
static CREDENTIALS: std::sync::OnceLock<(
    HashMap<String, String>,
    vike_bridge_core::credentials::StoreHealth,
)> = std::sync::OnceLock::new();

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
    workspace_credentials_checked().0.clone()
}

/// [`workspace_credentials`], plus **whether the store it came from could be OPENED** — the one
/// fact the infallible loader deliberately swallows.
///
/// ⚠ **ONE read, two answers**, never two reads: the map and the verdict describe the same open of
/// the same store, and both are memoized together in [`CREDENTIALS`] so every later caller sees the
/// pair that actually happened.
///
/// # Why this daemon needs the verdict at all
///
/// `load_workspace_secrets_from_env` is documented INFALLIBLE, and the empty map it returns for an
/// unreadable store is byte-identical to the map an UNCONFIGURED box produces. Downstream, an empty
/// credential map is not an error — it IS the live gate, so every venue drops to paper and nothing
/// fails. On a daemon that is the whole defect: `Restart=on-failure` never fires, `OnFailure=` never
/// pages, and the ready banner reads `LIVE (venue=none)`, which
/// `deploy/vike-tradehub.service` documents as a legitimate answer ("gate on, nothing
/// armed"). A box that cannot read its keys and a box that has none become the same line of output.
/// The root `CLAUDE.md` forbids exactly that by name: *"those two must never look the same to an
/// operator, because a permissions bug wearing the 'not configured' answer looks exactly like a
/// correct fresh install while every venue drops to paper for a different reason."*
///
/// `vike-desktop`'s own `workspace_credentials_checked` is the precedent, for the same reason in a
/// different surface: a UI folding the empty map into `0 set` states a measured number it never
/// measured. This daemon's count is the BANNER — see [`ready_mode_line`].
///
/// ⚠ The verdict is NOT a refusal, and [`credential_store_health`]'s doc argues why against
/// `docs/decisions/0013-degrade-vs-refuse.md`.
fn workspace_credentials_checked()
-> &'static (HashMap<String, String>, vike_bridge_core::credentials::StoreHealth) {
    CREDENTIALS.get_or_init(|| {
        vike_bridge_core::credentials::load_workspace_secrets_from_env_checked(process_env())
    })
}

/// **Whether the credential store this daemon read could be OPENED at all**, from the ONE read
/// [`workspace_credentials_checked`] memoized.
///
/// # The disposition, argued against `docs/decisions/0013-degrade-vs-refuse.md`
///
/// An unreadable store **does not stop this daemon**, and the record's own four questions are why:
///
/// 1. *Protection or capability?* 0013 names "a set of credentials" a CAPABILITY in as many words,
///    and its table already files this exact case — *credential store unreadable → `error!`, empty
///    map, all paper* — as a CONFORMING degrade.
/// 2. *Did the operator ask for it?* Nothing is set-but-unhonoured. A dead writer's rollback
///    journal is an accident of a crash, not a configuration anybody wrote.
/// 3. *Does the degrade reduce authority, or redirect it?* It reduces it to the floor: with no
///    credentials, `vike_mount::make_engine` builds a paper client for every venue, opens no socket
///    and signs nothing. This is the strongest possible reduction, not a redirection.
/// 4. *Would the failure be visible where the operator already looks?* ⚠ **THIS is the question
///    that was failing, and it is the only thing fixed here.** The operator's stated authority on
///    paper-vs-live is the ready banner (`docs/ops/tradehub-the CI box.md` and
///    `deploy/vike-tradehub.service` both say so), and it said `LIVE (venue=none)`.
///
/// So the defect was never the degrade — it was a FAULT wearing the capability's clothes. The cure
/// is to take the clothes off, not to convert the degrade into a refusal.
///
/// ⚠ **Refusing was considered and is WORSE, and 0013's own *What would reopen this* names the
/// shape:** *"an unattended-daemon deployment where a startup refusal is worse than the
/// misconfiguration it prevents — a node that will not start cannot flatten a position either."*
/// This is that deployment. A refusal here is not a one-off stop but a RESTART LOOP: the daemon
/// opens the store read-only, its settings directory is read-only in its own mount namespace, and
/// so **nothing inside the unit can replay the journal** — every restart meets the identical state.
/// `Restart=on-failure` would then cycle the process for as long as the operator is asleep, and
/// each cycle tears down a daemon that may be holding resting orders and positions, re-running the
/// teardown's cancel sweep with NO credentials to cancel them WITH. An all-paper daemon that
/// announces itself as broken keeps its control surface reachable; a crash-looping one answers
/// nothing and can neither report nor flatten.
fn credential_store_health() -> &'static vike_bridge_core::credentials::StoreHealth {
    &workspace_credentials_checked().1
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
/// of the policy the live mount projects onto `vike_mount::MountPolicy`. **Exactly one load DECIDES
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
        // ⚠ **REFUSE on an unsound SEAL.** This root mounts venues and signs orders, so it is the
        // one disposition that reverses `docs/decisions/0013` for that state — the argument is at
        // `vike_boot::SettingsLoad::LoadAndRefuseUnsoundSeal`, and the short form is that what
        // degrades here is the CEILING rather than a capability: an adopted box with unsound rows
        // resolves `max_notional_per_order` to `None`, which is no size cap at all. It refuses
        // including on a box it believes is paper, because `flags.tradehub_live` and
        // `policy.venues` are themselves in the layer that failed, so *am I live?* is not a
        // question it may answer from that layer's own faults.
        //
        // ⚠ **This comment READ "REFUSE on an unreadable settings store" while the arm was
        // `SettingsLoad::Load`, which refuses nothing** — an unreadable store is a MARK
        // (`Settings::store_refusal`) and this root started anyway. That half is still a
        // degrade-and-announce, deliberately: `vike_secrets::Backend` answers for CREDENTIALS on
        // the same probe, so an unopenable store means an empty credential map, which IS the live
        // gate — the box is all-paper and the ceiling cannot be reached. The SEAL is different
        // because the store opened FINE and said something illegal, so the credentials loaded and
        // the venues armed. Two marks, two dispositions, and the difference is now in the code
        // rather than only in a sentence.
        settings: vike_boot::SettingsLoad::LoadAndRefuseUnsoundSeal,
        // the daemon mounts on these ceilings; its unit grants `settings/db` read-write, so it
        // migrates a pre-0095 store itself and refuses one it cannot.
        ceilings: vike_boot::Ceilings::Interpret { now_ms: vike_model::now_ms() },

        // ...and the credential file may not ARM REAL MONEY. `secrets.env` is plaintext and parsed
        // last-wins, so appending ONE line to it — with no read access at all — would otherwise be
        // enough to flip this daemon onto a live venue. `vike-boot` runs the refusal at step 0,
        // before the settings load and a long way before every mount, precisely because this
        // process signs real orders. It does NOT change which sources arm a venue (decision 0095:
        // the ceiling alone, for binance/bybit/okx/hyperliquid); it makes an arming credential file
        // STOP the process rather than run it. See `vike_config::arming`.
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
) -> Option<vike_bridge_core::poller::StopHandle> {
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
/// (`CoreSnapshot::build` binds `let acc = &engine.account`, and `crate::wired_markets::WIRED_MARKETS` lists
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

#[path = "tradehub_cli_tests.rs"]
#[cfg(test)]
mod tradehub_cli_tests;
