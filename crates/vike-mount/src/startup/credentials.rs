//! The credential leg: one authed-read probe per armed venue, and the bounded-probe primitive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_exec::recon::ReconClient;

use crate::preflight::CredentialGap;
#[cfg(doc)]
use crate::preflight::PreflightConfig;

use super::{
    CREDENTIAL_PROBE_ATTEMPTS, CREDENTIAL_PROBE_THREAD, CREDENTIAL_PROBE_TIMEOUT,
    CREDENTIAL_RETRY_BACKOFF,
};

/// One venue's cheap authenticated read: `Ok(())` if it SIGNED and was ACCEPTED. A closure, since
/// the two probe SHAPES differ in WHEN the client may be built (the module doc).
///
/// ⚠ `Arc<… + Send + Sync>`: [`bounded_probe`] MOVES a handle onto a spawned thread and may spawn a
/// SECOND for the retry, so ownership must be shared. Each closure holds a
/// `Mutex<Box<dyn ReconClient>>` (`Mutex<T>: Sync` for `T: Send`) or config data.
pub(super) type CredentialProbe = BoundedProbe<Result<(), String>>;

/// Anything [`bounded_probe`] can run (a nullary closure it may walk away from): both legs' probes,
/// so the abandonment is written once. Why `Send + Sync`: [`CredentialProbe`].
pub(super) type BoundedProbe<T> = Arc<dyn Fn() -> T + Send + Sync>;

/// The venue → probe map, the authority for [`PreflightConfig::credential_venues`].
pub(super) type CredentialProbes = HashMap<String, CredentialProbe>;

/// Where an identity-recording probe hands the venue's answer:
/// `crate::book_identity::record_authenticated_account`, or a test's capture.
type IdentityRecorder = fn(
    &str,
    &vike_model::accounts::account_keys::AccountLabel,
    vike_config::VenueMode,
    Option<&dyn ReconClient>,
    &vike_bridge_core::account_directory::AccountDirectory,
);

/// **The probe that reads the balance, then records which account the key is**, for every row
/// whose `credential_probe` answers `CredentialProbe::RecordsIdentity` (binance, bybit, okx).
pub(super) fn identity_recording_probe(
    venue: String,
    client: Box<dyn ReconClient>,
    tier: vike_config::VenueMode,
    directory: vike_bridge_core::account_directory::AccountDirectory,
    record: IdentityRecorder,
) -> CredentialProbe {
    let client = Mutex::new(client);
    Arc::new(move || {
        let guard = client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
        // ⚠ ORDERING (as at `make_engine_for_account`'s call site): BALANCE first. On binance spot
        // the account id rides `/api/v3/account`, the body this read pulls, so recording after it
        // is free; before it would cost a second signed request.
        guard.fetch_balance()?;
        // ⚠ The VERDICT is the balance read alone: a key that answered its balance but could not
        // name its account is HEALTHY, and must not demote a live venue to paper.
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
/// the live gate: no keys, no probe, not in [`PreflightConfig::credential_venues`], never checked
/// (preflight only DEMOTES a venue it checked).
///
/// ⚠ **So is the arming ceiling — the account-scoped gate** (branch 4 below). A probe SIGNS a read
/// against the real account, so a `paper`-capped venue ("do not touch this account") never reaches
/// it: the gate is [`crate::would_mount_live_under_policy`], the predicate `make_engine` arms on.
/// Most of all for ctrader, whose probe OPENS AND AUTHENTICATES a socket
/// (`crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount::credential_probe`).
///
/// ⚠ The ceiling also picks the TIER ([`crate::ceiling_permits_live`], as `make_engine`), so a
/// `demo` ceiling never signs against real money — before decision 0095 a `demo`-capped binance
/// with `BINANCE_MAINNET=1` got a signed **MAINNET** balance read. Each CEX bridge pins
/// `a_demo_ceiling_never_signs_against_the_real_money_tier`.
///
/// ⚠ **And it is ENFORCED here**: a `RecordsIdentity { bound_tier: Live }` under a ceiling below
/// `live` is not built (`error!`, venue absent) — the third interlock beside `crate::contract`'s
/// resolve and mount halves (`within_the_ceiling`); pinned by
/// `a_probe_bound_to_the_live_tier_under_a_demo_ceiling_is_never_built`. A `ReadOnly` probe
/// carries no tier to check.
pub fn authed_read_probes(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> CredentialProbes {
    authed_read_probes_with(
        registry,
        vars,
        policy,
        crate::book_identity::record_authenticated_account,
    )
}

/// [`authed_read_probes`] over a caller-supplied identity recorder — the test seam.
pub(super) fn authed_read_probes_with(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
    record: IdentityRecorder,
) -> CredentialProbes {
    let mut out = CredentialProbes::new();

    // Cloned ONCE: the closures are `'static` (`bounded_probe` may abandon them), so they cannot
    // borrow the policy's `account` rows and key-name map.
    let directory = crate::arming::directory_of(policy).clone();

    // 4. Every CONTRACT row that offers a probe, behind the ceiling gate (the fn doc).
    let default_account = vike_model::accounts::account_keys::AccountLabel::Default;
    for row in registry {
        let crate::VenueRow::Mount(m) = row else { continue };
        let venue = m.venue();
        if !crate::would_mount_live_under_policy(registry, venue, vars, policy) {
            continue;
        }
        let process = crate::contract::process_facts();
        let live = crate::ceiling_permits_live(crate::venue_ceiling(policy, venue));
        let inputs =
            crate::contract::inputs_for(*m, &default_account, vars, live, policy, &process);
        match m.credential_probe(&inputs) {
            Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
                client,
                bound_tier,
            }) => {
                // ⚠ **THE CEILING INTERLOCK, startup-probe half.** A LIVE-bound probe under a
                // ceiling below `live` would sign a real-money read the ceiling forbids, and this
                // leg reaches neither `resolve` nor `mount`, so `crate::contract` cannot see it.
                // Not built: no row = never checked, never demoted. Same level and fields as
                // `crate::contract`'s `refuse_beyond_the_ceiling`; the DEFAULT account only.
                if bound_tier == vike_bridge_core::venue_mount::Tier::Live && !live {
                    tracing::error!(
                        venue,
                        account = %default_account,
                        "{venue}: the bridge went past this account's arming ceiling — its \
                         `credential_probe` bound the LIVE tier while the ceiling is below `live`. \
                         Refused: no startup credential probe is built for it. A bridge may reach \
                         its live tier only when `MountInputs::live_permitted` is true, so this is \
                         a defect in the bridge."
                    );
                    continue;
                }
                // ⚠ **THIS LEG ALSO RECORDS WHICH ACCOUNT THE KEY IS** — the site covering a venue
                // this box ARMS but never MOUNTS (MEASURED on the CI box 2026-09-20: 16 `account` rows
                // across 13 venues, one mounted). The argument, and why deribit needs the mount
                // site too: `crate::book_identity::record_authenticated_account`.
                //
                // The tier the bridge's credentials BIND — never the ceiling, which may be wider.
                let tier = crate::contract::tier_mode(bound_tier);
                out.insert(
                    venue.to_string(),
                    identity_recording_probe(
                        venue.to_string(),
                        client,
                        tier,
                        directory.clone(),
                        record,
                    ),
                );
            }
            Some(vike_bridge_core::venue_mount::CredentialProbe::ReadOnly(probe)) => {
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
/// its transport timeout (`ReconClient`'s 30 s agent; `crate::server_time::CLOCK_READ_TIMEOUT`),
/// but on a wedged `getaddrinfo` until the OS resolver gives up. One leaked thread on a DNS-broken
/// box instead of a mount that never arms, bounded in COUNT: at most ONE per venue per preflight
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

/// The credential leg over a [`CredentialProbes`] map: one authed read per venue, BOUNDED by
/// [`CREDENTIAL_PROBE_TIMEOUT`], [`CREDENTIAL_PROBE_ATTEMPTS`] tries.
///
/// `Ok` (even a `fetch_balance` of `Ok(None)`) = SIGNED and ACCEPTED; the balance VALUE is unused.
/// A venue absent from the map is [`CredentialGap::Rejected`], which cannot fire in practice:
/// [`run_startup_preflight`] derives the checked list from this same map.
///
/// ⚠ **The `Err` variant IS the enforcement decision** (module doc): ANSWERED and refused =
/// `Rejected`, mounted PAPER; never answered = `Unanswered`, mounted as configured. Only the first
/// is retried — re-waiting a timeout would double the worst case to learn nothing twice.
///
/// The `Mutex` guards the map, not a probe: released BEFORE the bounded call, so one wedged venue
/// cannot park the rest on the lock.
pub(super) fn authed_read_probe(
    probes: CredentialProbes,
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
        let mut last: Option<String> = None;
        for attempt in 0..CREDENTIAL_PROBE_ATTEMPTS.max(1) {
            if attempt > 0 {
                std::thread::sleep(CREDENTIAL_RETRY_BACKOFF);
            }
            match bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => last = Some(e),
                // No retry after a silence — see this fn's doc.
                Err(waited_ms) => {
                    let detail = format!(
                        "no answer from the venue within the mount's own bound (the client's \
                         transport keeps its own, longer timeout; this probe was abandoned, not \
                         cancelled){}",
                        last.map_or_else(String::new, |e| format!("; earlier attempt said: {e}"))
                    );
                    return Err(CredentialGap::Unanswered { waited_ms, detail });
                }
            }
        }
        // Every attempt ANSWERED and refused: confirmed, and this is the row that demotes.
        Err(CredentialGap::Rejected(
            last.unwrap_or_else(|| "authenticated read failed and reported nothing".to_string()),
        ))
    }
}
