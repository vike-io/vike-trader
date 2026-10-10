//! The reconciliation engine's configuration (`vike_core::spawn_recon` / `vike_core::ReconConfig`),
//! built from the settings rows the live root resolved, plus the feed-status health closure a
//! mounted `vike_core::recon_manager::ReconManager` consults at every pass boundary
//! ([`vike_core::ReconHealth`]). `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s
//! `live_mount_with` calls `vike_core::spawn_recon` with the `ReconConfig` this module produces,
//! gated on [`reconcile_gate`] — ON by DEFAULT for a mount that arms a live venue account since S2.
//! Nothing in THIS module spawns a thread or holds a driver; the mount and the driver's lifecycle
//! live in that CLI. The module came here from `vike-ops` on 2026-09-23, and before that out of the
//! GUI crate, so its `#[cfg(test)]` unit tests run in a gate.
//!
//! ## Where the values come from — rows, and nothing else
//!
//! Every knob is a settings row (decision 0111): `flags.reconcile`, `flags.reconcile_off`,
//! `flags.reconcile_balance`, `flags.reconcile_generate_missing` and the seven `config.reconcile_*`
//! rows. The live root reads them out of its ONE boot's resolved `vike_config::Settings` into a
//! [`ReconSettings`] (`crates/vike-tradehub/src/tradehub_cli.rs`'s `daemon_recon_settings`), and
//! [`build_recon_config`] is a pure function of that value. The `VIKE_RECONCILE*` variables that used
//! to carry the same knobs are refused at startup (`vike_config::REMOVED_ENV`), so a leftover line in
//! a unit cannot configure nothing in silence.
//!
//! ⚠ The env-map parsers this module used to be — `reconcile_enabled`,
//! `generate_missing_orders_enabled`, `reconcile_balance_enabled` and `quarantine_first_default`
//! — are DELETED with the environment layer. The quarantine-first default they paired with the gate
//! now lives in [`parse_policy`] itself: an unset policy IS `quarantine`, so no root can build a
//! `ReconConfig` that skipped the fold.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{ReconConfig, ReconHealth};
use vike_exec::recon::{BalanceTol, Divergence, DivergenceKind, ReconPolicy};
use vike_model::FillReport;
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::feed_status::{ConnectionState, parse_feed_status};

/// **Every reconcile setting the live root resolved**, as the rows say them — the one input
/// [`build_recon_config`] reads. `None` is "no row": each field's own default applies.
///
/// Built ONCE per mount from the boot's `vike_config::Settings`
/// (`crates/vike-tradehub/src/tradehub_cli.rs`'s `daemon_recon_settings`), so the gate, the policy
/// and the cadences can never be read from two different sources. The loader has already refused a
/// row this function would misread (an unknown policy word, a zero lookback, a negative tolerance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconSettings {
    /// `config.reconcile_policy`: one of `vike_config::config::RECONCILE_POLICIES`. `None` is
    /// `quarantine` ([`parse_policy`]).
    pub policy: Option<String>,
    /// `config.reconcile_interval_ms`: the continuous cadence; `Some(0)` turns the repeat off, `None`
    /// is 60 000.
    pub interval_ms: Option<u32>,
    /// `config.reconcile_audit_ms`: the stuck-order audit cadence; `Some(0)` turns audits off, `None`
    /// follows whatever the interval resolved to.
    pub audit_ms: Option<u32>,
    /// `config.reconcile_lookback_ms`: how far back a pass asks for reports; `None` is
    /// [`DEFAULT_LOOKBACK_MS`].
    pub lookback_ms: Option<u32>,
    /// `config.reconcile_startup_delay_ms`: the delay before the first pass; `None` is 2 000.
    pub startup_delay_ms: Option<u32>,
    /// `config.reconcile_balance_tol_abs`: the cash tolerance's absolute floor; `None` keeps
    /// `BalanceTol::default()`'s.
    pub balance_tol_abs: Option<f64>,
    /// `config.reconcile_balance_tol_rel`: the cash tolerance's relative band; `None` keeps
    /// `BalanceTol::default()`'s.
    pub balance_tol_rel: Option<f64>,
    /// `flags.reconcile_generate_missing`.
    pub generate_missing: bool,
    /// `flags.reconcile_balance`.
    pub balance: bool,
}

// ---------------------------------------------------------------------------------------------
// S2 — reconciliation is ON by default for a LIVE mount, and PAIRED with `quarantine`
// ---------------------------------------------------------------------------------------------

/// Why a composition root did — or did not — mount the reconciliation driver, as one value.
///
/// An enum rather than a `bool` because the ANSWER and the REASON are read by different people: the
/// mount needs [`Self::enabled`], and the operator reading a startup line needs to know whether
/// reconciliation is on because they asked, on because this is a live mount, or off because they
/// refused it. A bare `bool` leaves the second question unanswerable from the log — and "was the
/// venue ever queried on this box" is the first thing asked after a restart went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileGate {
    /// The operator asked outright — `flags.reconcile`. Honoured whatever the armed-live probe
    /// says, which is why the flag survives the default landing: that probe is a hand-written
    /// per-venue table (`vike_mount::venue_arming_under`), and a venue arm added without its row
    /// would make it answer zero for a genuinely live mount.
    Requested,
    /// **The S2 default**: at least one venue ACCOUNT arms live on this box, so the venue's own
    /// view is fetched at startup and at every interval instead of being assumed.
    LiveDefault,
    /// The operator refused the default — `flags.reconcile_off`. Wins over [`Self::Requested`]: a
    /// refusal is a deliberate act, and a stale `flags.reconcile` row must not silently overrule the
    /// operator who wrote the override.
    RefusedByOperator,
    /// No live venue account is armed and nothing asked. There is nothing to reconcile against — a
    /// paper mount builds no `ReconClient` at all.
    NothingArmedLive,
}

impl ReconcileGate {
    /// Whether the root should mount `vike_core::spawn_recon`.
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(self, Self::Requested | Self::LiveDefault)
    }

    /// The operator-facing reason, for the one startup line every root logs. Written in the
    /// operator's own vocabulary (the settings key they would edit), never in field names.
    #[must_use]
    pub fn disclosure(self) -> &'static str {
        match self {
            Self::Requested => {
                "reconcile ON — requested by flags.reconcile; the policy is quarantine (nothing \
                 auto-folds) unless config.reconcile_policy says otherwise"
            }
            Self::LiveDefault => {
                "reconcile ON by DEFAULT — this mount arms at least one LIVE venue account, so \
                 every start fetches that venue's own orders/positions and HOLDS every divergence \
                 for you (quarantine) — EXCEPT the BALANCE, which is seeded from the venue \
                 authoritatively on every pass under every policy and moves the equity your \
                 margin ceiling is judged against. It issues AUTHENTICATED READS against those \
                 accounts. Turn it off with flags.reconcile_off"
            }
            Self::RefusedByOperator => {
                "reconcile OFF — refused by flags.reconcile_off. This mount will NOT ask any venue \
                 what it holds, so an unclean restart can leave resting orders and position state \
                 wrong with nothing to say so"
            }
            Self::NothingArmedLive => {
                "reconcile OFF — no LIVE venue account is armed, so there is nothing to reconcile \
                 against (a paper mount builds no ReconClient)"
            }
        }
    }
}

/// **THE master-gate decision, in one place.** It was written "so the GUI and the daemon cannot
/// answer differently"; the daemon is its only caller since the GUI lost its local core (#1610),
/// and one place is still the right number for the verdict an operator reads at every start.
///
/// `requested` is the resolved `flags.reconcile` (its row, else `false`), `refused` the resolved
/// `flags.reconcile_off`, and `armed_live_accounts` the LENGTH of `vike_mount::armed_live_venues` —
/// the pure pre-mount probe of how many venue accounts this process is about to authenticate as.
/// Taking a COUNT rather than that `Vec` is what kept this module below `vike-run` while it lived
/// in `vike-ops` (the layer rule; vike-run has since merged into vike-mount, docs/decisions/0098),
/// and it makes the seam testable with no mount, no credentials and no network.
///
/// # Why the default flipped (S2)
///
/// Engines re-seed some local state on restart, but until this landed the venue's own view was
/// fetched only when an operator remembered to turn reconcile on by hand. A live daemon that
/// restarts and never asks the venue what it holds is trading against a BELIEF: resting orders it
/// has forgotten, a position it thinks is flat, a fill that landed while it was down. Comparable
/// platforms fetch cash, open orders and holdings from the brokerage at every live setup,
/// unconditionally; this is that, minus the folding.
///
/// # What makes the default safe, and what it is NOT
///
/// - It is paired with [`parse_policy`]'s quarantine-first default, so a default-on pass folds no
///   DIVERGENCE —
///   every one is HELD for an operator confirm. [`auto_applied_kinds`] over the resulting policy
///   is EMPTY, which is what the tests assert rather than restating a kind list.
///   ⚠ **"Folds no divergence" is not "writes nothing", and the gap is the BALANCE.** The
///   authoritative balance seed in `crates/vike-core/src/runtime/reconcile.rs`'s fold sits OUTSIDE the
///   divergence pipeline a policy governs: it is gated on the venue having answered with a balance
///   and on nothing else, so it applies under `quarantine` exactly as under `hybrid`, setting
///   `Account::balance` and flipping `Account::balance_mode` to `Authoritative` every pass. That
///   mode makes resolved equity `balance + unrealized`, which is the term the pre-trade gate's
///   margin lane judges against — so this default moves an admission ceiling, and a third party's
///   deposit on a shared account moves it by the full amount. This sentence read "folds NOTHING"
///   until 2026-09-06, and that is the claim `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` was written on.
/// - It is scoped to LIVE mounts BY CONSTRUCTION, twice over: `armed_live_accounts` is `0` on a
///   paper box, and even a forced-on gate builds no `ReconClient` for a venue whose arming ceiling
///   or absent credentials left it on paper — every `vike_mount::make_engine` recon factory sits
///   inside a credentialed arm.
/// - ⚠ It is NOT free. A default-on pass issues AUTHENTICATED READ calls against every armed
///   account, at startup and then every `config.reconcile_interval_ms` (default 60 s): a handful of
///   REST reads per venue per minute, inside every wired venue's published budget, but real traffic
///   against real accounts. **Aster has no testnet credentials configured** — the testnet itself
///   exists and is routed (`crates/bridges/aster/src/urls.rs`'s `urls_for` maps `Environment::Demo`
///   onto real testnet hosts); what the store holds is `ASTER_LIVE_*` — so on a box that arms aster
///   those are MAINNET reads. Arming aster live is already an explicit act — `policy.venues.aster`
///   must permit it and that arm posts `set_leverage` at startup — so this is not a new class of
///   contact with the account; but a demo or validation run must still keep aster out the way the
///   root `CLAUDE.md` has always said to (cap the ceiling to `paper`, or withhold `ASTER_*`).
///   `flags.reconcile_off` is the third answer, and the only one that keeps the venue armed while
///   silencing the reads.
/// - Polymarket is the one venue this default does not reach, and that is not an exception carved
///   here: that venue's reconcile has always had its own inner gate (`flags.poly_reconcile`) read in
///   its own mount (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s `poly_recon_wanted`).
///   `crates/vike-tradehub/tests/polymarket_mount.rs`'s
///   `polymarket_without_the_gates_is_inert_and_offline` passes this master gate ON and asserts no
///   handle is built.
#[must_use]
pub fn reconcile_gate(requested: bool, refused: bool, armed_live_accounts: usize) -> ReconcileGate {
    if refused {
        return ReconcileGate::RefusedByOperator;
    }
    if requested {
        return ReconcileGate::Requested;
    }
    if armed_live_accounts > 0 {
        return ReconcileGate::LiveDefault;
    }
    ReconcileGate::NothingArmedLive
}

/// The ONE startup disclosure the live root emits for [`reconcile_gate`]'s verdict — the sibling of
/// [`log_policy_effect`], and here for the same reason that one is: when there were two live roots
/// (the GUI and the daemon, until #1610), each writing its own `tracing` block would have been two
/// wordings that drift, and this one is the line an operator greps for after a restart went wrong.
///
/// Always WARN, whichever way the gate went. Both answers are news: "reconcile is on and nobody
/// asked for it" is a behaviour change an operator did not opt into, and "reconcile is off on a live
/// box" is a mount that will not notice an orphaned order. An `info` for the OFF arm would put the
/// more dangerous of the two below the level a production daemon usually keeps.
pub fn log_reconcile_gate(gate: ReconcileGate, armed_live_accounts: usize) {
    tracing::warn!(
        target: "vike_ops::reconcile",
        armed_live_accounts,
        gate = ?gate,
        "{}",
        gate.disclosure()
    );
}

/// Build the reconcile driver's [`ReconConfig`] from the resolved [`ReconSettings`] + a PER-VENUE
/// map of the live feed status strings (`venue -> Arc<Mutex<String>>`, one per credentialed feed).
/// The health probe is keyed by the venue being reconciled, so a bybit feed gap suppresses only
/// bybit's leg while binance still reconciles (see [`health_from_feed_status`]). A venue with no
/// entry in the map maps to [`ReconHealth::Healthy`] — an un-gated venue is never blocked.
///
/// What each setting resolves to (the parenthesised value is the default — what an operator gets
/// with no row):
/// - `config.reconcile_policy` (**`quarantine`**) — `hybrid` / `synthesize` / `quarantine` /
///   `external-quarantine`, [`parse_policy`]. ⚠ `hybrid` and `synthesize` auto-apply
///   `PositionDrift` ([`auto_applied_kinds`] over the resolved policy is the authority, never a
///   restated kind list); read the root `CLAUDE.md`'s rollout rule before writing either.
/// - `config.reconcile_lookback_ms` (`3600000`, one hour) — how far back to request order/fill
///   reports at each pass (`ReconConfig::lookback_ms`, [`lookback_ms`]).
/// - `config.reconcile_startup_delay_ms` (`2000`) — delay before the first pass.
/// - `config.reconcile_interval_ms` (`60000`) — continuous re-reconcile cadence; `0` disables it
///   (`None`).
/// - `config.reconcile_audit_ms` (whatever the interval resolved to, Some or None) — the lighter
///   stuck-order-watchdog poke cadence; set independently of the interval, or `0` to disable audits
///   even when the interval is set.
/// - `flags.reconcile_generate_missing` (`false`) — `ReconConfig::generate_missing_orders` (adopt
///   a venue order with no local match: only a terminal order whose executions fell outside the
///   fill-report lookback synthesizes a folding accept+fill; live/fill-lane-covered orders surface a
///   dedup-keyed alert only — see `vike_exec::recon::resolve`'s module doc).
/// - `flags.reconcile_balance` (`false`) — `ReconConfig::reconcile_balance` (Feature 2: promote
///   venue balance from a silent authoritative overwrite to a first-class DIFFED dimension — venue
///   cash vs the realized-PnL-corrected local balance, drift routed through `policy`, quarantined by
///   default). `false` is byte-identical to the silent seed.
/// - `config.reconcile_balance_tol_abs` / `config.reconcile_balance_tol_rel` (`BalanceTol::default()`
///   = 1.0 abs, 1e-4 rel) — override the cash-reconcile money tolerance's bands
///   ([`ReconConfig::balance_tol`], applied by `vike_exec::recon::diff_balance`), each its own band
///   only. Consulted only while `flags.reconcile_balance` is on.
pub fn build_recon_config(
    recon: &ReconSettings,
    feed_statuses: HashMap<String, Arc<Mutex<String>>>,
) -> ReconConfig {
    let interval = cadence(recon.interval_ms, Some(Duration::from_millis(DEFAULT_INTERVAL_MS)));
    // Documented default: an unset audit cadence mirrors whatever the interval resolved to (Some
    // or None), not a fixed literal — a `config.reconcile_audit_ms` row decouples it.
    let audit_interval = cadence(recon.audit_ms, interval);
    let policy = parse_policy(recon.policy.as_deref());
    log_policy_effect(&policy);
    let def = BalanceTol::default();
    ReconConfig {
        policy,
        lookback_ms: lookback_ms(recon.lookback_ms),
        startup_delay: Duration::from_millis(u64::from(
            recon.startup_delay_ms.unwrap_or(DEFAULT_STARTUP_DELAY_MS),
        )),
        interval,
        audit_interval,
        generate_missing_orders: recon.generate_missing,
        reconcile_balance: recon.balance,
        balance_tol: BalanceTol {
            abs_floor: recon.balance_tol_abs.unwrap_or(def.abs_floor),
            rel_frac: recon.balance_tol_rel.unwrap_or(def.rel_frac),
        },
        // Per-venue health: look up the venue's own feed status; a venue with no handle in the map
        // (e.g. an un-fed reconcile-only venue) reads Healthy so it is never blocked.
        health: Some(Arc::new(move |venue: &str| match feed_statuses.get(venue) {
            Some(s) => health_from_feed_status(&s.lock().unwrap()),
            None => ReconHealth::Healthy,
        })),
    }
}

/// A cadence row → `Option<Duration>`: no row is `default`, `0` is explicitly `None` (the opt-out
/// spelling both `ReconConfig::interval` and `ReconConfig::audit_interval` take), anything else is
/// that many milliseconds.
fn cadence(row: Option<u32>, default: Option<Duration>) -> Option<Duration> {
    match row {
        None => default,
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(u64::from(ms))),
    }
}

/// The documented default re-reconcile cadence, used when no `config.reconcile_interval_ms` row
/// is written: one minute.
pub const DEFAULT_INTERVAL_MS: u64 = 60_000;

/// The documented default delay before the first pass, used when no
/// `config.reconcile_startup_delay_ms` row is written: two seconds.
pub const DEFAULT_STARTUP_DELAY_MS: u32 = 2_000;

/// The documented default reconcile lookback: one hour. Named rather than repeated so
/// [`lookback_ms`] and this module's tests cannot disagree about it.
pub const DEFAULT_LOOKBACK_MS: i64 = 3_600_000;

/// `config.reconcile_lookback_ms` -> [`ReconConfig::lookback_ms`] — how far back a pass requests
/// order/fill reports, no row -> [`DEFAULT_LOOKBACK_MS`].
///
/// ⚠ **This is `pub` because the venue leg is not the only consumer the window has had, and the
/// second one carried a LITERAL.** `vike-app`'s `journal_view_provider` built the JOURNAL leg of
/// the same three-way `vike_exec::recon::diff` — and it scoped its store read with a hard-coded
/// `3_600_000` while the venue leg it is compared against read this setting. The two agreed only
/// as long as nobody set it, and the failure points COUNTER-INTUITIVELY: `diff` raises
/// `JournalDivergence` only when the journal CONTAINS the venue trade id, so a venue window WIDER
/// than the journal window turns the persistence-bug signal into a plain `MissingFill` — which
/// `hybrid` AUTO-APPLIES ([`auto_applied_kinds`]). A six-hour lookback therefore disarmed the third
/// leg for five of its six hours and folded silently instead of alerting. Both legs then resolved
/// the window through THIS function, so they could not disagree by construction.
///
/// ⚠ That journal leg went with the desktop's local core: no production root supplies a
/// `vike_core::JournalViewHook` today (`vike_core::CoreConfig::journal_view_provider` is `None` at
/// every live mount), so the venue leg is this function's only production caller. It stays `pub`
/// for the next journal leg, which owes the walk in `crates/vike-core/src/journal_view.rs` this
/// same window — that module's doc says so.
#[must_use]
pub fn lookback_ms(row: Option<u32>) -> i64 {
    row.map_or(DEFAULT_LOOKBACK_MS, i64::from)
}

// ⚠ `inflight_confirm_interval` (the in-flight-confirm cadence for
// `vike_core::CoreConfig::inflight_confirm`) is DELETED with its variable: no root ever called it,
// so `inflight_confirm` was `None` at every live mount whatever the environment said. Wiring the
// feature is a `CoreConfig` decision, taken with a settings row, not a revived variable
// (decision 0111).

/// Pure feed-status -> health mapping (unit-testable without a live `Arc<Mutex<String>>`).
///
/// Blocklist, not allowlist: only a feed status that is PROVABLY failing reads
/// [`ReconHealth::Degraded`] — everything else (including "no status yet") reads
/// [`ReconHealth::Healthy`]. This was flipped from an earlier allowlist (`Connected` alone was
/// Healthy, every other state Degraded) after a live run (2026-07-18,
/// `.superpowers/sdd/recon-live-verify-report.md`) showed bybit/okx/hyperliquid/aster
/// permanently `Degraded` and their reconcile legs suppressed EVERY cycle: those venues' market-
/// data feeds only start pumping once a chart subscribes, so with no chart open their status
/// `Mutex` never leaves its constructor default of `"connecting to {venue}…"` (see e.g.
/// `crates/bridges/bybit/src/market_feed.rs`'s `Feeds::new`) — [`ConnectionState::Connecting`]
/// forever, not a transient race. The old mapping treated that identically to a real outage and
/// disabled reconciliation for exactly the venues nobody was watching, for as long as the app ran.
///
/// The asymmetry that justifies over-permitting rather than over-suppressing: a reconcile pass
/// that runs against a feed that turns out to be down merely fails soft — `fetch_order_status_reports`
/// / `fetch_fill_reports` / `fetch_position_status_reports` / `fetch_balance` each log a
/// `"... report fetch failed: {e}"` warning and the pass is skipped for that cycle
/// (`crates/vike-core/src/recon_manager/manager.rs`) — while a pass wrongly suppressed can stay
/// suppressed indefinitely, silently defeating reconciliation with no error at all. An
/// occasional wasted/failed fetch is far cheaper than a reconcile leg that never runs.
///
/// ⚠ **"As observed live" USED TO NAME AN UNCURED PRODUCER, AND IT NO LONGER DOES.** The warning
/// above was written after the 2026-07-18 run and it came true on 2026-09-10, on bybit, for 42
/// hours: 2,516 consecutive suppressions at a one-minute cadence, zero gaps, while the venue held
/// four ESTABLISHED sockets, delivered ~2,100 core events a minute and answered its public API in
/// 204 ms — and the venue's authoritative wallet figure went unread across all 2,209 daemon
/// summaries. Exposure was zero only by luck from an unrelated subsystem (`vike_mm`'s maker was
/// HOLDING on its break-even half-spread and posted no quote).
///
/// The CAUSE was a WRITE-PATH hole, not a classification one, and it is fixed at the producer:
/// `vike_bridge_core::market_pump`'s `run_market_feed_on` disclosed only FAULTS, so a venue whose
/// healthy string was written once at spawn latched its last error forever — the pump redialled,
/// resumed streaming, and rewrote nothing. Every on-driver venue now discloses
/// `SessionStatus::Live` on the first confirmed frame of every session. Read the paragraph below
/// before concluding that closes the class.
///
/// ⚠ **WRITE-FREQUENCY ASYMMETRY — the property the success disclosure does NOT cure, and the real
/// disqualifier for oanda/deribit/ig.** A healthy lane writes its string once per SESSION (hours);
/// a faulting lane writes once per BACKOFF CYCLE (3 s on bybit's, ig's and deribit's `pump_spec`
/// rows). So on a venue whose lanes share ONE last-writer-wins string, a single permanently-broken
/// lane owns that string ~100% of the time while its healthy siblings stream perfectly, and a gate
/// sampling once a minute reads `Degraded` essentially always. The success disclosure raises a
/// healthy lane from "once ever" to "once per session" — still thousands of times rarer than its
/// faulting sibling. `vike_tradehub::feeds`'s `LiveFeeds::recon_feed_statuses` is where that
/// asymmetry decides which venues get a row at all.
///
/// Mapping:
/// - [`ConnectionState::Error`] -> [`ReconHealth::Degraded`] — an explicit fault/error/failed
///   message (ws error, seed error, subscription failed, ...); every real disconnect this
///   codebase's feeds report surfaces here (a mid-stream drop is always logged as an `"...error
///   (reconnecting): {e}"` message, which matches `Error` before it can match `Connecting`'s
///   `"reconnect"` substring — see `parse_feed_status`'s doc).
///
///   ⚠ This row used to end "— the one state that is an unambiguous, currently-observed proof of a
///   real fault", and that clause was FALSE in its load-bearing word. `Error` was unambiguous
///   about what had once happened and said nothing about whether it was CURRENT, because nothing
///   kept it current: no writer existed for the transition back. A string is only
///   "currently-observed" evidence if something observes the current state and writes it, which is
///   what the producer fix supplies and what the asymmetry above still bounds.
/// - [`ConnectionState::Disconnected`] -> [`ReconHealth::Degraded`] **only** when the raw string
///   explicitly names it (contains `"disconnected"`) — a real, spelled-out down-state. An EMPTY
///   string or `"idle"`/`"—"` also parses to this same `ConnectionState` bucket (parse_feed_status
///   deliberately collapses "no status" and "explicitly down" together for the Connections-tool
///   UI), but for the health gate those two must be told apart: empty/idle means "never reported
///   anything" (indistinguishable from "never started"), not "reported down".
/// - Everything else — [`ConnectionState::Connected`], [`ConnectionState::Connecting`] (covers
///   both the very-first-start default AND a live post-drop retry; the two are indistinguishable
///   from the status string alone, and the fail-soft argument above makes erring Healthy for both
///   the right call — see this module's/CLAUDE.md's note), and [`ConnectionState::Unknown`] — is
///   [`ReconHealth::Healthy`].
///
/// Deribit precedent unchanged: a venue absent from the feed-status map (deribit today) never
/// reaches this function at all — [`build_recon_config`]'s closure short-circuits it to `Healthy`
/// before ever calling this.
pub fn health_from_feed_status(s: &str) -> ReconHealth {
    let trimmed = s.trim();
    match parse_feed_status(trimmed) {
        ConnectionState::Error => ReconHealth::Degraded,
        ConnectionState::Disconnected if trimmed.to_lowercase().contains("disconnected") => {
            ReconHealth::Degraded
        }
        // Disconnected-but-not-literally-"disconnected" (empty / "idle" / "—"), Connecting,
        // Connected, and Unknown all read Healthy — see the mapping doc above.
        _ => ReconHealth::Healthy,
    }
}

/// The divergence kinds `policy` will fold WITHOUT an operator confirm, and that actually resolve
/// to something — i.e. what "auto-apply" costs an operator in practice. Ordered by
/// [`DivergenceKind`]'s own declaration order so the rendered line is stable. Each row is an
/// [`AutoApplied`], carrying the per-DIVERGENCE qualifier where the fold is not unconditional
/// (under `external-quarantine`, `MissingFill` folds coid-LINKED instances only — the startup
/// line must not report a kind as blanket-auto-applied when its foreign sub-case is held).
///
/// ⚠ Four kinds never appear here even when their mode IS auto-apply, because naming a kind that
/// folds nothing misinforms in exactly the direction that caused this function to be written.
/// `CANDIDATES` below is therefore the resolve-to-something set, not every kind:
/// - `MissingTerminal` — `resolve`'s `events_for` has no arm for it, so it lands in the catch-all
///   and resolves to an EMPTY event list under every policy. Reporting a kind that emits nothing as
///   auto-applied is precisely the false belief this function exists to stop: `OrphanLocalOrder`
///   was documented tree-wide as "auto-cancels every pass" while emitting nothing at all (see
///   `crates/vike-exec/tests/recon/recon_policy_pin.rs`).
/// - `OrphanLocalOrder` and `OrphanLocalPosition` — neither folds an event under ANY policy, so
///   neither can cost an operator an unreviewed fold. Both are now no-local-origin, so under
///   `hybrid`/`quarantine` they surface an event-free alert instead and under `synthesize` they are
///   a documented silent no-op; `vike_exec::recon::resolve`'s module doc is the authority on both.
/// - `JournalDivergence` — `resolve` short-circuits it into an investigative alert before the
///   policy is consulted, so its mode is decorative.
pub fn auto_applied_kinds(policy: &ReconPolicy) -> Vec<AutoApplied> {
    use DivergenceKind::*;
    /// The kinds that resolve to real events. Declaration order, so the logged line is stable.
    const CANDIDATES: [DivergenceKind; 5] =
        [MissingFill, PositionDrift, UnknownOrder, PositionOnlyExternal, BalanceDrift];
    // `mode_applies` (vike-exec) is the per-KIND authority on fold-vs-hold — never re-derive it
    // from `mode_for`, which misses the bare-`Hybrid` fallback. The per-DIVERGENCE qualifier is
    // likewise ASKED of `mode_applies_divergence` (with a coid-less probe), never restated here.
    CANDIDATES
        .into_iter()
        .filter(|&k| vike_exec::recon::mode_applies(policy, k))
        .map(|kind| AutoApplied {
            kind,
            coid_linked_only: kind == MissingFill
                && !vike_exec::recon::mode_applies_divergence(
                    policy,
                    &coidless_missing_fill_probe(),
                ),
        })
        .collect()
}

/// One row of [`auto_applied_kinds`]'s answer: a kind the policy folds without an operator,
/// qualified when the fold is per-DIVERGENCE rather than unconditional.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AutoApplied {
    pub kind: DivergenceKind,
    /// `true` ⇔ only instances whose evidence links a local client_order_id fold; a coid-less
    /// instance (the venue fill echoes no coid, so it names no local order) is held for an
    /// operator claim — `vike_exec::recon::mode_applies_divergence`'s refinement. Reachable
    /// today only for [`DivergenceKind::MissingFill`] under `external-quarantine`.
    pub coid_linked_only: bool,
}

/// Renders an unqualified row as the bare kind, so [`log_policy_effect`]'s line under the three
/// pre-existing policies is BYTE-IDENTICAL to what it printed before the qualifier existed
/// (e.g. `[MissingFill, PositionDrift]` under `hybrid`); a qualified row reads
/// `MissingFill (coid-linked only)`. Both renderings are pinned in this module's tests.
impl std::fmt::Debug for AutoApplied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.kind)?;
        if self.coid_linked_only {
            write!(f, " (coid-linked only)")?;
        }
        Ok(())
    }
}

/// A minimal coid-less `MissingFill` probe for [`auto_applied_kinds`]'s qualifier: the report
/// carries `client_order_id: None` (the one field the refinement classifies on) and placeholder
/// everything-else. Probing the real authority keeps this module from restating vike-exec's
/// refinement rule; `probe_agrees_with_the_kind_level_answer_when_nothing_refines` pins that a
/// coid-LINKED probe answers exactly as `mode_applies` does.
fn coidless_missing_fill_probe() -> Divergence {
    Divergence::MissingFill(FillReport {
        venue: String::new(),
        symbol: String::new(),
        trade_id: TradeId::prefixed("policy-probe", ""),
        venue_order_id: "".into(),
        client_order_id: None,
        side: 1,
        last_qty: 0.0,
        last_px: 0.0,
        commission: 0.0,
        commission_asset: String::new(),
        liquidity_side: LiquiditySide::Unknown,
        ts: 0,
    })
}

/// One startup line naming what the resolved policy will fold on its own — emitted from
/// [`build_recon_config`], the single place every composition root resolves the policy, so all ten
/// reconciled venues are covered with no per-venue plumbing.
///
/// This exists because the operator-facing question ("is my configuration safe?") was previously
/// answerable only by reading `resolve`, and the one venue warning that tried to answer it printed
/// IDENTICAL text for a quarantined and an auto-applying policy — while also naming the wrong kind.
fn log_policy_effect(policy: &ReconPolicy) {
    let kinds = auto_applied_kinds(policy);
    if kinds.is_empty() {
        tracing::info!(
            target: "vike_ops::reconcile",
            "reconcile policy: nothing auto-folds — every divergence is held for operator confirm"
        );
    } else {
        tracing::warn!(
            target: "vike_ops::reconcile",
            auto_applied = ?kinds,
            "reconcile policy AUTO-APPLIES these divergence kinds with no operator confirm; \
             write config.reconcile_policy external-quarantine to hold the EXTERNAL-origin kinds \
             (foreign venue activity) for operator claim, or quarantine to hold everything"
        );
    }
}

/// `config.reconcile_policy` -> [`ReconPolicy`] — and the QUARANTINE-FIRST default, which lives
/// HERE: no row is **`quarantine`** (mode `Quarantine`, no per-kind overrides — every divergence is
/// held for an operator confirm, nothing auto-folds).
///
/// ⚠ **That default is one half of a pair, and neither half is safe alone.** [`reconcile_gate`]
/// turns the driver ON for every mount that arms a live venue account; under `hybrid` that default
/// would auto-apply `PositionDrift` — rewriting local position size onto the venue's number and
/// booking realized PnL at the venue's average price, with no operator in front of it — at the
/// first pass after every restart. Fetching the venue's view is the point; folding it blind is the
/// hazard. `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` is the verdict that
/// pairs them. The fold used to be a separate map function every root had to remember to call;
/// resolving it here means no `ReconConfig` can be built without it.
///
/// A row is one of the four words `vike_config::config::RECONCILE_POLICIES` holds, refused at the
/// write and at the load otherwise, so a typo never reaches this function. The four presets are
/// CONSTRUCTED by `ReconPolicy::from_policy_name` (vike-exec), not here: `vike_docs` renders the
/// same four policies' per-kind verdicts for the published capability data, and a second
/// hand-built `ReconPolicy` in one of the two places is a second answer that can rot. `"hybrid"` is
/// [`ReconPolicy::hybrid()`] (auto-applies local-origin kinds, quarantines no-local-origin kinds);
/// `"synthesize"` is `ReconPolicy::default()` (fold everything immediately);
/// `"external-quarantine"` is [`ReconPolicy::external_quarantine`] (split-plane Pattern A: `hybrid`
/// with every EXTERNAL-origin kind held for an operator claim). A word `from_policy_name` does not
/// know — reachable only past the loader's own check — resolves to `quarantine` too, the
/// direction that folds LESS. [`log_policy_effect`]'s startup line names the resolved fold set.
#[must_use]
pub fn parse_policy(row: Option<&str>) -> ReconPolicy {
    // The last arm is unreachable: `quarantine` is one of `from_policy_name`'s words
    // (`crates/vike-exec/tests/recon/recon_policy_pin.rs` holds the four exhaustive).
    row.and_then(ReconPolicy::from_policy_name)
        .or_else(|| ReconPolicy::from_policy_name("quarantine"))
        .unwrap_or_else(ReconPolicy::hybrid)
}

#[path = "reconcile_config_tests.rs"]
#[cfg(test)]
mod reconcile_config_tests;
