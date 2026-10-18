//! **The lane's half of an account's leverage on a venue that POSTS one** (decision 0182): bybit, okx
//! and binance. The bridge's exec thread stays the ONLY writer of the venue's leverage; this lane only
//! READS ([`vike_bridge_core::leverage_read::LeverageReader`] has no write method), at two points of an
//! account's start:
//!
//! 1. **before the bridge starts** ([`probe`]): can this account safely be asked to hold N? A yes hands
//!    the bridge a profile whose `max_leverage` is N ([`profile_with`]), so the exec thread's ONE
//!    existing start-time POST carries the account's number instead of the profile's or the default 2x;
//!    a no hands it the profile unchanged, which is today's start byte for byte;
//! 2. **after it** ([`confirm`], from `TradeLane::leverage_cap`): does the venue now hold exactly N?
//!    The answer travels to the tradehub, which applies the figure only on an `Ok` equal to its ask.
//!
//! ## When anything here runs at all
//!
//! Only for a start that carries `AdapterStart::account_leverage` AND whose venue is a `Posted` row of
//! `vike_secrets::LEVERAGE_VENUES` open at the account's tier AND whose OWN account row (re-read here,
//! the node's reading is not trusted) states a leverage at least as large as the ask. Every other start —
//! no field, an older tradehub, a `live` row of a demo-only venue, a withheld (data-only) start, a
//! cleared row — takes none of these steps and no venue call is made: [`asked_figure`] is the one gate.
//!
//! The readers are the bridges' own (`vike_bybit::leverage_read`, `vike_okx::leverage_read`,
//! `vike_binance::leverage_read`), built per call from the account's credentials and thrown away; the
//! credentials never leave this process and no sentence built here carries one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_bridge_core::leverage_read::{self, LeverageReader};
use vike_gatewayhub_client::proto::AdapterStart;
use vike_model::AccountTier;
use vike_model::ProfileRisk;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::LeverageMeans;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::account_directory::AccountDirectory;

use crate::trade::lane::TradeTarget;

/// Builds the READ-ONLY reader for `(venue, the account's credentials, tier, symbol)`.
pub type ReaderFactory = Arc<
    dyn Fn(
            &str,
            &AccountCredentials<'_>,
            AccountTier,
            &str,
        ) -> Result<Box<dyn LeverageReader>, String>
        + Send
        + Sync,
>;

/// How the lane reads a posting venue: the reader factory and the clock the poll waits on.
#[derive(Clone)]
pub struct PostedReads {
    reader: ReaderFactory,
    pause: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl PostedReads {
    /// The production wiring: the bridges' own readers over their signed transports, and real sleeping.
    #[must_use]
    pub fn bridges() -> Self {
        // The parameter types are spelled out: a closure coerced to a `dyn Fn` over references is only
        // general in their lifetimes when it says they are references.
        let reader = |venue: &str,
                      creds: &AccountCredentials<'_>,
                      tier: AccountTier,
                      symbol: &str|
         -> Result<Box<dyn LeverageReader>, String> {
            match venue {
                "bybit" => vike_bybit::leverage_read::leverage_reader(creds, tier, symbol)
                    .map(|r| Box::new(r) as Box<dyn LeverageReader>),
                "okx" => vike_okx::leverage_read::leverage_reader(creds, tier, symbol)
                    .map(|r| Box::new(r) as Box<dyn LeverageReader>),
                "binance" => vike_binance::leverage_read::leverage_reader(creds, tier, symbol)
                    .map(|r| Box::new(r) as Box<dyn LeverageReader>),
                other => Err(format!("{other} has no leverage reader in this build")),
            }
        };
        PostedReads { reader: Arc::new(reader), pause: Arc::new(std::thread::sleep) }
    }

    /// A lane over doubles: a scripted reader factory and a fake clock.
    #[cfg(test)]
    pub(crate) fn scripted(
        reader: ReaderFactory,
        pause: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Self {
        PostedReads { reader, pause }
    }

    fn reader_for(
        &self,
        accounts: &AccountDirectory,
        target: &TradeTarget,
        label: &AccountLabel,
        symbol: &str,
    ) -> Result<Box<dyn LeverageReader>, String> {
        let creds = accounts.credentials_for(&target.venue, label, target.tier);
        (self.reader)(&target.venue, &creds, target.tier, symbol)
    }
}

/// **What each account's LAST start did about the venue's leverage**, kept so the read-back can tell a
/// venue that holds the figure BECAUSE the bridge was told to write it from one that happens to hold it.
///
/// `Ok(figure)` = the probe passed and the bridge was handed `figure` as its profile's number; `Err(why)` =
/// the probe refused and the bridge was started with the profile untouched (so it will write the default,
/// whatever the venue held before). Without this a transient read error at the start (refusal, default
/// written) followed by a clean probe after it could read back the figure a PREVIOUS run left on the venue
/// and call it confirmed, while the venue was about to be written the default. Cleared when the account's
/// adapter is (re)started, set by that start; shared by every generation of the lane.
#[derive(Default)]
pub struct Handovers {
    held: Mutex<HashMap<String, Result<f64, String>>>,
}

impl Handovers {
    /// Forget `account` (its adapter is being started: only THIS start may record).
    pub(crate) fn forget(&self, account: &str) {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).remove(account);
    }

    /// Record what the start did for `account`.
    pub(crate) fn record(&self, account: &str, outcome: Result<f64, String>) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_string(), outcome);
    }

    /// What the start recorded for `account`, if it recorded anything.
    pub(crate) fn of(&self, account: &str) -> Option<Result<f64, String>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).get(account).cloned()
    }
}

/// **Whether the read-back may run at all**: the start must have handed the bridge exactly `candidate`.
///
/// # Errors
/// The start's probe refused (its reason, unchanged), or the start recorded nothing for this ask.
pub(crate) fn written_by_the_start(
    handovers: &Handovers,
    account: &str,
    candidate: f64,
) -> Result<(), String> {
    match handovers.of(account) {
        Some(Ok(handed)) if vike_bridge_core::leverage_read::same_leverage(handed, candidate) => {
            Ok(())
        }
        Some(Ok(handed)) => Err(format!(
            "the start handed the bridge {handed}x, not the {candidate}x now asked: set the account's \
             `account.active` off and on to start it again"
        )),
        Some(Err(why)) => Err(format!("the start did not write it to the venue: {why}")),
        None => {
            Err("the start did not ask the venue for it (no record of a hand-over)".to_string())
        }
    }
}

/// **The whole-number figure this start asks the venue to hold, or `None` when it asks nothing** —
/// the ONE gate in front of every venue call this module makes (see the module doc for the list).
#[must_use]
pub(crate) fn asked_figure(
    accounts: &AccountDirectory,
    target: &TradeTarget,
    label: &AccountLabel,
    start: &AdapterStart,
) -> Option<f64> {
    let candidate = start.account_leverage?;
    // A withheld start is a DATA-ONLY start: its credentials are not to be used, not even to read.
    if start.withhold.is_some() {
        return None;
    }
    let row = vike_secrets::leverage_venue_at(&target.venue, target.tier)?;
    if row.means != LeverageMeans::Posted {
        return None;
    }
    if !candidate.is_finite() || candidate < 1.0 || candidate.fract() != 0.0 {
        tracing::warn!(
            venue = %target.venue,
            "{}: the start asks for {candidate}x, which is not a whole number of at least 1: nothing \
             is asked of the venue",
            target.venue
        );
        return None;
    }
    // The node's reading of the row is not trusted: the lane reads the account's row itself, and a
    // figure the row no longer states (cleared, lowered) or the table refuses is asked of nobody.
    let Some(Ok(rows)) = accounts.rows() else { return None };
    let stated = rows.leverage_of(&target.venue, label.text())?;
    if vike_secrets::leverage_refusal(&target.venue, target.tier, stated.get()).is_some() {
        return None;
    }
    if candidate > stated.get() {
        tracing::warn!(
            venue = %target.venue,
            "{}: the start asks for {candidate}x but the account row states {}x: nothing is asked of \
             the venue",
            target.venue,
            stated.get()
        );
        return None;
    }
    Some(candidate)
}

/// **Before the start**: whether the venue may be asked to hold `candidate` (read-only; the
/// refusal reasons are `vike_bridge_core::leverage_read::probe`'s).
///
/// # Errors
/// The reader could not be built (no key set), or the venue's answer refuses the figure.
pub(crate) fn probe(
    reads: &PostedReads,
    accounts: &AccountDirectory,
    target: &TradeTarget,
    label: &AccountLabel,
    symbol: &str,
    candidate: f64,
) -> Result<(), String> {
    let reader = reads.reader_for(accounts, target, label, symbol)?;
    leverage_read::probe(&*reader, candidate)
}

/// **After the start**: see the venue hold `candidate` (the probe again, then the bounded poll). `Ok`
/// carries the figure the venue was SEEN to hold, equal to `candidate`.
///
/// # Errors
/// The reader could not be built, the probe now refuses, a read failed, or the venue never held it.
pub(crate) fn confirm(
    reads: &PostedReads,
    accounts: &AccountDirectory,
    target: &TradeTarget,
    label: &AccountLabel,
    symbol: &str,
    candidate: f64,
) -> Result<f64, String> {
    let reader = reads.reader_for(accounts, target, label, symbol)?;
    leverage_read::confirm(&*reader, candidate, |wait| (reads.pause)(wait))
}

/// The profile the bridge is started with when the probe said yes: the start's own profile (or an
/// empty one) with `max_leverage` set to `candidate`, which is what the exec thread's one POST sends.
/// The bridges read nothing else of it (`vike_bridge_core::leverage::resolve_startup_leverage`).
#[must_use]
pub(crate) fn profile_with(profile: Option<ProfileRisk>, candidate: f64) -> ProfileRisk {
    ProfileRisk { max_leverage: Some(candidate), ..profile.unwrap_or_default() }
}

#[path = "posted_leverage_tests.rs"]
#[cfg(test)]
mod posted_leverage_tests;
