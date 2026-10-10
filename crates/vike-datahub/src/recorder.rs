//! `recorder` — the data daemon's RECORDING plane: venue feeds in, Parquet out, into the very store
//! this process serves.
//!
//! # ⚠ This was `vike-recorder`'s own binary until ruling 10
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5 (owner, signed off
//! 2026-09-10): *"Both are data-management processes and neither is whole: the recorder OWNS venue
//! subscriptions and has no network surface at all; the data server has the wire and no feeds. One
//! process should own venue connections, the store, and serving — the MultiCharts QuoteManager
//! shape this project already decided to follow."* So `vike-recorder`'s `recorder_cli` became this
//! file, the `recorder` multicall verb retired into `datahub`, and `vike-recorder` is now the
//! LIBRARY this mount is assembled from — its `lib.rs` carries the other half of the argument.
//!
//! ⚠ **The justification is RESPONSIBILITY, not socket economy, and nothing here may be written as
//! if it were the latter.** Measured before the merge: on the CI box `tradehub` held bybit BTCUSDT while
//! the recorder held polymarket + binance BTCUSDT.P — three venues, overlap ZERO — and
//! `crates/vike-model/src/venues/venue_caps.rs` has fifteen fields and not one subscription counter, so on
//! those venues a duplicate subscription consumes no rationed resource. Two crates answering one
//! question is the cost this removes; a saved socket is not.
//!
//! ⚠ **What did NOT move: the trading daemon keeps its own venue subscription** (spec §0.2),
//! because `crates/vike-core/tests/runtime_latency.rs`'s `P99_BUDGET_NS` gates the core hop at
//! 10 µs and market data enters the core through `tick_sender()`, inside that measured path.
//! Nothing in this file is a route for `vike-tradehub`'s ticks.
//!
//! # The shape of the merged process
//!
//! [`arm`] first — the signal handlers, and the position is a safety property rather than a style
//! (see its own doc) — then the caller opens the ONE store, then [`record`] mounts the feeds and
//! owns the tick loop and the whole teardown. `crates/vike-datahub/src/datahub_cli.rs` runs
//! [`record`] on the MAIN thread and moves `serve_authed` onto a spawned one, deliberately that way
//! round: the serve loop has no teardown at all — it is `listener.incoming()` for the process
//! lifetime, and SIGKILL has always been its stop — while the recorder's teardown is where the
//! buffered tape is either flushed or lost. Abandoning the serve thread when the process exits is
//! byte-identical to what a `systemctl stop` did to the data server before this merge; abandoning
//! the flush is the failure this whole budget exists to prevent.
//!
//! ⚠ **A data server with NO `--record` is byte-identical to before the merge**: no handler is
//! installed, no thread is spawned, and `serve_authed` runs on the main thread exactly as it did.
//! That is not a nicety — installing a stop handler with nothing reading the flag would make
//! SIGTERM stop killing a serve-only daemon, which is a worse regression than anything this merge
//! fixes.
//!
//! Everything below is the recorder binary's own documentation, unchanged except where the merge
//! made a sentence false.
//! ```text
//! vike-backend datahub --record recorder.toml [--tick-secs N] [--silent-secs N] [--once]
//! ```
//!
//! One TOML profile names the store and what to record; every tick resolves each subscription's
//! current symbols, publishes their family membership, and drives the venue feed to exactly that
//! set. Rows land in the customer's own DataFusion+Parquet store through `RecorderSink`, one
//! commit per family rather than one per symbol.
//!
//! Design authority: `docs/superpowers/specs/2026-08-02-live-recorder-design.md`. Everything
//! interesting lives in `vike-recorder` (`config`/`membership`/`session`/`runtime`/`resolve`) and is
//! tested without a network; the one table that names a venue for recording is
//! `crates/vike-datahub/src/recording.rs`'s `build_recording_feed`, whose feeds hold their clients
//! from the process's one feed broker (`docs/decisions/0092-one-venue-client-per-process.md`); this
//! file is wiring, the tick cadence, and teardown.
//!
//! **Stop path — one stop flag, many triggers.** A control word on stdin
//! (`quit`/`shutdown`/`stop`) raises the flag; so do **SIGTERM and SIGINT**, through
//! `vike_ops::stop`'s `install_handlers`, so `systemctl stop`, a bare `kill` and Ctrl-C all reach
//! the SAME teardown the typed word does. EOF stops only on a TTY: under systemd stdin is
//! `/dev/null`, which reads EOF immediately, so treating that as a stop would make the daemon exit
//! on startup — the same trap `vike-tradehub` documents.
//!
//! Until that handler existed, SIGTERM reached none of the teardown below: every one of [`record`]'s
//! stop steps — `stop_all`, the final `RecorderHandle` flush, the dropped-row report, the
//! `MaintenanceScheduler` join — was skipped, and the flush is where the tape was lost, on every
//! restart of every box. `docs/ops/recorder-deploy.md` quantifies the rows;
//! `docs/ops/graceful-stop.md` carries the the CI box measurements and the decision;
//! `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` pins the wiring. On Windows there is no signal to
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
//! series is always LOGGED and always ALERTED — both by `vike_recorder::alerts::watchdog_tick`,
//! which is in the LIBRARY so a test drives it (log-only delivery until a webhook target is
//! named; the `[alerting]` profile table names them). EXITING is
//! opt-in (`--exit-on-silence`, [`EXIT_SILENT`]), because a venue can be legitimately quiet: an
//! illiquid market, a closed session, a Polymarket window between rotations. Making a quiet venue
//! kill the daemon that records five other healthy ones would be a worse failure than the one being
//! fixed, and under `Restart=on-failure` it would crash-loop. ⚠ Since the merge that daemon is also
//! SERVING, so the same flag now takes the data wire down with the feeds — one more reason it stays
//! off unless an operator asks for it.
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
//! the tick's content rather than through that grace — a dry run is one tick. ⚠ `--once` binds NO
//! listener: it is a commissioning check an operator runs by hand while the daemon it is checking
//! is usually already running, and a dry run that failed on `EADDRINUSE` would be answering a
//! question nobody asked.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vike_data::{DataFusionHist, MaintenanceScheduler, RecorderConfig, RecorderSink};
use vike_model::now_ms;
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_ops::stop::{self, StopSignal};
use vike_recorder::alerts::{self, RecorderAlerts};
use vike_recorder::runtime::RecorderRuntime;
use vike_recorder::{Membership, Stream, config::RecorderProfile, liveness};

mod control;
mod profile;

use self::control::{report, spawn_stdin_control, webhook_targets};
pub use self::profile::{
    load_and_check_active_profile_row, load_and_check_profile, load_and_check_profile_row,
    one_store_root,
};

/// How often the desired set is re-resolved. A Polymarket window is 5 minutes and the planner holds
/// the NEXT one too, so 30s is many chances to see a new window listed before the current expires,
/// while a cached slug costs no request at all.
pub const DEFAULT_TICK_SECS: u64 = 30;

/// Warn when a subscribed series has received no rows for this long (`--silent-secs`; `0` = off).
///
/// **Five minutes, chosen against the slowest thing a HEALTHY recorder does.** A Polymarket
/// up/down family rotates its tokens every 5 minutes and a freshly-opened window can be genuinely
/// quiet for a stretch, so a threshold at or below the rotation period would cry wolf every cycle.
///
/// On by DEFAULT, deliberately: the failure it catches is silent by nature — a feed that is
/// subscribed, connected and receiving nothing raises no error and loses no rows — so an operator
/// who has to know to switch this on is exactly the operator who will not.
pub const DEFAULT_SILENT_SECS: u64 = 300;

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
/// Seconds rather than a `Duration` constant so `crates/vike-ops/tests/docs/docs_constants_gate.rs` can
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
/// ⚠ **The scope is BOTH venues this daemon can record**, because
/// `crates/vike-datahub/src/recording.rs`'s `supported` lists two and only one of them is binance.
/// The rows below cover the shared drivers, the binance family AND polymarket's own shard body — a
/// derivation over one venue is not a ceiling for a daemon running the other, and for three rounds
/// this table was exactly that while the CI box was recording polymarket.
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
/// the `flags.record_properties` row arms the properties recorder), and a shard reseats many tokens at once
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
/// `crates/vike-ops/tests/venues/feed_stop_windows_gate.rs` re-reads this table and re-scans the source: it
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
///   `crates/vike-ops/tests/venues/feed_stop_windows_gate.rs`.
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
/// `deploy/vike-datahub.service`'s `TimeoutStopSec=` must exceed** — the shipped unit RECORDS, and
/// recording is the only shape that installs a handler at all — and the number the loser of a
/// teardown claim waits for before it is allowed to end the process
/// (`vike_ops::stop::StopSignal::await_teardown`) — the two must be the same budget, or a second
/// stop route could exit while the first is still flushing.
///
/// ⚠ **"WHOLE" MEANS FROM THE FLAG BEING OBSERVED, NOT FROM THE SIGNAL ARRIVING**, and the
/// difference is a declared blind spot rather than a covered one. `run`'s loop calls
/// `report(rt.tick(now_ms()))` and checks `stop.is_requested()` AFTERWARDS, so a signal landing
/// mid-tick waits out that tick first. That prefix is unbounded here in a way the feed stop is not:
/// `rt.tick` → `RollingFamily::desired` → `RollingWindowPlanner::plan` performs a blocking,
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
/// `vike_recorder::alerts::RecorderAlerts` now registers those targets as `vike_alerting::QueuedSink`s, so
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
/// **Why there are threads to stop at all.** `vike_recorder::alerts::RecorderAlerts` registers its webhook
/// targets as `vike_alerting::QueuedSink`s, because the raw sink's `deliver` is a blocking HTTP
/// POST bounded only by its transport's 10 s global timeout and the tick loop below is the same
/// loop that polls the stop flag — one alert per silent series, times one POST per target, parked
/// that loop for tens of minutes on the first tick of a venue outage. `vike_recorder::alerts`' module doc
/// carries the arithmetic.
///
/// **Why it is SMALL, and why the step is ordered where it is.** The normal case is an IDLE worker
/// — the queue is empty in every tick that raised nothing — and an idle worker stops within one
/// `vike_alerting::STOP_POLL` (a tenth of a second; it re-reads its flag only when its receive
/// poll returns, so this is a poll, not microseconds), paid once PER TARGET because
/// `vike_recorder::alerts::stop_delivery` stops the handles one after another: two targets are a few
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

/// What `--record` and its four companion flags resolved to.
///
/// A PLAIN struct rather than a parser: `crates/vike-datahub/src/datahub_cli.rs` owns the whole
/// command line, in ONE parser, feature-free — so a build without `record` answers `--help` with
/// the identical text and rejects the identical typos, and only RUNNING a recording needs the
/// feature. That split is the same property `short_circuit` already held for `--help`, applied to
/// the flags the merge added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordArgs {
    /// **WHERE the profile came from, as one line for a message** — `--record <path>`,
    /// `--recorder-profile <name>`, or `--recorder-profile` with no value (the ACTIVE row). A
    /// `String` rather than the `PathBuf` it was until 2026-09-16, because a profile now has
    /// SEVERAL possible sources and a path could only ever describe one of them. [`record`] reads
    /// the profile itself from the already-validated `RecorderProfile` it is handed; this field is
    /// provenance, never an input to a read.
    pub profile: String,
    /// How often the desired symbol set is re-resolved.
    pub tick: Duration,
    /// Run ONE tick and exit — the commissioning dry run. Binds no listener; see the module doc.
    pub once: bool,
    /// Warn when a subscribed series has received no rows for this long; 0 disables.
    pub silent_secs: u64,
    /// Stop with [`EXIT_SILENT`] when a series is silent. OFF by default — see the module doc.
    pub exit_on_silence: bool,
}

/// How a completed [`record`] ended. `Silent` exists only so the caller can map it to
/// [`EXIT_SILENT`] — it is not an error (nothing failed; the daemon did exactly what it was asked),
/// which is why it is an `Ok` variant rather than an `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    Stopped,
    /// `--exit-on-silence` was set and these series were receiving no rows.
    Silent(Vec<String>),
    /// `--once` ran one tick and it proved nothing — one reason per feed that would record no
    /// data. See [`EXIT_DRY_RUN`] and `vike_recorder::runtime::dry_run_failures`.
    DryRunFailed(Vec<String>),
}

/// Install the shared stop handlers and hand back the flag every teardown below reads.
///
/// ⚠ **CALL THIS FIRST — before the store is opened, before the listener binds, before the
/// compaction scheduler starts, before the writer thread spawns and before a single feed is
/// built.** Every one of those can block (a store open takes a per-series advisory lock; a bind can
/// fail slowly), and until the handler is installed SIGTERM still has the OS default disposition:
/// the process dies where it stands with no teardown at all. The handler only stores a bool, so
/// installing it costs nothing and can never be too early — but it can very easily be too late,
/// which is the shape a reviewer found in the first cut of this change on the sibling daemon.
///
/// ⚠ It is a SEPARATE function from [`record`] because the merge moved the store open OUT of the
/// recorder: the data server resolves and opens the one store, and if the handlers were installed
/// inside [`record`] that open would sit in front of them — reintroducing exactly the window this
/// doc says must not exist. `datahub_cli` calls this immediately after `vike_log::init` (the
/// outcome is LOGGED, so a subscriber has to exist first) and passes the signal down.
///
/// ⚠ And it is called ONLY when a recording was asked for. A serve-only data server installs no
/// handler, because a raised flag nothing reads would take SIGTERM's default kill away from a
/// process that has no teardown to run instead.
pub fn arm() -> StopSignal {
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
    stop
}

/// Mount the venue feeds over an ALREADY-OPEN store, run the tick loop, and own the teardown.
///
/// `profile` is [`load_and_check_profile`]'s already-validated result, threaded rather than read —
/// see that function for why every refusal it performs has to happen before this one is reachable.
///
/// `store` is the data server's own handle — the same `Arc<DataFusionHist>` `serve_authed` answers
/// from — which is what makes a recorded row queryable on the next connection without a second
/// process, a second open and a second per-series lock. [`one_store_root`] is what guarantees the
/// caller opened the root this profile names.
///
/// `stop` is [`arm`]'s signal, threaded rather than created here: see [`arm`] for why the handler
/// must already be installed by the time this function starts blocking on anything.
///
/// `settings_dir` is `$VIKE_SETTINGS_DIR` as the boot resolved it, `cwd` is the composition root's
/// one `current_dir()` answer, and `env` is its ONE environment sweep — all three PARAMETERS, so
/// this function reads no ambient state and the daemon keeps one answer for where its project is.
///
/// `broker` is the process's one owner of venue clients
/// (`docs/decisions/0092-one-venue-client-per-process.md`): every feed mounted here holds a
/// `crate::feeds::FeedHandle` on it rather than a client of its own, so a key the market-data plane
/// also holds is subscribed ONCE.
pub fn record(
    args: &RecordArgs,
    profile: RecorderProfile,
    store: Arc<DataFusionHist>,
    stop: &StopSignal,
    settings_dir: Option<&str>,
    cwd: Option<&Path>,
    env: &std::collections::HashMap<String, String>,
    broker: &Arc<crate::feeds::FeedBroker>,
) -> Result<RecordOutcome, String> {
    // ⚠ THE ONE STORE, ASKED A SECOND TIME AND ON PURPOSE. `load_and_check_profile` answered this
    // against the root the server RESOLVED, before anything bound; this asks it of the handle that
    // was actually OPENED, which is the object rows land in. The two cannot disagree today (the
    // caller opens exactly the root it resolved), and that is the point: this is the library-side
    // invariant, so a future caller that opens something else is refused rather than silently
    // recording into a store the wire does not answer from. It is a pure string comparison.
    one_store_root(&profile.store, store.root(), cwd)?;

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
    // The recorder's sink joins the broker BEFORE any feed exists: a `PerHolder` venue's client is
    // built with it on the first subscribe, and a `Shared` venue's `RoutingSink` forwards to it only
    // the keys the recorder holds (`crate::feeds`' module doc carries both rules).
    broker.attach_sink(crate::feeds::Holder::Rec, sink);

    let mut watch = liveness::SilenceWatch::new();
    // The FEED-level twin: which venues have NEVER resolved a symbol. See
    // `vike_recorder::liveness::ResolveWatch` for why the series watch above cannot answer it.
    let mut resolve_watch = liveness::ResolveWatch::new();
    // The delivery half of the watchdog. Mounted UNCONDITIONALLY (an absent `[alerting]` table
    // means defaults, not off): with no webhook target it delivers to the log, which is where the
    // warning already went — naming a target escalates the same alert to a pager.
    let mut alerts = RecorderAlerts::mount(&profile.alerting, webhook_targets(settings_dir, env));
    let mut rt = RecorderRuntime::new(membership, Stream::ALL.to_vec());
    for sub in &profile.subscribe {
        let feed = crate::recording::build_recording_feed(sub, broker)?;
        tracing::info!(venue = %sub.venue, family = ?sub.family, "recorder: feed mounted");
        rt.add_feed(Box::new(feed));
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
        // Judge, log, alert — all three in the LIBRARY (`vike_recorder::alerts::watchdog_tick`), so
        // the alerting call is covered by `tests/silence_alert.rs` rather than living in a binary
        // no test drives. That is exactly how the previous shape of this reaction — compute the
        // verdict, log it, discard it — went unnoticed while it fired 20 times in 24 hours.
        let silent = alerts::watchdog_tick(
            &mut watch,
            &mut alerts,
            &rt.expected_series(),
            &rt.expected_families(),
            &handle.liveness(),
            now_ms(),
            args.silent_secs,
        );
        // ⚠ Judged on THIS TICK'S CONTENT, never through `resolve_watch` — that watch's grace is
        // `--silent-secs` (300 s by default) and a dry run is one tick, so routing the verdict
        // through it would make every `--once` run pass green again for a new reason.
        if args.once {
            let failures = vike_recorder::runtime::dry_run_failures(&ticks);
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
        // ⚠ It must WAIT, and it must not return early. Returning from here returns from `record`,
        // whose caller returns from `main`, which TERMINATES THE PROCESS — killing the winner's
        // teardown at
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
        return Ok(RecordOutcome::Stopped);
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
    // The PROCESS teardown: raise every venue client's flags first, the md plane's included — the
    // caller exits after this function, and a shared client's flags can only be raised whole
    // (`vike_data::FeedRegistry::raise_stops`). Without it, a recorder key sharing a client
    // with md would unsubscribe serially, one socket read-timeout each, into `TimeoutStopSec=`:
    // a feed handle's own `begin_shutdown` raises nothing while the other plane still holds a key
    // on its client (`crate::feeds`' rule 1).
    broker.begin_shutdown_all();
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
        (Some(series), _) => RecordOutcome::Silent(series),
        (None, Some(reasons)) => RecordOutcome::DryRunFailed(reasons),
        (None, None) => RecordOutcome::Stopped,
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

#[cfg(test)]
use self::control::control_loop;
#[cfg(test)]
use self::profile::{
    no_recorder_profile_selected, no_recorder_profile_stored, no_settings_database,
};
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[path = "recorder_tests.rs"]
#[cfg(test)]
mod recorder_tests;
