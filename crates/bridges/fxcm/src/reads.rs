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
//! conversion rate is unavailable, so a symbol quoted in another currency stays refused, in words. A login
//! that failed is retried (`crate::redial`): the retry that opens registers the session as a start does, and
//! the lookups below find it from then on; a currency a START already told as assumed is not re-told.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::home_conversion::{
    AccountCcy, CcySource, ConvertedFacts, FactsError, Offer, PropertiesRead, account_ccy,
    facts_from_offers, parse_offers,
};
use crate::subscription::{Attempts, Failure, Next, undone_words, wanted};

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

    /// **Make `instrument` (`EUR/USD`) tradable** on the login: FXCM's `SetSubscriptionStatus`, the ONE write this
    /// session makes. Bounded ([`SUBSCRIBE_WAIT`] to the asker, the shim's own budget inside it). A default that
    /// refuses, so a session that cannot (a scripted one, an older implementation) leaves the symbol exactly as
    /// refused as it was, in words.
    ///
    /// # Errors
    /// The venue refused, the instrument is not listed, the session did not answer, or the installed shim cannot
    /// ask: [`Failure`] carries what FXCM (or the SDK) said.
    fn subscribe(&self, instrument: &str) -> Result<(), Failure> {
        Err(Failure {
            why: format!("fxcm: this session cannot subscribe `{instrument}` for trading"),
            transient: false,
        })
    }
}

/// How long the asker waits for one subscription, in total: the queue behind the session's own work, the shim's
/// whole budget ([`crate::subscription::RECON_BUDGET_MS`], 25 s) and the reply.
pub const SUBSCRIBE_WAIT: Duration = Duration::from_secs(45);

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
    /// What this registration (= this session) already asked FXCM to subscribe, and how it went
    /// ([`crate::subscription::Attempts`]). NEVER held across the ask (see [`Self::read_subscribing`]): the facts lane
    /// and the price lane meet here, and one slow subscription must not stop the other.
    attempts: Mutex<Attempts>,
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
        Self {
            reads,
            told: Mutex::new(Told::Unknown),
            keeps: (malformed, unanswered),
            attempts: Mutex::new(Attempts::default()),
        }
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
        let (offers, as_of_ms, refused) = self
            .read_subscribing(ask_ms, |offers| wanted(offers, symbol, &account.code))
            .map_err(FactsError::Rate)?;
        facts_from_offers(&offers, symbol, &account, as_of_ms)
            .map_err(|error| name_the_refusals(error, &refused))
    }

    /// **The current Offers table, after the rows `wanted` names that FXCM holds as not tradable have been
    /// subscribed** (once per instrument per session, [`Attempts`]), the moment it was asked for (`ask_ms()` read
    /// BEFORE the read that produced it) and the refusals, in FXCM's words, of the subscriptions that did not come
    /// to pass. The table is read again when a subscription was sent, so the facts are judged on a table that
    /// shows it.
    ///
    /// # Errors
    /// The table cannot be read.
    pub(crate) fn read_subscribing(
        &self,
        ask_ms: &dyn Fn() -> i64,
        wanted: impl FnOnce(&[Offer]) -> Vec<String>,
    ) -> Result<(Vec<Offer>, i64, Vec<Failure>), String> {
        let mut as_of_ms = ask_ms();
        let mut offers = self.offers()?;
        let mut refused = Vec::new();
        let mut sent = false;
        // The table was re-read once because ANOTHER asker's subscription may have landed after it was read.
        let mut reread = false;
        for name in wanted(&offers) {
            // ⚠ The memory is locked for the lookup and for the record, NEVER across the ask: a subscription can
            // wait for the session thread for up to `SUBSCRIBE_WAIT`, and the price lane's two-second poll and the
            // control door would queue behind it. Two askers of one instrument may therefore both send; the request
            // is idempotent (FXCM answers a row that already reads `T` like any other), and the second record wins.
            let step = self
                .attempts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .next(&name, Instant::now());
            match step {
                Next::Done => {
                    // Subscribed by this bridge, and this table says the row is not tradable. It may only be OLDER than
                    // that subscription (another asker's landed between this table's read and now): judge it on a
                    // FRESH read, once, and call it undone only if the fresh row still is not tradable.
                    if !reread {
                        as_of_ms = ask_ms();
                        offers = self.offers()?;
                        reread = true;
                    }
                    let status = offers
                        .iter()
                        .find(|o| o.instrument == name)
                        .map_or("?", |o| o.subscription.as_str());
                    if status != crate::subscription::TRADABLE {
                        refused.push(undone_words(&name, status));
                    }
                }
                Next::Refused(failure) => refused.push(failure),
                Next::Ask => {
                    let result = self.reads.subscribe(&name);
                    self.attempts.lock().unwrap_or_else(PoisonError::into_inner).record(
                        &name,
                        &result,
                        Instant::now(),
                    );
                    match result {
                        Ok(()) => sent = true,
                        Err(failure) => {
                            tracing::warn!(
                                target: "vike_fxcm",
                                venue = "fxcm",
                                instrument = %name,
                                "{}",
                                failure.why
                            );
                            refused.push(failure);
                        }
                    }
                }
            }
        }
        if sent {
            as_of_ms = ask_ms();
            offers = self.offers()?;
        }
        Ok((offers, as_of_ms, refused))
    }

    /// The Offers table the PRICE lane judges `symbols` on (six-letter spelling): the table of
    /// [`Self::read_subscribing`] for the rows among them that FXCM holds as not tradable. A refusal is told at
    /// `warn` once per attempt (the memory of [`Attempts`] bounds how often), and the lane's own judgement then
    /// reports the symbol as not priced.
    ///
    /// # Errors
    /// The table cannot be read.
    pub(crate) fn priced_offers(&self, symbols: &[String]) -> Result<Vec<Offer>, String> {
        let (offers, _, _) = self.read_subscribing(&vike_model::now_ms, |offers| {
            symbols
                .iter()
                .map(|s| crate::exec::to_fxcm_instrument(s))
                .filter(|name| {
                    offers
                        .iter()
                        .find(|o| &o.instrument == name)
                        .is_some_and(crate::subscription::needs_subscribing)
                })
                .collect()
        })?;
        Ok(offers)
    }

    /// The CURRENT Offers table, parsed (one fresh read through the session).
    ///
    /// # Errors
    /// The table cannot be read, or is not an Offers table.
    pub fn offers(&self) -> Result<Vec<Offer>, String> {
        parse_offers(&self.reads.offers()?)
    }
}

/// `error` with the reasons FXCM gave for not subscribing what it needed, so that an operator reads WHAT FXCM said
/// and not the bare "not subscribed". The kind follows the cause: an unknown outcome (or a rate that is only
/// unavailable now) stays [`FactsError::Rate`], which the node forgets; a refusal of the symbol stays a verdict.
fn name_the_refusals(error: FactsError, refused: &[Failure]) -> FactsError {
    if refused.is_empty() {
        return error;
    }
    let said = refused.iter().map(|f| f.why.as_str()).collect::<Vec<_>>().join("; ");
    let message = format!("{}; {said}", error.message());
    if error.is_rate() || refused.iter().any(|f| f.transient) {
        FactsError::Rate(message)
    } else {
        FactsError::Symbol(message)
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

/// Each entry is stamped with the GENERATION of the start that made it ([`next_generation`]).
type Registry = Mutex<HashMap<String, (u64, Arc<AccountReads>)>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

/// The generation of a START, taken when it begins: later starts get larger numbers. A session that opens
/// LATE (a retried login, `crate::redial`) registers under the generation of the start that began it, so
/// that it cannot replace the session of a start that came after it ([`register_generation`]).
#[must_use]
pub fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Register `reads` as account `account`'s running session (`""` = the default account), replacing any
/// earlier one (a restarted account). The handle returned is what [`unregister`] takes back out.
pub fn register(account: &str, reads: Arc<dyn SessionReads>) -> Arc<AccountReads> {
    let entry = Arc::new(AccountReads::new(reads));
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(account.to_string(), (next_generation(), Arc::clone(&entry)));
    entry
}

/// [`register`] for a session whose start has generation `generation`, or `None` when the registry already
/// holds a NEWER start's session for `account`: the gatewayhub tears a replaced adapter down with a bounded
/// join (10 s) and a login can take 30 s, so an old start's retry can open after its successor registered, and
/// must not take the successor's place (nor, by its drop, empty the registry under a live session).
pub fn register_generation(
    account: &str,
    reads: Arc<dyn SessionReads>,
    generation: u64,
) -> Option<Arc<AccountReads>> {
    let mut held = registry().lock().unwrap_or_else(PoisonError::into_inner);
    if held.get(account).is_some_and(|(newest, _)| *newest > generation) {
        return None;
    }
    let entry = Arc::new(AccountReads::new(reads));
    held.insert(account.to_string(), (generation, Arc::clone(&entry)));
    Some(entry)
}

/// Remove `entry` from the registry, but only if it is still the one registered for `account`: a restart that
/// registered its successor first must not lose it to the old session's drop.
pub fn unregister(account: &str, entry: &Arc<AccountReads>) {
    let mut held = registry().lock().unwrap_or_else(PoisonError::into_inner);
    if held.get(account).is_some_and(|(_, current)| Arc::ptr_eq(current, entry)) {
        held.remove(account);
    }
}

/// Account `account`'s registered reads, if its reconcile session is up.
#[must_use]
pub fn account_reads(account: &str) -> Option<Arc<AccountReads>> {
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(account)
        .map(|(_, entry)| Arc::clone(entry))
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
