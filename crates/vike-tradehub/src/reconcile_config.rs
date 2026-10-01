//! Env-driven configuration for the reconciliation engine (`vike_core::spawn_recon` /
//! `vike_core::ReconConfig`) plus the feed-status health closure a mounted
//! `vike_core::recon_manager::ReconManager` consults at every pass boundary
//! ([`vike_core::ReconHealth`]). Task 5 of the reconciliation-activation plan built the
//! `VIKE_RECONCILE*` env-parsing + health-mapping helpers;
//! `crates/vike-tradehub/src/tradehub_cli.rs`'s `live_mount_with` calls `vike_core::spawn_recon`
//! with the `ReconConfig` this module produces, gated on [`reconcile_gate`] — ON by DEFAULT for a
//! mount that arms a live venue account since S2, and paired there with
//! [`quarantine_first_default`]. Nothing in THIS module spawns a thread or holds a driver — it
//! stays a pure env-parsing seam; the mount and the driver's lifecycle live in that CLI. ⚠ This
//! named the GUI shell's `App::new` (its `recon_driver` block, mounted after `spawn_core_multi`) as
//! the caller until 2026-09-28: that mount went with the desktop's local core (#1610), and the
//! module came here from `vike-ops` on 2026-09-23. Its first move was OUT of the GUI crate —
//! `vike-app` then — into a CI-able one, so its `#[cfg(test)]` unit tests would run in a gate; they
//! never did while the module lived in the CI-excluded GUI crate.
//!
//! ## Env source — pinned to raw process env, NOT the credentials `.env` map
//! The precedent this follows was set by the GUI shell's `main.rs` (then `vike-app`), which read
//! every other `VIKE_*` FEATURE-TOGGLE flag (`VIKE_RECORD_PROPERTIES`'s
//! `vike_data::properties_rec::RECORD_PROPERTIES_ENV`, `vike_core::journal_config_from_env`'s
//! `VIKE_JOURNAL_DIR`/`VIKE_JOURNAL_SNAPSHOT_EVERY`, `VIKE_STATE_DIR`, `VIKE_SHOT`, …) straight off
//! `std::env::var`/`var_os` against the REAL process environment.
//! `vike_bridge_core::credentials::load_workspace_dotenv()` — the `vars: HashMap<String, String>`
//! `vike_mount::make_engine` is handed — is a different, narrower thing: the credential store, which
//! never reads or merges the real process env, and is used only for venue credential lookups
//! (`{VENUE}_{ENV}_API_KEY` etc). This module follows the feature-toggle precedent, NOT the
//! credentials one.
//!
//! The helpers below still take a `&HashMap<String, String>` rather than calling
//! `std::env::var` directly — purely a TESTABILITY seam, so a test can hand in a literal map
//! instead of mutating the process-global env (`std::env::set_var` is unsound to call from
//! parallel test threads). The REAL call site (`crates/vike-tradehub/src/tradehub_cli.rs`'s
//! `daemon_recon_env`) builds that map from the actual process env — that CLI's `process_env`, the
//! one `std::env::vars()` sweep it owns — while `load_workspace_dotenv()`'s map would silently see
//! nothing for a shell-exported `VIKE_RECONCILE=1`. (It named `vike-app`'s `main.rs` as that call
//! site until 2026-09-28.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::{ReconConfig, ReconHealth};
use vike_exec::recon::{BalanceTol, Divergence, DivergenceKind, ReconPolicy};
use vike_model::FillReport;
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::feed_status::{ConnectionState, parse_feed_status};

/// The master gate for mounting `vike_core::spawn_recon`: `true` iff `VIKE_RECONCILE` is the EXACT
/// string `"1"`. Deliberately not a fuzzy truthy parse (`"true"`/`"yes"`/`"on"`/...) — one
/// unambiguous on-string to grep for in an incident, mirroring the existing `VIKE_RECORD_PROPERTIES`
/// gate's exact-`"1"` idiom (`vike_data::PropertiesRecorder::open_from_vars` today; the GUI shell's
/// `make_engine` bybit/okx/binance arms, `.ok().as_deref() == Some("1")`, when this was written).
///
/// ⚠ **No production caller since the settings files were wired.** `vike-tradehub` takes
/// `vike_config::Flags::reconcile` instead — the same variable, resolved `env > file > default`, so
/// the settings store can arm it too. (This said "`vike-app` and `vike-tradehub` both" until
/// 2026-09-28; the GUI shell has mounted no reconcile driver since its local core went.) This
/// function stays because the REST of the `VIKE_RECONCILE_*` family below is still parsed here out
/// of one map, and because it is this file's own statement of the exact-`"1"` grammar. A caller
/// that reaches for it now gets the environment-only answer and will disagree with what the
/// process actually mounted; use the resolved flag.
///
/// ⚠ **And since S2 the resolved flag is no longer the whole gate either** — it is one of three
/// inputs to [`reconcile_gate`], which is what the live root calls (there is ONE since the desktop
/// lost its local core; this said "both live roots" until 2026-09-28). `flags.reconcile` now means
/// "on even where the default would not turn it on"; the DEFAULT for a mount that arms a live venue
/// account is ON. A caller comparing this function's answer against "did that process reconcile"
/// will be wrong in the ordinary case, not the exotic one.
pub fn reconcile_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get("VIKE_RECONCILE").map(String::as_str) == Some("1")
}

// ---------------------------------------------------------------------------------------------
// S2 — reconciliation is ON by default for a LIVE mount, and PAIRED with `quarantine`
// ---------------------------------------------------------------------------------------------

/// Fold the QUARANTINE-FIRST `VIKE_RECONCILE_POLICY` default into a reconcile env map: default it
/// to `quarantine` ONLY when the operator has not set it (an explicit value — `hybrid` /
/// `synthesize` / `external-quarantine` — is honoured verbatim, so this can only ever make a mount
/// fold LESS).
///
/// ⚠ **This is one half of a pair, and neither half is safe alone.** [`reconcile_gate`] turns the
/// driver ON for every mount that arms a live venue account; under [`parse_policy`]'s own `hybrid`
/// fallback that default would auto-apply `PositionDrift` — rewriting local position size onto the
/// venue's number and booking realized PnL at the venue's average price, with no operator in front
/// of it — at the first pass after every restart. Fetching the venue's view is the point; folding
/// it blind is the hazard. So a root that calls [`reconcile_gate`] MUST build its `ReconConfig`
/// from a map that has been through this function, and
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` is the verdict that pairs
/// them.
///
/// Kept a PURE map-to-map function (rather than reading the process env here) for the reason this
/// module's doc gives: `std::env::set_var` is unsound from parallel test threads, so the default is
/// only unit-testable if the map is a parameter. It was lifted out of the daemon's CLI on
/// 2026-09-06, when that CLI had been the only mount applying it — `vike-app`'s live mount built
/// its `ReconConfig` off a raw `std::env::vars()` sweep and therefore ran `hybrid`, the
/// auto-folding policy, whenever an operator armed reconciliation in the GUI. One home, both
/// roots, then; one root now (the GUI's mount went with its local core), and the home stayed.
pub fn quarantine_first_default(mut env: HashMap<String, String>) -> HashMap<String, String> {
    env.entry("VIKE_RECONCILE_POLICY".to_string()).or_insert_with(|| "quarantine".to_string());
    env
}

/// Why a composition root did — or did not — mount the reconciliation driver, as one value.
///
/// An enum rather than a `bool` because the ANSWER and the REASON are read by different people: the
/// mount needs [`Self::enabled`], and the operator reading a startup line needs to know whether
/// reconciliation is on because they asked, on because this is a live mount, or off because they
/// refused it. A bare `bool` leaves the second question unanswerable from the log — and "was the
/// venue ever queried on this box" is the first thing asked after a restart went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileGate {
    /// The operator asked outright — `flags.reconcile` / `VIKE_RECONCILE=1`. Honoured whatever the
    /// armed-live probe says, which is why the flag survives the default landing: that probe is a
    /// hand-written per-venue table (`vike_mount::venue_arming_under`), and a venue arm added
    /// without its row would make it answer zero for a genuinely live mount.
    Requested,
    /// **The S2 default**: at least one venue ACCOUNT arms live on this box, so the venue's own
    /// view is fetched at startup and at every interval instead of being assumed.
    LiveDefault,
    /// The operator refused the default — `flags.reconcile_off` / `VIKE_RECONCILE_OFF=1`. Wins over
    /// [`Self::Requested`]: a refusal is a deliberate act, and a stale `flags.toml` line must not
    /// silently overrule the operator who just typed the override.
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
                "reconcile ON — requested by flags.reconcile / VIKE_RECONCILE=1; the policy is \
                 quarantine (nothing auto-folds) unless VIKE_RECONCILE_POLICY says otherwise"
            }
            Self::LiveDefault => {
                "reconcile ON by DEFAULT — this mount arms at least one LIVE venue account, so \
                 every start fetches that venue's own orders/positions and HOLDS every divergence \
                 for you (quarantine) — EXCEPT the BALANCE, which is seeded from the venue \
                 authoritatively on every pass under every policy and moves the equity your \
                 margin ceiling is judged against. It issues AUTHENTICATED READS against those \
                 accounts. Turn it off with flags.reconcile_off / VIKE_RECONCILE_OFF=1"
            }
            Self::RefusedByOperator => {
                "reconcile OFF — refused by flags.reconcile_off / VIKE_RECONCILE_OFF=1. This mount \
                 will NOT ask any venue what it holds, so an unclean restart can leave resting \
                 orders and position state wrong with nothing to say so"
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
/// `requested` is the resolved `flags.reconcile` (env > file > default), `refused` the resolved
/// `flags.reconcile_off`, and `armed_live_accounts` the LENGTH of `vike_mount::armed_live_venues` —
/// the pure pre-mount probe of how many venue accounts this process is about to authenticate as.
/// Taking a COUNT rather than that `Vec` is what kept this module below `vike-run` while it lived
/// in `vike-ops` (the layer rule; vike-run has since merged into vike-mount, docs/decisions/0098),
/// and it makes the seam testable with no mount, no credentials and no network.
///
/// # Why the default flipped (S2)
///
/// Engines re-seed some local state on restart, but until this landed the venue's own view was
/// fetched only when an operator remembered to export `VIKE_RECONCILE=1`. A live daemon that
/// restarts and never asks the venue what it holds is trading against a BELIEF: resting orders it
/// has forgotten, a position it thinks is flat, a fill that landed while it was down. Comparable
/// platforms fetch cash, open orders and holdings from the brokerage at every live setup,
/// unconditionally; this is that, minus the folding.
///
/// # What makes the default safe, and what it is NOT
///
/// - It is paired with [`quarantine_first_default`], so a default-on pass folds no DIVERGENCE —
///   every one is HELD for an operator confirm. [`auto_applied_kinds`] over the resulting policy
///   is EMPTY, which is what the tests assert rather than restating a kind list.
///   ⚠ **"Folds no divergence" is not "writes nothing", and the gap is the BALANCE.** The
///   authoritative balance seed in `crates/vike-core/src/runtime/mod.rs`'s fold sits OUTSIDE the
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
///   account, at startup and then every `VIKE_RECONCILE_INTERVAL_MS` (default 60 s): a handful of
///   REST reads per venue per minute, inside every wired venue's published budget, but real traffic
///   against real accounts. **Aster has no testnet credentials configured** — the testnet itself
///   exists and is routed (`crates/bridges/aster/src/urls.rs`'s `urls_for` maps `Environment::Demo`
///   onto real testnet hosts); what the store holds is `ASTER_LIVE_*` — so on a box that arms aster
///   those are MAINNET reads. Arming aster live is already an explicit act — `policy.venues.aster`
///   must permit it and that arm posts `set_leverage` at startup — so this is not a new class of
///   contact with the account; but a demo or validation run must still keep aster out the way the
///   root `CLAUDE.md` has always said to (cap the ceiling to `paper`, or withhold `ASTER_*`).
///   `VIKE_RECONCILE_OFF=1` is the third answer, and the only one that keeps the venue armed while
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

/// Build the reconcile driver's [`ReconConfig`] from env + a PER-VENUE map of the app's live feed
/// status strings (`venue -> Arc<Mutex<String>>`, one per credentialed feed — binance/bybit/okx).
/// The health probe is keyed by the venue being reconciled, so a bybit feed gap suppresses only
/// bybit's leg while binance still reconciles (see [`health_from_feed_status`]). A venue with no
/// entry in the map maps to [`ReconHealth::Healthy`] — an un-gated venue is never blocked. See
/// this module's doc for why `vars` must be sourced from the real process env, not the
/// credentials dotenv map.
///
/// Env knobs (all optional; the parenthesised value is what a LIVE MOUNT resolves — i.e. what an
/// operator actually gets — not merely this function's own fallback):
/// - `VIKE_RECONCILE_POLICY` (**`quarantine`**) — `hybrid` / `synthesize` / `quarantine` /
///   `external-quarantine`, case-insensitive. ⚠ **Read the split before copying this row.** THIS
///   FUNCTION's own fallback for an unset or unrecognized value is `hybrid` ([`parse_policy`]);
///   what makes the operator-facing default `quarantine` is that the live composition root builds
///   its map through [`quarantine_first_default`] first, and since S2 it MUST — the gate is on
///   by default, and `hybrid` auto-applies `PositionDrift` ([`auto_applied_kinds`] over the
///   resolved policy is the authority, never a restated kind list). A NEW composition root that
///   reads this row as "`hybrid` is the default" and skips the fold reproduces exactly the
///   `vike-app` defect S2 closed: two live mounts turning reconcile on under two different
///   policies, one of them folding blind.
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
/// ⚠ **This is `pub` because the venue leg is not the only consumer the window has had, and the
/// second one carried a LITERAL.** `vike-app`'s `journal_view_provider` built the JOURNAL leg of
/// the same three-way `vike_exec::recon::diff` — and it scoped its store read with a hard-coded
/// `3_600_000` while the venue leg it is compared against read this variable. The two agreed only
/// as long as nobody set it, and the failure points COUNTER-INTUITIVELY: `diff` raises
/// `JournalDivergence` only when the journal CONTAINS the venue trade id, so a venue window WIDER
/// than the journal window turns the persistence-bug signal into a plain `MissingFill` — which
/// `hybrid` AUTO-APPLIES ([`auto_applied_kinds`]). Setting
/// `VIKE_RECONCILE_LOOKBACK_MS=21600000` therefore disarmed the third leg for five of its six
/// hours and folded silently instead of alerting. Both legs then resolved the window through THIS
/// function, so they could not disagree by construction.
///
/// ⚠ That paragraph said the hook "builds" the journal leg, in the present tense, until
/// 2026-09-28. It went with the desktop's local core: no production root supplies a
/// `vike_core::JournalViewHook` today (`vike_core::CoreConfig::journal_view_provider` is `None` at
/// every live mount), so the venue leg is this function's only production caller. It stays `pub`
/// for the next journal leg, which owes the walk in `crates/vike-core/src/journal_view.rs` this
/// same window — that module's doc says so.
///
/// ⚠ That sentence said "`hybrid`, the default policy" until S2, and the correction matters for
/// how bad the mis-set window is rather than for whether it is a defect: since the live root folds
/// [`quarantine_first_default`] in, an operator reaches `hybrid` by NAMING it, so today the
/// mismatched window downgrades an alert's KIND on every mount and additionally folds it blind on a
/// mount that asked for `hybrid`.
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
/// (suggested `2000`). Unset OR `0` -> `None` -> no `vike_core::TimerKind::InflightConfirm` timer
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
/// indefinitely, silently defeating reconciliation with no error at all. An
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
    // The four presets are CONSTRUCTED by `ReconPolicy::from_policy_name` (vike-exec), not here:
    // `vike_docs` renders the same four policies' per-kind verdicts for the published
    // capability data, and a second hand-built `ReconPolicy { default: Quarantine, .. }` in one of
    // the two places is a second answer that can rot. What stays HERE is this reader's own
    // contract, which the type has no business holding: the variable's name, case-insensitivity,
    // and the fall back to `hybrid` for unset OR unrecognized (pinned by
    // `policy_unknown_or_absent_defaults_to_hybrid`).
    vars.get("VIKE_RECONCILE_POLICY")
        .map(|s| s.to_lowercase())
        .and_then(|name| ReconPolicy::from_policy_name(&name))
        .unwrap_or_else(ReconPolicy::hybrid)
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

#[path = "reconcile_config_tests.rs"]
#[cfg(test)]
mod reconcile_config_tests;
