//! `vike-recorder` — the market-data recorder's command line, as a LIBRARY function.
//!
//! This was the binary's body until the multicall merge. `main` became [`run`], and THREE pieces
//! of ambient state became parameters: the environment, the working directory and argv.
//!
//! ⚠ The environment mattered TWICE here, and the second one is easy to miss: `webhook_targets`
//! performed its own `std::env::vars()` sweep to overlay the credential store, so this body read
//! the environment at two different instants. It now takes the one map the composition root swept,
//! which is also the map the boot consumed — two sweeps could disagree if anything mutated the
//! environment between them.
//!
//! Everything below is the binary's own documentation, unchanged.
//! ```text
//! vike-recorder --profile recorder.toml [--tick-secs N] [--silent-secs N] [--once]
//! ```
//!
//! One TOML profile names the store and what to record; every tick resolves each subscription's
//! current symbols, publishes their family membership, and drives the venue feed to exactly that
//! set. Rows land in the customer's own DataFusion+Parquet store through `RecorderSink`, one
//! commit per family rather than one per symbol.
//!
//! Design authority: `docs/superpowers/specs/2026-08-02-live-recorder-design.md`. Everything
//! interesting lives in the library (`config`/`membership`/`session`/`runtime`/`venues`) and is
//! tested without a network; this file is argv, wiring, the tick cadence, and teardown.
//!
//! **Stop path — one stop flag, many triggers.** A control word on stdin
//! (`quit`/`shutdown`/`stop`) raises the flag; so do **SIGTERM and SIGINT**, through
//! `vike_ops::stop`'s `install_handlers`, so `systemctl stop`, a bare `kill` and Ctrl-C all reach
//! the SAME teardown the typed word does. EOF stops only on a TTY: under systemd stdin is
//! `/dev/null`, which reads EOF immediately, so treating that as a stop would make the daemon exit
//! on startup — the same trap `vike-tradehub` documents.
//!
//! Until that handler existed, SIGTERM reached none of the teardown below: every one of `run`'s
//! stop steps — `stop_all`, the final `RecorderHandle` flush, the dropped-row report, the
//! `MaintenanceScheduler` join — was skipped, and the flush is where the tape was lost, on every
//! restart of every box. `docs/ops/recorder-deploy.md` §2 quantifies the rows;
//! `docs/ops/graceful-stop.md` carries the the CI box measurements and the decision;
//! `crates/vike-ops/tests/graceful_stop_pin.rs` pins the wiring. On Windows there is no signal to
//! catch and `install_handlers` says so rather than pretending — the stdio word is the whole stop
//! story there, as it was everywhere before.
//!
//! The teardown is BUDGETED end to end, for the reason a graceful stop needs a bound at all: systemd
//! follows SIGTERM with SIGKILL after `TimeoutStopSec=`, so a teardown that can outrun that budget is
//! killed mid-write instead of finishing. ⚠ **Two numbers, not one** — [`FEED_STOP_BUDGET_SECS`] for
//! the feed unsubscribe (measured, warned on, and subtracted from what follows, because a non-`Send`
//! `VenueFeed` cannot move onto the orchestration thread) plus [`SHUTDOWN_DEADLINE_SECS`] for the
//! bounded flush/compaction tail, summing to [`TOTAL_STOP_BUDGET_SECS`], which is what the shipped
//! unit's `TimeoutStopSec=` is checked against. The sequence is ordered flush-first,
//! `MaintenanceScheduler` join LAST, so what a hard cap abandons is a compaction pass (restartable,
//! costs nothing) and never the flush (the rows).
//!
//! **Teardown order is load-bearing:** unsubscribe every feed FIRST, then shut the sink down. The
//! other order leaves feed threads pushing rows into a channel whose writer is gone, and those rows
//! are counted as dropped with no reporter left to announce them.
//!
//! **The silence watchdog has three possible reactions, and only two are on by default.** A silent
//! series is always LOGGED and always ALERTED — both by `crate::alerts::watchdog_tick`,
//! which is in the LIBRARY so a test drives it (log-only delivery until a webhook target is
//! named; the `[alerting]` profile table names them). EXITING is
//! opt-in (`--exit-on-silence`, [`EXIT_SILENT`]), because a venue can be legitimately quiet: an
//! illiquid market, a closed session, a Polymarket window between rotations. Making a quiet venue
//! kill the daemon that records five other healthy ones would be a worse failure than the one being
//! fixed, and under `Restart=on-failure` it would crash-loop. An operator whose subscriptions are
//! all continuous markets can turn it on and get systemd's restart machinery for free.
//!
//! **The RESOLVE watchdog is the feed-level twin, and it is on by default.** `alerts::resolve_tick`
//! escalates a venue that has NEVER resolved a symbol since startup — past the same `--silent-secs`
//! grace — to `error!` plus a real alert, while a venue that resolved before and is failing now
//! stays the per-tick `warn!` it always was. A clean-install validation on the CI box produced exactly
//! the first state (`POLY_PROXY_ENABLED` defaults ON, nothing was listening on the proxy port) and
//! the daemon ran on recording nothing, because the series watchdog above is structurally blind to
//! it: a venue that resolved nothing subscribed nothing, so its expected-series set is empty.
//!
//! **`--once` is the only reaction that EXITS on it**, with [`EXIT_DRY_RUN`], and it is judged on
//! the tick's content rather than through that grace — a dry run is one tick.

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::alerts::{self, RecorderAlerts};
use crate::liveness;
use crate::runtime::{FeedTick, RecorderRuntime};
use crate::{Membership, RecorderProfile, Stream};
use vike_data::live_rec::{RecorderConfig, RecorderSink};
use vike_data::{DataFusionHist, MaintenanceScheduler};
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_ops::stop::{self, StopSignal};

/// How often the desired set is re-resolved. A Polymarket window is 5 minutes and the planner holds
/// the NEXT one too, so 30s is many chances to see a new window listed before the current expires,
/// while a cached slug costs no request at all.
const DEFAULT_TICK_SECS: u64 = 30;

/// Warn when a subscribed series has received no rows for this long (`--silent-secs`; `0` = off).
///
/// **Five minutes, chosen against the slowest thing a HEALTHY recorder does.** A Polymarket
/// up/down family rotates its tokens every 5 minutes and a freshly-opened window can be genuinely
/// quiet for a stretch, so a threshold at or below the rotation period would cry wolf every cycle.
///
/// On by DEFAULT, deliberately: the failure it catches is silent by nature — a feed that is
/// subscribed, connected and receiving nothing raises no error and loses no rows — so an operator
/// who has to know to switch this on is exactly the operator who will not.
const DEFAULT_SILENT_SECS: u64 = 300;

/// The exit status `--exit-on-silence` produces. Distinct from the generic failure `1` so a systemd
/// unit, a supervisor or a shell can tell "a series went silent" apart from "the profile was bad"
/// or "the store would not open" — all three are non-zero, and only this one means the daemon was
/// working and the DATA was not.
pub const EXIT_SILENT: u8 = 3;

/// The exit status a FAILED `--once` dry run produces: the tick ran and proved nothing.
///
/// `docs/ops/recorder-deploy.md` tells an operator that `--once` "proves the profile parses, the
/// store opens, each family resolves to live symbols, and the venue accepted the subscriptions".
/// Until this constant existed that success criterion lived only in a LOG LINE a human had to
/// eyeball: a run whose only venue resolved nothing at all — measured on the CI box, `could not resolve
/// the desired set … Connection refused` — **exited 0**, so `--once && systemctl enable --now` gave
/// a green light to a daemon that would record nothing.
///
/// Distinct from every other status on purpose, like [`EXIT_SILENT`]: `1` is a startup failure (bad
/// profile, unopenable store), `2` is a usage error, `3` is `--exit-on-silence`. Only `4` means the
/// daemon started fine and the PROFILE is wrong.
///
/// ⚠ It is reachable ONLY under `--once`. The running daemon must stay lenient — a venue that
/// cannot resolve must not kill a process recording five healthy ones, and under
/// `Restart=on-failure` it would crash-loop — which is the same argument `--exit-on-silence`'s
/// off-by-default posture rests on.
pub const EXIT_DRY_RUN: u8 = 4;

/// The BOUNDED half of the teardown — the flush, the alert delivery stop and the compaction join,
/// in that order — is hard-capped at this.
///
/// ⚠ It is a SUFFIX of the stop, not the whole of it: [`FEED_STOP_BUDGET_SECS`] comes first and is
/// outside this cap. [`TOTAL_STOP_BUDGET_SECS`] is the number an operator sizes a `TimeoutStopSec=`
/// against, and `the_whole_stop_fits_inside_the_units_stop_timeout` is what checks it. An earlier
/// cut of this file compared THIS constant against the unit and claimed that proved "SIGKILL does
/// not cut the final flush in half" — it did not, because the quantity it bounded was not the
/// quantity it named.
///
/// What actually has to fit inside it is the final flush: one Parquet part per buffered series,
/// sub-second on the measured the CI box tape. Between the flush and the join sits the alert delivery
/// stop, which carries its own smaller budget INSIDE this cap ([`ALERT_STOP_BUDGET_SECS`] — the
/// reason it is not a term in [`TOTAL_STOP_BUDGET_SECS`]). What does NOT have to fit is the
/// `MaintenanceScheduler` join — a compaction pass merges up to `max_merge_rows` and can genuinely
/// take minutes — which is exactly why it is the LAST step of the sequential tail: a hard cap
/// abandons the pass, which is restartable and costs nothing, and never the rows.
///
/// Seconds rather than a `Duration` constant so `crates/vike-ops/tests/docs_constants_gate.rs` can
/// read the literal and hold `docs/ops/kill-switches.md`'s stated budget to it.
const SHUTDOWN_DEADLINE_SECS: u64 = 20;

/// The budget for the step that runs BEFORE the bounded region: unsubscribing every feed.
///
/// **Why it is outside the cap at all.** `RecorderRuntime` owns `Box<dyn VenueFeed>`, which is not
/// `Send`, so it cannot move onto `run_with_deadline`'s orchestration thread; widening that trait is
/// a change to every venue implementation and belongs in its own commit. So this step is BUDGETED
/// rather than enforced, and the code says so rather than implying otherwise — the daemon MEASURES
/// what it actually cost, warns when it overran, and [`remaining_teardown_budget`] takes the overrun
/// out of the bounded region so the TOTAL still aims at [`TOTAL_STOP_BUDGET_SECS`].
///
/// **Where the number comes from: the LARGEST blocking window, plus the read that can trail it.**
/// `RecorderRuntime::stop_all` raises every feed's stop flag before it joins anything, so the whole
/// profile costs about ONE socket wind-down rather than one per socket. When the flag goes up a feed
/// thread sits in exactly ONE blocking window, and each is followed by a stop check before the next
/// begins — so what must fit here is the biggest single one, not their sum.
///
/// ⚠ **The scope is BOTH venues this daemon can record**, because `crates/vike-recorder/src/venues/`
/// has two modules and only one of them is binance. The rows below cover the shared drivers, the
/// binance family AND polymarket's own shard body — a derivation over one venue is not a ceiling for
/// a daemon running the other, and for three rounds this table was exactly that while the CI box was
/// recording polymarket.
///
/// | window | ceiling | spelled in |
/// |---|---|---|
/// | live socket read | `READ_2S` | `crates/vike-bridge-core/src/pump_spec.rs` |
/// | reconnect backoff | 100 ms stop-poll ticks | `crates/vike-bridge-core/src/market_pump.rs` |
/// | the market dial — DNS **and** every resolved address | `CONNECT_10S` | `crates/vike-bridge-core/src/pump_spec.rs` |
/// | the depth driver's dial | `CONNECT_10S` | `crates/vike-bridge-core/src/depth.rs` |
/// | the depth driver's REST book seed | `DEPTH_SEED_TIMEOUT` | `crates/bridges/binance/src/family/depth.rs` |
/// | binance's one-time REST warmup | `WARMUP_TIMEOUT` | `crates/bridges/binance/src/family/trades.rs` |
/// | binance's post-warmup drain | `DRAIN_DEADLINE` | `crates/bridges/binance/src/family/trades.rs` |
/// | polymarket's per-token REST warmup | `FEED_WARMUP_TIMEOUT` | `crates/bridges/polymarket/src/market_feed.rs` |
///
/// The largest is the dial, and one in-flight read is what can trail it, so this constant is
/// `READ_2S + CONNECT_10S` — a CEILING for the profile, not a per-feed cost. Note what that table
/// makes visible and prose did not: the warmup, the seed and the drain are windows too, and a bound
/// on the dial alone would simply have moved the worst case rather than lowered it.
///
/// ⚠ **"One blocking window" is a property of CONTROL FLOW, not of the ceilings.** Two of these rows
/// are per-ITEM rather than once-per-thread — polymarket resolves a tick size per newly-seated token
/// (up to `1 + TICK_SIZE_LOOKUP_MAX_PAGES` requests each, plus two more when
/// `VIKE_RECORD_PROPERTIES=1` arms the properties recorder), and a shard reseats many tokens at once
/// — so a per-request ceiling alone would bound one request while the loop paid for fifty. What
/// makes the row a ceiling is the stop poll in front of every request
/// (`crates/bridges/polymarket/src/market_feed.rs`'s `reconcile_slots`, gated by
/// `a_stop_raised_during_the_warmup_stops_the_next_request`) and the one between the warmup and the
/// dial that follows it, so the two cannot sum.
///
/// ⚠ **"Every one of them" is a claim this table used to MAKE and nothing CHECKED, and it was
/// false when written — TWICE, in the same direction.** First the depth driver's REST book seed was
/// missing: it ran on the shared 30 s pager agent with no stop check in front of it, so the branch
/// that added the table bounded the dial and left a window three times larger immediately behind it,
/// then asserted exhaustiveness over the result. Then the gate written to retire that failure keyed
/// on the NAMES `blocking_agent`/`tungstenite::connect` — and polymarket's per-token warmup rode a
/// HAND-BUILT agent (`crates/bridges/polymarket/src/egress.rs`'s `agent`, 30 s, proxy-aware), which
/// no name-keyed scan could see, on the one venue the CI box was actually recording. So the claim is now
/// DERIVED rather than asserted, and derived by SHAPE rather than by a list of blessed names.
/// `crates/vike-ops/tests/feed_stop_windows_gate.rs` re-reads this table and re-scans the source: it
/// collects every file under `crates/bridges/*/src` or `crates/vike-bridge-core/src` that mounts a
/// feed driver, and requires each blocking window it opens to be **bounded by a ceiling this table
/// names**, or declared off-feed-path with a written reason. A `tungstenite::connect` (no bound at
/// all) fails outright; EVERY agent-shaped call — a callee path with a segment containing `agent`,
/// which catches a hand-rolled `ureq::Agent::config_builder()` and a venue-local `exec::agent()`
/// alike — must be a classified rung; the 30 s pager must be declared; and a bound taken from an
/// ARGUMENT must have that argument as a row. A venue added tomorrow inherits the gate; deleting the
/// row that covers a bounded REST window turns that window back into an undeclared one and the gate
/// says so; and every row's cited file must still spell its ceiling. That gate's own module doc
/// carries what it still cannot see — and, since the prose version of that residual was itself
/// wrong, the residual is now RUN (`the_windows_this_gate_cannot_see`,
/// `the_kline_seed_is_still_the_named_out_of_scope_window`) rather than described.
///
/// ⚠ **The derivation used to be an ASSUMPTION, and this is what it was missing.** It held only
/// where the DIAL is bounded, and five of the six on-driver rows — binance included — carried
/// `connect_timeout: None`, which selects plain `tungstenite::connect` and applies no connect bound
/// at all: a black-holed route pinned a feed thread inside the OS's SYN ladder (minutes) with the
/// flag `raise_stops` had just set going unread, blowing through this ceiling and through
/// [`TOTAL_STOP_BUDGET_SECS`] with it, so SIGKILL could land on the final Parquet flush. Binance was
/// the venue that mattered because `crates/bridges/binance/src/family/trades.rs`'s
/// `connect_trades_ws` owns its own connect closure (its startup drain needs the raw socket) and
/// hand-rolled that unbounded dial, so it would have ignored a bounded row anyway.
///
/// **What holds each row of the table** — a derivation nothing checks decays back into an assumption:
/// - every `OnDriver` row bounds its dial: `market_pump_spec`'s `every_on_driver_row_bounds_its_dial`;
/// - the trades lane consumes its row instead of re-spelling the dial:
///   `the_trades_dial_goes_through_the_bounded_shared_path`;
/// - the depth driver takes the same bounded arm:
///   `the_depth_dial_goes_through_the_bounded_shared_path`;
/// - that bound covers RESOLUTION and every resolved address under ONE window, so neither a hung
///   resolver nor a venue's DNS answer can multiply it:
///   `resolution_time_is_spent_from_the_same_window_as_the_connects` and
///   `the_whole_tcp_phase_is_bounded_however_many_addresses_resolve`;
/// - the warmup runs on a bounded agent that fits inside the dial window, not the shared 30 s pager
///   agent: `the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers`;
/// - the depth book seed does too, and its row is the twin of that one:
///   `the_depth_seed_runs_on_a_bounded_agent_not_the_pagers`;
/// - a stop raised during the dial skips that seed instead of paying for it:
///   `a_stop_raised_before_the_session_skips_the_rest_seed`;
/// - polymarket's per-token warmup rides a bounded agent rather than the venue's 30 s order-path
///   one: `the_feed_warmup_runs_on_a_bounded_agent_not_the_order_paths`;
/// - and a stop raised during that warmup ends the LOOP, not just the request — the half a
///   per-request ceiling cannot buy on a per-token window:
///   `a_stop_raised_during_the_warmup_stops_the_next_request`;
/// - the drain's ceiling is the row's read timeout: `the_drain_deadline_matches_the_rows_read_timeout`;
/// - and the TABLE ITSELF is exhaustive over the source, not over its author's memory:
///   `crates/vike-ops/tests/feed_stop_windows_gate.rs`.
///
/// It is still not ENFORCED (the `Send` reason above), which is why the daemon keeps measuring it,
/// `warn!`s an overrun, and subtracts it via [`remaining_teardown_budget`] down to the
/// [`MIN_FLUSH_BUDGET_SECS`] floor.
///
/// ⚠ **The residual left, named rather than implied:** every dial bound above ends where the TLS + WS
/// handshake begins — `crates/vike-bridge-core/src/ws_proxy.rs`'s `connect_ws` bounds the TCP phase
/// and hands the socket to `tungstenite::client_tls`, which carries no deadline. A peer that
/// completes a TCP handshake and then stalls the TLS one is outside this number. That needs a
/// half-open peer rather than a black hole, which is why it is the residual and the dial was the bug.
const FEED_STOP_BUDGET_SECS: u64 = 12;

/// The WHOLE stop: the feed unsubscribe plus the bounded flush/compaction tail. **This is the number
/// `deploy/vike-recorder.service`'s `TimeoutStopSec=` must exceed**, and the number the loser of a
/// teardown claim waits for before it is allowed to end the process
/// (`vike_ops::stop::StopSignal::await_teardown`) — the two must be the same budget, or a second
/// stop route could exit while the first is still flushing.
///
/// ⚠ **"WHOLE" MEANS FROM THE FLAG BEING OBSERVED, NOT FROM THE SIGNAL ARRIVING**, and the
/// difference is a declared blind spot rather than a covered one. `run`'s loop calls
/// `report(rt.tick(now_ms()))` and checks `stop.is_requested()` AFTERWARDS, so a signal landing
/// mid-tick waits out that tick first. That prefix is unbounded here in a way the feed stop is not:
/// `rt.tick` → `PolymarketFeed::desired` → `RollingWindowPlanner::plan` performs a blocking,
/// proxy-routed Gamma resolve whose agent (`crates/bridges/polymarket/src/egress.rs`'s `agent`) carries
/// a 30 s global timeout, and a window rotation can issue more than one.
///
/// It is NOT measured, NOT warned on and NOT subtracted — unlike the feed stop — so this constant and
/// the test named for the whole stop both exclude it. The consequence is mild: SIGKILL landing there
/// is the pre-change loss, and the manifest plus `_wal.arrow` mean no torn store. But anyone sizing
/// `TimeoutStopSec=` from this number is reading a figure that leaves out a 30 s network call, so it
/// is named here, in that test's failure message, and in `docs/ops/recorder-deploy.md` §2. Checking
/// the flag before the tick, or slicing the tick's blocking work, would close it.
///
/// ⚠ **That prefix used to contain a SECOND, larger unbounded window, and it is now gone.** Alert
/// delivery ran inline in the same tick: `AlertEngine::dispatch` is a nested serial loop and a
/// webhook sink's `deliver` was a blocking POST bounded only by its transport's 10 s global
/// timeout, dispatched once per SILENT SERIES — so the first tick of a venue outage could hold this
/// loop for tens of minutes with the stop flag unread, dwarfing the Gamma resolve above.
/// `crate::alerts::RecorderAlerts` now registers those targets as `vike_alerting::QueuedSink`s, so
/// the tick pays an enqueue; what is left in the prefix is the resolve, which is what this
/// paragraph describes.
const TOTAL_STOP_BUDGET_SECS: u64 = FEED_STOP_BUDGET_SECS + SHUTDOWN_DEADLINE_SECS;

/// The floor [`remaining_teardown_budget`] never goes below, however long the feed stop overran.
///
/// A feed stop that blows its budget has already put the total past `TimeoutStopSec=`; handing the
/// flush zero seconds in response would guarantee the loss the whole bound exists to prevent, for
/// no gain. The flush is sub-second on the measured tape, so five seconds is generous for it and
/// still bounded.
const MIN_FLUSH_BUDGET_SECS: u64 = 5;

/// How long the alert delivery workers get to stop, INSIDE the bounded region above rather than
/// beside it — which is why this constant does not appear in [`TOTAL_STOP_BUDGET_SECS`].
///
/// **Why there are threads to stop at all.** `crate::alerts::RecorderAlerts` registers its webhook
/// targets as `vike_alerting::QueuedSink`s, because the raw sink's `deliver` is a blocking HTTP
/// POST bounded only by its transport's 10 s global timeout and the tick loop below is the same
/// loop that polls the stop flag — one alert per silent series, times one POST per target, parked
/// that loop for tens of minutes on the first tick of a venue outage. `crate::alerts`' module doc
/// carries the arithmetic.
///
/// **Why it is SMALL, and why the step is ordered where it is.** The normal case is an IDLE worker
/// — the queue is empty in every tick that raised nothing — and an idle worker stops within one
/// `vike_alerting::STOP_POLL` (a tenth of a second; it re-reads its flag only when its receive
/// poll returns, so this is a poll, not microseconds), paid once PER TARGET because
/// `crate::alerts::stop_delivery` stops the handles one after another: two targets are a few
/// hundred milliseconds, a tenth of this budget. A worker caught mid-POST cannot be interrupted, so
/// past this budget the thread is ABANDONED (`vike_alerting::QueuedSinkStop::shutdown`) — and
/// abandoning it costs at most one un-POSTed page, while spending ten seconds waiting for it costs
/// a share of the flush's budget. The step therefore runs AFTER the flush (rows first, always) and
/// BEFORE the `MaintenanceScheduler` join, which stays last because a compaction pass is the one
/// thing a hard cap may eat for free.
const ALERT_STOP_BUDGET_SECS: u64 = 2;

/// A floor is a floor, never a raise — and [`remaining_teardown_budget`]'s `clamp` would PANIC in a
/// running daemon's teardown if these two were ever edited the other way round. Compile time is the
/// right place to learn that.
const _: () = assert!(MIN_FLUSH_BUDGET_SECS <= SHUTDOWN_DEADLINE_SECS);

pub fn run_main(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    argv: &[String],
) -> std::process::ExitCode {
    let args = match Args::parse(argv.iter().cloned()) {
        Ok(Parsed::Run(a)) => a,
        // `--help`: the usage on STDOUT, exit 0. It used to come back as `Err("")` — an EMPTY error
        // string, so the caller below would print the usage with no message above it — which meant
        // `--help` exited **2** with its whole text on stderr and a leading blank line. A non-zero
        // help breaks `set -e` and every packaging smoke test.
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        // `<name> <version> (<build identity>)` on stdout, exit 0 — the shape every `--version` on
        // the box prints (`git version 2.x`, `cargo 1.x`), with this build's PROVENANCE appended. It
        // was unrecognised once, so it hit the `unknown argument` arm and exited **2** on stderr,
        // which a packaging probe reads as "this binary is broken".
        //
        // ⚠ The parenthesised half is what this daemon in particular needed: the near-miss that
        // motivated `crates/vike-buildinfo/src/lib.rs` was a binary built from a bare repo four
        // commits behind `main`, about to be installed on the LIVE RECORDER. `vike-recorder
        // --version` on the box now answers "is this the code I think it is?" before the install.
        Ok(Parsed::Version) => {
            println!(
                "{}",
                vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            );
            return std::process::ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };

    // ONE environment sweep, owned by the BINARY — the settings-registry rule's `Layer::Binary`
    // shape, the same spelling `vike-app`'s and `vike-tradehub`'s `main` use. It must happen before
    // `vike_log::init`, because the log DIRECTORY depends on it.
    //
    // ⚠ `VIKE_SETTINGS_DIR` was DECORATIVE on this daemon until 2026-08-08: all three shipped units
    // set it and both runbooks claimed it "makes the answer EXPLICIT and independent of
    // WorkingDirectory", while `strings` on the shipped binary found the name ZERO times.
    // Resolution depended entirely on `WorkingDirectory=`, so a run started from anywhere else put
    // the rolling log beside the BINARY (measured: `<exe_dir>/logs`, vike-log's last resort) and
    // read its alerting credentials from whatever project the CWD happened to sit under.

    // ⚠ THE WALK HAPPENS ONCE, and that is why the sequence is `vike-boot`'s rather than this
    // file's. Three answers hang off it here — the rolling log's home, the disclosure's subject, and
    // (through `run`) the alerting credential store — and this `main` used to resolve it twice with
    // two different functions, which is the "two walks, two answers" shape the disclosure exists to
    // make visible. `LogHome::UnderSettings` is `project_log_dir_from` by another name:
    // `project_state_dir_from(..).join(LOGS_SUBDIR)` is `<settings dir>/state/logs`, the same two
    // joins, off the same resolver.
    let booted = match vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "a recorder must not acquire a new reason to fail to start, and it reads none of the \
             ceilings whose variables were removed — it takes an explicit `--profile` and no \
             settings key at all. `vike-cli config check`, which the runbook puts in the unit's \
             `ExecStartPre=`, is the surface that refuses a stale environment on this box.",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "this daemon consumes no settings key of its own. It DISCLOSES them below, which is a \
             different thing: the walk it depends on for the log directory, the alerting rules file \
             and the credential store is otherwise entirely silent about where it landed.",
        ),
        credentials: vike_boot::Credentials::Deferred(
            "the alerting webhook targets are read inside `run`, from the profile's own \
             `[alerting]` section, and only when one is configured — see `webhook_targets`.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Render,
    }) {
        Ok(b) => b,
        // Unreachable with the two `Ignore`/`Skip` arms above (nothing here can fail), but a
        // refusal must never become an unwrap in a daemon's `main`.
        Err(e) => {
            eprintln!("vike-recorder: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // `project_dir` is the DEFAULT log directory: `<project>/settings/state/logs`, beside every
    // other file the program writes. Left `None` it fell through to vike-log's `<exe_dir>/logs` last
    // resort — `target/debug/logs/…` in a checkout, which `cargo clean` deletes. `$VIKE_LOG_DIR`
    // still wins; a run with no project above it and no override still lands beside the exe.
    let _guards = vike_log::init(vike_log::LogConfig {
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });

    // THE STARTUP DISCLOSURE, and it must be here: after `vike_log::init`, because the log
    // destination is itself resolved from the settings directory this is about to describe. It is
    // rendered by `vike-boot` and EMITTED here, for the reason that crate logs nothing: it runs
    // before there is a subscriber, and a binary whose stdout is a protocol picks the stream.
    //
    // 1. WHICH BINARY. `Booted::identity_line` is the same string `--version` prints — see
    //    `crates/vike-buildinfo/src/lib.rs` for the release binary built four commits behind `main`
    //    that was nearly installed on this very daemon.
    // 2. WHICH SETTINGS. `Booted::boot_lines` names the directory that answered, each settings file
    //    present or absent, the resolved risk ceilings, and whether a credential store sits beside
    //    them. It is a DISCLOSURE, never a gate — a broken `policy.toml` becomes a line here, not a
    //    refusal, because a recorder must not acquire a new reason to fail to start.
    tracing::info!("{}", booted.identity_line);
    for line in &booted.boot_lines {
        tracing::info!("{line}");
    }

    match run(args, booted.settings_dir_override.as_deref(), vars) {
        Ok(Outcome::Stopped) => std::process::ExitCode::SUCCESS,
        // The daemon ran correctly and the DATA did not arrive — a distinct status from a
        // configuration or store failure, so a supervisor can react differently. Opt-in only.
        Ok(Outcome::Silent(series)) => {
            tracing::error!(
                series = ?series,
                "recorder: exiting on silence (--exit-on-silence) — these subscribed series are \
                 receiving no rows"
            );
            eprintln!("vike-recorder: exiting on silence: {}", series.join(", "));
            std::process::ExitCode::from(EXIT_SILENT)
        }
        // The dry run RAN and proved nothing. A distinct status so `--once && systemctl enable
        // --now` can no longer green-light a profile that resolves nothing — see [`EXIT_DRY_RUN`].
        Ok(Outcome::DryRunFailed(reasons)) => {
            for why in &reasons {
                tracing::error!(reason = %why, "recorder: --once dry run FAILED");
            }
            eprintln!(
                "vike-recorder: --once proved NOTHING — this profile would record no data:\n  {}",
                reasons.join("\n  ")
            );
            std::process::ExitCode::from(EXIT_DRY_RUN)
        }
        Err(e) => {
            tracing::error!(error = %e, "recorder failed");
            eprintln!("vike-recorder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage: vike-recorder --profile PATH [--tick-secs N] [--silent-secs N] [--exit-on-silence] [--once]

  --profile PATH    the recorder profile TOML (store root + subscriptions)
  --tick-secs N     how often to re-resolve the desired symbol set (default 30)
  --silent-secs N   warn when a subscribed series has received no rows for N seconds
                    (default 300; 0 disables). Catches a feed that is subscribed and
                    connected but receiving nothing — which raises no error anywhere.
  --exit-on-silence stop with status 3 when a series is silent, so Restart=on-failure
                    can act. OFF by default: a venue can be legitimately quiet, and a
                    quiet market must not kill a daemon recording five healthy ones.
                    Alerting (the [alerting] profile table) is the always-on reaction.
  --once            run ONE tick and exit — the dry run. Exits 0 only if EVERY feed
                    ended that tick with at least one live subscription and no
                    refused subscribe; otherwise status 4, naming each feed that
                    would record nothing. Stricter than the daemon on purpose: this
                    is a commissioning check of your own profile, with you watching.
  -h, --help        print this and exit 0
  -V, --version     print the version and exit 0
";

/// What a successful parse produced. `--help` and `--version` are neither a run nor an error — the
/// third and fourth outcomes. Named VARIANTS rather than an `Option` (which had room for exactly
/// one of them) so no call site can confuse them; the same shape `vike-tradehub`'s `Parsed` uses.
enum Parsed {
    Run(Args),
    Help,
    Version,
}

/// How a completed [`run`] ended. `Silent` exists only so `main` can map it to [`EXIT_SILENT`] —
/// it is not an error (nothing failed; the daemon did exactly what it was asked), which is why it
/// is an `Ok` variant rather than an `Err`.
enum Outcome {
    Stopped,
    /// `--exit-on-silence` was set and these series were receiving no rows.
    Silent(Vec<String>),
    /// `--once` ran one tick and it proved nothing — one reason per feed that would record no
    /// data. See [`EXIT_DRY_RUN`] and `crate::runtime::dry_run_failures`.
    DryRunFailed(Vec<String>),
}

struct Args {
    profile: PathBuf,
    tick: Duration,
    once: bool,
    /// Warn when a subscribed series has received no rows for this long; 0 disables.
    silent_secs: u64,
    /// Stop with [`EXIT_SILENT`] when a series is silent. OFF by default — see the module doc.
    exit_on_silence: bool,
}

impl Args {
    /// [`Parsed::Help`] and [`Parsed::Version`] are `-h`/`--help` and `-V`/`--version`: nothing to
    /// run, and nothing wrong either — see `main`'s arms for what each used to cost. A genuine
    /// usage problem is still the `Err`.
    fn parse(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
        let mut profile = None;
        let mut tick = Duration::from_secs(DEFAULT_TICK_SECS);
        let mut once = false;
        let mut silent_secs = DEFAULT_SILENT_SECS;
        let mut exit_on_silence = false;
        let mut it = argv.peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--profile" => {
                    profile =
                        Some(PathBuf::from(it.next().ok_or("--profile needs a path".to_string())?));
                }
                "--tick-secs" => {
                    let v = it.next().ok_or("--tick-secs needs a number".to_string())?;
                    let n: u64 =
                        v.parse().map_err(|_| format!("--tick-secs: not a number: {v}"))?;
                    if n == 0 {
                        return Err("--tick-secs must be > 0".into());
                    }
                    tick = Duration::from_secs(n);
                }
                "--silent-secs" => {
                    let v = it.next().ok_or("--silent-secs needs a number".to_string())?;
                    silent_secs =
                        v.parse().map_err(|_| format!("--silent-secs: not a number: {v}"))?;
                }
                "--exit-on-silence" => exit_on_silence = true,
                "--once" => once = true,
                "-h" | "--help" => return Ok(Parsed::Help),
                "-V" | "--version" => return Ok(Parsed::Version),
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        if exit_on_silence && silent_secs == 0 {
            // `--silent-secs 0` disables detection outright, so the exit could never fire — an
            // operator who wrote both believes something is armed that is not.
            return Err("--exit-on-silence needs silence detection on (--silent-secs > 0)".into());
        }
        Ok(Parsed::Run(Self {
            profile: profile.ok_or("--profile is required".to_string())?,
            tick,
            once,
            silent_secs,
            exit_on_silence,
        }))
    }
}

/// `settings_dir` is `$VIKE_SETTINGS_DIR` as `main` resolved it — a PARAMETER, so this function
/// reads no environment of its own and the one sweep in `main` stays the only one.
fn run(
    args: Args,
    settings_dir: Option<&str>,
    env: &std::collections::HashMap<String, String>,
) -> Result<Outcome, String> {
    // ⚠ FIRST — before the store is opened, before the compaction scheduler starts, before the
    // writer thread spawns and before a single feed is built. Every one of those can block (a store
    // open takes a per-series advisory lock), and until the handler is installed SIGTERM still has
    // the OS default disposition: the process dies where it stands with no teardown at all. The
    // handler only stores a bool, so installing it costs nothing and can never be too early — but it
    // can very easily be too late, which is the shape a reviewer found in the first cut of this
    // change on the sibling daemon. `vike_log::init` already ran in `main`, so the outcome is logged
    // here rather than carried.
    let stop = StopSignal::new();
    match stop::install_handlers(&stop.flag()) {
        stop::HandlerOutcome::Installed => {
            tracing::info!("recorder: SIGTERM/SIGINT will stop gracefully (flush, then exit)");
        }
        // Windows: not a degradation, just the truth about the platform — stated so an operator
        // never believes a graceful service stop is armed when no such signal exists.
        stop::HandlerOutcome::Unsupported => {
            tracing::info!(
                "recorder: no POSIX signals on this platform — stop with the stdio `quit` word"
            );
        }
        // The one case an operator MUST see: the daemon runs, but a `systemctl stop` is back to
        // losing the buffered tape and nothing else would say so.
        stop::HandlerOutcome::Failed(e) => {
            tracing::error!(
                error = %e,
                "recorder: could NOT install the signal handler — a SIGTERM will lose buffered rows; \
                 stop with the stdio `quit` word instead"
            );
        }
    }

    let text = std::fs::read_to_string(&args.profile)
        .map_err(|e| format!("reading {}: {e}", args.profile.display()))?;
    // ⚠ NAME THE FILE. `ProfileError` is a library type and deliberately carries no path — it never
    // saw one — so its message opens `recorder profile: TOML parse error at line 4, column 1`. The
    // READ error above names the file and the parse error did not, which is backwards: a read
    // failure is usually a typo in the `--profile` the operator just typed, while a parse failure
    // lands in a systemd journal hours later, where `--profile` is a line in a unit nobody has
    // open. The path goes FIRST, so `journalctl` shows which file even when the line is truncated.
    let profile = RecorderProfile::from_toml(&text)
        .map_err(|e| format!("{}: {e}", args.profile.display()))?;

    let store = Arc::new(
        DataFusionHist::open(&profile.store)
            .map_err(|e| format!("opening store {}: {e}", profile.store.display()))?,
    );

    // Compaction runs alongside recording, in its own thread, and takes the SAME per-series lock the
    // appends do — so a racing flush just serializes and loses nothing. Started BEFORE the sink so
    // its first pass can already be tidying yesterday's parts while today's rows arrive.
    let maint = profile.maintenance.scheduler_args().map(|(cfg, interval)| {
        tracing::info!(
            interval_secs = interval.as_secs(),
            min_parts = profile.maintenance.min_parts,
            retention_days = ?profile.maintenance.retention_days,
            "recorder: maintenance scheduled"
        );
        MaintenanceScheduler::start(store.clone(), cfg, interval)
    });
    if maint.is_none() {
        tracing::warn!(
            "recorder: maintenance DISABLED ([maintenance].interval_secs = 0) — a busy series \
             commits a part every few seconds, so parts will accumulate until something else \
             compacts them"
        );
    }

    // ONE membership map, shared by the store's GroupResolver and the runtime that publishes into
    // it. That shared handle is what makes a 5-minute rotation visible to the very next flush.
    let membership = Membership::new();
    let (sink, handle) = RecorderSink::spawn(
        store,
        RecorderConfig { grouping: Some(membership.group_resolver()), ..Default::default() },
    )
    .map_err(|e| format!("spawning the recorder writer thread: {e}"))?;

    let mut watch = liveness::SilenceWatch::new();
    // The FEED-level twin: which venues have NEVER resolved a symbol. See
    // `crate::liveness::ResolveWatch` for why the series watch above cannot answer it.
    let mut resolve_watch = liveness::ResolveWatch::new();
    // The delivery half of the watchdog. Mounted UNCONDITIONALLY (an absent `[alerting]` table
    // means defaults, not off): with no webhook target it delivers to the log, which is where the
    // warning already went — naming a target escalates the same alert to a pager.
    let mut alerts = RecorderAlerts::mount(&profile.alerting, webhook_targets(settings_dir, env));
    let mut rt = RecorderRuntime::new(membership, Stream::ALL.to_vec());
    for sub in &profile.subscribe {
        let feed = crate::venues::build_feed(sub, sink.clone())?;
        tracing::info!(venue = %sub.venue, family = ?sub.family, "recorder: feed mounted");
        rt.add_feed(feed);
    }
    if rt.feed_count() == 0 {
        return Err("profile has no subscriptions — nothing to record".into());
    }

    // The stdio half of "one stop flag, many triggers" (`vike_ops::stop`'s module doc is the
    // contract) — the signal half was armed at the top of this function, before any of the state
    // above existed. `--once` deliberately gets no stdin thread but IS covered by the handler: a
    // dry run that is Ctrl-C'd mid-tick should flush what it has, not be reaped.
    if !args.once {
        spawn_stdin_control(stop.flag());
    }

    tracing::info!(
        feeds = rt.feed_count(),
        tick_secs = args.tick.as_secs(),
        store = %profile.store.display(),
        "recorder: started"
    );

    let mut silent_exit: Option<Vec<String>> = None;
    let mut dry_run_failed: Option<Vec<String>> = None;
    loop {
        let ticks = rt.tick(now_ms());
        report(&ticks);
        // The FEED-level escalation: a venue that has never resolved a symbol past its grace is an
        // `error!` AND a real alert, while a venue that resolved before and is failing now stays
        // the per-tick `warn!` `report` just emitted. In the LIBRARY for the same reason as
        // `watchdog_tick` — `tests/resolve_alert.rs` drives this exact call.
        alerts::resolve_tick(&mut resolve_watch, &mut alerts, &ticks, now_ms(), args.silent_secs);
        // Judge, log, alert — all three in the LIBRARY (`crate::alerts::watchdog_tick`), so
        // the alerting call is covered by `tests/silence_alert.rs` rather than living in a binary
        // no test drives. That is exactly how the previous shape of this reaction — compute the
        // verdict, log it, discard it — went unnoticed while it fired 20 times in 24 hours.
        let silent = alerts::watchdog_tick(
            &mut watch,
            &mut alerts,
            &rt.expected_series(),
            &handle.liveness(),
            now_ms(),
            args.silent_secs,
        );
        // ⚠ Judged on THIS TICK'S CONTENT, never through `resolve_watch` — that watch's grace is
        // `--silent-secs` (300 s by default) and a dry run is one tick, so routing the verdict
        // through it would make every `--once` run pass green again for a new reason.
        if args.once {
            let failures = crate::runtime::dry_run_failures(&ticks);
            if !failures.is_empty() {
                dry_run_failed = Some(failures);
            }
        }
        if args.exit_on_silence && !silent.is_empty() {
            silent_exit = Some(silent);
            break;
        }
        if args.once || stop.is_requested() {
            break;
        }
        // Sleep in slices so a stop is honored promptly rather than after a whole tick.
        let deadline = std::time::Instant::now() + args.tick;
        while std::time::Instant::now() < deadline && !stop.is_requested() {
            std::thread::sleep(Duration::from_millis(200));
        }
        if stop.is_requested() {
            break;
        }
    }

    // Claim the teardown. There is exactly ONE claim site, so this cannot lose today — it is here
    // so a future second stop route cannot quietly become a second teardown (a double flush, a
    // second join of an already-joined writer). `vike_ops::stop`'s tests prove the claim is
    // exactly-once under concurrency.
    if !stop.begin_teardown() {
        // ⚠ It must WAIT, and it must not return early. Returning from here returns from `run`,
        // which returns from `main`, which TERMINATES THE PROCESS — killing the winner's teardown at
        // whatever instruction it had reached, i.e. a Parquet part torn between its footer and its
        // manifest entry. That is worse than the double flush the claim protects against, so the
        // loser blocks until the winner announces completion, bounded by the SAME total budget the
        // unit's `TimeoutStopSec=` is sized against.
        tracing::warn!("recorder: teardown already claimed — waiting for it, not running it twice");
        if !stop.await_teardown(Duration::from_secs(TOTAL_STOP_BUDGET_SECS)) {
            tracing::error!(
                budget_secs = TOTAL_STOP_BUDGET_SECS,
                "recorder: the running teardown outran the whole stop budget — exiting anyway, \
                 because SIGKILL is already due; check the store's manifest on next open"
            );
        }
        return Ok(Outcome::Stopped);
    }

    // Stop the feeds BEFORE the writer: rows arriving after the writer is gone are counted as
    // dropped with nothing left to report them.
    //
    // ⚠ This step is OUTSIDE the bounded region below, and that is a fact about the type rather than
    // a choice: `RecorderRuntime` owns `Box<dyn VenueFeed>`, which is not `Send`, so it cannot move
    // onto `run_with_deadline`'s orchestration thread. Widening the trait to `Send` is a change to
    // every venue implementation and belongs in its own commit, not as a rider on the stop path.
    //
    // ⚠ It is therefore BUDGETED, not capped — so it is MEASURED, reported when it overruns, and
    // subtracted from the bounded region below, which is the closest an unbounded step can get to
    // being part of a total. `stop_all` raises every feed's flag before it joins anything, so the
    // whole profile costs about one socket wind-down rather than one per socket: an earlier shape of
    // this line was a serial unsubscribe-and-join, and a growing profile walked the total stop time
    // through `TimeoutStopSec=` while a test compared only the suffix below against it.
    let feed_stop_started = std::time::Instant::now();
    rt.stop_all();
    let feed_stop = feed_stop_started.elapsed();
    if feed_stop > Duration::from_secs(FEED_STOP_BUDGET_SECS) {
        tracing::warn!(
            took_ms = feed_stop.as_millis() as u64,
            budget_secs = FEED_STOP_BUDGET_SECS,
            "recorder: unsubscribing the feeds overran its budget — the flush below gets what is \
             left of the total stop budget, and the whole stop may now outrun the unit's \
             TimeoutStopSec="
        );
    }

    // The alert delivery workers' stop handles, taken out of the mount so they can move into the
    // bounded closure below (`alerts` itself cannot: it owns the engine, and the engine must keep
    // its sinks). Taken HERE rather than at the top of the teardown because the losing arm of the
    // claim above returns early — a loser that stopped the winner's delivery workers would be a
    // second teardown, which is the exact thing the claim exists to prevent. On that path they are
    // stopped by their own sender-disconnect when `alerts` drops at the end of `run`.
    let alert_stops = alerts.take_delivery_stops();

    // The rest of the teardown, BOUNDED — see [`SHUTDOWN_DEADLINE_SECS`] for why a graceful stop needs a
    // bound at all and why the ordering below is what a hard cap is allowed to eat. No parallel
    // tasks: all three steps are load-bearing SEQUENTIAL, so it is all the `then` tail.
    let teardown: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
        let mut maint = maint;
        let dropped = handle.dropped();
        // Read BEFORE `shutdown` (which consumes the handle) and beside the scalar, because the
        // scalar alone cannot answer the only question worth asking of it: WHICH tape has the hole.
        // One bounded queue carries every series, so a quiet series loses the largest share of its
        // own rows while `liveness` still reads it as alive.
        let by_series = handle.dropped_summary();
        handle.shutdown();
        if dropped > 0 {
            tracing::warn!(
                dropped,
                %by_series,
                "recorder: rows were dropped during this session — by series: {by_series}"
            );
        }
        // The alert delivery workers, between the rows and the compaction — see
        // [`ALERT_STOP_BUDGET_SECS`] for why they exist, why the budget is small, and why this is
        // the position. Nothing is lost by abandoning one; everything is lost by letting one
        // spend the flush's budget.
        alerts::stop_delivery(alert_stops, Duration::from_secs(ALERT_STOP_BUDGET_SECS));
        // Maintenance last: its `stop` joins, and a pass in flight holds series locks the final
        // flush above may still have needed.
        if let Some(m) = maint.as_mut() {
            m.stop();
            tracing::info!(
                passes = m.passes_completed(),
                parts_compacted = m.parts_compacted(),
                "recorder: maintenance stopped"
            );
        }
    });
    let budget = remaining_teardown_budget(feed_stop);
    match run_with_deadline(Vec::new(), teardown, budget) {
        ShutdownOutcome::Graceful => {
            tracing::info!(feed_stop_ms = feed_stop.as_millis() as u64, "recorder: stopped")
        }
        // The flush ran first, so this almost certainly means a compaction pass was abandoned — but
        // say it rather than assume it, because the other reading is that rows did not make it.
        // The line names all THREE sequential steps, in reverse order of likelihood: the alert
        // delivery stop sits between the other two and is capped at [`ALERT_STOP_BUDGET_SECS`]
        // inside this deadline, so a cap landing on it means the flush already ate the rest.
        // Same reason as vike-tradehub's: an ABORT returns at once and means the teardown
        // panicked, which is a different fact from outrunning the budget.
        ShutdownOutcome::Aborted { stage } => tracing::error!(
            stage = ?stage,
            "recorder: teardown ABORTED — the shutdown orchestration panicked while {}; a \
             compaction pass may be incomplete",
            stage.describe()
        ),
        ShutdownOutcome::HardCapped { .. } => tracing::warn!(
            deadline_secs = budget.as_secs(),
            feed_stop_ms = feed_stop.as_millis() as u64,
            "recorder: teardown hard-capped at its deadline — a compaction pass was abandoned \
             (restartable); if the alert delivery stop was still running, its workers are left to \
             exit on their own and a page mid-POST may not have arrived; if a flush was still \
             running, check the store's manifest on next open"
        ),
    }
    // Release any loser of the claim above — see `vike_ops::stop`'s module doc. Announced on BOTH
    // outcomes: a hard-capped teardown has finished waiting, which is the only thing a loser can act
    // on, and parking it past this point would add one bound to another.
    stop.finish_teardown();
    // ⚠ The dry-run verdict is reported AFTER the teardown, never instead of it: a `--once` run
    // that resolved nothing has still opened the store and may still hold buffered rows, and
    // returning early would skip the flush to deliver an exit code.
    Ok(match (silent_exit, dry_run_failed) {
        (Some(series), _) => Outcome::Silent(series),
        (None, Some(reasons)) => Outcome::DryRunFailed(reasons),
        (None, None) => Outcome::Stopped,
    })
}

/// How long the BOUNDED half of the teardown gets, given what the unbounded half actually cost.
///
/// The feed unsubscribe cannot be capped (a non-`Send` `VenueFeed` cannot move onto the
/// orchestration thread — see [`FEED_STOP_BUDGET_SECS`]), so it is measured and then SUBTRACTED, and
/// this is that arithmetic, pure so it is tested rather than trusted:
///
/// * inside its budget ⇒ the flush keeps its full [`SHUTDOWN_DEADLINE_SECS`], and the total stays at
///   or under [`TOTAL_STOP_BUDGET_SECS`];
/// * over its budget ⇒ the overrun comes out of the flush, so the total still AIMS at
///   [`TOTAL_STOP_BUDGET_SECS`] instead of sailing past it by whatever the feeds cost;
/// * far over ⇒ [`MIN_FLUSH_BUDGET_SECS`] is the floor. Handing the flush zero seconds because
///   something else was slow would guarantee the data loss the whole bound exists to prevent, and
///   the total is already past `TimeoutStopSec=` at that point anyway — the daemon says so out loud
///   rather than pretending the arithmetic held.
fn remaining_teardown_budget(feed_stop: Duration) -> Duration {
    let total = Duration::from_secs(TOTAL_STOP_BUDGET_SECS);
    let left = total.saturating_sub(feed_stop);
    left.clamp(
        Duration::from_secs(MIN_FLUSH_BUDGET_SECS),
        Duration::from_secs(SHUTDOWN_DEADLINE_SECS),
    )
}

/// The webhook targets a silent-series alert may be delivered to, from the credential store.
///
/// Read HERE, in the binary, which is where the settings-registry rule puts an I/O-performing
/// configuration read — `vike_alerting::webhook_configs_from_env` is PURE over a caller-supplied
/// map and the alerting library never opens a store itself (its one self-sweeping wrapper was
/// deleted for exactly that reason).
///
/// The PROCESS environment is layered OVER the store, and that order is the point on this daemon:
/// the recorder runs under systemd, where `Environment=` / `EnvironmentFile=` is the channel an
/// operator has, while the project store is the channel a workstation has. Absent both ⇒ an empty
/// list ⇒ log-only delivery, the absent-credentials-is-the-gate idiom.
///
/// A store that exists and cannot be read is reported and treated as empty rather than failing the
/// mount: a missing pager must never stop a recorder from recording.
///
/// `settings_dir` is `$VIKE_SETTINGS_DIR` as `main` resolved it. ⚠ It used to be a hard-coded
/// `None` — the walk with no override — so the unit's `Environment=VIKE_SETTINGS_DIR=` line named a
/// directory this daemon never consulted for its own credentials.
fn webhook_targets(
    settings_dir: Option<&str>,
    env: &std::collections::HashMap<String, String>,
) -> Vec<vike_alerting::WebhookConfig> {
    let mut vars = match vike_secrets::resolve_project(settings_dir) {
        Ok(resolved) => resolved.secrets.into_map(),
        Err(e) => {
            tracing::warn!(error = %e, "recorder: credential store unreadable — alerting will \
                 deliver to the log only");
            Default::default()
        }
    };
    vars.extend(env.clone());
    vike_alerting::webhook_configs_from_env(&vars)
}

/// The per-tick OBSERVATION column. Takes a slice rather than owning the ticks: the same
/// `Vec<FeedTick>` then feeds `alerts::resolve_tick` and, under `--once`,
/// `runtime::dry_run_failures`. It used to CONSUME them, which is why the tick's content could
/// never reach the exit status.
fn report(ticks: &[FeedTick]) {
    for t in ticks {
        match t {
            FeedTick::Reconciled { venue, symbols, report, .. } => {
                if !report.is_quiet() {
                    tracing::info!(
                        %venue,
                        symbols,
                        started = report.started.len(),
                        stopped = report.stopped.len(),
                        failed = report.failed.len(),
                        "recorder: subscriptions changed"
                    );
                }
                for (sym, stream, err) in &report.failed {
                    tracing::warn!(%venue, %sym, stream = stream.as_str(), error = %err,
                        "recorder: subscribe failed — retrying next tick");
                }
                for stream in &report.learned_unsupported {
                    tracing::info!(%venue, stream = stream.as_str(),
                        "recorder: venue serves no such stream — not recorded");
                }
            }
            FeedTick::ResolveFailed { venue, error, .. } => {
                // Subscriptions were left untouched; this is a retry, not a gap. ⚠ UNCHANGED, and
                // deliberately: for a venue that HAS resolved before, that sentence is exactly
                // right. The venue that has NEVER resolved is escalated separately, by
                // `alerts::resolve_tick`, to `error!` plus a real alert.
                tracing::warn!(%venue, %error,
                    "recorder: could not resolve the desired set — subscriptions unchanged");
            }
        }
    }
}

/// Reads control words off stdin. EOF sets the stop flag ONLY on a TTY — see the module doc.
fn spawn_stdin_control(stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let is_tty = std::io::stdin().is_terminal();
        control_loop(std::io::stdin().lock(), is_tty, &stop);
    });
}

/// The control channel's whole decision, as a function of the LINES and one boolean — so the
/// non-tty-EOF rule can be tested instead of trusted.
///
/// That rule is the one a reader gets wrong: **EOF is a stop only on a TTY.** Under systemd stdin is
/// `/dev/null` and reads EOF at once, so treating EOF as a stop would exit the daemon at startup,
/// every start, on every box. It is not a hypothetical either — it is why the stdio channel could
/// never be the systemd stop path, and therefore why `vike_ops::stop` exists.
///
/// Split out of [`spawn_stdin_control`] purely for testability: that function is three lines of
/// stdin wiring, and this is the part with a rule in it.
fn control_loop(reader: impl BufRead, is_tty: bool, stop: &AtomicBool) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "quit" | "shutdown" | "stop" | "exit" => {
                stop::request_stop(stop);
                return;
            }
            "" => {}
            other => {
                eprintln!("vike-recorder: unknown control word `{other}` (quit|shutdown|stop)")
            }
        }
    }
    if is_tty {
        stop::request_stop(stop);
    } else {
        tracing::info!(
            "stdin closed (non-tty) — the recorder keeps recording headless; stop via SIGTERM (unix) / Ctrl-C (windows)"
        );
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn the_profile_path_is_required() {
        assert!(Args::parse(argv(&["--once"])).is_err());
    }

    #[test]
    fn arguments_parse() {
        let parsed = Args::parse(argv(&["--profile", "r.toml", "--tick-secs", "5", "--once"]))
            .expect("a full command line parses");
        let Parsed::Run(a) = parsed else {
            panic!("a full command line is a run, not a help/version request");
        };
        assert_eq!(a.profile, PathBuf::from("r.toml"));
        assert_eq!(a.tick, Duration::from_secs(5));
        assert!(a.once);
    }

    /// `--help` is neither a run nor an error — the third outcome, which used to be spelled as an
    /// empty `Err` and so exited 2. The exit status and stream it produces are asserted over the
    /// shipped binary in `tests/help_cli.rs`; this pins the parse result it rests on.
    #[test]
    fn help_is_a_third_outcome_not_an_error() {
        for flag in ["-h", "--help"] {
            assert!(
                matches!(Args::parse(argv(&[flag])), Ok(Parsed::Help)),
                "{flag} must parse to Ok(Parsed::Help)"
            );
        }
    }

    /// …and `--version` is the FOURTH. It used to fall into `unknown argument`, so `vike-recorder
    /// --version` exited 2 on stderr. `-V`, never `-v`: lowercase is verbosity everywhere else.
    #[test]
    fn version_is_a_fourth_outcome_not_an_unknown_argument() {
        for flag in ["-V", "--version"] {
            assert!(
                matches!(Args::parse(argv(&[flag])), Ok(Parsed::Version)),
                "{flag} must parse to Ok(Parsed::Version)"
            );
        }
        // …and the lowercase spelling is still NOT a version request (it is free for verbosity).
        assert!(Args::parse(argv(&["-v"])).is_err(), "-v must stay an unknown argument");
    }

    /// A zero tick would spin the resolver flat out against the venue's directory API.
    #[test]
    fn a_zero_tick_is_rejected() {
        assert!(Args::parse(argv(&["--profile", "r.toml", "--tick-secs", "0"])).is_err());
    }

    #[test]
    fn an_unknown_argument_is_rejected_rather_than_ignored() {
        assert!(Args::parse(argv(&["--profile", "r.toml", "--turbo"])).is_err());
    }

    /// Exiting on silence is OPT-IN. A venue can be legitimately quiet, and a quiet market killing
    /// a daemon that records five healthy ones — then crash-looping under `Restart=on-failure` —
    /// would be a worse failure than the one being fixed.
    #[test]
    fn exit_on_silence_is_off_unless_asked_for() {
        let Ok(Parsed::Run(a)) = Args::parse(argv(&["--profile", "r.toml"])) else {
            panic!("a plain command line is a run");
        };
        assert!(!a.exit_on_silence, "the default reaction is alert, never exit");

        let Ok(Parsed::Run(a)) = Args::parse(argv(&["--profile", "r.toml", "--exit-on-silence"]))
        else {
            panic!("--exit-on-silence is a run");
        };
        assert!(a.exit_on_silence);
    }

    /// `--silent-secs 0` turns DETECTION off, so an exit gated on it could never fire. Accepting
    /// both would leave an operator believing a supervisor trigger is armed when nothing can arm
    /// it — the same class of lie the whole watchdog exists to stop telling.
    #[test]
    fn exit_on_silence_with_detection_disabled_is_rejected() {
        // `unwrap_err` would need `Parsed: Debug`; matching keeps the enum free of a derive it has
        // no other use for.
        let Err(err) =
            Args::parse(argv(&["--profile", "r.toml", "--silent-secs", "0", "--exit-on-silence"]))
        else {
            panic!("--exit-on-silence with detection off must be a usage error");
        };
        assert!(err.contains("--silent-secs"), "{err}");
    }

    // -- the stop path ---------------------------------------------------------------------------

    /// ⚠ **The regression this daemon cannot afford.** Under systemd stdin is `/dev/null`, which
    /// reads EOF the instant the daemon starts. If EOF meant "stop", the recorder would exit at
    /// startup on every box, every start — so a non-tty EOF must leave the flag DOWN and the daemon
    /// recording headless.
    #[test]
    fn a_non_tty_eof_does_not_stop_the_recorder() {
        let stop = AtomicBool::new(false);
        control_loop(std::io::Cursor::new(b"" as &[u8]), false, &stop);
        assert!(
            !stop.load(std::sync::atomic::Ordering::SeqCst),
            "a non-tty EOF must NOT stop the daemon — systemd wires stdin to /dev/null, so this \
             would exit at startup on every service box"
        );
    }

    /// …and the mirror image, so the rule above is not bought by ignoring EOF entirely: on a TTY,
    /// Ctrl-D IS the operator saying stop.
    #[test]
    fn a_tty_eof_stops_the_recorder() {
        let stop = AtomicBool::new(false);
        control_loop(std::io::Cursor::new(b"" as &[u8]), true, &stop);
        assert!(stop.load(std::sync::atomic::Ordering::SeqCst), "Ctrl-D on a TTY is a stop");
    }

    /// Every stop word raises the flag, TTY or not — the word is explicit, so the channel's
    /// tty-ness has nothing to add.
    #[test]
    fn every_control_word_stops_on_either_channel() {
        for word in ["quit", "shutdown", "stop", "exit"] {
            for is_tty in [true, false] {
                let stop = AtomicBool::new(false);
                let input = format!("{word}\n");
                control_loop(std::io::Cursor::new(input.as_bytes()), is_tty, &stop);
                assert!(
                    stop.load(std::sync::atomic::Ordering::SeqCst),
                    "`{word}` must stop the recorder (is_tty={is_tty})"
                );
            }
        }
    }

    /// A typo must not stop a daemon that is recording tape, and a blank line must not either.
    #[test]
    fn an_unknown_word_does_not_stop_the_recorder() {
        let stop = AtomicBool::new(false);
        control_loop(std::io::Cursor::new(b"\n  \nhalt\n" as &[u8]), false, &stop);
        assert!(
            !stop.load(std::sync::atomic::Ordering::SeqCst),
            "an unrecognised word is reported, never obeyed"
        );
    }

    /// The unit's declared stop timeout, read rather than restated so the two cannot drift.
    fn unit_stop_timeout_secs() -> u64 {
        let unit = include_str!("../../../deploy/vike-recorder.service");
        unit.lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix("TimeoutStopSec="))
            .expect("the shipped unit must set TimeoutStopSec= explicitly")
            .trim()
            .parse()
            .expect("TimeoutStopSec= is a plain number of seconds")
    }

    /// ⚠ **The WHOLE stop must fit inside the unit's `TimeoutStopSec=`** — every step SIGTERM starts,
    /// not just the capped suffix.
    ///
    /// This test replaces one that compared `SHUTDOWN_DEADLINE_SECS` alone against the unit and
    /// whose failure message claimed that established "SIGKILL does not cut the final flush in
    /// half". It did not: the feed unsubscribe runs FIRST and outside that cap, so the assertion
    /// bounded a suffix while naming the total — the repo's single most-repeated defect shape, in
    /// the gate written to prevent the consequence.
    ///
    /// MUTATION PROOF: raise `FEED_STOP_BUDGET_SECS` past the unit's headroom (e.g. 12 → 25) and
    /// this goes red; the deleted version stayed green through exactly that change, because the
    /// number it read never moved.
    #[test]
    fn the_whole_stop_fits_inside_the_units_stop_timeout() {
        let timeout = unit_stop_timeout_secs();
        assert!(
            TOTAL_STOP_BUDGET_SECS < timeout,
            "the TOTAL stop budget ({TOTAL_STOP_BUDGET_SECS}s = {FEED_STOP_BUDGET_SECS}s \
             unsubscribing the feeds + {SHUTDOWN_DEADLINE_SECS}s flushing, stopping alert delivery \
             and joining compaction) \
             must be strictly under the unit's TimeoutStopSec={timeout}s. Only the second half is \
             hard-capped; the first is budgeted, measured and warned on (a non-Send VenueFeed cannot \
             move onto the orchestration thread), so the unit is what actually protects the flush \
             from SIGKILL."
        );
    }

    /// The arithmetic that makes the budgeted half count: what the feed stop actually took comes
    /// OUT of the bounded half, so the total keeps aiming at the same number the unit was sized
    /// against.
    #[test]
    fn the_feed_stop_overrun_is_taken_out_of_the_flush_budget() {
        let flush = Duration::from_secs(SHUTDOWN_DEADLINE_SECS);
        let total = Duration::from_secs(TOTAL_STOP_BUDGET_SECS);

        // Fast feeds: the flush keeps its whole cap (it is a CAP, not a target — the extra seconds
        // the feeds did not use are not handed to it).
        assert_eq!(remaining_teardown_budget(Duration::ZERO), flush);
        assert_eq!(remaining_teardown_budget(Duration::from_secs(FEED_STOP_BUDGET_SECS)), flush);

        // Over budget: the overrun is subtracted, so feed_stop + budget stays at the total.
        let over = Duration::from_secs(FEED_STOP_BUDGET_SECS + 5);
        assert_eq!(over + remaining_teardown_budget(over), total);

        // Far over: the floor wins, and the daemon has already warned that the total is blown.
        let way_over = Duration::from_secs(TOTAL_STOP_BUDGET_SECS + 60);
        assert_eq!(
            remaining_teardown_budget(way_over),
            Duration::from_secs(MIN_FLUSH_BUDGET_SECS),
            "a slow feed stop must never leave the flush with nothing — that is the loss the bound \
             exists to prevent"
        );
    }

    /// The status is a DISTINCT non-zero, not the generic failure: `1` already means "the profile
    /// was bad" or "the store would not open", and a supervisor should be able to tell "the daemon
    /// worked and the data did not" apart from those.
    #[test]
    fn the_silence_exit_status_is_distinct_from_a_generic_failure() {
        assert_eq!(EXIT_SILENT, 3);
        assert_ne!(EXIT_SILENT, 0, "it must be non-zero for Restart=on-failure to see it");
        assert_ne!(EXIT_SILENT, 1, "…and distinguishable from a startup failure");
        assert_ne!(EXIT_SILENT, 2, "…and from the usage error --help/argv parsing uses");
    }

    /// The same rule for the dry run, and the reason it needed its own status: a `--once` run that
    /// resolved NOTHING used to exit **0**, so `--once && systemctl enable --now` green-lit a
    /// daemon that would record nothing. Measured on the CI box against the shipped binary, with the two
    /// halves of an A/B on one variable both exiting 0.
    #[test]
    fn the_dry_run_exit_status_is_distinct_from_every_other_status() {
        assert_eq!(EXIT_DRY_RUN, 4);
        assert_ne!(EXIT_DRY_RUN, 0, "a dry run that proved nothing must NOT look like success");
        assert_ne!(EXIT_DRY_RUN, 1, "…nor like a startup failure (bad profile, unopenable store)");
        assert_ne!(EXIT_DRY_RUN, 2, "…nor like a usage error");
        assert_ne!(EXIT_DRY_RUN, EXIT_SILENT, "…nor like --exit-on-silence");
    }
}
