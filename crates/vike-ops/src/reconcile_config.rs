//! Env-driven configuration for the reconciliation engine (`vike_core::spawn_recon` /
//! `vike_core::ReconConfig`) plus the feed-status health closure a mounted
//! `vike_core::recon_manager::ReconManager` consults at every pass boundary
//! ([`vike_core::ReconHealth`]). Task 5 of the reconciliation-activation plan built the
//! `VIKE_RECONCILE*` env-parsing + health-mapping helpers; `main.rs`'s `App::new` (in the
//! `recon_driver` block mounted after `spawn_core_multi`) calls `vike_core::spawn_recon` with the
//! `ReconConfig` this module produces, gated on `VIKE_RECONCILE=1` (see [`reconcile_enabled`]).
//! Nothing in THIS module spawns a thread or touches `App` state — it stays a pure env-parsing
//! seam; the mount + the `App.recon_driver` lifecycle live in `vike-app`'s `main.rs`. Moved down
//! from `vike-app` into this CI-able crate so its `#[cfg(test)]` unit tests actually run in a gate
//! (they never did while the module lived in the CI-excluded GUI crate).
//!
//! ## Env source — pinned to raw process env, NOT the credentials `.env` map
//! Every other `VIKE_*` FEATURE-TOGGLE flag `main.rs` reads (`VIKE_RECORD_PROPERTIES`'s
//! `vike_data::properties_rec::RECORD_PROPERTIES_ENV`, `vike_core::journal_config_from_env`'s
//! `VIKE_JOURNAL_DIR`/`VIKE_JOURNAL_SNAPSHOT_EVERY`, `VIKE_STATE_DIR`, `VIKE_SHOT`, …) reads
//! straight off `std::env::var`/`var_os` against the REAL process environment.
//! `vike_bridge_core::credentials::load_workspace_dotenv()` — the `vars: HashMap<String, String>`
//! `main.rs`'s `make_engine` threads through — is a different, narrower thing: a pure parse of
//! the workspace-root `.env` FILE (`parse_dotenv`) that never reads or merges the real process
//! env, and is used only for venue credential lookups (`{VENUE}_{ENV}_API_KEY` etc). This module
//! follows the feature-toggle precedent, NOT the credentials one.
//!
//! The helpers below still take a `&HashMap<String, String>` rather than calling
//! `std::env::var` directly — purely a TESTABILITY seam, so a test can hand in a literal map
//! instead of mutating the process-global env (`std::env::set_var` is unsound to call from
//! parallel test threads). The REAL call site (`vike-app`'s `main.rs`) builds that map from the
//! actual process env, e.g. `std::env::vars().collect::<std::collections::HashMap<_, _>>()` —
//! `load_workspace_dotenv()`'s map would silently see nothing for a shell-exported
//! `VIKE_RECONCILE=1`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::feed_status::{parse_feed_status, ConnectionState};
use vike_core::{ReconConfig, ReconHealth};
use vike_exec::recon::{BalanceTol, Divergence, DivergenceKind, ReconMode, ReconPolicy};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::FillReport;

/// The master gate for mounting `vike_core::spawn_recon`: `true` iff `VIKE_RECONCILE` is the EXACT
/// string `"1"`. Deliberately not a fuzzy truthy parse (`"true"`/`"yes"`/`"on"`/...) — one
/// unambiguous on-string to grep for in an incident, mirroring the existing `VIKE_RECORD_PROPERTIES`
/// gate's `.ok().as_deref() == Some("1")` idiom used in `main.rs`'s `make_engine`
/// (bybit/okx/binance arms).
///
/// ⚠ **No production caller since the settings files were wired.** `vike-app` and `vike-tradehub`
/// both take `vike_config::Flags::reconcile` instead — the same variable, resolved
/// `env > file > default`, so `<project>/settings/flags.toml` can arm it too. This function stays
/// because the REST of the `VIKE_RECONCILE_*` family below is still parsed here out of one map, and
/// because it is this file's own statement of the exact-`"1"` grammar. A caller that reaches for it
/// now gets the environment-only answer and will disagree with what the process actually mounted;
/// use the resolved flag.
pub fn reconcile_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get("VIKE_RECONCILE").map(String::as_str) == Some("1")
}

/// Build the reconcile driver's [`ReconConfig`] from env + a PER-VENUE map of the app's live feed
/// status strings (`venue -> Arc<Mutex<String>>`, one per credentialed feed — binance/bybit/okx).
/// The health probe is keyed by the venue being reconciled, so a bybit feed gap suppresses only
/// bybit's leg while binance still reconciles (see [`health_from_feed_status`]). A venue with no
/// entry in the map maps to [`ReconHealth::Healthy`] — an un-gated venue is never blocked. See
/// this module's doc for why `vars` must be sourced from the real process env, not the
/// credentials dotenv map.
///
/// Env knobs (all optional; defaults in parens):
/// - `VIKE_RECONCILE_POLICY` (`hybrid`) — `hybrid` / `synthesize` / `quarantine` /
///   `external-quarantine`, case-insensitive; unset or unrecognized -> `hybrid` (see
///   [`parse_policy`]).
/// - `VIKE_RECONCILE_LOOKBACK_MS` (`3600000`, one hour) — how far back to request order/fill
///   reports at each pass (`ReconConfig::lookback_ms`).
/// - `VIKE_RECONCILE_STARTUP_DELAY_MS` (`2000`) — delay before the first pass.
/// - `VIKE_RECONCILE_INTERVAL_MS` (`60000`) — continuous re-reconcile cadence; `0` (or any
///   non-positive value) disables it (`None`).
/// - `VIKE_RECONCILE_AUDIT_MS` (defaults to whatever `VIKE_RECONCILE_INTERVAL_MS` resolved to,
///   Some or None) — the lighter stuck-order-watchdog poke cadence; set independently of
///   `interval`, or explicitly `0` to disable audits even when `interval` is set.
/// - `VIKE_RECONCILE_GENERATE_MISSING` (`false`) — the EXACT string `"1"` turns on
///   `ReconConfig::generate_missing_orders` (adopt a venue order with no local match: only a
///   terminal order whose executions fell outside the fill-report lookback synthesizes a folding
///   accept+fill; live/fill-lane-covered orders surface a dedup-keyed alert only — see
///   `vike_exec::recon::resolve`'s module doc); anything else (unset, `"true"`, `"0"`, ...) stays
///   `false`, mirroring [`reconcile_enabled`]'s own exact-`"1"` idiom (see
///   [`generate_missing_orders_enabled`]).
/// - `VIKE_RECONCILE_BALANCE` (`false`) — the EXACT string `"1"` turns on
///   `ReconConfig::reconcile_balance` (Feature 2: promote venue balance from a silent
///   authoritative overwrite to a first-class DIFFED dimension — venue cash vs the
///   realized-PnL-corrected local balance, drift routed through `policy`, quarantined by default).
///   Anything else stays `false` = byte-identical legacy silent seed (see
///   [`reconcile_balance_enabled`]).
/// - `VIKE_RECONCILE_BALANCE_TOL_ABS` / `VIKE_RECONCILE_BALANCE_TOL_REL` (unset) — override the
///   abs-floor (quote units) / relative-fraction (of wallet) bands of the Feature-2 cash-reconcile
///   money tolerance ([`ReconConfig::balance_tol`], applied by `vike_exec::recon::diff_balance`).
///   Each knob overrides ONLY its own band; an unset OR malformed value keeps that band's
///   conservative default (`BalanceTol::default()` = 1.0 abs, 1e-4 rel), so both-unset is
///   byte-identical to the hard-coded default (see [`parse_balance_tol`]). Only consulted when
///   `VIKE_RECONCILE_BALANCE` is on.
pub fn build_recon_config(
    vars: &HashMap<String, String>,
    feed_statuses: HashMap<String, Arc<Mutex<String>>>,
) -> ReconConfig {
    let interval =
        parse_ms_opt(vars, "VIKE_RECONCILE_INTERVAL_MS", Some(Duration::from_millis(60_000)));
    // Documented default: an unset audit cadence mirrors whatever `interval` resolved to
    // (Some or None), not a fixed literal — set `VIKE_RECONCILE_AUDIT_MS` to decouple it.
    let audit_interval = parse_ms_opt(vars, "VIKE_RECONCILE_AUDIT_MS", interval);
    let policy = parse_policy(vars);
    log_policy_effect(&policy);
    ReconConfig {
        policy,
        lookback_ms: lookback_ms(vars),
        startup_delay: Duration::from_millis(
            parse_i64(vars, "VIKE_RECONCILE_STARTUP_DELAY_MS", 2_000).max(0) as u64,
        ),
        interval,
        audit_interval,
        generate_missing_orders: generate_missing_orders_enabled(vars),
        reconcile_balance: reconcile_balance_enabled(vars),
        balance_tol: parse_balance_tol(vars),
        // Per-venue health: look up the venue's own feed status; a venue with no handle in the map
        // (e.g. an un-fed reconcile-only venue) reads Healthy so it is never blocked.
        health: Some(Arc::new(move |venue: &str| match feed_statuses.get(venue) {
            Some(s) => health_from_feed_status(&s.lock().unwrap()),
            None => ReconHealth::Healthy,
        })),
    }
}

/// The documented default reconcile lookback: one hour. Named rather than repeated so
/// [`lookback_ms`] and this module's tests cannot disagree about it.
pub const DEFAULT_LOOKBACK_MS: i64 = 3_600_000;

/// `VIKE_RECONCILE_LOOKBACK_MS` -> [`ReconConfig::lookback_ms`] — how far back a pass requests
/// order/fill reports, absent/malformed -> [`DEFAULT_LOOKBACK_MS`].
///
/// ⚠ **This is `pub` because the venue leg is not the only consumer of the window, and the second
/// one used to carry a LITERAL.** `vike-app`'s `journal_view_provider` builds the JOURNAL leg of
/// the same three-way `vike_exec::recon::diff` — and it scoped its store read with a hard-coded
/// `3_600_000` while the venue leg it is compared against read this variable. The two agreed only
/// as long as nobody set it, and the failure points COUNTER-INTUITIVELY: `diff` raises
/// `JournalDivergence` only when the journal CONTAINS the venue trade id, so a venue window WIDER
/// than the journal window turns the persistence-bug signal into a plain `MissingFill` — which
/// `hybrid`, the default policy, AUTO-APPLIES ([`auto_applied_kinds`]). Setting
/// `VIKE_RECONCILE_LOOKBACK_MS=21600000` therefore disarmed the third leg for five of its six
/// hours and folded silently instead of alerting. Both legs now resolve the window through THIS
/// function, so they cannot disagree by construction.
pub fn lookback_ms(vars: &HashMap<String, String>) -> i64 {
    parse_i64(vars, "VIKE_RECONCILE_LOOKBACK_MS", DEFAULT_LOOKBACK_MS)
}

/// `VIKE_RECONCILE_GENERATE_MISSING` -> [`ReconConfig::generate_missing_orders`]. `true` iff the
/// EXACT string `"1"` — same deliberately-unfuzzy idiom as [`reconcile_enabled`] (one unambiguous
/// on-string to grep for in an incident); unset or any other value (`"true"`, `"yes"`, `"0"`, ...)
/// stays `false`.
pub fn generate_missing_orders_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get("VIKE_RECONCILE_GENERATE_MISSING").map(String::as_str) == Some("1")
}

/// `VIKE_RECONCILE_INFLIGHT_MS` -> [`vike_core::CoreConfig::inflight_confirm`] (recon
/// path-to-superset, F1-A): the fast in-flight-confirm cadence + age threshold. Reuses the same
/// [`parse_ms_opt`] `<= 0`/absent -> `None` opt-out spelling as `VIKE_RECONCILE_INTERVAL_MS`, but
/// with a `None` DEFAULT — the feature is OFF unless the operator names a positive cadence
/// (suggested `2000`). Unset OR `0` -> `None` -> no [`vike_core::TimerKind::InflightConfirm`] timer
/// is armed and the core is byte-identical to today. Lives in `CoreConfig`, not `ReconConfig`,
/// because the sweep runs on the core's own boundary timer wheel (F1-A) — the recon driver never
/// pokes it — so `main.rs` reads this straight into the `CoreConfig` it builds, not into
/// [`build_recon_config`]. Sourced from the REAL process env (see this module's doc), same as every
/// other `VIKE_RECONCILE_*` knob.
pub fn inflight_confirm_interval(vars: &HashMap<String, String>) -> Option<Duration> {
    parse_ms_opt(vars, "VIKE_RECONCILE_INFLIGHT_MS", None)
}

/// `VIKE_RECONCILE_BALANCE` -> [`ReconConfig::reconcile_balance`] (Feature 2: first-class cash
/// reconcile). `true` iff the EXACT string `"1"` — same deliberately-unfuzzy idiom as
/// [`reconcile_enabled`]/[`generate_missing_orders_enabled`] (one unambiguous on-string to grep for
/// in an incident); unset or any other value (`"true"`, `"yes"`, `"0"`, ...) stays `false`.
/// `false` is byte-identical to before this feature: the fold thread silently seeds venue balance
/// authoritatively every pass, exactly as it always did — no diff, no epsilon, no alert.
pub fn reconcile_balance_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get("VIKE_RECONCILE_BALANCE").map(String::as_str) == Some("1")
}

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
/// (`vike-core/src/recon_manager.rs`) — while a pass wrongly suppressed can stay suppressed
/// indefinitely (as observed live), silently defeating reconciliation with no error at all. An
/// occasional wasted/failed fetch is far cheaper than a reconcile leg that never runs.
///
/// Mapping:
/// - [`ConnectionState::Error`] -> [`ReconHealth::Degraded`] — an explicit fault/error/failed
///   message (ws error, seed error, subscription failed, ...); every real disconnect this
///   codebase's feeds report surfaces here (a mid-stream drop is always logged as an `"...error
///   (reconnecting): {e}"` message, which matches `Error` before it can match `Connecting`'s
///   `"reconnect"` substring — see `parse_feed_status`'s doc) — the one state that is an
///   unambiguous, currently-observed proof of a real fault.
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
             set VIKE_RECONCILE_POLICY=external-quarantine to hold the EXTERNAL-origin kinds \
             (foreign venue activity) for operator claim, or =quarantine to hold everything"
        );
    }
}

/// `VIKE_RECONCILE_POLICY` -> [`ReconPolicy`]. `"hybrid"` (or unset/unrecognized) is the
/// documented default choice: [`ReconPolicy::hybrid()`] auto-applies local-origin divergence
/// kinds and quarantines no-local-origin kinds for operator confirm. `"synthesize"` is
/// `ReconPolicy::default()` (mode `Synthesize`, no per-kind overrides — fold everything
/// immediately). `"quarantine"` is the inverse: mode `Quarantine`, no per-kind overrides — hold
/// every divergence for operator confirm, nothing auto-folds. `"external-quarantine"` is
/// [`ReconPolicy::external_quarantine`] (split-plane Pattern A): `hybrid` with every
/// EXTERNAL-origin kind (`vike_exec::recon::DivergenceOrigin` — foreign venue activity nothing
/// local explains) HELD for an operator claim instead of auto-applied; that constructor's doc is
/// the authority on what changes and on the claim path. Case-insensitive; ONE spelling each — no
/// aliases.
///
/// ⚠ An unrecognized value still falls back to `hybrid` (the pre-existing contract, pinned by
/// `policy_unknown_or_absent_defaults_to_hybrid`), which for a mis-spelled hold-first policy
/// (`external_quarantine`, `external`, …) means MORE auto-folding than the operator intended.
/// [`log_policy_effect`]'s startup line naming the resolved fold set is the operator's check that
/// the spelling took.
fn parse_policy(vars: &HashMap<String, String>) -> ReconPolicy {
    match vars.get("VIKE_RECONCILE_POLICY").map(|s| s.to_lowercase()).as_deref() {
        Some("synthesize") => ReconPolicy::default(),
        Some("quarantine") => ReconPolicy {
            default: ReconMode::Quarantine,
            per_kind: BTreeMap::new(),
            hold_external_instances: false,
        },
        Some("external-quarantine") => ReconPolicy::external_quarantine(),
        _ => ReconPolicy::hybrid(),
    }
}

/// Parse a millisecond-count env var into `Option<Duration>`. `key` present and parsed as a
/// non-positive integer (`<= 0`) -> explicitly `None` (the opt-out spelling used by both
/// `ReconConfig::interval` and `ReconConfig::audit_interval`). `key` absent, or present but not a
/// parseable integer, -> `default` (matches the codebase's established
/// `.ok().and_then(|s| s.parse().ok())` numeric-env-with-fallback idiom, e.g.
/// `vike_core::run_profile::journal_config_from_env`'s `VIKE_JOURNAL_SNAPSHOT_EVERY` read — a
/// malformed value falls back rather than panicking or silently zeroing).
fn parse_ms_opt(
    vars: &HashMap<String, String>,
    key: &str,
    default: Option<Duration>,
) -> Option<Duration> {
    let Some(raw) = vars.get(key) else { return default };
    match raw.trim().parse::<i64>() {
        Ok(ms) if ms > 0 => Some(Duration::from_millis(ms as u64)),
        Ok(_) => None,     // explicit 0 (or negative) => disabled
        Err(_) => default, // malformed => fall back, same idiom as elsewhere in the codebase
    }
}

/// Parse a plain `i64` env var, falling back to `default` when absent or malformed.
fn parse_i64(vars: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    vars.get(key).and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(default)
}

/// Parse a plain `f64` env var, falling back to `default` when absent, unparseable, OR non-finite
/// (a `NaN`/`inf` tolerance would silently make `diff_balance`'s `drift.abs() > threshold` compare
/// forever-false — a "never flags" footgun — so it is treated as malformed). Same absent/malformed
/// -> default idiom as [`parse_i64`], with the finite guard added because this value is money.
fn parse_f64(vars: &HashMap<String, String>, key: &str, default: f64) -> f64 {
    vars.get(key)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// `VIKE_RECONCILE_BALANCE_TOL_ABS` / `VIKE_RECONCILE_BALANCE_TOL_REL` -> [`BalanceTol`], the
/// money tolerance `vike_exec::recon::diff_balance` compares venue cash against (Feature 2). Each
/// knob overrides ONLY its own band; an unset OR malformed value keeps that band's conservative
/// default (`BalanceTol::default()` — `abs_floor` 1.0 quote unit, `rel_frac` 1e-4 of wallet). Both
/// unset ⇒ exactly [`BalanceTol::default`], so the diff is byte-identical to the pre-knob default.
fn parse_balance_tol(vars: &HashMap<String, String>) -> BalanceTol {
    let def = BalanceTol::default();
    BalanceTol {
        abs_floor: parse_f64(vars, "VIKE_RECONCILE_BALANCE_TOL_ABS", def.abs_floor),
        rel_frac: parse_f64(vars, "VIKE_RECONCILE_BALANCE_TOL_REL", def.rel_frac),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::recon::DivergenceKind;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // -- reconcile_enabled ------------------------------------------------------------------

    #[test]
    fn reconcile_enabled_true_only_for_exact_one() {
        assert!(reconcile_enabled(&map(&[("VIKE_RECONCILE", "1")])));
        assert!(!reconcile_enabled(&map(&[("VIKE_RECONCILE", "true")])));
        assert!(!reconcile_enabled(&map(&[("VIKE_RECONCILE", "0")])));
        assert!(!reconcile_enabled(&map(&[])));
    }

    // -- generate_missing_orders_enabled --------------------------------------------------------

    #[test]
    fn generate_missing_orders_enabled_true_only_for_exact_one() {
        assert!(generate_missing_orders_enabled(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "1")])));
        assert!(!generate_missing_orders_enabled(&map(&[(
            "VIKE_RECONCILE_GENERATE_MISSING",
            "true"
        )])));
        assert!(!generate_missing_orders_enabled(&map(&[(
            "VIKE_RECONCILE_GENERATE_MISSING",
            "0"
        )])));
        assert!(!generate_missing_orders_enabled(&map(&[])));
    }

    #[test]
    fn build_recon_config_wires_generate_missing_orders_from_env() {
        let off = build_recon_config(&map(&[]), HashMap::new());
        assert!(!off.generate_missing_orders, "unset defaults to false");

        let on =
            build_recon_config(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "1")]), HashMap::new());
        assert!(on.generate_missing_orders);
    }

    // -- inflight_confirm_interval (recon path-to-superset, F1-A) -------------------------------

    #[test]
    fn inflight_confirm_interval_unset_is_none() {
        // OFF by default: no knob ⇒ no fast in-flight confirm timer, byte-identical core.
        assert_eq!(inflight_confirm_interval(&map(&[])), None);
    }

    #[test]
    fn inflight_confirm_interval_zero_is_none() {
        // Explicit opt-out spelling, same as the interval/audit knobs.
        assert_eq!(inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "0")])), None);
    }

    #[test]
    fn inflight_confirm_interval_positive_value_parses() {
        assert_eq!(
            inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "2000")])),
            Some(Duration::from_millis(2000))
        );
    }

    #[test]
    fn inflight_confirm_interval_malformed_falls_back_to_none() {
        // Malformed ⇒ the `None` default (feature stays off), never a panic.
        assert_eq!(
            inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "not-a-number")])),
            None
        );
    }

    // -- reconcile_balance_enabled (Feature 2) --------------------------------------------------

    #[test]
    fn reconcile_balance_enabled_true_only_for_exact_one() {
        assert!(reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "1")])));
        assert!(!reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "true")])));
        assert!(!reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "0")])));
        assert!(!reconcile_balance_enabled(&map(&[])));
    }

    #[test]
    fn build_recon_config_wires_reconcile_balance_from_env() {
        let off = build_recon_config(&map(&[]), HashMap::new());
        assert!(!off.reconcile_balance, "unset defaults to false (byte-identical legacy seed)");

        let on = build_recon_config(&map(&[("VIKE_RECONCILE_BALANCE", "1")]), HashMap::new());
        assert!(on.reconcile_balance);
    }

    // -- balance tolerance parse (VIKE_RECONCILE_BALANCE_TOL_{ABS,REL}) --------------------------

    #[test]
    fn balance_tol_unset_is_the_conservative_default() {
        // Both knobs unset ⇒ exactly BalanceTol::default() ⇒ byte-identical to the pre-knob diff.
        assert_eq!(parse_balance_tol(&map(&[])), BalanceTol::default());
    }

    #[test]
    fn balance_tol_overrides_each_band_independently() {
        let def = BalanceTol::default();

        // ABS only: abs overridden, rel keeps its default.
        let abs_only = parse_balance_tol(&map(&[("VIKE_RECONCILE_BALANCE_TOL_ABS", "25.0")]));
        assert_eq!(abs_only.abs_floor, 25.0);
        assert_eq!(abs_only.rel_frac, def.rel_frac);

        // REL only: rel overridden, abs keeps its default.
        let rel_only = parse_balance_tol(&map(&[("VIKE_RECONCILE_BALANCE_TOL_REL", "0.0005")]));
        assert_eq!(rel_only.rel_frac, 0.0005);
        assert_eq!(rel_only.abs_floor, def.abs_floor);

        // Both set: both overridden.
        let both = parse_balance_tol(&map(&[
            ("VIKE_RECONCILE_BALANCE_TOL_ABS", "10.0"),
            ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.002"),
        ]));
        assert_eq!(both, BalanceTol { abs_floor: 10.0, rel_frac: 0.002 });
    }

    #[test]
    fn balance_tol_malformed_or_nonfinite_falls_back_per_band_no_panic() {
        let def = BalanceTol::default();
        // Malformed ABS with a valid REL: only the bad band falls back, the good one applies.
        let mixed = parse_balance_tol(&map(&[
            ("VIKE_RECONCILE_BALANCE_TOL_ABS", "not-a-number"),
            ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.003"),
        ]));
        assert_eq!(mixed.abs_floor, def.abs_floor, "malformed abs falls back to default");
        assert_eq!(mixed.rel_frac, 0.003);
        // NaN/inf parse as f64 but are rejected (a non-finite tolerance is a "never flags" footgun).
        let nan = parse_balance_tol(&map(&[
            ("VIKE_RECONCILE_BALANCE_TOL_ABS", "NaN"),
            ("VIKE_RECONCILE_BALANCE_TOL_REL", "inf"),
        ]));
        assert_eq!(nan, def);
    }

    #[test]
    fn build_recon_config_wires_balance_tol_from_env() {
        // Unset ⇒ the default rides into ReconConfig unchanged.
        let off = build_recon_config(&map(&[]), HashMap::new());
        assert_eq!(off.balance_tol, BalanceTol::default());

        // Set ⇒ the overridden tolerance is threaded into ReconConfig (and thence each pass).
        let on = build_recon_config(
            &map(&[
                ("VIKE_RECONCILE_BALANCE_TOL_ABS", "50.0"),
                ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.001"),
            ]),
            HashMap::new(),
        );
        assert_eq!(on.balance_tol, BalanceTol { abs_floor: 50.0, rel_frac: 0.001 });
    }

    // -- policy parse -------------------------------------------------------------------------

    #[test]
    fn policy_hybrid_synthesizes_local_origin_quarantines_the_rest() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "hybrid")]));
        assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
        assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
    }

    #[test]
    fn policy_synthesize_is_synthesize_default_with_no_overrides() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "synthesize")]));
        assert_eq!(p.default, ReconMode::Synthesize);
        // no hybrid overrides: even the no-local-origin kinds synthesize
        assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Synthesize);
    }

    #[test]
    fn policy_quarantine_is_quarantine_default_with_no_overrides() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "quarantine")]));
        assert_eq!(p.default, ReconMode::Quarantine);
        assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    }

    #[test]
    fn policy_unknown_or_absent_defaults_to_hybrid() {
        let unset = parse_policy(&map(&[]));
        assert_eq!(unset.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
        assert_eq!(unset.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);

        let bogus = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "bogus")]));
        assert_eq!(bogus.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
    }

    #[test]
    fn policy_parse_is_case_insensitive() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "SYNTHESIZE")]));
        assert_eq!(p.default, ReconMode::Synthesize);

        // ...including the new hold-first value (same lowercasing path as the other three).
        let eq = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "External-Quarantine")]));
        assert_eq!(eq.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
    }

    /// `external-quarantine` = `hybrid` with the EXTERNAL-origin kinds held: `PositionDrift`
    /// stops auto-applying, the already-quarantined External kinds stay held, and hybrid's
    /// local-origin folds are untouched. `ReconPolicy::external_quarantine`'s doc is the
    /// authority; the exhaustive per-kind pin lives in
    /// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
    #[test]
    fn policy_external_quarantine_holds_external_kinds_and_keeps_local_folds() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "external-quarantine")]));
        assert_eq!(p.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
        assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
        assert_eq!(p.mode_for(DivergenceKind::BalanceDrift), ReconMode::Quarantine);
        assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
        // ...and it is the ONE policy that opts in to the per-divergence refinement (the
        // coid-less MissingFill hold); the other three arms must leave the flag false, which is
        // what makes them byte-identical by construction.
        assert!(p.hold_external_instances);
        for value in ["hybrid", "synthesize", "quarantine"] {
            let flat = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", value)]));
            assert!(!flat.hold_external_instances, "{value} must not opt in");
        }
    }

    /// Near-miss spellings fall back to `hybrid` — the PRE-EXISTING unknown-value contract
    /// (`policy_unknown_or_absent_defaults_to_hybrid`), pinned separately for the one value where
    /// the fallback direction is the dangerous one: a typo'd hold-first policy AUTO-APPLIES
    /// `PositionDrift`. [`log_policy_effect`]'s startup line naming the resolved fold set is the
    /// operator's check that the spelling took.
    #[test]
    fn policy_external_quarantine_typos_fall_back_to_hybrid_which_folds_more() {
        for typo in ["external_quarantine", "externalquarantine", "external"] {
            let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", typo)]));
            assert_eq!(
                p.mode_for(DivergenceKind::PositionDrift),
                ReconMode::Synthesize,
                "{typo}: an unrecognized value keeps the documented hybrid fallback"
            );
        }
    }

    // -- what each policy actually auto-folds ---------------------------------------------------

    /// The DEFAULT policy an operator gets from an unset `VIKE_RECONCILE_POLICY` folds exactly two
    /// kinds without asking. `OrphanLocalOrder` is NOT one of them — it folds no events at all
    /// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`), which is the fact six documents got
    /// backwards. Its reclassification to a quarantined kind therefore left this list unchanged,
    /// which is the point: what it gained was an operator ALERT, not an automatic fold.
    #[test]
    fn hybrid_auto_applies_missing_fill_and_position_drift_only() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "hybrid")]));
        let kinds = auto_applied_kinds(&p);
        assert_eq!(
            kinds.iter().map(|a| a.kind).collect::<Vec<_>>(),
            vec![DivergenceKind::MissingFill, DivergenceKind::PositionDrift]
        );
        assert!(kinds.iter().all(|a| !a.coid_linked_only), "hybrid's folds are unconditional");
        // The startup line's rendering is BYTE-IDENTICAL to the pre-qualifier era — the three
        // pre-existing policies' operator-facing output must not move.
        assert_eq!(format!("{kinds:?}"), "[MissingFill, PositionDrift]");
    }

    /// `quarantine` is the only policy under which the reported list is empty — which is what makes
    /// the startup line worth printing.
    #[test]
    fn quarantine_auto_applies_nothing() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "quarantine")]));
        assert!(auto_applied_kinds(&p).is_empty());
    }

    /// `external-quarantine`'s whole point, stated as what the operator's startup line will say:
    /// only `MissingFill` still folds without a confirm — `PositionDrift` left the list (the
    /// delta from [`hybrid_auto_applies_missing_fill_and_position_drift_only`]) — and the row is
    /// QUALIFIED: only coid-LINKED fills fold; a coid-less one (a foreign order's fill) is held
    /// for an operator claim (`vike_exec::recon::mode_applies_divergence`'s refinement). The
    /// rendered vocabulary is pinned because the startup line is the operator's check that the
    /// policy took — an unqualified `MissingFill` here would claim a blanket fold that no longer
    /// happens.
    #[test]
    fn external_quarantine_auto_applies_missing_fill_coid_linked_only() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "external-quarantine")]));
        let kinds = auto_applied_kinds(&p);
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].kind, DivergenceKind::MissingFill);
        assert!(kinds[0].coid_linked_only, "the foreign sub-case is held, so the row qualifies");
        assert_eq!(format!("{kinds:?}"), "[MissingFill (coid-linked only)]");
    }

    /// The probe asks the real authority instead of restating its rule — and for a coid-LINKED
    /// fill (nothing to refine) the instance-level answer must equal the kind-level one under
    /// every reachable policy, which is what lets [`auto_applied_kinds`] keep filtering rows on
    /// `mode_applies` alone.
    #[test]
    fn probe_agrees_with_the_kind_level_answer_when_nothing_refines() {
        let linked = match coidless_missing_fill_probe() {
            Divergence::MissingFill(mut f) => {
                f.client_order_id = Some("c-1".into());
                Divergence::MissingFill(f)
            }
            _ => unreachable!("the probe is a MissingFill by construction"),
        };
        for value in ["hybrid", "synthesize", "quarantine", "external-quarantine"] {
            let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", value)]));
            assert_eq!(
                vike_exec::recon::mode_applies_divergence(&p, &linked),
                vike_exec::recon::mode_applies(&p, DivergenceKind::MissingFill),
                "{value}: a coid-linked fill must answer at the kind level"
            );
        }
    }

    #[test]
    fn synthesize_auto_applies_every_venue_reported_kind() {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "synthesize")]));
        let kinds = auto_applied_kinds(&p);
        assert_eq!(
            kinds.iter().map(|a| a.kind).collect::<Vec<_>>(),
            vec![
                DivergenceKind::MissingFill,
                DivergenceKind::PositionDrift,
                DivergenceKind::UnknownOrder,
                DivergenceKind::PositionOnlyExternal,
                DivergenceKind::BalanceDrift,
            ]
        );
        assert!(kinds.iter().all(|a| !a.coid_linked_only), "synthesize folds unconditionally");
    }

    // -- interval / audit parse ----------------------------------------------------------------

    const DEFAULT_INTERVAL: Option<Duration> = Some(Duration::from_millis(60_000));

    #[test]
    fn interval_zero_is_none() {
        let v = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0")]);
        assert_eq!(parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL), None);
    }

    #[test]
    fn interval_absent_defaults_to_60s() {
        let v = map(&[]);
        assert_eq!(
            parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL),
            DEFAULT_INTERVAL
        );
    }

    #[test]
    fn interval_positive_value_parses() {
        let v = map(&[("VIKE_RECONCILE_INTERVAL_MS", "5000")]);
        assert_eq!(
            parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL),
            Some(Duration::from_millis(5000))
        );
    }

    #[test]
    fn audit_ms_defaults_to_whatever_interval_resolved_to() {
        // interval unset (defaults 60s) -> audit unset mirrors it.
        let v1 = map(&[]);
        let interval1 = parse_ms_opt(&v1, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
        assert_eq!(parse_ms_opt(&v1, "VIKE_RECONCILE_AUDIT_MS", interval1), interval1);

        // interval explicitly disabled (0) -> unset audit mirrors None too.
        let v2 = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0")]);
        let interval2 = parse_ms_opt(&v2, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
        assert_eq!(interval2, None);
        assert_eq!(parse_ms_opt(&v2, "VIKE_RECONCILE_AUDIT_MS", interval2), None);

        // audit set independently of a disabled interval.
        let v3 = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0"), ("VIKE_RECONCILE_AUDIT_MS", "30000")]);
        let interval3 = parse_ms_opt(&v3, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
        assert_eq!(interval3, None);
        assert_eq!(
            parse_ms_opt(&v3, "VIKE_RECONCILE_AUDIT_MS", interval3),
            Some(Duration::from_millis(30_000))
        );
    }

    // -- lookback / startup delay --------------------------------------------------------------

    #[test]
    fn lookback_ms_default_and_override() {
        assert_eq!(parse_i64(&map(&[]), "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 3_600_000);
        let v = map(&[("VIKE_RECONCILE_LOOKBACK_MS", "7200000")]);
        assert_eq!(parse_i64(&v, "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 7_200_000);
    }

    #[test]
    fn malformed_numeric_env_falls_back_to_default() {
        let v = map(&[("VIKE_RECONCILE_LOOKBACK_MS", "not-a-number")]);
        assert_eq!(parse_i64(&v, "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 3_600_000);
    }

    /// The public window accessor and the `ReconConfig` the venue leg is driven by must be ONE
    /// answer — that is the entire reason [`lookback_ms`] is `pub`. `vike-app`'s journal leg
    /// carried its own `3_600_000` literal until 2026-08-30, and the two agreed only while nobody
    /// set the variable; this pins that they now cannot disagree for any value.
    #[test]
    fn the_public_lookback_is_the_same_window_build_recon_config_uses() {
        for vars in [
            map(&[]),
            map(&[("VIKE_RECONCILE_LOOKBACK_MS", "21600000")]),
            map(&[("VIKE_RECONCILE_LOOKBACK_MS", "not-a-number")]),
            map(&[("VIKE_RECONCILE_LOOKBACK_MS", "0")]),
        ] {
            assert_eq!(
                lookback_ms(&vars),
                build_recon_config(&vars, HashMap::new()).lookback_ms,
                "the journal leg and the venue leg must resolve ONE window: {vars:?}"
            );
        }
        // ...and the unset answer is still the documented hour, spelled once.
        assert_eq!(lookback_ms(&map(&[])), DEFAULT_LOOKBACK_MS);
        assert_eq!(DEFAULT_LOOKBACK_MS, 3_600_000);
        // A six-hour window — the value that used to disarm the journal leg for five of its six
        // hours — is honoured rather than clamped to the default.
        assert_eq!(lookback_ms(&map(&[("VIKE_RECONCILE_LOOKBACK_MS", "21600000")])), 21_600_000);
    }

    // -- health_from_feed_status ----------------------------------------------------------------

    #[test]
    fn health_connected_is_healthy() {
        assert_eq!(health_from_feed_status("Connected"), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("LIVE \u{b7} Binance"), ReconHealth::Healthy);
    }

    #[test]
    fn health_explicit_disconnected_is_degraded() {
        assert_eq!(health_from_feed_status("Disconnected"), ReconHealth::Degraded);
        assert_eq!(health_from_feed_status("disconnected"), ReconHealth::Degraded);
    }

    #[test]
    fn health_idle_never_started_is_healthy() {
        // NEW: an empty/idle status — the constructor default before any feed activity, or a
        // venue whose feed never started because no chart subscribed to it — is Healthy, not
        // Degraded. This is the live-verify bugfix (2026-07-18): these used to permanently
        // suppress bybit/okx/hyperliquid/aster's reconcile legs.
        assert_eq!(health_from_feed_status(""), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("   "), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("idle"), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("\u{2014}"), ReconHealth::Healthy);
    }

    #[test]
    fn health_error_is_degraded_but_connecting_is_healthy() {
        assert_eq!(health_from_feed_status("feed fault: reset"), ReconHealth::Degraded);
        assert_eq!(
            health_from_feed_status("btcusdt@kline_1m ws error (reconnecting): timeout"),
            ReconHealth::Degraded
        );
        // NEW: "connecting to X…" is every venue feed's real constructor-default status string
        // until a chart subscribes and its WS pump actually runs — no longer treated as a fault.
        assert_eq!(health_from_feed_status("connecting to Binance"), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("connecting to Bybit\u{2026}"), ReconHealth::Healthy);
        assert_eq!(health_from_feed_status("reconnecting"), ReconHealth::Healthy);
    }

    #[test]
    fn health_unknown_is_healthy() {
        assert_eq!(health_from_feed_status("some unrecognized status text"), ReconHealth::Healthy);
    }

    // -- build_recon_config wiring (end-to-end smoke) --------------------------------------------

    #[test]
    fn build_recon_config_wires_env_and_health_closure() {
        let vars = map(&[
            ("VIKE_RECONCILE_POLICY", "quarantine"),
            ("VIKE_RECONCILE_LOOKBACK_MS", "1000"),
            ("VIKE_RECONCILE_STARTUP_DELAY_MS", "500"),
            ("VIKE_RECONCILE_INTERVAL_MS", "0"),
        ]);
        // An actually-failing status (Error), not "connecting" — a never-started/idle feed is
        // Healthy under the new mapping, so this test uses a genuine fault to still exercise the
        // Degraded->Healthy recovery transition below.
        let binance_status = Arc::new(Mutex::new("feed fault: reset".to_string()));
        let bybit_status = Arc::new(Mutex::new("Connected".to_string()));
        let feeds: HashMap<String, Arc<Mutex<String>>> = [
            ("binance".to_string(), Arc::clone(&binance_status)),
            ("bybit".to_string(), Arc::clone(&bybit_status)),
        ]
        .into_iter()
        .collect();
        let cfg = build_recon_config(&vars, feeds);
        assert_eq!(cfg.policy.default, ReconMode::Quarantine);
        assert_eq!(cfg.lookback_ms, 1000);
        assert_eq!(cfg.startup_delay, Duration::from_millis(500));
        assert_eq!(cfg.interval, None);
        assert_eq!(cfg.audit_interval, None); // mirrors interval: VIKE_RECONCILE_AUDIT_MS unset
        assert!(!cfg.generate_missing_orders);

        // PER-VENUE: binance's fault status is Degraded while bybit "Connected" is Healthy — in
        // the SAME config. An unknown venue (no map entry) is never blocked (Healthy).
        let health = cfg.health.expect("health closure always Some");
        assert_eq!(health("binance"), ReconHealth::Degraded);
        assert_eq!(health("bybit"), ReconHealth::Healthy);
        assert_eq!(health("deribit"), ReconHealth::Healthy, "un-mapped venue never blocked");

        // binance recovers → its own leg reads Healthy, independent of the others.
        *binance_status.lock().unwrap() = "LIVE \u{b7} Binance".to_string();
        assert_eq!(health("binance"), ReconHealth::Healthy);
    }

    #[test]
    fn build_recon_config_defaults_when_vars_are_empty() {
        let cfg = build_recon_config(&map(&[]), HashMap::new());
        assert_eq!(cfg.lookback_ms, 3_600_000);
        assert_eq!(cfg.startup_delay, Duration::from_millis(2_000));
        assert_eq!(cfg.interval, DEFAULT_INTERVAL);
        assert_eq!(cfg.audit_interval, DEFAULT_INTERVAL);
        assert_eq!(cfg.balance_tol, BalanceTol::default());
    }
}
