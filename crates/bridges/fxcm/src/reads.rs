//! The read-only questions the gatewayhub's facts lane asks of an account's RUNNING FXCM session: its
//! currency and its current Offers table (decision 0183, [`crate::home_conversion`] judges the answers).
//!
//! # Why a process-wide registry, and why the RECONCILE session
//!
//! FXCM sessions are scarce and the daemon owns the one the account trades on, so a facts request must not
//! open another login (cTrader's lane opens a one-shot data session per rate; this venue cannot afford
//! that). The adapter already holds a SECOND session for reconciliation (`crate::recon_client`), logged in
//! for exactly this kind of read-only table question and idle between its interval passes, so the facts lane
//! asks that one: it never contends the exec session, and a rate read can never delay an order. The two
//! halves meet the way dukascopy's quote bus does (`vike_dukascopy::quotes::process_bus`): this crate's
//! reconcile client registers its handle here under the ACCOUNT's label (`""` = the default account) when
//! it comes up and removes it when it is dropped, and the gatewayhub's lane looks it up by the same label.
//!
//! **No reconcile session, no read**: with reconciliation off (`flags.reconcile_off`) or its login failed,
//! nothing is registered, the account's currency is then ASSUMED (the owner's ruling, said out loud) and a
//! conversion rate is unavailable, so a symbol quoted in another currency stays refused, in words.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::home_conversion::{
    AccountCcy, CcySource, ConvertedFacts, FactsError, Offer, PropertiesRead, account_ccy,
    facts_from_offers, parse_offers,
};

/// What the facts lane may ask of one account's running session. Implemented by the reconcile client's
/// request port; scripted in the tests.
pub trait SessionReads: Send + Sync {
    /// The login's system properties (`BASE_CRNCY` among them). A login-time read: it asks the venue nothing.
    fn system_properties(&self) -> PropertiesRead;

    /// The CURRENT Offers table as the shim's JSON array (a refresh-table request, bounded in the shim).
    ///
    /// # Errors
    /// The session is gone, the installed shim exports no `fc_offers`, or the refresh failed: the reason.
    fn offers(&self) -> Result<String, String>;
}

/// How long the facts lane waits for the session's system properties (a login-time read that asks the venue
/// nothing, queued behind whatever the session thread is doing), in total.
pub const SYSTEM_PROPERTIES_WAIT: Duration = Duration::from_secs(15);

/// How long the facts lane waits for the session's current Offers table, in total: the queue behind the
/// session's own work, ONE venue refresh (the shim bounds it at 8 s and the table is read with a buffer that
/// fits, so there is no second refresh) and the reply. Past it the ask fails as the rate's failure and the
/// session thread skips the request if it ever reaches it.
pub const OFFERS_WAIT: Duration = Duration::from_secs(20);

/// How long a currency statement that is NOT a currency code is remembered as a refusal, so that every start
/// does not re-read it under the adapter lock: a venue that states `US$` states it again next time.
pub const MALFORMED_KEEP: Duration = Duration::from_secs(300);

/// How long a currency ask the session did not answer is remembered, so that a stuck session costs one wait and
/// not one per start; short, because the session may well answer next time.
pub const UNANSWERED_KEEP: Duration = Duration::from_secs(20);

/// What an account's reads know of its currency.
enum Told {
    /// Not asked yet.
    Unknown,
    /// Decided, for the life of this registration.
    Decided(AccountCcy),
    /// Asked and refused (`why`); asked again once `until` has passed.
    Refused { why: String, until: Instant },
}

/// One account's reads and what it was told its currency is.
pub struct AccountReads {
    reads: Arc<dyn SessionReads>,
    /// The currency decided on the first ask and kept for the life of this registration (a new session is a
    /// new registration), so the facts' rates and the start's `account_ccy` are expressed in ONE currency.
    /// Never held across a read: a session that does not answer would otherwise stall every asker behind it.
    told: Mutex<Told>,
    /// How long a malformed statement and an unanswered ask are remembered ([`MALFORMED_KEEP`],
    /// [`UNANSWERED_KEEP`]).
    keeps: (Duration, Duration),
}

impl AccountReads {
    /// A reads handle over `reads`, with nothing decided yet.
    #[must_use]
    pub fn new(reads: Arc<dyn SessionReads>) -> Self {
        Self::with_keeps(reads, MALFORMED_KEEP, UNANSWERED_KEEP)
    }

    /// [`Self::new`] with the two refusal memories set (the tests shorten them).
    #[must_use]
    pub(crate) fn with_keeps(
        reads: Arc<dyn SessionReads>,
        malformed: Duration,
        unanswered: Duration,
    ) -> Self {
        Self { reads, told: Mutex::new(Told::Unknown), keeps: (malformed, unanswered) }
    }

    /// **The account's currency**, decided ONCE ([`account_ccy`]) and said once: the venue's `BASE_CRNCY`
    /// at `info!`, or USD ASSUMED at `warn!` in the words the owner asked for. The read happens with no lock
    /// held (two first askers may both read; the first to finish decides and the other returns its answer), a
    /// refusal is remembered for a while ([`MALFORMED_KEEP`], [`UNANSWERED_KEEP`]) and asked again after it,
    /// and an ask the session did not answer decides nothing: it is not the venue's silence, so it is not
    /// assumed over.
    ///
    /// # Errors
    /// The venue stated something that is not a currency code, or the session did not answer (see
    /// [`account_ccy`]).
    pub fn account_currency(&self) -> Result<AccountCcy, String> {
        match &*self.told.lock().unwrap_or_else(PoisonError::into_inner) {
            Told::Decided(known) => return Ok(known.clone()),
            Told::Refused { why, until } if Instant::now() < *until => return Err(why.clone()),
            Told::Unknown | Told::Refused { .. } => {}
        }
        let read = self.reads.system_properties();
        let unanswered = matches!(read, PropertiesRead::Failed(_));
        let decided = account_ccy(read);
        let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
        if let Told::Decided(first) = &*told {
            return Ok(first.clone());
        }
        match decided {
            Ok(ccy) => {
                say(&ccy, true);
                *told = Told::Decided(ccy.clone());
                Ok(ccy)
            }
            Err(why) => {
                let keep = if unanswered { self.keeps.1 } else { self.keeps.0 };
                tracing::warn!(
                    target: "vike_fxcm",
                    venue = "fxcm",
                    "{why}; asked again in {} s",
                    keep.as_secs()
                );
                *told = Told::Refused { why: why.clone(), until: Instant::now() + keep };
                Err(why)
            }
        }
    }

    /// **The facts of `symbol`** for a node that converts a quote currency: one fresh Offers read, the
    /// account's currency as already told, the rate stamped with `ask_ms()` read BEFORE the request goes out
    /// (the rate is no newer than the moment it was asked for).
    ///
    /// # Errors
    /// [`FactsError::Rate`] when the account's currency is unusable or the table cannot be read (nothing is
    /// known about the symbol), else what [`facts_from_offers`] says about the symbol and its conversion.
    pub fn quoted_facts(
        &self,
        symbol: &str,
        ask_ms: &dyn Fn() -> i64,
    ) -> Result<ConvertedFacts, FactsError> {
        let account = self.account_currency().map_err(FactsError::Rate)?;
        let as_of_ms = ask_ms();
        let offers = self.offers().map_err(FactsError::Rate)?;
        facts_from_offers(&offers, symbol, &account, as_of_ms)
    }

    /// The CURRENT Offers table, parsed (one fresh read through the session).
    ///
    /// # Errors
    /// The table cannot be read, or is not an Offers table.
    pub fn offers(&self) -> Result<Vec<Offer>, String> {
        parse_offers(&self.reads.offers()?)
    }
}

/// Say how the account's currency is known, once, when it is decided.
///
/// `asked` is whether the venue's session was ASKED at all: with reconciliation off (or its login failed) no
/// session is registered, the venue is never asked, and the code goes straight to the assumption, which the
/// words must not present as "the venue did not report it" ([`assumed_words`]).
fn say(ccy: &AccountCcy, asked: bool) {
    match &ccy.source {
        CcySource::Venue => tracing::info!(
            target: "vike_fxcm",
            venue = "fxcm",
            currency = %ccy.code,
            "fxcm: the venue states the account's currency ({}) in its system properties",
            ccy.code
        ),
        CcySource::Assumed { why } => tracing::warn!(
            target: "vike_fxcm",
            venue = "fxcm",
            currency = %ccy.code,
            asked,
            "{}",
            assumed_words(&ccy.code, why, asked)
        ),
    }
}

/// The warn line for an ASSUMED account currency, pure so the two cases are pinned apart: the venue was asked and
/// stated none (its words say why), or it was never asked.
fn assumed_words(code: &str, why: &str, asked: bool) -> String {
    let consequence = "The node uses this only for a `demo` account with `policy.max_account_exposure` \
                       set; a live or paper account, or one with no ceiling, keeps refusing a symbol \
                       quoted in another currency";
    if asked {
        format!(
            "fxcm account currency not reported by the venue: assuming {code}, verify with the drill \
             ({why}). {consequence}"
        )
    } else {
        format!(
            "fxcm account currency NOT ASKED: the venue was never asked ({why}), so {code} is assumed \
             with no statement from the venue at all; verify with the drill. {consequence}"
        )
    }
}

// ---- the registry ----------------------------------------------------------------------------------

type Registry = Mutex<HashMap<String, Arc<AccountReads>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

/// Register `reads` as account `account`'s running session (`""` = the default account), replacing any
/// earlier one (a restarted account). The handle returned is what [`unregister`] takes back out.
pub fn register(account: &str, reads: Arc<dyn SessionReads>) -> Arc<AccountReads> {
    let entry = Arc::new(AccountReads::new(reads));
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(account.to_string(), Arc::clone(&entry));
    entry
}

/// Remove `entry` from the registry, but only if it is still the one registered for `account`: a restart that
/// registered its successor first must not lose it to the old session's drop.
pub fn unregister(account: &str, entry: &Arc<AccountReads>) {
    let mut held = registry().lock().unwrap_or_else(PoisonError::into_inner);
    if held.get(account).is_some_and(|current| Arc::ptr_eq(current, entry)) {
        held.remove(account);
    }
}

/// Account `account`'s registered reads, if its reconcile session is up.
#[must_use]
pub fn account_reads(account: &str) -> Option<Arc<AccountReads>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner).get(account).cloned()
}

/// **The account's currency for the start's reply** (`AdapterReply::Live::account_ccy` and its `assumed`
/// flag): the venue's word, or USD ASSUMED when the API cannot give it, which includes an account with no
/// reconcile session to ask. `Err` only for a malformed statement.
///
/// # Errors
/// The venue stated something that is not a currency code.
pub fn told_account_currency(account: &str) -> Result<AccountCcy, String> {
    match account_reads(account) {
        Some(reads) => reads.account_currency(),
        // Nothing to ask: the API cannot give it, so the ruling's USD, flagged, and not remembered (a later
        // registration decides afresh).
        None => {
            let decided = account_ccy(PropertiesRead::Unreadable(
                "no reconcile session is running for this account: reconciliation is off, or its login \
                 failed"
                    .to_string(),
            ))?;
            say(&decided, false);
            Ok(decided)
        }
    }
}

/// **The facts of `symbol` for account `account`** ([`AccountReads::quoted_facts`]), stamped on this
/// process's clock.
///
/// # Errors
/// There is no reconcile session to ask ([`FactsError::Rate`]), or see [`AccountReads::quoted_facts`].
pub fn quoted_facts(account: &str, symbol: &str) -> Result<ConvertedFacts, FactsError> {
    let Some(reads) = account_reads(account) else {
        return Err(FactsError::Rate(
            "fxcm: no reconcile session is running for this account, so the venue's quotes cannot be \
             read (a second login is not opened: FXCM sessions are scarce), and a symbol quoted in \
             another currency stays refused"
                .to_string(),
        ));
    };
    reads.quoted_facts(symbol, &vike_model::now_ms)
}

#[path = "reads_tests.rs"]
#[cfg(test)]
mod reads_tests;
