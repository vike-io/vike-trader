//! **The start WAITS for the hubs a box cannot run without** (owner 2026-11-14: the tradehub never falls into a
//! restart every 5 s; decision 0156's last section). A box whose every account the rows run is `demo`/`live` has no
//! engine without the gatewayhub, and none without the datahub either when every one of them is on a mounted venue
//! (`crate::account_follow::nothing_runs_without_the_hubs`). That start used to end in
//! `vike_tradehub_core::RuntimeError::NoAccountRuns` and be restarted into the same answer. It waits here instead,
//! BEFORE the assembly, where the FX wait holds a box of FX accounts, its door
//! answering `starting: waiting for ...`.
//!
//! [`hub_wait`] is the pure ladder (scripted in `tradehub_cli/tests/hub_wait.rs`, no clock and no socket);
//! [`wait_for_the_hubs`] is the production shell over it. Each ask re-reads the account rows (an operator who
//! activates a `paper` account, or deactivates the rest, ends the wait), asks the gatewayhub for its arming table,
//! then, when the datahub is needed too, its probe, which reads the strategy-lane key again each time. The pauses are
//! `vike_utility::retry::retry_on_pauses` over [`hub_wait_pauses`] (5 s to 30 s, the account watcher's and the late
//! arm's cadence), then one round of [`HUB_WAIT_ROUND`] after another for as long as it takes: the shared ladder is
//! finite by design, so this loop carries "for ever".
//!
//! Not waited for: a box with a `paper` account (the account watcher and the late arm serve it, unchanged). Refused at
//! once and parked (exit 78, `super::refused`): a box with no gatewayhub Trade key, because the link read its key once
//! at the boot and nothing in this process can mint one. After the wait the start goes on as it always did:
//! `VenueBackendGateway::connect`'s bounded wait answers at its first ask, and every refusal of the live start stands.

use std::time::Duration;

use vike_hub_protocol::auth::Scope;
use vike_secrets::account_directory::AccountDirectory;

use super::GatewayDial;
use super::live_runtime::{LiveStartFailed, read_account_directory};
use crate::feeds::StrategyLaneFactories;
use crate::late_arm::MAX_POLL;

/// The first ladder of pauses: the first four of [`crate::late_arm::poll_ladder`] (5 s doubling to 30 s), the account
/// watcher's and the late arm's cadence.
pub(super) fn hub_wait_pauses() -> [Duration; 4] {
    let mut ladder = crate::late_arm::poll_ladder();
    std::array::from_fn(|_| ladder.failed())
}

/// Each round after the first ladder, behind one more [`MAX_POLL`]: ten asks [`MAX_POLL`] apart, five minutes, with
/// one line at its end.
pub(super) const HUB_WAIT_ROUND: [Duration; 9] = [MAX_POLL; 9];

/// The words of a stop that came during the wait: the start ends there (the supervisor restarts no stop it asked for).
pub(super) const STOPPED_WAITING: &str = "stopped while waiting for the gatewayhub";

/// One ask's answer.
pub(super) enum HubAsk {
    /// Nothing to wait for: the start goes on over these rows.
    Go(AccountDirectory),
    /// A hub does not answer, and why in words (`the gatewayhub at 127.0.0.1:7900 (...)`).
    Wait(String),
    /// The process is stopping.
    Stopped,
}

/// What the wait tells the operator ([`hub_wait`]'s `say`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HubWaitNews {
    /// The wait began, or its reason changed.
    Waiting(String),
    /// A round ended and the hub still does not answer, after `waited` (the pauses taken so far).
    StillWaiting { waited: Duration, why: String },
    /// What was waited for answered, after `waited`.
    Answered { waited: Duration },
}

/// **Ask until nothing is waited for**: `ask` once, then again after each pause of [`hub_wait_pauses`] while it
/// answers [`HubAsk::Wait`], then round after round of [`HUB_WAIT_ROUND`]. `say` hears each NEW reason once, the end
/// of every ladder that did not settle, and the answer after a wait; `pause` is the wait itself (production's ends
/// early on a stop, and the next ask then says [`HubAsk::Stopped`]). `Ok` is the rows of the last ask, `Err` the
/// stop's words ([`STOPPED_WAITING`]).
pub(super) fn hub_wait(
    mut ask: impl FnMut() -> HubAsk,
    mut say: impl FnMut(HubWaitNews),
    mut pause: impl FnMut(Duration),
) -> Result<AccountDirectory, String> {
    let mut said: Option<String> = None;
    let mut waited = Duration::ZERO;
    let first = hub_wait_pauses();
    let mut ladder: &[Duration] = &first;
    loop {
        let answer = vike_utility::retry::retry_on_pauses(
            ladder,
            |_| {
                let answer = ask();
                if let HubAsk::Wait(why) = &answer
                    && said.as_ref() != Some(why)
                {
                    say(HubWaitNews::Waiting(why.clone()));
                    said = Some(why.clone());
                }
                answer
            },
            |answer| !matches!(answer, HubAsk::Wait(_)),
            |d| {
                pause(d);
                waited += d;
            },
        );
        match answer {
            HubAsk::Go(rows) => {
                if said.is_some() {
                    say(HubWaitNews::Answered { waited });
                }
                return Ok(rows);
            }
            HubAsk::Stopped => return Err(STOPPED_WAITING.to_string()),
            HubAsk::Wait(why) => say(HubWaitNews::StillWaiting { waited, why }),
        }
        // The ladder's first ask follows no pause: the round's pause comes before it.
        pause(MAX_POLL);
        waited += MAX_POLL;
        ladder = &HUB_WAIT_ROUND;
    }
}

/// **The production wait** over `accounts` (the boot's read) and the `mounted` venues: at once when a `paper` account
/// runs or nothing runs, else [`hub_wait`] over the gatewayhub's arming table and the datahub's probe (`feeds`, whose
/// pause is the stop-aware one). `dial` carries the link, the door's progress and the stop flag.
///
/// # Errors
/// A box that runs nothing without the gatewayhub and holds no Trade key for it: refused as setup, exit 78. A stop
/// during the wait: [`STOPPED_WAITING`].
pub(super) fn wait_for_the_hubs(
    accounts: AccountDirectory,
    mounted: &[String],
    dial: &GatewayDial,
    feeds: &dyn StrategyLaneFactories,
) -> Result<AccountDirectory, LiveStartFailed> {
    if crate::account_follow::nothing_runs_without_the_hubs(&accounts, mounted).is_none() {
        return Ok(accounts);
    }
    if !dial.link.can_sign(Scope::Trade) {
        return Err(LiveStartFailed::refused(format!(
            "vike-tradehub: setup needed: run `vike-cli gatewayhub setup` in this project, then start this daemon \
             again — every account this tradehub runs is demo or live, none of them runs without the gatewayhub, \
             and this tradehub holds no gatewayhub Trade key ({}) to reach it",
            vike_venues::credential_keys::gatewayhub_trade_key()
        )));
    }
    let addr = dial.link.addr().to_string();
    let mut boot_rows = Some(accounts);
    hub_wait(
        || {
            if feeds.stopping() {
                return HubAsk::Stopped;
            }
            let rows = boot_rows.take().unwrap_or_else(read_account_directory);
            let Some(datahub_too) =
                crate::account_follow::nothing_runs_without_the_hubs(&rows, mounted)
            else {
                return HubAsk::Go(rows);
            };
            if let Err(e) = vike_gatewayhub_client::trade::rpc::arming_table(&dial.link) {
                return HubAsk::Wait(format!("the gatewayhub at {addr} ({e})"));
            }
            if datahub_too
                && let Some(why) = feeds.datahub_probe(mounted).and_then(|probe| probe.check())
            {
                return HubAsk::Wait(format!("the datahub ({why})"));
            }
            HubAsk::Go(rows)
        },
        |news| say_hub_wait(news, dial),
        |d| {
            feeds.pause(d);
        },
    )
    .map_err(LiveStartFailed::from)
}

/// The wait's words: the door's `starting` phase and a `warn!` for each new reason, an `error!` at the end of each
/// round, an `info!` when it answered.
fn say_hub_wait(news: HubWaitNews, dial: &GatewayDial) {
    match news {
        HubWaitNews::Waiting(why) => {
            if let Some(progress) = &dial.progress {
                progress.phase(&format!("waiting for {why}"));
            }
            tracing::warn!(
                "ARMING: no account this tradehub runs can start without {why}, which does not answer: the start \
                 WAITS for it here, asking again every 5 to 30 s, and does not exit into a restart loop"
            );
        }
        HubWaitNews::StillWaiting { waited, why } => tracing::error!(
            "ARMING: still waiting for {why} after {} min; no account runs until it answers",
            waited.as_secs() / 60
        ),
        HubWaitNews::Answered { waited } => tracing::info!(
            waited_s = waited.as_secs(),
            "ARMING: the hubs answered after {} s; the start goes on",
            waited.as_secs()
        ),
    }
}
