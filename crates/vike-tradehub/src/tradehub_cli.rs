//! `vike-tradehub` — the live trading daemon's command line, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/main.rs`'s body until the multicall merge. `main` became [`run`], taking
//! the environment, the working directory and argv as parameters.
//!
//! ⚠ **THE ORDER OF THE FIRST STATEMENTS IS THE PROPERTY, NOT A STYLE.**
//! `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs`'s
//! `the_tradehub_handler_is_installed_before_anything_can_place_an_order` checks BY POSITION IN
//! THIS FILE that `install_handlers` precedes every mount call. Between a mount returning and the
//! handler installing, the maker is already folding ticks and may already hold RESTING ORDERS at
//! the venue while SIGTERM still carries the OS default disposition — the process would die where
//! it stands, no teardown, no cancel sweep, book abandoned. Do not reorder the opening of [`run`].
//!
//! ⚠ The environment reaches its few readers (the settings directory, the credential and node-key
//! variables, `RUST_LOG`) through [`PROCESS_ENV`], seeded ONCE by [`resolve_settings`] from the
//! caller's map rather than swept again here. That is what keeps every one of those reads seeing
//! the SAME map the boot consumed. No settings key is among them: each is a row (decision 0111).
//!
//! Everything below is the binary's own documentation, unchanged.
//! Runs a strategy on a server with NO GUI, controlled by the ACTIVE daemon-profile row of the
//! settings database (its body is TOML, lowered through `DaemonProfile::from_toml_str`) +
//! newline-JSON stdio, and survives an SSH disconnect (it is a systemd service, detached from the login session — steal R7:
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
//! The `flags.tradehub_live` FLAG instead mounts the SAME strategy on the REAL wired-market [`vike_mount::build_node`] core
//! with per-venue credential-gated exec AND wires the venue's own
//! LIVE feed. OFF (the default) is byte-identical to the pure-paper daemon. The path is defended by
//! FIVE gates: (1) the `tradehub_live` master gate; (2) per-venue creds in the credential store
//! (absent creds keep that venue paper even with the gate on); (3) each venue's own network gate —
//! for binance/bybit/okx/hyperliquid the ACCOUNT's own tier IS the network (decisions 0095 and
//! 0119: `live` means mainnet, `demo` the demo/testnet); (4) the daemon's own venue ALLOW-LIST
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
//! `vike-app` did while the desktop mounted venues — same [`crate::reconcile_config`] builder over
//! the `config.reconcile_*` rows, same `node.recon_clients`/`node.recon_trigger`, same per-venue
//! `ReconConfig`. This daemon is the only reconciling root now.
//!
//! ⚠ **The gate is ON for a live mount, and that is the whole point of this section.** A live
//! daemon that restarted and never asked the venue what it held would be trading against a BELIEF.
//! The decision is
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
//! unless the operator writes a `config.reconcile_policy` row — the default is
//! [`crate::reconcile_config::parse_policy`]'s, and the pairing is load-bearing rather than a
//! preference: under `hybrid` a default-on daemon would auto-apply `PositionDrift` at the
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
//! caller), so reconcile mounts with the feed-status map `LiveFeeds::recon_feed_statuses` builds
//! from the feeds this mount built (CEX venues only, and only while one lane writes the status
//! string); every other reconciled venue reads [`vike_core::ReconHealth::Healthy`] and is never
//! health-blocked (a pass against a briefly-down venue fails soft, whereas a wrongly-suppressed
//! pass can stay suppressed).
//!
//! ## Optional observe + control server (PR-11/12/13)
//! When `config.tradehub_addr` is set, the daemon also starts an authenticated node
//! server (`crate::server` + `publish`) so a laptop GUI (`vike-desktop`) can WATCH this
//! live node's snapshots over an SSH tunnel — and, when the `tradehub_control` FLAG is on and a
//! `VIKE_TRADEHUB_CONTROL_KEY` is set, TRADE it (place/cancel orders over a `Scope::Write`
//! connection). When `config.datahub_advertise_addr` is ALSO set, every `Welcome.features` carries `datahub=<addr>` — the REQ-2 advertisement of
//! the datahub this backend fronts, so a client configures one address (advertisement, never
//! proxying). OFF by default: with no address the daemon is byte-identical to the pure-stdio PR-9
//! daemon; absent the control gate/key the server is read-only (an Observe peer's command is refused).
//! The publisher NEVER touches the vike-core fold (it reads only the arc-swap snapshot cell), so the
//! p99 latency gate is unaffected. Control is DOUBLE-GATED (key + `CommandSink`) and, at the server
//! edge, notional/rate-limited (the notional ceiling from
//! `policy.max_notional_per_order`, the rate from `config.tradehub_control_rate` — both resolved ONCE at
//! startup and passed into `serve` as a `ControlLimitsConfig`, audit F13) as defense-in-depth
//! beyond the core `RiskGate` every remote order still passes through. ⚠ The ceiling was
//! `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` until Phase 5 of the settings-unification design removed it;
//! a daemon that still finds that variable set REFUSES TO START (see `resolve_settings`).
//!
//! ## The settings DATABASE is the standing configuration (decision 0086; settings-unification Phase 6c/6d)
//! `resolve_settings` loads `<project>/settings` (its `db/vike.db` rows) exactly once at startup —
//! BEFORE the log subscriber,
//! because the log destination and both log levels are themselves settings — and returns the whole
//! `vike_config::Settings`. These of its fields reach this daemon — no count is written down,
//! because `vike_config::CONSUMPTION` is the machine-checked list and a prose number beside it only
//! ever rots — each a row or its default (`the settings database > default`, resolved inside the
//! loader; no environment variable reaches one, decision 0111): `config.log_dir` plus
//! `preferences.log_level`/`log_file_level` build the `vike_log::LogConfig`; `config.tradehub_addr`
//! and `flags.tradehub_control` gate the node server; `flags.telegram_control` the Telegram channel;
//! `flags.tradehub_live` the live mount; `flags.tradehub_record` the recorder; `flags.reconcile` the
//! reconciliation engine; `flags.oco_cancel_sibling_on_dead_exit` the bracket-sibling behaviour on
//! BOTH mounts. `vike_config::CONSUMPTION` is the machine-checked record of that list
//! (`crates/vike-config/tests/settings_are_consumed.rs`) AND of every setting this daemon still does
//! not read — a settings key that validates and is displayed as effective while nothing reads it is
//! worse than an unimplemented feature, and that gate is what makes it un-shippable.
//!
//! The POLICY half comes from the same one load: `live_mount` projects it onto
//! `vike_mount::MountPolicy` and threads it into every `vike_mount::make_engine` call `build_node`
//! makes, and so into every venue's mount.
//!
//! ⚠ **The `account` table's `tier` + `active` decide whether this daemon trades at all**
//! (decision 0119). Each account trades at its own `tier` (`paper` / `demo` / `live`) while
//! `active = 1`, read at the top of `make_engine` above the credential read — so **no ACTIVE
//! non-paper `account` row on this box means an ALL-PAPER daemon**, whatever the credential store
//! holds, and two ACTIVE non-paper tiers for one account mount that account PAPER until one is
//! deactivated. That is deliberate: this daemon's own startup log once printed nine live
//! authenticated venues for a run profile that named one, because credential presence was the
//! only gate. ⚠ Writing a credential mints an ACTIVE row at that key's tier when none exists, so a
//! stored key set arms at the next restart; `vike-cli secrets accounts` lists the rows and
//! `vike-cli secrets account deactivate --id <N>` is the per-account off switch.
//!
//! `market_slippage` is the other field that binds a venue arm — the aggression band a venue with
//! NO NATIVE MARKET ORDER prices its emulated market (and tripped stop-MARKET) orders at.
//! Hyperliquid is the only such venue on the roster and this daemon's primary live venue, so that
//! band is the worst price every one of its market intents is allowed to reach. For THAT field, no
//! `policy.market_slippage` row is byte-identical: `Policy::default()` carries `None` and
//! hyperliquid keeps its own compiled-in literal.
//!
//! ## Optional TELEGRAM control channel (behind the `telegram` FEATURE, then FOUR runtime gates)
//! Compiled ONLY under the crate's off-by-default `telegram` Cargo feature — a default daemon does
//! not contain this code, so no runtime misconfiguration (a stale systemd unit, an inherited
//! environment) can reach it. With the feature on: the `flags.tradehub_control` **and**
//! `flags.telegram_control` FLAGS **and** a
//! `VIKE_TELEGRAM_BOT_TOKEN` **and** a non-empty
//! `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` in the credential store mount `crate::telegram` — a
//! `getUpdates` long-poll (outbound HTTPS only, NO inbound listener) that lets an allowlisted chat
//! drive this node from a phone. Absent ANY of the four ⇒ nothing is constructed: no thread, no bot
//! token read into memory, no network call (the two control flags are checked purely FIRST, and the
//! credential-store loader is passed as a FUNCTION, so the OFF path never even opens it).
//!
//! ⚠ **Accepted risk, stated once:** with this on, the bot token plus membership in an allowlisted
//! chat is sufficient to place REAL orders — Telegram is a third party in an order-origination path.
//! Every write is preview + `/confirm`-gated (single-use token, 60 s, bound to the exact command and
//! chat), an unlisted chat is ignored and NEVER answered, and every confirmed command goes through
//! the SAME [`crate::server::control::accept_command`] the TCP control path uses — the same
//! `ControlLimits` notional/rate caps, the same audit record, and the same core `RiskGate`.
//!
//! ## Optional alerting (the rules FILE is the gate, OFF by default)
//! When `<project>/settings/state/alerts.json` is a rules file with at least one ENABLED rule, the daemon mounts `vike_alerting`'s `AlertEngine`
//! ([`crate::alerts`]) so alerts keep firing on an unattended node instead of dying with a
//! GUI window. It is a strict OFF-FOLD consumer: the engine is MOVED onto the existing periodic
//! snapshot thread and fed the same lossy `arc-swap` read the JSON summary already does, so the
//! `p99 < 10µs` core gate is untouched. Delivery is a `tracing` record always (stdout stays
//! protocol-only) plus any Telegram/webhook target the credential store configures. Absent file,
//! zero rules, or all-disabled rules ⇒ nothing is constructed (no engine, no sink, no resolved
//! webhook) and the daemon is byte-identical to before. Only SNAPSHOT-driven triggers
//! (`Price`/`Drawdown`/`ReconAlert`) have a source here today; `Fill`/`OrderRejected`/`Indicator`/
//! `Feed`/`FillRateBreaker`/`PolymarketResolution` rules load but cannot fire — see
//! [`crate::alerts`]'s module doc for why each is unfed.
//!
//! ## The lifecycle, in one place
//! 0. [`vike_ops::stop::install_handlers`] — the FIRST statement of [`run`], before argv and before
//!    the settings load. Until it returns, SIGTERM carries the OS default disposition, so every
//!    later step of this list is a window in which a service stop runs no teardown at all.
//! 1. [`vike_log::init`] (the daemon HOLDS the returned guards for the whole process), then the
//!    STARTUP DISCLOSURE — `vike_buildinfo::version_line` (which commit this binary is),
//!    `vike_config::boot_lines` (the settings directory that answered, each file present or absent,
//!    the resolved ceilings, and whether a credential store sits beside them) — and
//!    `log_handler_outcome`, reporting what step 0 did. All four are the first moment there is a
//!    subscriber to say anything through, which is why none of them can happen earlier.
//! 2. Resolve the ACTIVE daemon-profile row (`load_daemon_profile`; `--config` is RETIRED and
//!    ignored) into a [`DaemonProfile`](crate::config::DaemonProfile) → a
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
//! `flags.cancel_orders_on_shutdown` makes the bounded teardown cancel
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
//! `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` pins the wiring that landed, so this paragraph
//! cannot outlive it.
//!
//! The phase functions `run` calls live in `tradehub_cli/` children (`fold_flags_into_vars` in
//! `flags.rs`, `mounts_wire_params` in `mount_rows.rs`, the rest named by the `use` lines below);
//! `run`, `declare_polymarket_egress` and `mount_paper` stay in this file because text gates read
//! them here. The live mount — `LiveMount`, [`live_mount`] and [`live_mount_with`] — is
//! `tradehub_cli/live_mount.rs`, which shares this file's `use` block; the gates that follow a value
//! from `run` into it read the two files together, this one first.
//!
//! [`live_mount`]: live_mount::live_mount
//! [`live_mount_with`]: live_mount::live_mount_with

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::reconcile_config;
use vike_core::CoreHandle;
use vike_ops::shutdown::run_with_deadline;
use vike_ops::stop::{self, StopSignal};
// LIVE path only: the live feed is wired through a `LiveDataSink` onto the core lanes. `DataClient`
// itself is no longer named here — Task 3 moved every site that named it directly (`FeedCtors`,
// `LiveFeeds`'s `Oanda`/`Deribit` variants) into `feeds.rs`.
use vike_data::LiveDataSink;
#[cfg(test)]
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
use crate::node;
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
// this file's own `#[cfg(test)] mod tests` (and `tests::feed_splice`, gated identically) —
// gated below accordingly rather than left unconditional and reported unused by a default-build
// clippy run. `check_ctrader_intervals`/`ResolvedMount`/`VenuePlan` keep a REAL production call
// site here (`live_mount_with`'s interval gate and the mount-resolution loop) and stay
// unconditional. `CexArming`/`cex_mainnet_enabled` had no real
// use site left ANYWHERE — production or test — and were deleted outright rather than gated.
#[cfg(test)]
use crate::CexVenue;
#[cfg(test)]
use crate::venue_arming::arming::EXEC_PAPER;
#[cfg(test)]
use crate::venue_arming::arming::{EXEC_LIVE, EXEC_LIVE_MULTI_ACCOUNT, EXEC_OTHER_ACCOUNT_LIVE};
#[cfg(test)]
use crate::venue_arming::arming::{alpaca_arming, cex_arming, ctrader_arming, data_only_arming};
#[cfg(test)]
use crate::venue_arming::arming::{deribit_arming, exec_badge, ig_arming, oanda_arming};
#[cfg(test)]
use crate::venue_arming::arming::{other_live_accounts, with_other_live_accounts};
use crate::venue_arming::deadman::{
    MountLinkDisclosure, deadman_config_from_policy, link_deadman_arming_report,
    link_deadman_config_from_policy, mount_link_disclosure,
};
use crate::venue_arming::warn_deadman_absent;
use crate::venue_plan::check_ctrader_intervals;
#[cfg(test)]
use crate::venue_plan::wired_symbol_for;
#[cfg(test)]
use crate::venue_plan::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{ResolvedMount, VenuePlan};

mod args;
mod banner;
pub(crate) mod flags;
mod lifecycle;
mod live_mount;
mod mount_rows;
mod paper;
mod paths;
mod profile;
pub(crate) mod settings;
mod stdio;
#[cfg(feature = "telegram")]
mod telegram_boot;

#[cfg(test)]
use args::{Args, parse_args_from};
use args::{Parsed, USAGE, parse_args};
#[cfg(test)]
use banner::{NO_VENUE_ARMED, STORE_UNREADABLE_BANNER, ready_mode_line};
use banner::{node_identity, ready_banner};
use flags::fold_flags_into_vars;
#[cfg(test)]
use flags::{FoldTier, flag_wire_value, folded_flag_rows};
use lifecycle::{log_handler_outcome, report_teardown, spawn_summary_thread};
use live_mount::live_mount;
#[cfg(test)]
use live_mount::live_mount_with;
#[cfg(test)]
use mount_rows::{WireMountSeed, wire_mount_rows};
use paper::wire_mount_seeds_for;
use paper::{announce_paper_mount, distinct_mount_venues, refuse_data_only_on_paper};
use paths::{log_dir, maybe_mount_alerts, state_dir, strategy_state_dir};
use profile::{load_daemon_profile, paper_risk_budget, primary_mount_identity};
use profile::{resolve_mounts, resolve_run_profile};
#[cfg(test)]
use settings::{CREDENTIALS, credential_store_health};
use settings::{resolve_settings, settings_warning_lines, workspace_credentials};
use stdio::control_loop;
#[cfg(feature = "telegram")]
use telegram_boot::maybe_start_telegram;

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
    // are settings, so a subscriber built before the load could only ever honour the defaults —
    // which is exactly the defect this ordering fixes; and a stale retired variable must stop a
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
    // THREE layers of this config are settings rows. Only two environment variables still reach
    // `vike_log::init`, and neither is a settings key: `RUST_LOG` (the console-only developer
    // switch) and `$VIKE_LOG_DIR` (the logger's own bootstrap directory, read before any database
    // could be).
    //
    // `project_dir` is the DEFAULT log directory: `<state root>/logs`, the state directory this
    // daemon already resolves for `alerts.json` (`<project>/settings/state`). It sits BELOW
    // `config.log_dir` — a value a human wrote beats one the program derived for itself — and above
    // vike-log's `<exe_dir>/logs` last resort.
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
        // it, because which records a BINARY may not lose is a binary's decision. Without it a
        // file level of `warn` swallowed every accepted control command on the live box — see
        // that constant's doc for the measurement.
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
    // (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet that may shrink
    // and never grow), so the override reaches rung 2 — `<project>/settings/state/HALT` — only as a
    // PARAMETER from here. Without it the sentinel's default was decided by the WORKING DIRECTORY
    // even under the override; on the CI box the two happen to name the same project, so the defect was
    // LATENT there rather than live, and `deploy/vike-tradehub.service`'s own comment says the
    // variable is there to make the answer "independent of WorkingDirectory".
    //
    // The shipped units' `ReadWritePaths=` grants `<project>/settings/state`, which is this rung.
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
    // set of `policy` rows from a settings store this process failed to find (the two produce
    // identical behaviour, which is the point, and therefore identical silence). On the CI box the walk answered with an
    // unrelated directory and this daemon ran with no policy and NO CREDENTIALS, every venue
    // silently on paper.
    //
    // ⚠ This REPLACES the four-field line that stood here. That line named the two policy ceilings
    // and the log destination, which is the part of the answer this daemon happens to consume — and
    // the failure it exists for is the one where NOTHING was consumed, where the useful fact is the
    // DIRECTORY and whether a credential store sits in it. `Booted::boot_lines`
    // (`vike_config::boot_lines`) renders all of it, the same rows `vike-cli config show` prints,
    // from the settings directory this process really loaded from —
    // `vike-boot` hands the renderer its OWN walk's answer, so nothing here can walk a second time.
    // It NEVER opens the credential store — see that module's doc — so no key name and no key value
    // can reach this log.
    for line in &booted.boot_lines {
        // ⚠ Not all INFO: a `credential store: ⚠` finding is a WARN line
        // (`vike_config::boot_line_level`).
        match vike_config::boot_line_level(line) {
            vike_config::BootLineLevel::Warn => tracing::warn!("{line}"),
            vike_config::BootLineLevel::Info => tracing::info!("{line}"),
        }
    }
    // The LIVE mount below takes the venue-facing projection (`vike_mount::MountPolicy`) from this
    // value, while the control-server ceiling rides the `POLICY_MAX_NOTIONAL` OnceLock
    // `resolve_settings` also set — ONE load, two consumers, no chance of the two disagreeing about
    // what this machine's `policy` rows say.
    let policy = settings.policy.clone();

    // …and the DURABLE anchor for those same ceilings — one JSONL line per start in
    // `<state root>/changes/`, actor origin `boot`.
    //
    // The disclosure emitted directly above is the console/journald copy and it is not a record a
    // week later: `vike_log::DEFAULT_MAX_LOG_FILES` prunes the rolling file, and a
    // `preferences.log_file_level` row of `warn` silences the `info` layer the boot lines ride
    // outright. `vike_model::change_journal`'s module doc carries the
    // measurement from this very daemon's live log file; the anchor survives both.
    //
    // ⚠ **A BRACKET, not a detector.** Nothing in this daemon observes a hand edit of a settings
    // row's value in `<project>/settings/db/vike.db` — there is no file watcher in the tree and
    // this process does not notice the edit at all. Two consecutive anchors that DISAGREE prove something changed
    // between them, and that is the whole claim; `vike_boot::journal_boot_settings` carries the
    // argument and the rate arithmetic.
    //
    // It is [`state_dir`]: the anchor belongs in the same STATE tree as the rolling log
    // ([`log_dir`]), `alerts.json` and the telegram ledger — the shipped units' `ReadWritePaths=`
    // grants exactly that tree. `None` (no project) writes NOTHING rather than inventing a location.
    //
    // Placed HERE rather than in [`resolve_settings`] for the reason `declare_project_state_dir`
    // above is: the write can fail, and this is the first point at which a subscriber exists to say
    // so through. That function runs before one does and returns everything it has to say as data.
    if let Some(Err(e)) = vike_boot::journal_boot_settings(
        state_dir().as_deref(),
        &policy,
        env!("CARGO_PKG_VERSION"),
        vike_model::now_ms(),
    ) {
        tracing::warn!("the boot anchor was not journalled: {e}");
    }

    // ⚠ WHICH PROFILE IS LIVE — since decision 0086 ("settings live only in the database") verdict
    // 1, the ACTIVE ROW IS THE ONLY SOURCE. [`load_daemon_profile`] carries the whole argument and
    // every refusal; an `Err` is the exit code the inline block used to `return`.
    let (store_profiles, active_daemon, profile) = match load_daemon_profile(&booted, &args) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    // Per-mount resolution (split-plane I10): the historical single-mount profile is ONE row
    // (`mount_rows` returns the profile itself), a `[[mounts]]` profile is N — and every row runs
    // the SAME lowering + resolve a single-mount profile always has, so the two spellings cannot
    // disagree about what one mount means.
    let multi = !profile.mounts.is_empty();
    let resolved = match resolve_mounts(&profile, multi) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    // The PRIMARY mount's identity — its config, the strategy name to echo and the params line to
    // echo. See [`primary_mount_identity`] for why the primary is a declared fact and not
    // `resolved[0]`.
    let (cfg, strategy_name, strategy_params) = primary_mount_identity(&profile, &resolved, multi);

    // The WAL's directory rung — the `config.journal_dir` and `config.journal_snapshot_every` rows —
    // resolved ONCE for the whole process: the paper and live mounts' `CoreConfig::journal` and the
    // journal rung's disclosure below all read it, and two resolutions could name two directories.
    let journal_rung = crate::profile_rows::JournalRung {
        dir: settings.config.journal_dir.clone(),
        snapshot_every: settings.config.journal_snapshot_every.map(u64::from),
    };
    let run_profile = match resolve_run_profile(&store_profiles, &journal_rung) {
        Ok(run_profile) => run_profile,
        Err(code) => return code,
    };
    // THE WAL, resolved ONCE from the two facts above: the active run row's `[sinks].journal` when
    // a run profile is in force, else the directory rung. The paper core and the node's wire
    // tearsheet verb both take THIS value, so the reply can never fold a different journal than
    // the core writes (the live arm computes the same function over the same two inputs).
    let journal = crate::profile_rows::journal_config_for(run_profile.as_ref(), &journal_rung);
    let paper_risk_limits = match paper_risk_budget(&run_profile) {
        Ok(limits) => limits,
        Err(code) => return code,
    };

    // The LIVE master gate (safety gate #1) — the `flags.tradehub_live` row (resolved database >
    // default, inside the loader). OFF (the default, and the reading of an absent row) ⇒ the PAPER mount below,
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
    let mount_venues = distinct_mount_venues(&resolved);

    // The per-mount wire rows the `StrategyStatus` read verb answers with (split-plane B4 → I10:
    // that Vec was designed for exactly this), captured HERE as SEEDS and completed into rows
    // AFTER the mount. Two reasons it cannot simply be built here: both mount arms below MOVE
    // `resolved`, and each row's `live` is a per-VENUE ARMING FACT that does not exist until the
    // mount has run. See `wire_mounts` below the mount arms.
    let wire_mount_seeds = wire_mount_seeds_for(&resolved, multi, &strategy_name, &strategy_params);

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
            "the live gate is ON (the `flags.tradehub_live` row) — mounting the \
             LIVE build_node core; real orders MAY be placed on any venue with creds in the \
             credential store and an ACTIVE account row (each account's network is its own \
             tier: `live` means mainnet)"
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
                tracing::error!(
                    "flags.tradehub_live is on but the run profile is not live-mode: {e}"
                );
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
            &journal_rung,
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
        if let Err(code) = refuse_data_only_on_paper(&resolved) {
            return code;
        }
        announce_paper_mount(
            &mount_venues,
            &cfg,
            &resolved,
            &strategy_name,
            &strategy_params,
            &profile,
        );
        let handle = mount_paper(resolved, multi, paper_risk_limits, settings, journal.clone());
        // No live client is constructed on this arm — no venue, no credential, no exception — so
        // the arming record is EMPTY, and every paper-vs-live report derived from it below reads
        // paper without a second boolean having to agree with this one.
        (handle, None, std::collections::HashSet::new())
    };

    // ⚠ THE PAPER-VS-LIVE BANNER names the set that is ACTUALLY ARMED — never the set the profile
    // mounts; [`ready_banner`] carries the argument and the measured the CI box startup it replaces. The
    // same arming record decides each wire row's `live`, so the banner and the `StrategyStatus`
    // verb cannot disagree about one startup.
    let (mode, wire_mounts) = ready_banner(live, &live_venues, wire_mount_seeds);

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
    // BOTH inputs are settings rows — `config.tradehub_addr` and `flags.tradehub_control`. They
    // arrive as PARAMETERS rather than being read inside the function so this call site is the one
    // place that decides whether a remote order-write surface opens, and so that decision is
    // visible in a diff. The public-bind consent is the row ORed with `--allow-public-bind`, the
    // argument a container's entrypoint passes (see [`Args::allow_public_bind`]).
    let identity = node_identity(settings, &active_daemon, &strategy_name, &strategy_params, live);
    // The HOT-APPLY seam (REQ-7 v2): the server end goes into `SettingsShowSource.hot` below;
    // the ticker end is moved onto the summary thread, which drains it on every wake — so every
    // runtime apply executes on that ONE existing off-fold thread, never on a connection thread.
    let (hot_handle, hot_ticker) = hot_reload::hot_apply_channel();
    let observe = node::start_observe_server(
        &handle,
        identity,
        wire_mounts,
        settings.config.tradehub_addr.as_deref(),
        settings.flags.tradehub_control,
        settings.flags.tradehub_allow_public_bind || args.allow_public_bind,
        // The REQ-7 settings source, built HERE because this binary owns both of its facts: the
        // settings DIRECTORY the one boot walk resolved (so the wire can never describe a
        // different project than the one that is running), and the hot-apply seam — whose `Some`
        // is what makes `restart_required: false` REACHABLE at all: the write arm consults it
        // only for a key the classification calls hot-safe, and waits for the summary tick's
        // verdict before answering.
        server::settings::SettingsShowSource {
            settings_dir: booted.settings_dir.clone(),
            hot: Some(hot_handle),
            // The ONE resolved WAL (see `journal` above), so the wire tearsheet folds the journal
            // the core writes — the active run row's, or `config.journal_dir`'s.
            journal,
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
    // summary thread below is byte-identical to the pre-alerting daemon. The engine carries the
    // per-rule edge/latch/cooldown state and is MUTATED (off-fold) on each tick after the move into
    // that thread — which happens in [`spawn_summary_thread`], so the `mut` is on its parameter.
    let alerts = maybe_mount_alerts();

    // Periodic snapshot SUMMARY to stdout (the protocol/result surface). A LOSSY reader off the
    // arc-swap cell (`snapshot_cell`) — it never touches the core fold; its own thread so the stdin
    // control loop can block on the main thread. Sleeps in small increments so `stop` is prompt.
    // It is ALSO the alerting engine's one input tick (see `maybe_mount_alerts`): the engine is
    // MOVED onto this thread, so every rule evaluation happens off-fold, on the same lossy read the
    // summary line already does — never inside the core.
    let (summary_stop, summary_handle) =
        spawn_summary_thread(&handle, &cfg, &profile, log_reload, &booted, hot_ticker, alerts);

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
    // (`crates/vike-alerting/src/delivery/webhook.rs`'s `UreqTransport::new`). A stop landing while an
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
        // A `join` Err is a summary-thread panic its hook already reported; teardown goes on.
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
    report_teardown(outcome, teardown_took, deadline)
}

/// **Paper-mount phase — build and spawn the PAPER core** (the production runtime over the paper
/// exchange), then replay the runtime-mount topology sidecar through it. Every input is a fact the
/// caller resolved once: this function reads no settings of its own.
fn mount_paper(
    mut resolved: Vec<ResolvedMount>,
    multi: bool,
    paper_risk_limits: vike_model::RiskLimits,
    settings: &vike_config::Settings,
    journal: Option<vike_core::JournalConfig>,
) -> CoreHandle {
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
        // Order ownership across a restart (decision 0113): the same file in the same directory
        // as the live arm's, so a rehearsal's resting orders find their mount after a restart.
        order_owners: strategy_state_dir()
            .map(|d| vike_core::order_owners::OrderOwnerLog::in_state_dir(&d)),
        // The WAL, resolved ONCE for this process by `run` — the same answer the LIVE arm's
        // `CoreConfig::journal` gets (`crate::profile_rows::journal_config_for` over the same
        // active run row and the same `journal_rung`). The builder resolves nothing of its own,
        // so a rehearsal cannot journal somewhere else (or nowhere) than the live mount would.
        journal,
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
    handle
}

/// **The daemon's reconcile settings** — the `flags.reconcile_*` and `config.reconcile_*` rows its
/// ONE boot resolved, read into the one value [`crate::reconcile_config::build_recon_config`] takes.
///
/// Rows and nothing else (decision 0111): the `VIKE_RECONCILE_*` variables this used to start from
/// are refused at startup, so there is no second source for a pass to disagree with. It replaced
/// `daemon_recon_env`, which built the same knobs as a map starting from the process environment and
/// folded the rows in only where a variable was absent. The quarantine-first default that map
/// carried is [`crate::reconcile_config::parse_policy`]'s own now: no row is `quarantine`.
///
/// `flags` is the value the gate in `live_mount_with` reads too, handed down rather than re-read, so
/// the verdict and the policy come from one resolution.
fn daemon_recon_settings(
    flags: vike_config::Flags,
    config: &vike_config::Config,
) -> reconcile_config::ReconSettings {
    reconcile_config::ReconSettings {
        policy: config.reconcile_policy.clone(),
        interval_ms: config.reconcile_interval_ms,
        audit_ms: config.reconcile_audit_ms,
        lookback_ms: config.reconcile_lookback_ms,
        startup_delay_ms: config.reconcile_startup_delay_ms,
        balance_tol_abs: config.reconcile_balance_tol_abs,
        balance_tol_rel: config.reconcile_balance_tol_rel,
        generate_missing: flags.reconcile_generate_missing,
        balance: flags.reconcile_balance,
    }
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

/// The REAL process environment, swept ONCE at startup by [`resolve_settings`] — where the settings
/// directory (`VIKE_SETTINGS_DIR`) is named, when a deployment names it. See
/// [`workspace_credentials`].
///
/// It must be the REAL process env and not the credential map: a systemd unit's `Environment=` /
/// `EnvironmentFile=` line is the only channel an unattended daemon has, and a store cannot name
/// where it itself lives.
static PROCESS_ENV: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// [`PROCESS_ENV`], borrowed — the ONE sweep this binary owns, handed to the pure library functions
/// that take configuration as a parameter (the boot, the credential loader above).
///
/// `get_or_init` rather than `get().expect(..)`: [`resolve_settings`] fills it at startup, but a
/// helper that PANICS when called before it would make the ordering of two startup steps a crash
/// risk rather than a detail.
pub(crate) fn process_env() -> &'static HashMap<String, String> {
    PROCESS_ENV.get_or_init(|| std::env::vars().collect())
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
    // Best-effort flush of a status line: a closed stdout has no reader to inform.
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
                &mut std::io::stdout(),
                // LOSSLESS, exactly as `CoreHandle::send_command` was here before: an operator's
                // typed order must not be dropped because the ingest lane was momentarily full.
                |cmd| sink.send_blocking(cmd),
                || crate::summary::summary_line(&cell.load_full(), &token),
            );
        })
        .expect("spawn vt-tradehub-stdio thread");
}

// The unit tests: `tradehub_cli/tests.rs` (the daemon CLI's own tests) and, beside them,
// `tradehub_cli/tests/feed_splice.rs` — the DETERMINISTIC venue-feed splice test, a whole scripted
// venue double rather than one more case, which `tests/venue_feed_splice_smoke.rs`'s module doc
// points at.
#[cfg(test)]
mod tests;
