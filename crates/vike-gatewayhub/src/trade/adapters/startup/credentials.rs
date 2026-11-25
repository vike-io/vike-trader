//! The credential leg: one authed-read probe per armed venue, and the bounded-probe primitive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_execution::recon::{ReconcileClient, ReconcileFetchError};

use vike_model::CredentialGap;
use vike_utility::errors::FailureOutcome;

use super::{
    CREDENTIAL_PROBE_ATTEMPTS, CREDENTIAL_PROBE_THREAD, CREDENTIAL_PROBE_TIMEOUT,
    CREDENTIAL_RETRY_BACKOFF,
};

/// One venue's cheap authenticated read: `Ok(())` if it SIGNED and was ACCEPTED, else the bridge's typed failure,
/// whose `outcome` [`authed_read_probe`] decides on. A closure, since
/// the two probe SHAPES differ in WHEN the client may be built (the module doc).
///
/// ⚠ `Arc<… + Send + Sync>`: [`bounded_probe`] MOVES a handle onto a spawned thread and may spawn a
/// SECOND for the retry, so ownership must be shared. Each closure holds a
/// `Mutex<Box<dyn ReconcileClient>>` (`Mutex<T>: Sync` for `T: Send`) or config data.
pub(super) type CredentialProbeFn = BoundedProbe<Result<(), ReconcileFetchError>>;

/// Anything [`bounded_probe`] can run (a nullary closure it may walk away from): both legs' probes,
/// so the abandonment is written once. Why `Send + Sync`: [`CredentialProbeFn`].
pub(super) type BoundedProbe<T> = Arc<dyn Fn() -> T + Send + Sync>;

/// The venue → probe map, the authority for `PreflightConfig::credential_venues` (`vike-tradehub-core`).
pub(super) type CredentialProbeFunctions = HashMap<String, CredentialProbeFn>;

/// Where an identity-recording probe hands the venue's answer:
/// `crate::trade::adapters::book_identity::record_authenticated_account`, or a test's capture.
type IdentityRecorder = fn(
    &str,
    &vike_model::accounts::account_keys::AccountLabel,
    vike_model::AccountTier,
    Option<&dyn ReconcileClient>,
    &vike_secrets::account_directory::AccountDirectory,
);

/// **The probe that reads the balance, then records which account the key is**, for every row
/// whose `credential_probe` answers `CredentialProbeFn::RecordsIdentity` (binance, bybit, okx).
pub(super) fn identity_recording_probe(
    venue: String,
    client: Box<dyn ReconcileClient>,
    tier: vike_model::AccountTier,
    directory: vike_secrets::account_directory::AccountDirectory,
    record: IdentityRecorder,
) -> CredentialProbeFn {
    let client = Mutex::new(client);
    Arc::new(move || {
        let guard = client
            .lock()
            .map_err(|_| ReconcileFetchError::unclassified("preflight probe lock poisoned"))?;
        // ⚠ ORDERING (as at `make_engine_for_account`'s call site): BALANCE first. On binance spot
        // the account id rides `/api/v3/account`, the body this read pulls, so recording after it
        // is free; before it would cost a second signed request.
        guard.fetch_balance()?;
        // ⚠ The VERDICT is the balance read alone: a key that answered its balance but could not
        // name its account is HEALTHY, and must not stop a live venue.
        // `record_authenticated_account` returns `()` and warns on its own failures.
        record(
            &venue,
            &vike_model::accounts::account_keys::AccountLabel::Default,
            tier,
            Some(&**guard),
            &directory,
        );
        Ok(())
    })
}

/// Build one credential probe per venue whose credentials RESOLVE, from each contract row's own
/// `credential_probe` (alpaca, binance, bybit, okx eager; ctrader lazy). Absent credentials ARE
/// the live gate: no keys, no probe, not in `PreflightConfig::credential_venues` (`vike-tradehub-core`), never checked
/// (preflight only DEMOTES a venue it checked).
///
/// ⚠ **So is the account's tier — the account-scoped gate** (branch 4 below). A probe SIGNS a read
/// against the real account, so a `paper` account ("do not touch this account") never reaches it:
/// the gate is [`crate::trade::adapters::arming::would_go_live_under_policy`], the predicate `make_engine` arms on. Most
/// of all for ctrader, whose probe OPENS AND AUTHENTICATES a socket
/// (`crates/bridges/ctrader/src/adapter.rs`'s `VenueAdapterCtrader::credential_probe`).
///
/// ⚠ The tier also picks the NETWORK (`AdapterInputs::tier`, as `make_engine`), so a `demo`
/// account never signs against real money — before decision 0095 a `demo`-capped binance with
/// `BINANCE_MAINNET=1` got a signed **MAINNET** balance read. Each CEX bridge pins
/// `a_demo_ceiling_never_signs_against_the_real_money_tier`.
///
/// ⚠ **And it is ENFORCED here**: a `RecordsIdentity` whose `bound_tier` is not the account's tier
/// (LIVE for a `demo` account; DEMO for a `live` one, the no-downgrade rule) is not built
/// (`error!`, venue absent) — the third interlock beside `crate::contract`'s resolve and start
/// halves (`within_the_tier`); pinned by
/// `a_probe_bound_to_the_live_tier_under_a_demo_tier_is_never_built`. A `ReadOnly` probe carries no
/// tier to check.
///
/// `fx_clock` is the FX login gate's clock a probe that logs in carries (`AdapterInputs::fx_clock`: ctrader's opens
/// no socket while the FX market is closed at its time); `None` is no gate.
pub fn authed_read_probes(
    registry: &'static [crate::trade::adapters::registry::VenueAdapterRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::trade::adapters::snapshot::TradeLaneSnapshot>,
    fx_clock: Option<&Arc<dyn vike_model::Clock + Send + Sync>>,
) -> CredentialProbeFunctions {
    authed_read_probes_with(
        registry,
        vars,
        policy,
        crate::trade::adapters::book_identity::record_authenticated_account,
        fx_clock,
    )
}

/// [`authed_read_probes`] over a caller-supplied identity recorder — the test seam.
pub(super) fn authed_read_probes_with(
    registry: &'static [crate::trade::adapters::registry::VenueAdapterRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::trade::adapters::snapshot::TradeLaneSnapshot>,
    record: IdentityRecorder,
    fx_clock: Option<&Arc<dyn vike_model::Clock + Send + Sync>>,
) -> CredentialProbeFunctions {
    let mut out = CredentialProbeFunctions::new();

    // Cloned ONCE: the closures are `'static` (`bounded_probe` may abandon them), so they cannot
    // borrow the policy's `account` rows and key-name map.
    let directory = crate::trade::adapters::snapshot::directory_of(policy).clone();

    // 4. Every CONTRACT row that offers a probe, behind the tier gate (the fn doc).
    let default_account = vike_model::accounts::account_keys::AccountLabel::Default;
    for row in registry {
        let crate::trade::adapters::registry::VenueAdapterRow::Adapter(m) = row else { continue };
        let venue = m.venue();
        if !crate::trade::adapters::arming::would_go_live_under_policy(
            registry, venue, vars, policy,
        ) {
            continue;
        }
        let process = crate::trade::adapters::contract::process_facts();
        // `would_go_live_under_policy` answered yes, so the default account's row states one.
        let Some(tier) =
            vike_config::account_arming::venue_tier(policy.map(|p| &p.accounts), venue)
        else {
            continue;
        };
        let inputs = crate::trade::adapters::contract::inputs_for(
            *m,
            &default_account,
            vars,
            tier,
            policy,
            &process,
            fx_clock,
        );
        match m.credential_probe(&inputs) {
            Some(vike_bridge_core::venue_adapter::CredentialProbe::RecordsIdentity {
                client,
                bound_tier,
            }) => {
                // ⚠ **THE TIER INTERLOCK, startup-probe half.** A probe bound to another tier
                // than the account's would sign a read the tier forbids (real money for a `demo`
                // account; demo for a `live` one), and this leg reaches neither `resolve` nor
                // `start`, so `crate::contract` cannot see it. Not built: no row = never checked,
                // never demoted. Same level and fields as `crate::contract`'s
                // `refuse_beyond_the_tier`; the DEFAULT account only.
                let bound = bound_tier;
                if bound != tier {
                    tracing::error!(
                        venue,
                        account = %default_account,
                        tier = tier.as_str(),
                        "{venue}: the bridge went past this account's tier — its \
                         `credential_probe` bound the {} tier while the account's tier is \
                         `{tier}`. Refused: no startup credential probe is built for it. A bridge \
                         binds exactly the account's tier, so this is a defect in the bridge.",
                        bound.as_str().to_uppercase()
                    );
                    continue;
                }
                // ⚠ **THIS LEG ALSO RECORDS WHICH ACCOUNT THE KEY IS** — the site covering a venue
                // this box ARMS but never STARTS (MEASURED on the CI box 2026-09-20: 16 `account` rows
                // across 13 venues, one started). The argument, and why deribit needs the start
                // site too: `crate::trade::adapters::book_identity::record_authenticated_account`.
                //
                // The tier the bridge's credentials BIND (equal to the account's, checked above).
                out.insert(
                    venue.to_string(),
                    identity_recording_probe(
                        venue.to_string(),
                        client,
                        bound,
                        directory.clone(),
                        record,
                    ),
                );
            }
            Some(vike_bridge_core::venue_adapter::CredentialProbe::ReadOnly(probe)) => {
                out.insert(venue.to_string(), probe);
            }
            None => {}
        }
    }

    out
}

/// Run ONE probe with a hard wall-clock bound, on a thread the caller can walk away from: the seam
/// that makes a credential FAIL mean something, shared with the CLOCK leg (module doc).
///
/// `Ok(v)` = answered inside `timeout`; `Err(waited)` = nothing came back. For a credential probe
/// `Ok(Ok(()))` = accepted, `Ok(Err(e))` = the venue ANSWERED and refused.
///
/// ⚠ Generic DELIBERATELY: the legs keep separate vocabularies (`Result<(), String>` vs
/// `Result<i64, ServerTimeGap>`) because a refusal demotes and a stale clock only warns; only the
/// ABANDONMENT is shared. `name` (the thread name a stack dump shows) is the one per-leg knob.
///
/// ⚠ A blocking `ureq` call cannot be cancelled, so the probe is ABANDONED: the thread runs to its
/// agent's completion, finds the channel dropped and exits ([`NetProbeThread`]'s "signal stop,
/// never join"), owning its handle via [`BoundedProbe`] and writing nothing else.
///
/// ⚠ **The residual, for both legs: an abandoned probe LEAVES A THREAD RUNNING** — normally until
/// its transport timeout (`ReconcileClient`'s 30 s agent; `crate::trade::adapters::server_time::CLOCK_READ_TIMEOUT`),
/// but on a wedged `getaddrinfo` until the OS resolver gives up. One leaked thread on a DNS-broken
/// box instead of a start that never arms, bounded in COUNT: at most ONE per venue per preflight
/// (once per process) — `crate::preflight::check_clock_skew` stops at the first
/// [`ServerTimeGap::Unreachable`], and [`authed_read_probe`] never retries a silence.
///
/// ⚠ A worker that DIES (panicked; `recv_timeout` reads `Disconnected`) lands in the same
/// `Err(bound)` arm as a timeout, after however long the panic took. Both legs render it as their
/// un-answer ([`abandoned_clock_gap`] says why that verdict is right).
pub(super) fn bounded_probe<T: Send + 'static>(
    probe: &BoundedProbe<T>,
    timeout: Duration,
    name: &str,
) -> Result<T, u64> {
    // one-shot: at most 1 message (the probe's result).
    let (tx, rx) = std::sync::mpsc::channel();
    let probe = Arc::clone(probe);
    // A spawn failure is a fault on THIS box, not a venue refusal; running the call inline would
    // restore the unbounded wait, so it is reported as an un-answer.
    if std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            // The receiver is gone on a timeout; that send failing is the normal abandoned path.
            let _ = tx.send(probe());
        })
        .is_err()
    {
        return Err(0);
    }
    match rx.recv_timeout(timeout) {
        Ok(result) => Ok(result),
        // Timeout AND Disconnected (the worker panicked) alike: neither is evidence about the
        // operator's keys, so neither may demote a venue.
        Err(_) => Err(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
    }
}

/// The credential leg over a [`CredentialProbeFunctions`] map: one authed read per venue, BOUNDED by
/// [`CREDENTIAL_PROBE_TIMEOUT`], [`CREDENTIAL_PROBE_ATTEMPTS`] tries.
///
/// `Ok` (even a `fetch_balance` of `Ok(None)`) = SIGNED and ACCEPTED; the balance VALUE is unused.
/// A venue absent from the map is [`CredentialGap::Rejected`], which cannot fire in practice:
/// [`run_startup_preflight`] derives the checked list from this same map.
///
/// ⚠ **The `Err` variant IS the enforcement decision** (module doc): ANSWERED and refused =
/// `Rejected`, started PAPER; never answered = `Unanswered`, started as configured. Only the first
/// is retried (`vike_utility::retry::retry_on_pauses`) — re-waiting a timeout would double the worst case to learn
/// nothing twice. The LAST answer's `outcome` (`ReconcileFetchError`, set by the bridge from the venue's own error
/// table, never read off its text) is the verdict: only `Refused` (a 401, the venue's invalid-key or signature code,
/// a failure the bridge holds only as words) is `Rejected`. Any other word (a network failure, an ambiguous timeout,
/// a 5xx, a rate limit, a maintenance window, Bybit's Cloudflare 403) said nothing about the keys: `Unanswered`, and
/// the start's own connect decides (owner 2026-11-16 for a network failure; deploy 2, 2026-11-17, for binance's demo
/// spot host answering 502).
///
/// The `Mutex` guards the map, not a probe: released BEFORE the bounded call, so one wedged venue
/// cannot park the rest on the lock.
pub(crate) fn authed_read_probe(
    probes: CredentialProbeFunctions,
) -> impl Fn(&str) -> Result<(), CredentialGap> + Send + Sync + 'static {
    let probes = Mutex::new(probes);
    move |venue: &str| {
        let probe = {
            let guard = probes.lock().map_err(|_| {
                CredentialGap::Rejected("preflight probe lock poisoned".to_string())
            })?;
            guard.get(venue).map(Arc::clone).ok_or_else(|| {
                CredentialGap::Rejected(format!("no reconcile client built for {venue}"))
            })?
        };
        // The latest ANSWERED failure: a silence that ends the ladder names the attempt before it.
        let mut answered: Option<ReconcileFetchError> = None;
        let answer = vike_utility::retry::retry_on_pauses(
            &[CREDENTIAL_RETRY_BACKOFF; CREDENTIAL_PROBE_ATTEMPTS - 1],
            |_| {
                let answer =
                    bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD);
                if let Ok(Err(e)) = &answer {
                    answered = Some(e.clone());
                }
                answer
            },
            // Settled: accepted, or silent (never retried — see this fn's doc). Every answered failure is asked again.
            |answer| !matches!(answer, Ok(Err(_))),
            std::thread::sleep,
        );
        match answer {
            Ok(Ok(())) => Ok(()),
            Err(waited_ms) => {
                let detail = format!(
                    "no answer from the venue within the mount's own bound (the client's \
                     transport keeps its own, longer timeout; this probe was abandoned, not \
                     cancelled){}",
                    answered.map_or_else(String::new, |e| format!("; earlier attempt said: {e}"))
                );
                Err(CredentialGap::Unanswered { waited_ms, detail })
            }
            // Every attempt ANSWERED and refused: confirmed, and this is the row that demotes.
            Ok(Err(e)) if e.outcome == FailureOutcome::Refused => {
                Err(CredentialGap::Rejected(e.text))
            }
            // The venue gave no verdict on the keys (not sent, outcome unknown, not now): no evidence about the
            // credentials, so the start goes on and its own connect decides (`AdapterOutcome::failed`).
            Ok(Err(e)) => Err(CredentialGap::Unanswered {
                waited_ms: 0,
                detail: format!(
                    "the venue gave no verdict on the keys (a network failure, a server error, a rate \
                     limit or a maintenance window, which a later try can pass): {e}"
                ),
            }),
        }
    }
}
